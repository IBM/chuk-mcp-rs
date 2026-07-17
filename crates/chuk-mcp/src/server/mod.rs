//! High-level MCP server, mirroring `chuk_mcp.server`.

pub mod protocol_handler;
pub mod session;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serde_json::{json, Value};

use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::errors::{INTERNAL_ERROR, INVALID_PARAMS};
use crate::protocol::types::info::ServerInfo;

pub use protocol_handler::{method_handler, params_object, HandlerResult, ProtocolHandler};
pub use session::{SessionInfo, SessionManager};

/// An async tool handler: arguments object in, JSON value (or error text) out.
/// String results become text content; other values are pretty-printed JSON.
pub type ToolHandler =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync>;

/// An async resource handler: returns the resource body as a string.
pub type ResourceHandler =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

struct RegisteredTool {
    handler: ToolHandler,
    schema: Value,
    description: String,
}

struct RegisteredResource {
    handler: ResourceHandler,
    name: String,
    description: String,
    mime_type: String,
}

/// High-level MCP server: register tools and resources, then feed it
/// messages (e.g. via [`McpServer::run_stdio`]).
pub struct McpServer {
    pub protocol_handler: ProtocolHandler,
    tools: BTreeMap<String, RegisteredTool>,
    resources: BTreeMap<String, RegisteredResource>,
}

impl McpServer {
    pub fn new(name: &str, version: &str, capabilities: Option<ServerCapabilities>) -> Self {
        let server_info = ServerInfo::new(name, version);
        let capabilities = capabilities.unwrap_or_default();
        tracing::info!("Initialized MCP server: {name} v{version}");
        McpServer {
            protocol_handler: ProtocolHandler::new(server_info, capabilities),
            tools: BTreeMap::new(),
            resources: BTreeMap::new(),
        }
    }

    /// Register a tool with its JSON Schema and async handler.
    pub fn register_tool<F, Fut>(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        handler: F,
    ) where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.tools.insert(
            name.to_string(),
            RegisteredTool {
                handler: Arc::new(move |args| Box::pin(handler(args))),
                schema,
                description: description.to_string(),
            },
        );
        tracing::debug!("Registered tool: {name}");
    }

    /// Register a resource with its async handler.
    pub fn register_resource<F, Fut>(
        &mut self,
        uri: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        let name = if name.is_empty() {
            uri.rsplit('/').next().unwrap_or(uri).to_string()
        } else {
            name.to_string()
        };
        self.resources.insert(
            uri.to_string(),
            RegisteredResource {
                handler: Arc::new(move || Box::pin(handler())),
                name,
                description: description.to_string(),
                mime_type: if mime_type.is_empty() {
                    "text/plain".to_string()
                } else {
                    mime_type.to_string()
                },
            },
        );
        tracing::debug!("Registered resource: {uri}");
    }

    /// Handle one message, producing an optional response and possibly a new
    /// session id.
    pub async fn handle_message(
        &self,
        message: JsonRpcMessage,
        session_id: Option<&str>,
    ) -> HandlerResult {
        match message.method() {
            Some("tools/list") => (self.handle_tools_list(&message), None),
            Some("tools/call") => (self.handle_tools_call(&message).await, None),
            Some("resources/list") => (self.handle_resources_list(&message), None),
            Some("resources/read") => (self.handle_resources_read(&message).await, None),
            _ => {
                self.protocol_handler
                    .handle_message(message, session_id)
                    .await
            }
        }
    }

    fn handle_tools_list(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|(name, tool)| {
                json!({
                    "name": name,
                    "description": tool.description,
                    "inputSchema": tool.schema,
                })
            })
            .collect();
        let id = message.id()?.clone();
        Some(
            self.protocol_handler
                .create_response(id, Some(json!({"tools": tools}))),
        )
    }

    async fn handle_tools_call(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        let params = message.params().cloned().unwrap_or(Value::Null);
        let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        let Some(tool) = self.tools.get(tool_name) else {
            return Some(self.protocol_handler.create_error_response(
                id,
                INVALID_PARAMS,
                &format!("Unknown tool: {tool_name}"),
            ));
        };

        match (tool.handler)(arguments).await {
            Ok(result) => Some(
                self.protocol_handler
                    .create_response(id, Some(json!({"content": format_content(&result)}))),
            ),
            Err(e) => {
                tracing::error!("Tool execution error for {tool_name}: {e}");
                Some(self.protocol_handler.create_error_response(
                    id,
                    INTERNAL_ERROR,
                    &format!("Tool execution error: {e}"),
                ))
            }
        }
    }

    fn handle_resources_list(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let resources: Vec<Value> = self
            .resources
            .iter()
            .map(|(uri, resource)| {
                json!({
                    "uri": uri,
                    "name": resource.name,
                    "description": resource.description,
                    "mimeType": resource.mime_type,
                })
            })
            .collect();
        let id = message.id()?.clone();
        Some(
            self.protocol_handler
                .create_response(id, Some(json!({"resources": resources}))),
        )
    }

    async fn handle_resources_read(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        let params = message.params().cloned().unwrap_or(Value::Null);
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");

        let Some(resource) = self.resources.get(uri) else {
            return Some(self.protocol_handler.create_error_response(
                id,
                INVALID_PARAMS,
                &format!("Unknown resource: {uri}"),
            ));
        };

        match (resource.handler)().await {
            Ok(content) => Some(self.protocol_handler.create_response(
                id,
                Some(json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": resource.mime_type,
                        "text": content,
                    }]
                })),
            )),
            Err(e) => {
                tracing::error!("Resource read error for {uri}: {e}");
                Some(self.protocol_handler.create_error_response(
                    id,
                    INTERNAL_ERROR,
                    &format!("Resource read error: {e}"),
                ))
            }
        }
    }

    /// Serve over stdio: newline-delimited JSON-RPC on stdin/stdout, until
    /// stdin closes. This is how a subprocess-based MCP server runs.
    pub async fn run_stdio(&self) -> Result<(), crate::protocol::types::errors::McpError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut lines = BufReader::new(stdin).lines();
        let mut session_id: Option<String> = None;

        while let Some(line) = lines.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let message = match crate::protocol::json_rpc::parse_message_str(line) {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::error!("Invalid message: {e}");
                    continue;
                }
            };

            let (response, new_session) = self.handle_message(message, session_id.as_deref()).await;
            if let Some(new_session) = new_session {
                session_id = Some(new_session);
            }
            if let Some(response) = response {
                let mut json = response.to_json();
                json.push('\n');
                stdout.write_all(json.as_bytes()).await?;
                stdout.flush().await?;
            }
        }
        Ok(())
    }
}

