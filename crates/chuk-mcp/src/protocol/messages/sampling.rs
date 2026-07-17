//! Sampling feature messages, mirroring `chuk_mcp.protocol.messages.sampling`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::content::{create_text_content, Content, Role};
use crate::protocol::types::errors::McpError;

/// How much server context to include in the sampling request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncludeContext {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "thisServer")]
    ThisServer,
    #[serde(rename = "allServers")]
    AllServers,
}

/// A message in a sampling conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SamplingMessage {
    pub role: Role,
    pub content: Content,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A hint for model selection (substring match on model name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The client's model selection preferences.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ModelPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Vec<ModelHint>>,
    /// 0.0–1.0
    #[serde(rename = "costPriority", skip_serializing_if = "Option::is_none")]
    pub cost_priority: Option<f64>,
    /// 0.0–1.0
    #[serde(rename = "speedPriority", skip_serializing_if = "Option::is_none")]
    pub speed_priority: Option<f64>,
    /// 0.0–1.0
    #[serde(
        rename = "intelligencePriority",
        skip_serializing_if = "Option::is_none"
    )]
    pub intelligence_priority: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of `sampling/createMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateMessageResult {
    pub role: Role,
    pub content: Content,
    pub model: String,
    /// `endTurn`, `stopSequence`, `maxTokens`, or a provider-specific string.
    #[serde(rename = "stopReason", skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Optional parameters for [`send_sampling_create_message`].
#[derive(Debug, Clone, Default)]
pub struct SamplingOptions {
    pub model_preferences: Option<ModelPreferences>,
    pub system_prompt: Option<String>,
    pub include_context: Option<IncludeContext>,
    pub temperature: Option<f64>,
    pub stop_sequences: Option<Vec<String>>,
    pub metadata: Option<Map<String, Value>>,
}

/// Send a `sampling/createMessage` request. Returns the raw response value,
/// matching the Python function; wrap with [`CreateMessageResult`] as needed.
pub async fn send_sampling_create_message(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    messages: &[SamplingMessage],
    max_tokens: u64,
    options: SamplingOptions,
) -> Result<Value, McpError> {
    let mut params = json!({
        "messages": serde_json::to_value(messages)?,
        "maxTokens": max_tokens,
    });

    if let Some(prefs) = options.model_preferences {
        params["modelPreferences"] = serde_json::to_value(prefs)?;
    }
    if let Some(prompt) = options.system_prompt {
        params["systemPrompt"] = json!(prompt);
    }
    if let Some(ctx) = options.include_context {
        params["includeContext"] = serde_json::to_value(ctx)?;
    }
    if let Some(temp) = options.temperature {
        params["temperature"] = json!(temp);
    }
    if let Some(stops) = options.stop_sequences {
        params["stopSequences"] = json!(stops);
    }
    if let Some(metadata) = options.metadata {
        params["metadata"] = Value::Object(metadata);
    }

    send_message(
        read_stream,
        write_stream,
        MessageMethod::SAMPLING_CREATE_MESSAGE,
        Some(params),
    )
    .await
}

/// Create a sampling message from a role and plain text.
pub fn create_sampling_message(role: Role, text: impl Into<String>) -> SamplingMessage {
    SamplingMessage {
        role,
        content: create_text_content(text, None),
        extra: Map::new(),
    }
}

/// Build model preferences from name hints and priorities.
pub fn create_model_preferences(
    hints: Option<Vec<String>>,
    cost_priority: Option<f64>,
    speed_priority: Option<f64>,
    intelligence_priority: Option<f64>,
) -> ModelPreferences {
    ModelPreferences {
        hints: hints.map(|hints| {
            hints
                .into_iter()
                .map(|name| ModelHint {
                    name: Some(name),
                    extra: Map::new(),
                })
                .collect()
        }),
        cost_priority,
        speed_priority,
        intelligence_priority,
        extra: Map::new(),
    }
}

/// Convenience: sample a completion for a single user prompt.
pub async fn sample_text(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    prompt: &str,
    max_tokens: u64,
    model_hint: Option<&str>,
    temperature: Option<f64>,
    system_prompt: Option<&str>,
) -> Result<CreateMessageResult, McpError> {
    let messages = vec![create_sampling_message(Role::User, prompt)];
    let options = SamplingOptions {
        model_preferences: model_hint
            .map(|hint| create_model_preferences(Some(vec![hint.to_string()]), None, None, None)),
        system_prompt: system_prompt.map(str::to_string),
        temperature,
        ..Default::default()
    };
    let response =
        send_sampling_create_message(read_stream, write_stream, &messages, max_tokens, options)
            .await?;
    Ok(serde_json::from_value(response)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_wire_names() {
        let prefs =
            create_model_preferences(Some(vec!["claude".to_string()]), Some(0.2), None, Some(0.9));
        let value = serde_json::to_value(&prefs).unwrap();
        assert_eq!(value["costPriority"], json!(0.2));
        assert_eq!(value["intelligencePriority"], json!(0.9));
        assert!(value.get("speedPriority").is_none());
        assert_eq!(value["hints"][0]["name"], json!("claude"));
    }

    #[test]
    fn sampling_message_shape() {
        let msg = create_sampling_message(Role::User, "hi");
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["role"], json!("user"));
        assert_eq!(value["content"]["type"], json!("text"));
    }
}
