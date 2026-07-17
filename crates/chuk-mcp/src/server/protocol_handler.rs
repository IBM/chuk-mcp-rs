//! Server-side MCP protocol handler, mirroring `chuk_mcp.server.protocol_handler`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::Future;
use serde_json::{json, Map, Value};

use crate::protocol::json_rpc::{
    create_error_response, create_response, JsonRpcMessage, RequestId,
};
use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::errors::{INTERNAL_ERROR, INVALID_REQUEST, METHOD_NOT_FOUND};
use crate::protocol::types::info::ServerInfo;
use crate::server::session::SessionManager;

/// Result of handling a message: an optional response to send back, and an
/// optional newly-created session id.
pub type HandlerResult = (Option<JsonRpcMessage>, Option<String>);

/// An async handler for one JSON-RPC method: `(message, session_id) -> result`.
pub type MethodHandler = Arc<
    dyn Fn(
            JsonRpcMessage,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<HandlerResult, String>> + Send>>
        + Send
        + Sync,
>;

/// Server-side MCP protocol handler: sessions, core methods (`initialize`,
/// `ping`, `notifications/initialized`), and custom method registration.
pub struct ProtocolHandler {
    pub server_info: ServerInfo,
    pub capabilities: ServerCapabilities,
    pub session_manager: Arc<Mutex<SessionManager>>,
    handlers: HashMap<String, MethodHandler>,
}

impl ProtocolHandler {
    pub fn new(server_info: ServerInfo, capabilities: ServerCapabilities) -> Self {
        ProtocolHandler {
            server_info,
            capabilities,
            session_manager: Arc::new(Mutex::new(SessionManager::new())),
            handlers: HashMap::new(),
        }
    }

    /// Register a custom method handler (overrides built-ins on collision).
    pub fn register_method(&mut self, method: &str, handler: MethodHandler) {
        self.handlers.insert(method.to_string(), handler);
    }

    /// Handle an incoming message, producing an optional response.
    pub async fn handle_message(
        &self,
        message: JsonRpcMessage,
        session_id: Option<&str>,
    ) -> HandlerResult {
        // Responses/batches are not requests to handle.
        if message.is_batch() || message.is_response() || message.is_error_response() {
            return (None, None);
        }

        let Some(method) = message.method().map(str::to_string) else {
            return (
                self.error_for(&message, INVALID_REQUEST, "Invalid request"),
                None,
            );
        };

        if let Some(session_id) = session_id {
            self.session_manager
                .lock()
                .expect("session lock")
                .update_activity(session_id);
        }

        // Custom handlers take priority, then built-ins.
        if let Some(handler) = self.handlers.get(&method) {
            let result = handler(message.clone(), session_id.map(str::to_string)).await;
            return match result {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Handler error for {method}: {e}");
                    (
                        self.error_for(&message, INTERNAL_ERROR, &format!("Internal error: {e}")),
                        None,
                    )
                }
            };
        }

        match method.as_str() {
            "initialize" => self.handle_initialize(&message),
            "notifications/initialized" => (None, None),
            "ping" => (
                message
                    .id()
                    .map(|id| JsonRpcMessage::Response(create_response(id.clone(), None))),
                None,
            ),
            _ => (
                self.error_for(
                    &message,
                    METHOD_NOT_FOUND,
                    &format!("Method not found: {method}"),
                ),
                None,
            ),
        }
    }

    fn handle_initialize(&self, message: &JsonRpcMessage) -> HandlerResult {
        let params = message.params().cloned().unwrap_or(Value::Null);
        let client_info = params
            .get("clientInfo")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let protocol_version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-03-26")
            .to_string();

        let new_session_id = self
            .session_manager
            .lock()
            .expect("session lock")
            .create_session(client_info, &protocol_version, None);

        let result = json!({
            "protocolVersion": protocol_version,
            "serverInfo": serde_json::to_value(&self.server_info).expect("serialize server info"),
            "capabilities": serde_json::to_value(&self.capabilities)
                .expect("serialize capabilities"),
        });

        (
            message
                .id()
                .map(|id| JsonRpcMessage::Response(create_response(id.clone(), Some(result)))),
            Some(new_session_id),
        )
    }

    /// Build an error response bound to the message's id (or a null-ish id).
    fn error_for(&self, message: &JsonRpcMessage, code: i64, text: &str) -> Option<JsonRpcMessage> {
        let id = message
            .id()
            .cloned()
            .unwrap_or_else(|| RequestId::Str(String::new()));
        Some(JsonRpcMessage::Error(create_error_response(
            id, code, text, None,
        )))
    }

    /// Create a success response (helper mirroring the Python API).
    pub fn create_response(&self, id: RequestId, result: Option<Value>) -> JsonRpcMessage {
        JsonRpcMessage::Response(create_response(id, result))
    }

    /// Create an error response (helper mirroring the Python API).
    pub fn create_error_response(&self, id: RequestId, code: i64, message: &str) -> JsonRpcMessage {
        JsonRpcMessage::Error(create_error_response(id, code, message, None))
    }
}

/// Convenience for building [`MethodHandler`]s from async closures.
pub fn method_handler<F, Fut>(f: F) -> MethodHandler
where
    F: Fn(JsonRpcMessage, Option<String>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<HandlerResult, String>> + Send + 'static,
{
    Arc::new(move |msg, session| Box::pin(f(msg, session)))
}

/// Extract `params` as an object map from a request message.
pub fn params_object(message: &JsonRpcMessage) -> Map<String, Value> {
    message
        .params()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::parse_message;

    fn handler() -> ProtocolHandler {
        ProtocolHandler::new(
            ServerInfo::new("test", "1.0"),
            ServerCapabilities::default(),
        )
    }

    #[tokio::test]
    async fn initialize_creates_session() {
        let handler = handler();
        let msg = parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "c"}}
        }))
        .unwrap();

        let (response, session) = handler.handle_message(msg, None).await;
        let response = response.unwrap();
        assert_eq!(
            response.result().unwrap()["protocolVersion"],
            json!("2025-06-18")
        );
        assert!(session.is_some());
        assert_eq!(handler.session_manager.lock().unwrap().session_count(), 1);
    }

    #[tokio::test]
    async fn ping_and_unknown_method() {
        let handler = handler();
        let ping = parse_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})).unwrap();
        let (response, _) = handler.handle_message(ping, None).await;
        assert!(response.unwrap().is_response());

        let nope = parse_message(&json!({"jsonrpc": "2.0", "id": 3, "method": "bogus"})).unwrap();
        let (response, _) = handler.handle_message(nope, None).await;
        assert_eq!(response.unwrap().error().unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn custom_handler_dispatch() {
        let mut handler = handler();
        handler.register_method(
            "custom/echo",
            method_handler(|msg, _session| async move {
                let id = msg.id().cloned().unwrap();
                Ok((
                    Some(JsonRpcMessage::Response(create_response(
                        id,
                        msg.params().cloned(),
                    ))),
                    None,
                ))
            }),
        );

        let msg = parse_message(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "custom/echo", "params": {"x": 1}}),
        )
        .unwrap();
        let (response, _) = handler.handle_message(msg, None).await;
        assert_eq!(response.unwrap().result().unwrap()["x"], json!(1));
    }
}
