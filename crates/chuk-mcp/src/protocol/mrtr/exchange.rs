//! The two maps that carry a round trip: what the server asked, and what the
//! client answered.
//!
//! Both are keyed by server-assigned identifiers, and the response map's keys
//! **must** be the request map's keys — that correspondence is the only thing
//! relating an answer to its question, since the answers are heterogeneous and
//! the order is not meaningful.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::mrtr::elicitation::{ElicitRequest, ElicitResult};
use crate::protocol::types::errors::McpError;

/// One server-to-client request, as it appears inside `inputRequests`.
///
/// Shaped like a JSON-RPC request without the envelope: it has a method and
/// params but no `jsonrpc` or `id`, because it is not a message — it is a
/// question embedded in a result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl InputRequest {
    /// Read this as an elicitation request, if that is what it is.
    ///
    /// `None` for `sampling/createMessage` or `roots/list`, which a handler
    /// answers from its own sources rather than by asking a user.
    pub fn as_elicitation(&self) -> Option<Result<ElicitRequest, McpError>> {
        if self.method != MessageMethod::ELICITATION_CREATE {
            return None;
        }
        Some(
            serde_json::from_value(self.params.clone()).map_err(|error| {
                McpError::validation(format!("malformed elicitation/create request: {error}"))
            }),
        )
    }
}

/// The requests a server needs answered, keyed by its own identifiers.
///
/// A `BTreeMap` rather than the wire order: the keys are the correspondence,
/// order carries no meaning, and a deterministic iteration order makes the
/// resulting `inputResponses` reproducible.
pub type InputRequests = BTreeMap<String, InputRequest>;

/// The client's answers, keyed to match [`InputRequests`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputResponses(BTreeMap<String, Value>);

impl InputResponses {
    pub fn new() -> Self {
        InputResponses(BTreeMap::new())
    }

    /// Record an answer under the key its question arrived with.
    pub fn insert(&mut self, key: impl Into<String>, response: Value) {
        self.0.insert(key.into(), response);
    }

    /// Record an elicitation answer.
    pub fn insert_elicit(&mut self, key: impl Into<String>, result: &ElicitResult) {
        // An ElicitResult is a closed enum plus an optional object, so this
        // cannot fail in practice; falling back to null rather than panicking
        // keeps a serialization bug from taking the process down.
        let value = serde_json::to_value(result).unwrap_or(Value::Null);
        self.insert(key, value);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The map as request params, for the retry.
    pub fn to_value(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Map<String, Value>>(),
        )
    }

    /// Which of `requests` this has no answer for.
    ///
    /// A server **SHOULD** re-ask rather than error when an answer is missing,
    /// so this reports the gap instead of refusing to send.
    pub fn missing<'a>(&self, requests: &'a InputRequests) -> Vec<&'a str> {
        requests
            .keys()
            .filter(|key| !self.0.contains_key(*key))
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn specification_requests() -> InputRequests {
        serde_json::from_value(json!({
            "github_login": {
                "method": "elicitation/create",
                "params": {
                    "mode": "form",
                    "message": "Please provide your GitHub username",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}},
                        "required": ["name"],
                    },
                },
            },
            "capital_of_france": {
                "method": "sampling/createMessage",
                "params": {
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": "What is the capital of France?"},
                    }],
                    "systemPrompt": "You are a helpful assistant.",
                    "maxTokens": 100,
                },
            },
        }))
        .unwrap()
    }

    #[test]
    fn the_specification_request_map_decodes() {
        let requests = specification_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests["github_login"].method,
            MessageMethod::ELICITATION_CREATE
        );
        assert_eq!(
            requests["capital_of_france"].method,
            MessageMethod::SAMPLING_CREATE_MESSAGE
        );
    }

    #[test]
    fn only_elicitation_requests_read_as_elicitations() {
        let requests = specification_requests();

        let elicitation = requests["github_login"]
            .as_elicitation()
            .expect("an elicitation/create request")
            .expect("well-formed");
        assert_eq!(elicitation.message, "Please provide your GitHub username");

        assert!(requests["capital_of_france"].as_elicitation().is_none());
    }

    #[test]
    fn a_malformed_elicitation_is_an_error_not_a_none() {
        // `None` would mean "not an elicitation", which would send the caller
        // looking for a handler that does not exist instead of reporting the
        // real problem.
        let request = InputRequest {
            method: MessageMethod::ELICITATION_CREATE.to_string(),
            params: json!({"mode": "form"}), // no message
        };
        assert!(request
            .as_elicitation()
            .expect("is an elicitation")
            .is_err());
    }

    #[test]
    fn responses_serialize_to_the_specification_shape() {
        let mut content = Map::new();
        content.insert("name".into(), json!("octocat"));

        let mut responses = InputResponses::new();
        responses.insert_elicit("github_login", &ElicitResult::accept(content));
        responses.insert(
            "capital_of_france",
            json!({
                "role": "assistant",
                "content": {"type": "text", "text": "The capital of France is Paris."},
                "model": "claude-3-sonnet-20240307",
                "stopReason": "endTurn",
            }),
        );

        assert_eq!(
            responses.to_value(),
            json!({
                "github_login": {"action": "accept", "content": {"name": "octocat"}},
                "capital_of_france": {
                    "role": "assistant",
                    "content": {"type": "text", "text": "The capital of France is Paris."},
                    "model": "claude-3-sonnet-20240307",
                    "stopReason": "endTurn",
                },
            })
        );
        assert_eq!(responses.len(), 2);
        assert!(!responses.is_empty());
    }

    #[test]
    fn missing_answers_are_reported_by_key() {
        let requests = specification_requests();
        let mut responses = InputResponses::new();
        assert_eq!(
            responses.missing(&requests),
            vec!["capital_of_france", "github_login"]
        );

        responses.insert_elicit("github_login", &ElicitResult::decline());
        assert_eq!(responses.missing(&requests), vec!["capital_of_france"]);

        responses.insert("capital_of_france", json!({}));
        assert!(responses.missing(&requests).is_empty());
        assert!(InputResponses::new()
            .missing(&InputRequests::new())
            .is_empty());
    }
}
