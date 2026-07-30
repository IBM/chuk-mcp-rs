//! Tools feature messages, mirroring `chuk_mcp.protocol.messages.tools`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::result_envelope::default_result_type;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::meta;
use crate::protocol::types::errors::McpError;

/// Envelope key for structured tool output (2025-06-18+, carried through 2026).
const STRUCTURED_CONTENT_KEY: &str = "structuredContent";

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
    /// The modern result envelope's `resultType`. A legacy result carries none
    /// and normalises upward to `"complete"` (D4), so a caller reads the same
    /// shape whichever era produced the result.
    #[serde(rename = "resultType", default = "default_result_type")]
    pub result_type: String,
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

    /// Structured content blocks, if the tool returned any (2025-06-18+).
    pub fn structured_content(&self) -> Option<&Vec<Value>> {
        self.extra
            .get(STRUCTURED_CONTENT_KEY)
            .and_then(Value::as_array)
    }

    /// The self-reported server identity from the result's `_meta`
    /// (`io.modelcontextprotocol/serverInfo`), if present.
    ///
    /// Unverified and self-reported — for display, logging and multi-server
    /// attribution only, never for behavioural or security decisions.
    pub fn server_identity(&self) -> Option<Value> {
        self.meta.as_ref()?.get(meta::SERVER_INFO).cloned()
    }

    /// The flattened 0.9-era value, so caller code that predates content blocks
    /// and the modern envelope keeps working:
    ///
    /// * a single structured block → its `data`,
    /// * several structured blocks → the list of them,
    /// * otherwise the concatenated text if any,
    /// * otherwise the raw content blocks.
    pub fn value(&self) -> Value {
        if let Some(blocks) = self.structured_content() {
            if blocks.len() == 1 {
                return blocks[0]
                    .get("data")
                    .cloned()
                    .unwrap_or_else(|| blocks[0].clone());
            }
            return Value::Array(blocks.clone());
        }
        let text = self.text();
        if !text.is_empty() {
            return Value::String(text);
        }
        Value::Array(self.content.clone())
    }
}

/// Result of `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(rename = "resultType", default = "default_result_type")]
    pub result_type: String,
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
    use crate::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;

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

    #[test]
    fn legacy_result_normalises_result_type_to_complete() {
        // A legacy `tools/call` result carries no `resultType`.
        let result: ToolResult =
            serde_json::from_value(json!({"content": [{"type": "text", "text": "hi"}]})).unwrap();
        assert_eq!(result.result_type, RESULT_TYPE_COMPLETE);
    }

    #[test]
    fn modern_result_type_is_preserved() {
        let result: ToolResult = serde_json::from_value(json!({
            "resultType": "incomplete",
            "content": [{"type": "text", "text": "partial"}]
        }))
        .unwrap();
        assert_eq!(result.result_type, "incomplete");
    }

    #[test]
    fn value_prefers_structured_data_then_text_then_content() {
        // single structured block -> its data
        let structured: ToolResult = serde_json::from_value(json!({
            "content": [],
            "structuredContent": [{"type": "structured", "data": {"answer": 42}}]
        }))
        .unwrap();
        assert_eq!(structured.value(), json!({"answer": 42}));

        // no structured content, but text -> the text
        let text: ToolResult =
            serde_json::from_value(json!({"content": [{"type": "text", "text": "hello"}]}))
                .unwrap();
        assert_eq!(text.value(), json!("hello"));

        // neither -> the raw content blocks
        let raw: ToolResult =
            serde_json::from_value(json!({"content": [{"type": "image", "data": "x"}]})).unwrap();
        assert_eq!(raw.value(), json!([{"type": "image", "data": "x"}]));
    }

    #[test]
    fn server_identity_reads_the_reserved_meta_key() {
        let result: ToolResult = serde_json::from_value(json!({
            "content": [],
            "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "srv", "version": "2.0"}}
        }))
        .unwrap();
        assert_eq!(
            result.server_identity(),
            Some(json!({"name": "srv", "version": "2.0"}))
        );

        let none: ToolResult = serde_json::from_value(json!({"content": []})).unwrap();
        assert_eq!(none.server_identity(), None);
    }
}
