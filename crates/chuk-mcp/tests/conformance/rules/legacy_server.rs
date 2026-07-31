//! How our server must answer in the legacy era.
//!
//! The server speaks the legacy lifecycle only; a modern (`2026-07-28`) server
//! is not built yet, which is why there is no `modern_server` module. The
//! absence is deliberate and visible in the rendered matrix rather than hidden
//! behind rules that quietly do not exist.

use serde_json::{json, Value};

use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::types::errors::{INVALID_REQUEST, METHOD_NOT_FOUND};
use chuk_mcp::protocol::versioning;

use crate::harness::server::{
    self, RESOURCE_URI, SERVER_NAME, SERVER_VERSION, TOOL_ARGUMENT, TOOL_NAME, UNKNOWN_TOOL_NAME,
};
use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

/// Request ids the rules use. Distinct per rule so a stray response cannot be
/// mistaken for the right one.
const INITIALIZE_ID: &str = "conformance-initialize";
const TOOLS_LIST_ID: &str = "conformance-tools-list";
const TOOLS_CALL_ID: &str = "conformance-tools-call";
const UNKNOWN_TOOL_ID: &str = "conformance-unknown-tool";
const UNKNOWN_METHOD_ID: &str = "conformance-unknown-method";
const PING_ID: &str = "conformance-ping";
const RESOURCES_READ_ID: &str = "conformance-resources-read";

/// A method no server implements.
const UNKNOWN_METHOD: &str = "conformance/not-a-method";

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::new(
            "legacy.server.initialize-result",
            Era::Legacy,
            Subject::Server,
            "`initialize` returns protocolVersion, capabilities and serverInfo",
            initialize_returns_required_fields,
        ),
        Rule::new(
            "legacy.server.version-negotiation",
            Era::Legacy,
            Subject::Server,
            "The negotiated version is one the server supports",
            negotiates_a_supported_version,
        ),
        Rule::new(
            "legacy.server.response-id-echoed",
            Era::Legacy,
            Subject::Server,
            "A response carries the id of the request it answers",
            echoes_the_request_id,
        ),
        Rule::new(
            "legacy.server.tools-list-shape",
            Era::Legacy,
            Subject::Server,
            "`tools/list` returns tools, each with a name and an inputSchema",
            tools_list_is_well_formed,
        ),
        Rule::new(
            "legacy.server.tools-call-content",
            Era::Legacy,
            Subject::Server,
            "`tools/call` returns a content array",
            tools_call_returns_content,
        ),
        Rule::new(
            "legacy.server.unknown-tool-is-error",
            Era::Legacy,
            Subject::Server,
            "Calling an unregistered tool is reported as an error, not a success",
            unknown_tool_is_an_error,
        ),
        Rule::new(
            "legacy.server.unknown-method",
            Era::Legacy,
            Subject::Server,
            "An unknown method returns -32601 Method not found",
            unknown_method_returns_method_not_found,
        ),
        Rule::new(
            "legacy.server.ping-answers",
            Era::Legacy,
            Subject::Server,
            "`ping` returns an empty result",
            ping_returns_empty_result,
        ),
        Rule::new(
            "legacy.server.notification-unanswered",
            Era::Legacy,
            Subject::Server,
            "A notification receives no response",
            notifications_get_no_response,
        ),
        Rule::new(
            "legacy.server.resources-read",
            Era::Legacy,
            Subject::Server,
            "`resources/read` returns contents for a registered uri",
            resources_read_returns_contents,
        ),
    ]
}

/// The `initialize` params a conforming client sends.
fn initialize_params(version: &str) -> Value {
    json!({
        "protocolVersion": version,
        "capabilities": {},
        "clientInfo": {"name": "conformance-client", "version": "1.0.0"},
    })
}

/// Ask the fixture server one question, requiring a response.
async fn answered(request: Value, what: &str) -> Result<Value, String> {
    let fixture = server::fixture();
    server::ask(&fixture, request)
        .await
        .ok_or_else(|| format!("{what}: the server returned no response at all"))
}

/// The `result` of a response, requiring success.
fn success<'a>(response: &'a Value, what: &str) -> Result<&'a Value, String> {
    if let Some(error) = server::error_of(response) {
        return Err(format!("{what}: the server returned an error: {error}"));
    }
    server::result_of(response).ok_or_else(|| format!("{what}: the response carried no result"))
}

async fn initialize_returns_required_fields() -> Verdict {
    let response = answered(
        server::request(
            INITIALIZE_ID,
            MessageMethod::INITIALIZE,
            initialize_params(versioning::LATEST_LEGACY_VERSION),
        ),
        MessageMethod::INITIALIZE,
    )
    .await?;
    let result = success(&response, MessageMethod::INITIALIZE)?;

    for field in ["protocolVersion", "capabilities", "serverInfo"] {
        expect(
            result.get(field).is_some(),
            format!("the initialize result omitted `{field}`: {result}"),
        )?;
    }
    expect_eq(
        "serverInfo.name",
        result
            .pointer("/serverInfo/name")
            .and_then(Value::as_str)
            .unwrap_or(""),
        SERVER_NAME,
    )?;
    expect_eq(
        "serverInfo.version",
        result
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or(""),
        SERVER_VERSION,
    )
}

