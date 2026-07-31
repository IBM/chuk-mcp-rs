//! Reading an `input_required` result.

use serde_json::Value;

use crate::protocol::mrtr::exchange::InputRequests;
use crate::protocol::mrtr::state::RequestState;
use crate::protocol::types::errors::McpError;

/// The `resultType` marking a result as needing more input before it can
/// complete.
pub const RESULT_TYPE_INPUT_REQUIRED: &str = "input_required";

/// Field names, so the parser and any producer cannot drift apart.
const FIELD_RESULT_TYPE: &str = "resultType";
const FIELD_INPUT_REQUESTS: &str = "inputRequests";
const FIELD_REQUEST_STATE: &str = "requestState";

/// A server's answer of "not yet — I need this first".
#[derive(Debug, Clone, PartialEq)]
pub struct InputRequired {
    /// What the client must answer before retrying. May be empty: a server
    /// that only needs the client to come back — because an out-of-band
    /// interaction is in flight — sends state and no requests.
    pub input_requests: InputRequests,
    /// Echoed verbatim on the retry when present, and **must not** be
    /// fabricated when absent.
    pub request_state: Option<RequestState>,
}

impl InputRequired {
    /// Read a result as an `input_required`, or `None` if it is an ordinary
    /// result.
    ///
    /// Errors only when the result claims to be `input_required` and then is
    /// not usable as one — a malformed request map, or neither field present.
    /// Treating those as "not an input_required" would leave the caller with a
    /// result it cannot interpret and no idea why.
    pub fn from_result(result: &Value) -> Result<Option<Self>, McpError> {
        let Some(object) = result.as_object() else {
            return Ok(None);
        };
        if object.get(FIELD_RESULT_TYPE).and_then(Value::as_str) != Some(RESULT_TYPE_INPUT_REQUIRED)
        {
            return Ok(None);
        }

        let input_requests = match object.get(FIELD_INPUT_REQUESTS) {
            None | Some(Value::Null) => InputRequests::new(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                McpError::validation(format!("malformed {FIELD_INPUT_REQUESTS}: {error}"))
            })?,
        };

        let request_state = match object.get(FIELD_REQUEST_STATE) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => Some(RequestState::new(raw.clone())),
            Some(other) => {
                return Err(McpError::validation(format!(
                    "{FIELD_REQUEST_STATE} must be a string, got {other}"
                )))
            }
        };

        // "Servers MUST include at least one of inputRequests or requestState."
        // Neither would ask the client to retry an identical request and expect
        // a different answer.
        if input_requests.is_empty() && request_state.is_none() {
            return Err(McpError::validation(format!(
                "an {RESULT_TYPE_INPUT_REQUIRED} result carried neither \
                 {FIELD_INPUT_REQUESTS} nor {FIELD_REQUEST_STATE}"
            )));
        }

        Ok(Some(InputRequired {
            input_requests,
            request_state,
        }))
    }
}

