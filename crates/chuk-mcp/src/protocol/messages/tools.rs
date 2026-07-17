//! Tools feature messages, mirroring `chuk_mcp.protocol.messages.tools`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// A tool definition as returned by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `tools/call`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content blocks returned by the tool (kept as raw JSON like the Python
    /// message-layer `ToolResult`).
    pub content: Vec<Value>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ToolResult {
    /// Concatenated text of all `{"type": "text"}` content blocks.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Result of `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Send a `tools/list` request.
pub async fn send_tools_list(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    cursor: Option<&str>,
) -> Result<ListToolsResult, McpError> {
    let params = match cursor {
        Some(cursor) if !cursor.is_empty() => json!({"cursor": cursor}),
        _ => json!({}),
    };
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::TOOLS_LIST,
        Some(params),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Send a `tools/call` request.
pub async fn send_tools_call(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    name: &str,
    arguments: Value,
) -> Result<ToolResult, McpError> {
    if !arguments.is_object() {
        return Err(McpError::validation("Tool arguments must be an object"));
    }
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::TOOLS_CALL,
        Some(json!({"name": name, "arguments": arguments})),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Whether a message is a `notifications/tools/list_changed` notification.
pub fn is_tools_list_changed_notification(msg: &crate::protocol::json_rpc::JsonRpcMessage) -> bool {
    msg.method() == Some(MessageMethod::NOTIFICATION_TOOLS_LIST_CHANGED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_text() {
        let result: ToolResult = serde_json::from_value(json!({
            "content": [
                {"type": "text", "text": "line one"},
                {"type": "image", "data": "x", "mimeType": "image/png"},
                {"type": "text", "text": "line two"},
            ]
        }))
        .unwrap();
        assert_eq!(result.text(), "line one\nline two");
        assert!(!result.is_error);
    }
}
