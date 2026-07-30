//! Prompts feature messages, mirroring `chuk_mcp.protocol.messages.prompts`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// An argument a prompt template accepts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A prompt definition as returned by `prompts/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A message within a prompt (content kept as raw JSON like the Python model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `prompts/get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetPromptResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<PromptMessage>>,
    #[serde(
        rename = "resultType",
        default = "crate::protocol::messages::result_envelope::default_result_type"
    )]
    pub result_type: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl GetPromptResult {
    /// The flattened 0.9-era value: the prompt messages if present, otherwise
    /// the description.
    pub fn value(&self) -> Value {
        match &self.messages {
            Some(messages) => serde_json::to_value(messages).unwrap_or(Value::Null),
            None => self
                .description
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        }
    }
}

/// Result of `prompts/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListPromptsResult {
    pub prompts: Vec<Prompt>,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(
        rename = "resultType",
        default = "crate::protocol::messages::result_envelope::default_result_type"
    )]
    pub result_type: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Send a `prompts/list` request.
pub async fn send_prompts_list(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    cursor: Option<&str>,
) -> Result<ListPromptsResult, McpError> {
    let params = match cursor {
        Some(cursor) if !cursor.is_empty() => json!({"cursor": cursor}),
        _ => json!({}),
    };
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::PROMPTS_LIST,
        Some(params),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Send a `prompts/get` request.
pub async fn send_prompts_get(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    name: &str,
    arguments: Option<Value>,
) -> Result<GetPromptResult, McpError> {
    if let Some(args) = &arguments {
        if !args.is_object() {
            return Err(McpError::validation("Prompt arguments must be an object"));
        }
    }
    let mut params = json!({"name": name});
    if let Some(args) = arguments {
        if args.as_object().is_some_and(|o| !o.is_empty()) {
            params["arguments"] = args;
        }
    }
    let response = send_message(
        read_stream,
        write_stream,
        MessageMethod::PROMPTS_GET,
        Some(params),
    )
    .await?;
    Ok(serde_json::from_value(response)?)
}

/// Whether a message is a `notifications/prompts/list_changed` notification.
pub fn is_prompts_list_changed_notification(
    msg: &crate::protocol::json_rpc::JsonRpcMessage,
) -> bool {
    msg.method() == Some(MessageMethod::NOTIFICATION_PROMPTS_LIST_CHANGED)
}
