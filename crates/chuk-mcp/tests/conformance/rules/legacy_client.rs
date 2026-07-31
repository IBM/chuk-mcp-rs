//! What our client must put on the wire in the legacy era.

use serde_json::Value;

use chuk_mcp::client::McpClient;
use chuk_mcp::protocol::json_rpc::JsonRpcMessage;
use chuk_mcp::protocol::messages::method::MessageMethod;

use crate::harness::recorder::{RecordingTransport, DEFAULT_NEGOTIATED_VERSION, TOOL_NAME};
use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::new(
            "legacy.client.initialize-first",
            Era::Legacy,
            Subject::Client,
            "The first message a client sends is `initialize`",
            initialize_is_first,
        ),
        Rule::new(
            "legacy.client.initialize-params",
            Era::Legacy,
            Subject::Client,
            "`initialize` declares protocolVersion, capabilities and clientInfo",
            initialize_declares_required_params,
        ),
        Rule::new(
            "legacy.client.initialized-notification",
            Era::Legacy,
            Subject::Client,
            "The client sends `notifications/initialized` once initialize returns",
            sends_initialized_notification,
        ),
        Rule::new(
            "legacy.client.unique-request-ids",
            Era::Legacy,
            Subject::Client,
            "Every request carries an id, and no id is reused within a session",
            request_ids_are_unique,
        ),
        Rule::new(
            "legacy.client.tools-call-params",
            Era::Legacy,
            Subject::Client,
            "`tools/call` names the tool and passes an arguments object",
            tools_call_params_are_well_formed,
        ),
        Rule::new(
            "legacy.client.version-pushed-down",
            Era::Legacy,
            Subject::Client,
            "The negotiated version is pushed to the transport, which gates batching",
            negotiated_version_reaches_transport,
        ),
    ]
}

/// Drive a client through a handshake and one tool call, returning everything
/// it sent.
async fn transcript() -> Result<Vec<JsonRpcMessage>, String> {
    let transport = RecordingTransport::new(DEFAULT_NEGOTIATED_VERSION);
    let recording = transport.recording();

    let mut client = McpClient::new(transport);
    client
        .initialize()
        .await
        .map_err(|error| format!("initialize failed: {error}"))?;
    client
        .list_tools()
        .await
        .map_err(|error| format!("tools/list failed: {error}"))?;
    client
        .call_tool(TOOL_NAME, serde_json::json!({"name": "world"}))
        .await
        .map_err(|error| format!("tools/call failed: {error}"))?;

    let sent = recording.lock().expect("recorder mutex poisoned").clone();
    Ok(sent)
}

/// The method of the nth message sent, if there is one.
fn method_at(sent: &[JsonRpcMessage], index: usize) -> Option<&str> {
    sent.get(index).and_then(|message| message.method())
}

async fn initialize_is_first() -> Verdict {
    let sent = transcript().await?;
    expect_eq(
        "first message sent",
        method_at(&sent, 0).unwrap_or("<nothing>"),
        MessageMethod::INITIALIZE,
    )
}

async fn initialize_declares_required_params() -> Verdict {
    let sent = transcript().await?;
    let params = sent
        .iter()
        .find(|message| message.method() == Some(MessageMethod::INITIALIZE))
        .and_then(|message| message.params())
        .ok_or("no initialize request was sent")?;

    for field in ["protocolVersion", "capabilities", "clientInfo"] {
        expect(
            params.get(field).is_some(),
            format!("initialize params omitted `{field}`: {params}"),
        )?;
    }
    Ok(())
}

async fn sends_initialized_notification() -> Verdict {
    let sent = transcript().await?;
    let initialize_index = sent
        .iter()
        .position(|message| message.method() == Some(MessageMethod::INITIALIZE))
        .ok_or("no initialize request was sent")?;
    let notification_index = sent
        .iter()
        .position(|message| message.method() == Some(MessageMethod::NOTIFICATION_INITIALIZED))
        .ok_or("no notifications/initialized was sent")?;

    expect(
        notification_index > initialize_index,
        format!(
            "notifications/initialized was sent at position {notification_index}, \
             before initialize at {initialize_index}"
        ),
    )?;
    expect(
        sent[notification_index].id().is_none(),
        "notifications/initialized carried an id, making it a request",
    )
}

async fn request_ids_are_unique() -> Verdict {
    let sent = transcript().await?;
    let mut seen = Vec::new();
    for message in &sent {
        // Notifications legitimately have no id; everything else must.
        let Some(method) = message.method() else {
            continue;
        };
        if method.starts_with("notifications/") {
            continue;
        }
        let id = message
            .id()
            .ok_or_else(|| format!("request `{method}` was sent without an id"))?;
        let rendered = id.to_string();
        expect(
            !seen.contains(&rendered),
            format!("request id {rendered:?} was reused by `{method}`"),
        )?;
        seen.push(rendered);
    }
    expect(!seen.is_empty(), "no requests were sent at all")
}

async fn tools_call_params_are_well_formed() -> Verdict {
    let sent = transcript().await?;
    let params = sent
        .iter()
        .find(|message| message.method() == Some(MessageMethod::TOOLS_CALL))
        .and_then(|message| message.params())
        .ok_or("no tools/call request was sent")?;

    expect_eq(
        "tools/call params.name",
        params.get("name").and_then(Value::as_str).unwrap_or(""),
        TOOL_NAME,
    )?;
    expect(
        params
            .get("arguments")
            .map(Value::is_object)
            .unwrap_or(false),
        format!("tools/call params.arguments was not an object: {params}"),
    )
}

async fn negotiated_version_reaches_transport() -> Verdict {
    let transport = RecordingTransport::new(DEFAULT_NEGOTIATED_VERSION);
    let negotiated = transport.negotiated_version();

    let mut client = McpClient::new(transport);
    client
        .initialize()
        .await
        .map_err(|error| format!("initialize failed: {error}"))?;

    let observed = negotiated
        .lock()
        .expect("recorder mutex poisoned")
        .clone()
        .ok_or("the client never told the transport which version was negotiated")?;
    expect_eq(
        "version pushed to the transport",
        observed.as_str(),
        DEFAULT_NEGOTIATED_VERSION,
    )
}