async fn negotiates_a_supported_version() -> Verdict {
    let response = answered(
        server::request(
            INITIALIZE_ID,
            MessageMethod::INITIALIZE,
            initialize_params(versioning::LATEST_LEGACY_VERSION),
        ),
        MessageMethod::INITIALIZE,
    )
    .await?;
    let result = success(&response, MessageMethod::INITIALIZE)?;

    let negotiated = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or("the initialize result carried no protocolVersion")?;
    expect(
        versioning::is_supported(negotiated),
        format!("the server negotiated {negotiated:?}, which it does not support"),
    )
}

async fn echoes_the_request_id() -> Verdict {
    let response = answered(
        server::request(TOOLS_LIST_ID, MessageMethod::TOOLS_LIST, json!({})),
        MessageMethod::TOOLS_LIST,
    )
    .await?;

    expect(
        server::is_response(&response),
        format!("the server answered with something that is not a response: {response}"),
    )?;
    let id = server::id_of(&response).ok_or("the response carried no usable id")?;
    expect_eq("response id", id.to_string(), TOOLS_LIST_ID.to_string())
}

async fn tools_list_is_well_formed() -> Verdict {
    let response = answered(
        server::request(TOOLS_LIST_ID, MessageMethod::TOOLS_LIST, json!({})),
        MessageMethod::TOOLS_LIST,
    )
    .await?;
    let result = success(&response, MessageMethod::TOOLS_LIST)?;

    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("the tools/list result carried no tools array: {result}"))?;
    expect(!tools.is_empty(), "the server advertised no tools")?;

    for tool in tools {
        expect(
            tool.get("name").and_then(Value::as_str).is_some(),
            format!("a tool was advertised without a name: {tool}"),
        )?;
        expect(
            tool.get("inputSchema").is_some(),
            format!("a tool was advertised without an inputSchema: {tool}"),
        )?;
    }
    Ok(())
}

async fn tools_call_returns_content() -> Verdict {
    let response = answered(
        server::request(
            TOOLS_CALL_ID,
            MessageMethod::TOOLS_CALL,
            json!({"name": TOOL_NAME, "arguments": {TOOL_ARGUMENT: "world"}}),
        ),
        MessageMethod::TOOLS_CALL,
    )
    .await?;
    let result = success(&response, MessageMethod::TOOLS_CALL)?;

    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("the tools/call result carried no content array: {result}"))?;
    expect(
        !content.is_empty(),
        "the tools/call result carried an empty content array",
    )
}

async fn unknown_tool_is_an_error() -> Verdict {
    let response = answered(
        server::request(
            UNKNOWN_TOOL_ID,
            MessageMethod::TOOLS_CALL,
            json!({"name": UNKNOWN_TOOL_NAME, "arguments": {}}),
        ),
        MessageMethod::TOOLS_CALL,
    )
    .await?;

    // The spec allows either shape: a JSON-RPC error, or a result flagged
    // `isError`. What it does not allow is a plain success.
    if server::error_of(&response).is_some() {
        return Ok(());
    }
    let result = server::result_of(&response)
        .ok_or_else(|| format!("the server answered neither result nor error: {response}"))?;
    expect(
        result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        format!("calling an unregistered tool succeeded: {result}"),
    )
}

async fn unknown_method_returns_method_not_found() -> Verdict {
    let response = answered(
        server::request(UNKNOWN_METHOD_ID, UNKNOWN_METHOD, json!({})),
        UNKNOWN_METHOD,
    )
    .await?;

    let error = server::error_of(&response)
        .ok_or_else(|| format!("an unknown method was not an error: {response}"))?;
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("the error carried no code: {error}"))?;
    expect(
        code == METHOD_NOT_FOUND || code == INVALID_REQUEST,
        format!("expected {METHOD_NOT_FOUND} (method not found), got {code}"),
    )
}

async fn ping_returns_empty_result() -> Verdict {
    let response = answered(
        server::request(PING_ID, MessageMethod::PING, json!({})),
        MessageMethod::PING,
    )
    .await?;
    let result = success(&response, MessageMethod::PING)?;

    expect(
        result
            .as_object()
            .map(|map| map.is_empty())
            .unwrap_or(false),
        format!("ping returned a non-empty result: {result}"),
    )
}

async fn notifications_get_no_response() -> Verdict {
    let fixture = server::fixture();
    let response = server::ask(
        &fixture,
        server::notification(MessageMethod::NOTIFICATION_INITIALIZED, json!({})),
    )
    .await;

    expect(
        response.is_none(),
        format!("the server answered a notification: {response:?}"),
    )
}

async fn resources_read_returns_contents() -> Verdict {
    let response = answered(
        server::request(
            RESOURCES_READ_ID,
            MessageMethod::RESOURCES_READ,
            json!({"uri": RESOURCE_URI}),
        ),
        MessageMethod::RESOURCES_READ,
    )
    .await?;
    let result = success(&response, MessageMethod::RESOURCES_READ)?;

    let contents = result
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("the resources/read result carried no contents: {result}"))?;
    expect(
        !contents.is_empty(),
        "the resources/read result carried no content entries",
    )
}
