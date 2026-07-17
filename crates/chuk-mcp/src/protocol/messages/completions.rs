//! Completion feature messages, mirroring `chuk_mcp.protocol.messages.completions`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// Reference to a resource or prompt for completion, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Reference {
    #[serde(rename = "ref/resource")]
    Resource { uri: String },
    #[serde(rename = "ref/prompt")]
    Prompt { name: String },
}

/// The argument being completed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgumentInfo {
    pub name: String,
    pub value: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `completion/complete`. At most 100 values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionResult {
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(rename = "hasMore", skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CompletionResult {
    /// Validate the ≤100-values constraint from the spec.
    pub fn validate(&self) -> Result<(), McpError> {
        if self.values.len() > 100 {
            return Err(McpError::validation(
                "Completion values must not exceed 100 items",
            ));
        }
        Ok(())
    }
}

/// Send a `completion/complete` request.
pub async fn send_completion_complete(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    reference: &Reference,
    argument: &ArgumentInfo,
) -> Result<CompletionResult, McpError> {
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::COMPLETION_COMPLETE,
        Some(json!({
            "ref": serde_json::to_value(reference)?,
            "argument": serde_json::to_value(argument)?,
        })),
    )
    .await?;
    let completion = response.get("completion").cloned().unwrap_or(json!({}));
    let result: CompletionResult = serde_json::from_value(completion)?;
    result.validate()?;
    Ok(result)
}

/// Complete an argument of a resource reference.
pub async fn complete_resource_argument(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    resource_uri: &str,
    argument_name: &str,
    argument_value: &str,
) -> Result<CompletionResult, McpError> {
    send_completion_complete(
        read_stream,
        write_stream,
        &Reference::Resource {
            uri: resource_uri.to_string(),
        },
        &ArgumentInfo {
            name: argument_name.to_string(),
            value: argument_value.to_string(),
            extra: Map::new(),
        },
    )
    .await
}

/// Complete an argument of a prompt reference.
pub async fn complete_prompt_argument(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    prompt_name: &str,
    argument_name: &str,
    argument_value: &str,
) -> Result<CompletionResult, McpError> {
    send_completion_complete(
        read_stream,
        write_stream,
        &Reference::Prompt {
            name: prompt_name.to_string(),
        },
        &ArgumentInfo {
            name: argument_name.to_string(),
            value: argument_value.to_string(),
            extra: Map::new(),
        },
    )
    .await
}

/// Local helper: complete enum values by prefix (case-insensitive by default).
pub fn complete_enum_value(
    current_value: &str,
    allowed_values: &[&str],
    case_sensitive: bool,
) -> Vec<String> {
    allowed_values
        .iter()
        .filter(|v| {
            if case_sensitive {
                v.starts_with(current_value)
            } else {
                v.to_lowercase().starts_with(&current_value.to_lowercase())
            }
        })
        .map(|v| v.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_wire_format() {
        assert_eq!(
            serde_json::to_value(Reference::Resource {
                uri: "file:///x".into()
            })
            .unwrap(),
            json!({"type": "ref/resource", "uri": "file:///x"})
        );
        assert_eq!(
            serde_json::to_value(Reference::Prompt { name: "p".into() }).unwrap(),
            json!({"type": "ref/prompt", "name": "p"})
        );
    }

    #[test]
    fn enum_completion() {
        let values = complete_enum_value("Py", &["python", "rust", "PyPy"], false);
        assert_eq!(values, vec!["python", "PyPy"]);
    }
}