/// Build an `input_required` result — the server half of [`InputRequired`].
///
/// A server that needs more input returns this instead of a completed result,
/// and the client answers it and retries. Rejects the one shape the
/// specification forbids: neither field present, which would ask the client to
/// send an identical request and expect a different answer.
pub fn input_required_result(
    input_requests: InputRequests,
    request_state: Option<RequestState>,
) -> Result<Value, McpError> {
    if input_requests.is_empty() && request_state.is_none() {
        return Err(McpError::validation(format!(
            "an {RESULT_TYPE_INPUT_REQUIRED} result needs at least one of \
             {FIELD_INPUT_REQUESTS} or {FIELD_REQUEST_STATE}"
        )));
    }

    let mut result = serde_json::Map::new();
    result.insert(
        FIELD_RESULT_TYPE.to_string(),
        Value::String(RESULT_TYPE_INPUT_REQUIRED.to_string()),
    );
    if !input_requests.is_empty() {
        result.insert(
            FIELD_INPUT_REQUESTS.to_string(),
            serde_json::to_value(&input_requests)?,
        );
    }
    if let Some(state) = &request_state {
        result.insert(
            FIELD_REQUEST_STATE.to_string(),
            Value::String(state.echo().to_string()),
        );
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::method::MessageMethod;
    use serde_json::json;

    #[test]
    fn the_specification_example_decodes() {
        let required = InputRequired::from_result(&json!({
            "resultType": "input_required",
            "inputRequests": {
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
            },
            "requestState": "AEAD-protected blob",
        }))
        .unwrap()
        .expect("an input_required result");

        assert_eq!(required.input_requests.len(), 1);
        assert_eq!(
            required.input_requests["github_login"].method,
            MessageMethod::ELICITATION_CREATE
        );
        assert!(required.request_state.is_some());
    }

    #[test]
    fn an_ordinary_result_is_not_an_input_required() {
        for result in [
            json!({"resultType": "complete", "content": []}),
            json!({"content": []}),
            json!("not an object"),
            json!(null),
        ] {
            assert_eq!(InputRequired::from_result(&result).unwrap(), None);
        }
    }

    #[test]
    fn state_alone_is_enough_to_retry() {
        // The URL-mode case: nothing to ask, the client just comes back.
        let required = InputRequired::from_result(&json!({
            "resultType": "input_required",
            "requestState": "opaque",
        }))
        .unwrap()
        .expect("an input_required result");
        assert!(required.input_requests.is_empty());
        assert!(required.request_state.is_some());
    }

    #[test]
    fn requests_alone_are_enough_to_retry() {
        let required = InputRequired::from_result(&json!({
            "resultType": "input_required",
            "inputRequests": {
                "roots": {"method": "roots/list", "params": {}},
            },
        }))
        .unwrap()
        .expect("an input_required result");
        assert_eq!(required.input_requests.len(), 1);
        assert!(required.request_state.is_none());
    }

    #[test]
    fn neither_field_is_rejected() {
        let error = InputRequired::from_result(&json!({"resultType": "input_required"}))
            .expect_err("must be rejected");
        assert!(error.to_string().contains("inputRequests"));
    }

    #[test]
    fn a_malformed_request_map_is_reported_not_ignored() {
        let error = InputRequired::from_result(&json!({
            "resultType": "input_required",
            "inputRequests": {"broken": {"no_method": true}},
        }))
        .expect_err("must be rejected");
        assert!(error.to_string().contains("inputRequests"));
    }

    #[test]
    fn what_we_build_is_what_we_parse() {
        // The two halves of the same contract: a result this server produces
        // must read back through the client's own parser unchanged.
        let mut requests = InputRequests::new();
        requests.insert(
            "who".to_string(),
            crate::protocol::mrtr::InputRequest {
                method: MessageMethod::ELICITATION_CREATE.to_string(),
                params: json!({"message": "Who are you?"}),
            },
        );

        let built = input_required_result(requests, Some(RequestState::new("opaque")))
            .expect("a valid input_required result");
        let parsed = InputRequired::from_result(&built)
            .expect("parses")
            .expect("is an input_required");

        assert_eq!(parsed.input_requests.len(), 1);
        assert_eq!(
            parsed.input_requests["who"].method,
            MessageMethod::ELICITATION_CREATE
        );
        assert!(parsed.request_state.is_some());
    }

    #[test]
    fn building_with_neither_field_is_refused() {
        // The same rule the parser enforces, applied where the mistake is made.
        assert!(input_required_result(InputRequests::new(), None).is_err());
        // State alone is enough — the URL-mode case.
        assert!(input_required_result(InputRequests::new(), Some(RequestState::new("s"))).is_ok());
    }

    #[test]
    fn a_non_string_request_state_is_rejected() {
        let error = InputRequired::from_result(&json!({
            "resultType": "input_required",
            "requestState": {"not": "a string"},
        }))
        .expect_err("must be rejected");
        assert!(error.to_string().contains("requestState"));
    }
}
