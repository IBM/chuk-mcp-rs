//! Tool-related types, mirroring `chuk_mcp.protocol.types.tools`
//! (2025-06-18 spec, including structured tool output).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::content::{create_text_content, Content};

/// JSON Schema for tool input validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInputSchema {
    /// The type of the schema (typically `object` for tool inputs).
    #[serde(rename = "type")]
    pub schema_type: String,
    /// Properties of the input object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
    /// List of required property names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for ToolInputSchema {
    fn default() -> Self {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: None,
            required: None,
            extra: Map::new(),
        }
    }
}

/// Definition of a tool that can be invoked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    /// Unique identifier for the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema defining the expected input format.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Structured content for tool outputs (new in 2025-06-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredContent {
    /// Always `"structured"`.
    #[serde(rename = "type")]
    pub content_type: String,
    /// The structured data returned by the tool.
    pub data: Map<String, Value>,
    /// Optional JSON Schema describing the structure of the data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    /// Optional MIME type for the structured data.
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of tool execution, supporting text/media and structured content.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolResult {
    /// Textual/media content returned by the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Content>>,
    /// Structured content returned by the tool (new in 2025-06-18).
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Vec<StructuredContent>>,
    /// Whether this result represents an error.
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ToolResult {
    /// Whether the result has any content (text/media or structured).
    pub fn is_valid(&self) -> bool {
        self.content.as_ref().is_some_and(|c| !c.is_empty())
            || self
                .structured_content
                .as_ref()
                .is_some_and(|c| !c.is_empty())
    }

    /// Concatenated text of all text content blocks.
    pub fn text(&self) -> String {
        self.content
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(Content::as_text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Create a simple text tool result.
pub fn create_text_tool_result(text: impl Into<String>, is_error: bool) -> ToolResult {
    ToolResult {
        content: Some(vec![create_text_content(text, None)]),
        is_error: Some(is_error),
        ..Default::default()
    }
}

/// Create a structured tool result (new in 2025-06-18).
pub fn create_structured_tool_result(
    data: Map<String, Value>,
    schema: Option<Value>,
    mime_type: Option<String>,
    is_error: bool,
) -> ToolResult {
    ToolResult {
        structured_content: Some(vec![StructuredContent {
            content_type: "structured".to_string(),
            data,
            schema,
            mime_type: Some(mime_type.unwrap_or_else(|| "application/json".to_string())),
            extra: Map::new(),
        }]),
        is_error: Some(is_error),
        ..Default::default()
    }
}

/// Create an error tool result with optional structured error data.
pub fn create_error_tool_result(
    error_message: impl Into<String>,
    error_data: Option<Map<String, Value>>,
) -> ToolResult {
    ToolResult {
        content: Some(vec![create_text_content(error_message, None)]),
        structured_content: error_data.map(|data| {
            vec![StructuredContent {
                content_type: "structured".to_string(),
                data,
                schema: None,
                mime_type: Some("application/json".to_string()),
                extra: Map::new(),
            }]
        }),
        is_error: Some(true),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_wire_format() {
        let tool = Tool {
            name: "greet".into(),
            description: Some("Say hello".into()),
            input_schema: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            meta: None,
            extra: Map::new(),
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["inputSchema"]["type"], "object");
        assert!(value.get("_meta").is_none());
    }

    #[test]
    fn text_result_helpers() {
        let result = create_text_tool_result("hello", false);
        assert!(result.is_valid());
        assert_eq!(result.text(), "hello");
        assert_eq!(result.is_error, Some(false));
    }
}