/// Format a tool handler's result as MCP content blocks, matching the Python
/// `MCPServer._format_content`.
fn format_content(result: &Value) -> Vec<Value> {
    match result {
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(items) => items.iter().flat_map(format_content).collect(),
        Value::Object(_) => vec![json!({
            "type": "text",
            "text": serde_json::to_string_pretty(result).expect("serialize result"),
        })],
        other => vec![json!({"type": "text", "text": other.to_string()})],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::parse_message;

    fn server() -> McpServer {
        let mut server = McpServer::new("test-server", "1.0.0", None);
        server.register_tool(
            "greet",
            json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            "Say hello",
            |args| async move {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
                Ok(json!(format!("Hello, {name}!")))
            },
        );
        server.register_resource(
            "demo://greeting",
            "greeting",
            "A demo resource",
            "text/plain",
            || async { Ok("hi there".to_string()) },
        );
        server
    }

    #[tokio::test]
    async fn tools_roundtrip() {
        let server = server();

        let list =
            parse_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
        let (response, _) = server.handle_message(list, None).await;
        let result = response.unwrap();
        assert_eq!(result.result().unwrap()["tools"][0]["name"], json!("greet"));

        let call = parse_message(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "greet", "arguments": {"name": "Rust"}}
        }))
        .unwrap();
        let (response, _) = server.handle_message(call, None).await;
        let result = response.unwrap();
        assert_eq!(
            result.result().unwrap()["content"][0]["text"],
            json!("Hello, Rust!")
        );

        let bad_call = parse_message(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "missing", "arguments": {}}
        }))
        .unwrap();
        let (response, _) = server.handle_message(bad_call, None).await;
        assert_eq!(response.unwrap().error().unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn resources_roundtrip() {
        let server = server();
        let read = parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "resources/read",
            "params": {"uri": "demo://greeting"}
        }))
        .unwrap();
        let (response, _) = server.handle_message(read, None).await;
        assert_eq!(
            response.unwrap().result().unwrap()["contents"][0]["text"],
            json!("hi there")
        );
    }
}
