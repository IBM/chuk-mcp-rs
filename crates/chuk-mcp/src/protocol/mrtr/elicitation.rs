//! `elicitation/create` as the `2026-07-28` revision defines it.
//!
//! Distinct from [`crate::protocol::types::elicitation`], which is the
//! pre-2026 helper surface the Python package exposes: that one has `schema`
//! rather than `requestedSchema`, a `cancelled` boolean rather than the
//! three-action model, and no notion of URL mode. These are the types that go
//! on the wire.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How the user is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitMode {
    /// Structured data collected in-band, validated against `requestedSchema`.
    /// The default: a request that omits `mode` is a form request.
    #[default]
    Form,
    /// Out-of-band interaction at a URL. Nothing but the URL is exposed to the
    /// client, which is what makes it the required mode for credentials.
    Url,
}

/// A server's request for user input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitRequest {
    /// Absent means [`ElicitMode::Form`], for compatibility with servers
    /// written before URL mode existed.
    #[serde(default)]
    pub mode: ElicitMode,
    /// Why the input is needed, for display to the user.
    pub message: String,
    /// Form mode: the shape of the expected response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<Value>,
    /// URL mode: where to send the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What the user did.
///
/// Three outcomes rather than a boolean, because "declined" and "dismissed"
/// call for different server behaviour: an explicit no can be answered with
/// alternatives, a dismissal can be asked again later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitAction {
    /// Approved and submitted. Form mode carries the data in `content`.
    Accept,
    /// Explicitly refused.
    Decline,
    /// Dismissed without a choice — closed, escaped, clicked away.
    Cancel,
}

/// The client's answer to an [`ElicitRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitResult {
    pub action: ElicitAction,
    /// Present for an accepted form request; omitted otherwise, including for
    /// an accepted URL request — the interaction happens out of band and its
    /// outcome never reaches the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Map<String, Value>>,
}

impl ElicitResult {
    /// The user submitted `content`.
    pub fn accept(content: Map<String, Value>) -> Self {
        ElicitResult {
            action: ElicitAction::Accept,
            content: Some(content),
        }
    }

    /// The user consented to a URL interaction. Carries no content: consent is
    /// not the outcome, and the outcome is not the client's to know.
    pub fn accept_url() -> Self {
        ElicitResult {
            action: ElicitAction::Accept,
            content: None,
        }
    }

    /// The user said no.
    pub fn decline() -> Self {
        ElicitResult {
            action: ElicitAction::Decline,
            content: None,
        }
    }

    /// The user dismissed the request.
    pub fn cancel() -> Self {
        ElicitResult {
            action: ElicitAction::Cancel,
            content: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_without_a_mode_is_a_form_request() {
        let request: ElicitRequest = serde_json::from_value(json!({
            "message": "Please provide your GitHub username",
            "requestedSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
        }))
        .unwrap();
        assert_eq!(request.mode, ElicitMode::Form);
        assert!(request.requested_schema.is_some());
    }

    #[test]
    fn the_specification_url_example_decodes() {
        let request: ElicitRequest = serde_json::from_value(json!({
            "mode": "url",
            "url": "https://mcp.example.com/ui/set_api_key",
            "message": "Please provide your API key to continue.",
        }))
        .unwrap();
        assert_eq!(request.mode, ElicitMode::Url);
        assert_eq!(
            request.url.as_deref(),
            Some("https://mcp.example.com/ui/set_api_key")
        );
        assert!(request.requested_schema.is_none());
    }

    #[test]
    fn results_serialize_to_the_specification_shape() {
        let mut content = Map::new();
        content.insert("name".into(), json!("octocat"));
        assert_eq!(
            serde_json::to_value(ElicitResult::accept(content)).unwrap(),
            json!({"action": "accept", "content": {"name": "octocat"}})
        );

        // Everything else omits content rather than sending null.
        for result in [
            ElicitResult::accept_url(),
            ElicitResult::decline(),
            ElicitResult::cancel(),
        ] {
            let value = serde_json::to_value(&result).unwrap();
            assert!(value.get("content").is_none(), "content leaked: {value}");
        }
        assert_eq!(
            serde_json::to_value(ElicitResult::decline()).unwrap(),
            json!({"action": "decline"})
        );
    }

    #[test]
    fn every_action_round_trips() {
        for (text, action) in [
            ("accept", ElicitAction::Accept),
            ("decline", ElicitAction::Decline),
            ("cancel", ElicitAction::Cancel),
        ] {
            let decoded: ElicitAction = serde_json::from_value(json!(text)).unwrap();
            assert_eq!(decoded, action);
            assert_eq!(serde_json::to_value(action).unwrap(), json!(text));
        }
    }
}
