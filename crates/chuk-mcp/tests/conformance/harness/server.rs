//! An in-process server fixture, driven one message at a time.
//!
//! Server-side conformance asks what the server puts on the wire in response
//! to a given request, which `McpServer::handle_message` answers directly —
//! no transport, no subprocess, and so no way for a timing artefact to be
//! mistaken for a protocol answer.

use serde_json::{json, Value};

use chuk_mcp::protocol::json_rpc::{parse_message, RequestId};
use chuk_mcp::protocol::types::capabilities::{
    ResourcesCapability, ServerCapabilities, ToolsCapability,
};
use chuk_mcp::server::McpServer;

/// Identity the fixture server reports.
pub const SERVER_NAME: &str = "conformance-server";
pub const SERVER_VERSION: &str = "1.0.0";

/// The tool the fixture registers, and its one argument.
pub const TOOL_NAME: &str = "greet";
pub const TOOL_ARGUMENT: &str = "name";
/// A tool name the fixture deliberately does not register.
pub const UNKNOWN_TOOL_NAME: &str = "no-such-tool";

/// The resource the fixture registers.
pub const RESOURCE_URI: &str = "conformance://motd";
pub const RESOURCE_NAME: &str = "motd";
pub const RESOURCE_MIME_TYPE: &str = "text/plain";
pub const RESOURCE_BODY: &str = "hello";

/// A server with one tool and one resource — enough surface for every
/// server-side rule, and small enough that a rule's failure points at the
/// protocol rather than at the fixture.
pub fn fixture() -> McpServer {
    let capabilities = ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        resources: Some(ResourcesCapability::default()),
        ..Default::default()
    };
    let mut server = McpServer::new(SERVER_NAME, SERVER_VERSION, Some(capabilities));

    server.register_tool(
        TOOL_NAME,
        json!({
            "type": "object",
            "properties": {TOOL_ARGUMENT: {"type": "string"}},
            "required": [TOOL_ARGUMENT],
        }),
        "Greet someone by name",
        |arguments| async move {
            let name = arguments
                .get(TOOL_ARGUMENT)
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!(format!("Hello, {name}!")))
        },
    );

    server.register_resource(
        RESOURCE_URI,
        RESOURCE_NAME,
        "Message of the day",
        RESOURCE_MIME_TYPE,
        || async { Ok(RESOURCE_BODY.to_string()) },
    );

    server
}

/// Send one request to the fixture and return its response.
///
/// `None` means the server chose to answer nothing, which for a notification
/// is the required behaviour and for a request is a conformance failure — so
/// the distinction is the caller's to make, not this helper's.
pub async fn ask(server: &McpServer, request: Value) -> Option<Value> {
    let message =
        parse_message(&request).expect("fixture request must be a valid JSON-RPC message");
    let (response, _session) = server.handle_message(message, None).await;
    response.map(|message| message.to_value())
}

/// A JSON-RPC request with the given id.
pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// A JSON-RPC notification — no id, so no response is permitted.
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// The `error` member of a response, if it carries one.
pub fn error_of(response: &Value) -> Option<&Value> {
    response.get("error")
}

/// The `result` member of a response, if it carries one.
pub fn result_of(response: &Value) -> Option<&Value> {
    response.get("result")
}

/// The response's id, as a request id.
pub fn id_of(response: &Value) -> Option<RequestId> {
    match response.get("id")? {
        Value::String(text) => Some(RequestId::Str(text.clone())),
        Value::Number(number) => number.as_i64().map(RequestId::Num),
        _ => None,
    }
}

/// Whether a message is a response at all, as opposed to a request echoed back.
pub fn is_response(response: &Value) -> bool {
    response.get("result").is_some() || response.get("error").is_some()
}
