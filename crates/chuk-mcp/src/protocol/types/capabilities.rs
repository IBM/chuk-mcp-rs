//! MCP capability definitions, mirroring `chuk_mcp.protocol.types.capabilities`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Capability for logging operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LoggingCapability {
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for prompts operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PromptsCapability {
    /// Whether this supports notifications for changes to the prompt list.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for resources operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ResourcesCapability {
    /// Whether this supports subscribing to resource updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    /// Whether this supports notifications for changes to the resource list.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for tools operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolsCapability {
    /// Whether this supports notifications for changes to the tool list.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for completion operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CompletionCapability {
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for roots operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RootsCapability {
    /// Whether this supports notifications for changes to the roots list.
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for sampling operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SamplingCapability {
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capability for elicitation operations.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ElicitationCapability {
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capabilities that a server may support.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    /// Experimental, non-standard capabilities that the server supports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, Value>>,
    /// Present if the server supports sending log messages to the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingCapability>,
    /// Present if the server offers any prompt templates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    /// Present if the server offers any resources to read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    /// Present if the server offers any tools to call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Present if the server supports argument completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionCapability>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Capabilities a client may support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// Experimental, non-standard capabilities that the client supports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<HashMap<String, Value>>,
    /// Present if the client supports sampling from an LLM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
    /// Present if the client supports elicitation from the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationCapability>,
    /// Present if the client supports listing roots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for ClientCapabilities {
    /// Matches the Python defaults: empty `experimental` map and
    /// `roots.listChanged = true`.
    fn default() -> Self {
        ClientCapabilities {
            experimental: Some(HashMap::new()),
            sampling: None,
            elicitation: None,
            roots: Some(RootsCapability {
                list_changed: Some(true),
                extra: Map::new(),
            }),
            extra: Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_defaults_match_python() {
        let caps = ClientCapabilities::default();
        let value = serde_json::to_value(&caps).unwrap();
        assert_eq!(value["roots"]["listChanged"], json!(true));
        assert_eq!(value["experimental"], json!({}));
    }

    #[test]
    fn extra_fields_roundtrip() {
        let raw = json!({"tools": {"listChanged": true}, "customCap": {"x": 1}});
        let caps: ServerCapabilities = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(caps.tools.as_ref().unwrap().list_changed, Some(true));
        assert_eq!(serde_json::to_value(&caps).unwrap(), raw);
    }
}
