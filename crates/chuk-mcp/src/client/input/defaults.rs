//! Answering a form request from the schema's own defaults.
//!
//! Every primitive an elicitation schema may ask for supports an optional
//! `default`, and clients that support defaults **SHOULD** pre-populate fields
//! with them. For an interactive client that means seeding a form; for a
//! non-interactive one — a test harness, a batch job, a conformance runner —
//! the defaults *are* the answer.

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::protocol::mrtr::{ElicitMode, ElicitRequest, ElicitResult};

use super::InputHandler;

/// Where a schema keeps the fields it is asking about.
const FIELD_PROPERTIES: &str = "properties";
/// The per-property default value.
const FIELD_DEFAULT: &str = "default";
/// Properties the schema says must be present.
const FIELD_REQUIRED: &str = "required";

/// Accepts form requests using the defaults in the requested schema.
///
/// URL mode is always cancelled: consenting to open a URL is a decision only a
/// user can make, and a handler with no user must not make it on their behalf.
/// [`supports_url_mode`](InputHandler::supports_url_mode) stays `false`, so a
/// conforming server will not send one in the first place.
pub struct AcceptDefaults;

#[async_trait]
impl InputHandler for AcceptDefaults {
    async fn elicit(&self, request: ElicitRequest) -> ElicitResult {
        if request.mode == ElicitMode::Url {
            return ElicitResult::cancel();
        }

        let schema = request.requested_schema.unwrap_or(Value::Null);
        let content = schema_defaults(&schema);

        // A schema whose required fields have no defaults cannot be answered
        // from defaults alone. Declining says so honestly; accepting with the
        // fields missing would look like a user who submitted an incomplete
        // form.
        if missing_required(&schema, &content).is_empty() {
            ElicitResult::accept(content)
        } else {
            ElicitResult::decline()
        }
    }
}

/// The default value of every property that declares one.
pub fn schema_defaults(schema: &Value) -> Map<String, Value> {
    let mut content = Map::new();
    let Some(properties) = schema.get(FIELD_PROPERTIES).and_then(Value::as_object) else {
        return content;
    };

    for (name, property) in properties {
        if let Some(default) = property.get(FIELD_DEFAULT) {
            content.insert(name.clone(), default.clone());
        }
    }
    content
}

/// Required properties `content` has no value for.
fn missing_required<'a>(schema: &'a Value, content: &Map<String, Value>) -> Vec<&'a str> {
    schema
        .get(FIELD_REQUIRED)
        .and_then(Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| !content.contains_key(*name))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One schema covering every primitive the specification allows a default
    /// on, shaped as its examples are.
    fn schema_with_every_default() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "default": "user@example.com"},
                "age": {"type": "number", "default": 30},
                "count": {"type": "integer", "default": 7},
                "subscribed": {"type": "boolean", "default": false},
                "colour": {"type": "string", "enum": ["Red", "Green"], "default": "Red"},
                "shades": {
                    "type": "array",
                    "items": {"type": "string", "enum": ["Red", "Green"]},
                    "default": ["Red"],
                },
            },
        })
    }

    #[test]
    fn every_primitive_default_is_collected() {
        assert_eq!(
            Value::Object(schema_defaults(&schema_with_every_default())),
            json!({
                "name": "user@example.com",
                "age": 30,
                "count": 7,
                "subscribed": false,
                "colour": "Red",
                "shades": ["Red"],
            })
        );
    }

    #[test]
    fn a_property_without_a_default_is_left_out() {
        // Absent is not the same as null: sending null would assert a value the
        // schema never offered.
        let content = schema_defaults(&json!({
            "type": "object",
            "properties": {
                "given": {"type": "string", "default": "yes"},
                "withheld": {"type": "string"},
            },
        }));
        assert_eq!(Value::Object(content), json!({"given": "yes"}));
    }

    #[test]
    fn a_schema_with_no_properties_yields_nothing() {
        for schema in [json!({}), json!({"type": "object"}), json!(null)] {
            assert!(schema_defaults(&schema).is_empty());
        }
    }

    #[tokio::test]
    async fn a_form_request_is_accepted_with_its_defaults() {
        let request = ElicitRequest {
            mode: ElicitMode::Form,
            message: "Tell me about yourself".into(),
            requested_schema: Some(schema_with_every_default()),
            url: None,
            extra: Map::new(),
        };

        let result = AcceptDefaults.elicit(request).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap()["action"],
            json!("accept")
        );
        assert_eq!(result.content.unwrap()["count"], json!(7));
    }

    #[tokio::test]
    async fn a_required_field_with_no_default_is_declined() {
        let request = ElicitRequest {
            mode: ElicitMode::Form,
            message: "Your name?".into(),
            requested_schema: Some(json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            })),
            url: None,
            extra: Map::new(),
        };

        let result = AcceptDefaults.elicit(request).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"action": "decline"})
        );
    }

    #[tokio::test]
    async fn a_required_field_that_has_a_default_is_accepted() {
        let request = ElicitRequest {
            mode: ElicitMode::Form,
            message: "Your name?".into(),
            requested_schema: Some(json!({
                "type": "object",
                "properties": {"name": {"type": "string", "default": "octocat"}},
                "required": ["name"],
            })),
            url: None,
            extra: Map::new(),
        };

        let result = AcceptDefaults.elicit(request).await;
        assert_eq!(result.content.unwrap()["name"], json!("octocat"));
    }

    #[tokio::test]
    async fn a_url_request_is_never_consented_to_on_a_users_behalf() {
        let request = ElicitRequest {
            mode: ElicitMode::Url,
            message: "Please authorise".into(),
            requested_schema: None,
            url: Some("https://example.com/authorize".into()),
            extra: Map::new(),
        };

        let result = AcceptDefaults.elicit(request).await;
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            json!({"action": "cancel"})
        );
        assert!(!AcceptDefaults.supports_url_mode());
    }
}
