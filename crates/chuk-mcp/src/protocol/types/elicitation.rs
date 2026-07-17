//! Elicitation types (servers asking users for input mid-session; new in
//! 2025-06-18), mirroring `chuk_mcp.protocol.types.elicitation`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Parameters for an `elicitation/create` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationParams {
    /// Human-readable message explaining what input is needed.
    pub message: String,
    /// JSON Schema defining the expected structure of the user's response.
    pub schema: Value,
    /// Optional title for the input request (for UI display).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional longer description of what input is needed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Response from an elicitation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationResponse {
    /// The user's response data, structured per the request schema.
    pub data: Map<String, Value>,
    /// Whether the user cancelled the input request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Create an elicitation request for simple text input.
pub fn create_text_input_elicitation(
    message: impl Into<String>,
    field_name: &str,
    title: Option<String>,
    required: bool,
) -> ElicitationParams {
    let message = message.into();
    let mut schema = json!({
        "type": "object",
        "properties": {field_name: {"type": "string", "description": message}},
    });
    if required {
        schema["required"] = json!([field_name]);
    }
    ElicitationParams {
        message,
        schema,
        title,
        description: None,
        extra: Map::new(),
    }
}

/// Create an elicitation request for selecting from choices.
pub fn create_choice_elicitation(
    message: impl Into<String>,
    choices: &[&str],
    field_name: &str,
    title: Option<String>,
) -> ElicitationParams {
    let message = message.into();
    let schema = json!({
        "type": "object",
        "properties": {
            field_name: {"type": "string", "enum": choices, "description": message}
        },
        "required": [field_name],
    });
    ElicitationParams {
        message,
        schema,
        title,
        description: None,
        extra: Map::new(),
    }
}

/// Create an elicitation request for yes/no confirmation.
pub fn create_confirmation_elicitation(
    message: impl Into<String>,
    field_name: &str,
    title: Option<String>,
) -> ElicitationParams {
    let message = message.into();
    let schema = json!({
        "type": "object",
        "properties": {field_name: {"type": "boolean", "description": message}},
        "required": [field_name],
    });
    ElicitationParams {
        message,
        schema,
        title,
        description: None,
        extra: Map::new(),
    }
}

/// Create an elicitation request for multiple form fields.
pub fn create_form_elicitation(
    message: impl Into<String>,
    fields: Map<String, Value>,
    required_fields: Option<Vec<String>>,
    title: Option<String>,
) -> ElicitationParams {
    let mut schema = json!({"type": "object", "properties": fields});
    if let Some(required) = required_fields {
        schema["required"] = json!(required);
    }
    ElicitationParams {
        message: message.into(),
        schema,
        title,
        description: None,
        extra: Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_schema_shape() {
        let params = create_confirmation_elicitation("Proceed?", "confirmed", None);
        assert_eq!(params.schema["required"], json!(["confirmed"]));
        assert_eq!(
            params.schema["properties"]["confirmed"]["type"],
            json!("boolean")
        );
    }
}
