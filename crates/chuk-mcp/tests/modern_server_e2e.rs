//! Our own client against our own server, over the `2026-07-28` protocol.
//!
//! The server has spoken only the legacy lifecycle until now, so every modern
//! exchange in this repo has been verified against a hand-written peer. This
//! drives both real implementations against each other, which is the check that
//! a shared misreading of the spec cannot pass — the two sides were written
//! from the same document, but the client's reader and the server's writer are
//! separate code.

use serde_json::json;

use chuk_mcp::protocol::era::ServerProfile;
use chuk_mcp::protocol::json_rpc::{create_request, parse_message, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use chuk_mcp::protocol::types::capabilities::{ServerCapabilities, ToolsCapability};
use chuk_mcp::protocol::versioning;
use chuk_mcp::server::{modern, McpServer};

const TOOL_NAME: &str = "greet";
const TOOL_ARGUMENT: &str = "name";
const SERVER_NAME: &str = "modern-server";
const SERVER_VERSION: &str = "2.0.0";
const INSTRUCTIONS: &str = "Call greet before anything else.";

fn server() -> McpServer {
    let capabilities = ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut server = McpServer::new(SERVER_NAME, SERVER_VERSION, Some(capabilities))
        .with_instructions(INSTRUCTIONS);

    server.register_tool(
        TOOL_NAME,
        json!({
            "type": "object",
            "properties": {TOOL_ARGUMENT: {"type": "string"}},
            "required": [TOOL_ARGUMENT],
        }),
        "Greet someone",
        |arguments| async move {
            let name = arguments
                .get(TOOL_ARGUMENT)
                .and_then(|value| value.as_str())
                .unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );
    server
}

/// Send one modern request — version declared in `_meta`, as every modern
/// request must — and return the result.
async fn modern_call(
    server: &McpServer,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let request = JsonRpcMessage::Request(create_request(
        method,
        Some(modern::params_with_version(
            versioning::FIRST_MODERN_VERSION,
            params,
        )),
        Some(RequestId::Str(format!("modern-{method}"))),
        None,
    ));

    let (response, _session) = server.handle_message(request, None).await;
    let response = response.expect("the server answered");
    assert!(
        response.error().is_none(),
        "{method} failed: {:?}",
        response.error()
    );
    response.result().cloned().expect("a result")
}

#[tokio::test]
async fn the_client_can_discover_our_server() {
    let result = modern_call(&server(), MessageMethod::SERVER_DISCOVER, json!({})).await;

    // Parsed by the *client's* reader, not by re-reading our own fields.
    let profile = ServerProfile::from_discover(&result).expect("the client reads it");

    assert!(profile.era.is_modern());
    assert_eq!(profile.protocol_version, versioning::FIRST_MODERN_VERSION);
    assert_eq!(
        profile.server_info.as_ref().map(|info| info.name.as_str()),
        Some(SERVER_NAME)
    );
    assert_eq!(profile.instructions.as_deref(), Some(INSTRUCTIONS));
    assert!(profile.capabilities.tools.is_some());
}

#[tokio::test]
async fn modern_results_carry_a_result_type() {
    let server = server();

    for (method, params) in [
        (MessageMethod::SERVER_DISCOVER, json!({})),
        (MessageMethod::TOOLS_LIST, json!({})),
        (
            MessageMethod::TOOLS_CALL,
            json!({"name": TOOL_NAME, "arguments": {TOOL_ARGUMENT: "world"}}),
        ),
    ] {
        let result = modern_call(&server, method, params).await;
        assert_eq!(
            result["resultType"],
            json!(RESULT_TYPE_COMPLETE),
            "{method} returned no resultType"
        );
    }
}

#[tokio::test]
async fn a_legacy_request_still_gets_a_legacy_result() {
    // The era is a property of the request, not the server: one server answers
    // both, and a legacy caller must not start receiving fields its revision
    // never defined.
    let request = JsonRpcMessage::Request(create_request(
        MessageMethod::TOOLS_LIST,
        Some(json!({})),
        Some(RequestId::Str("legacy-1".into())),
        None,
    ));

    let (response, _session) = server().handle_message(request, None).await;
    let result = response
        .expect("the server answered")
        .result()
        .cloned()
        .expect("a result");

    assert!(result.get("tools").is_some());
    assert!(
        result.get("resultType").is_none(),
        "a legacy result must not carry resultType"
    );
}

#[tokio::test]
async fn an_unsupported_version_is_rejected_with_the_list_to_retry_from() {
    let request = parse_message(&json!({
        "jsonrpc": "2.0",
        "id": "bad-version",
        "method": MessageMethod::TOOLS_LIST,
        "params": modern::params_with_version("1999-01-01", json!({})),
    }))
    .expect("valid message");

    let (response, _session) = server().handle_message(request, None).await;
    let response = response.expect("the server answered");
    let error = response.error().expect("an error");

    assert_eq!(
        error.code,
        chuk_mcp::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION
    );

    // The list is what makes the rejection recoverable, and is exactly what
    // our own client renegotiates from.
    let supported = error
        .data
        .as_ref()
        .and_then(|data| data.get("supported"))
        .expect("the supported list");
    assert_eq!(supported, &json!(versioning::SUPPORTED_VERSIONS));
}

#[tokio::test]
async fn a_modern_tool_call_round_trips() {
    let result = modern_call(
        &server(),
        MessageMethod::TOOLS_CALL,
        json!({"name": TOOL_NAME, "arguments": {TOOL_ARGUMENT: "modern"}}),
    )
    .await;

    // Read through the client's own typed result, not by poking at fields.
    let tool_result: chuk_mcp::protocol::messages::tools::ToolResult =
        serde_json::from_value(result).expect("the client decodes it");

    assert_eq!(tool_result.result_type, RESULT_TYPE_COMPLETE);
    assert!(!tool_result.is_error);
    assert!(tool_result.text().contains("Hello, modern!"));
}
