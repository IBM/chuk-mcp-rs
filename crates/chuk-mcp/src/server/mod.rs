//! High-level MCP server, mirroring `chuk_mcp.server`.

pub mod discover;
pub mod http;
pub mod modern;
pub mod prompts;
pub mod protocol_handler;
pub mod session;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serde_json::{json, Value};

use crate::protocol::json_rpc::{create_error_response, JsonRpcMessage};
use crate::protocol::messages::method::MessageMethod;
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
    prompts: prompts::PromptRegistry,
    max_buffer_size: usize,
    /// Optional natural-language guidance for a model on using this server,
    /// returned by `server/discover`.
    instructions: Option<String>,
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
            prompts: prompts::PromptRegistry::new(),
            max_buffer_size: crate::transports::limits::DEFAULT_MAX_BUFFER_SIZE,
            instructions: None,
        }
    }

    /// Natural-language guidance for a model on how to use this server,
    /// returned by `server/discover`.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Set the maximum bytes buffered for a single inbound message (0 disables).
    ///
    /// Bounds how much a client can send without a newline before the server
    /// gives up, rather than accumulating it all in memory.
    pub fn with_max_buffer_size(mut self, max_buffer_size: usize) -> Self {
        self.max_buffer_size = max_buffer_size;
        self
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
        // A custom method handler registered on the protocol handler takes
        // precedence over the built-in tool/resource dispatch, so servers can
        // override tools/list, tools/call, etc. (matching the Python semantics).
        if let Some(method) = message.method() {
            if self.protocol_handler.has_custom_handler(method) {
                return self
                    .protocol_handler
                    .handle_message(message, session_id)
                    .await;
            }
        }
        // A modern request declares its version on every call, and a server
        // that cannot speak it owes the client the list it can — a bare
        // rejection leaves nothing to renegotiate from.
        let modern = modern::is_modern_request(&message);
        if let Some((code, text, data)) = modern::reject_unsupported_version(&message) {
            if let Some(id) = message.id().cloned() {
                return (
                    Some(JsonRpcMessage::Error(create_error_response(
                        id, code, &text, data,
                    ))),
                    None,
                );
            }
        }

        let (mut response, session) = self.dispatch(message, session_id).await;
        if let Some(response) = response.as_mut() {
            modern::finish_response(response, modern);
        }
        (response, session)
    }

    /// Route one message to whatever answers it.
    async fn dispatch(&self, message: JsonRpcMessage, session_id: Option<&str>) -> HandlerResult {
        match message.method() {
            Some(MessageMethod::SERVER_DISCOVER) => (self.handle_discover(&message), None),
            Some("tools/list") => (self.handle_tools_list(&message), None),
            Some("tools/call") => (self.handle_tools_call(&message).await, None),
            Some("resources/list") => (self.handle_resources_list(&message), None),
            Some("resources/read") => (self.handle_resources_read(&message).await, None),
            Some(MessageMethod::PROMPTS_LIST) => (self.handle_prompts_list(&message), None),
            Some(MessageMethod::PROMPTS_GET) => (self.handle_prompts_get(&message).await, None),
            _ => {
                self.protocol_handler
                    .handle_message(message, session_id)
                    .await
            }
        }
    }

    /// Answer `server/discover`, which replaces `initialize` in the modern era
    /// and establishes nothing: it may be asked at any time, by anyone.
    fn handle_discover(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        let handler = &self.protocol_handler;
        Some(self.protocol_handler.create_response(
            id,
            Some(discover::discover_result(
                handler.server_info(),
                handler.capabilities(),
                self.instructions.as_deref(),
            )),
        ))
    }

    /// Register a prompt: a named template a model can ask to be rendered.
    pub fn register_prompt<F, Fut>(
        &mut self,
        name: &str,
        description: &str,
        arguments: Vec<crate::protocol::messages::prompts::PromptArgument>,
        handler: F,
    ) where
        F: Fn(serde_json::Map<String, Value>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<
                Output = Result<Vec<crate::protocol::messages::prompts::PromptMessage>, String>,
            > + Send
            + 'static,
    {
        let handler = std::sync::Arc::new(handler);
        self.prompts.insert(
            name.to_string(),
            prompts::RegisteredPrompt {
                definition: crate::protocol::messages::prompts::Prompt {
                    name: name.to_string(),
                    description: Some(description.to_string()),
                    arguments: (!arguments.is_empty()).then_some(arguments),
                    extra: serde_json::Map::new(),
                },
                handler: std::sync::Arc::new(move |args| {
                    let handler = handler.clone();
                    Box::pin(async move { handler(args).await })
                }),
            },
        );
        tracing::debug!("Registered prompt: {name}");
    }

    fn handle_prompts_list(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        Some(
            self.protocol_handler
                .create_response(id, Some(prompts::list_result(&self.prompts))),
        )
    }

    async fn handle_prompts_get(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        let params = protocol_handler::params_object(message);
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = params
            .get("arguments")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        Some(
            match prompts::get_result(&self.prompts, name, arguments).await {
                Ok(result) => self.protocol_handler.create_response(id, Some(result)),
                Err((code, text)) => self.protocol_handler.create_error_response(id, code, &text),
            },
        )
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
        let reader = tokio::io::BufReader::new(tokio::io::stdin());
        self.serve(reader, tokio::io::stdout()).await
    }

    /// Serve newline-delimited JSON-RPC over the given reader/writer until the
    /// reader reaches EOF. [`run_stdio`](Self::run_stdio) is this with
    /// stdin/stdout.
    pub async fn serve<R, W>(
        &self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), crate::protocol::types::errors::McpError>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;

        let mut session_id: Option<String> = None;

        // Bounded read: a client that never terminates a line would otherwise
        // grow this buffer until the server runs out of memory.
        while let Some(line) = crate::transports::limits::read_line_bounded(
            &mut reader,
            self.max_buffer_size,
            "inbound message",
        )
        .await?
        {
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
                writer.write_all(json.as_bytes()).await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }
}

/// Format a tool handler's result as MCP content blocks, matching the Python
/// `MCPServer._format_content`.
/// Whether a handler's value is already a complete result rather than content
/// to wrap.
///
/// A tool that needs more input returns an `input_required` result; wrapping it
/// in content blocks would turn a question into a paragraph of JSON the client
/// would read as an answer.
fn is_input_required(value: &Value) -> bool {
    value
        .get("resultType")
        .and_then(Value::as_str)
        .map(|kind| kind == crate::protocol::mrtr::RESULT_TYPE_INPUT_REQUIRED)
        .unwrap_or(false)
}

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
