//! Resources feature messages, mirroring `chuk_mcp.protocol.messages.resources`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// A resource definition as returned by `resources/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Contents of a read resource: text or base64 blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A resource template (RFC 6570 URI template).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `resources/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResourcesResult {
    pub resources: Vec<Resource>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `resources/read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContent>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `resources/templates/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResourceTemplatesResult {
    #[serde(rename = "resourceTemplates")]
    pub resource_templates: Vec<ResourceTemplate>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Send a `resources/list` request.
pub async fn send_resources_list(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    cursor: Option<&str>,
) -> Result<ListResourcesResult, McpError> {
    let params = match cursor {
        Some(cursor) if !cursor.is_empty() => json!({"cursor": cursor}),
        _ => json!({}),
    };
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::RESOURCES_LIST,
        Some(params),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Send a `resources/read` request.
pub async fn send_resources_read(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    uri: &str,
) -> Result<ReadResourceResult, McpError> {
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::RESOURCES_READ,
        Some(json!({"uri": uri})),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Send a `resources/templates/list` request.
pub async fn send_resources_templates_list(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
) -> Result<ListResourceTemplatesResult, McpError> {
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::RESOURCES_TEMPLATES_LIST,
        None,
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Send a `resources/subscribe` request. Returns `false` on any failure,
/// matching the Python behavior of swallowing errors.
pub async fn send_resources_subscribe(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    uri: &str,
) -> bool {
    send_message(
        read_stream,
        write_stream,
        MessageMethod::RESOURCES_SUBSCRIBE,
        Some(json!({"uri": uri})),
    )
    .await
    .is_ok()
}

/// Send a `resources/unsubscribe` request. Returns `false` on any failure.
pub async fn send_resources_unsubscribe(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    uri: &str,
) -> bool {
    match send_message(
        read_stream,
        write_stream,
        MessageMethod::RESOURCES_UNSUBSCRIBE,
        Some(json!({"uri": uri})),
    )
    .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("resources/unsubscribe failed: {e}");
            false
        }
    }
}

/// Whether a message is a `notifications/resources/list_changed` notification.
pub fn is_resources_list_changed_notification(
    msg: &crate::protocol::json_rpc::JsonRpcMessage,
) -> bool {
    msg.method() == Some(MessageMethod::NOTIFICATION_RESOURCES_LIST_CHANGED)
}

/// Extract the updated URI if the message is a `notifications/resources/updated`
/// notification.
pub fn parse_resources_updated_notification(
    msg: &crate::protocol::json_rpc::JsonRpcMessage,
) -> Option<String> {
    if msg.method() != Some(MessageMethod::NOTIFICATION_RESOURCES_UPDATED) {
        return None;
    }
    msg.params()?
        .get("uri")
        .and_then(Value::as_str)
        .map(str::to_string)
}
