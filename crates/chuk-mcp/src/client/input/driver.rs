//! The modern retry loop.
//!
//! Sends a request; if the server answers `input_required`, answers what it
//! asked and sends the *same request again* under a new id with the answers
//! and the echoed state attached, until the server completes it.

use serde_json::{Map, Value};

use crate::protocol::messages::send_message::{
    send_message_with_options, ReadStream, SendMessageOptions, WriteStream,
};
use crate::protocol::mrtr::{supports_input_required, InputRequired, InputResponses};
use crate::protocol::types::errors::McpError;

use super::{answer, InputHandler};

/// How many times a single call may be sent before the client gives up.
///
/// A server is explicitly allowed to answer `input_required` repeatedly, so
/// there is no protocol-level end to the loop; without a bound, a server that
/// always asks again would hang the caller forever. Eight is far above any
/// plausible interaction and low enough to fail while someone is still
/// watching.
pub const MAX_INPUT_ROUNDS: usize = 8;

/// Params field names, matching the specification exactly.
const FIELD_INPUT_RESPONSES: &str = "inputResponses";
const FIELD_REQUEST_STATE: &str = "requestState";

/// Send `method` and drive any input rounds it provokes to completion.
pub(crate) async fn call_with_input(
    read: &ReadStream,
    write: &WriteStream,
    method: &str,
    params: Value,
    handler: Option<&dyn InputHandler>,
    options: SendMessageOptions,
) -> Result<Value, McpError> {
    let base = as_object(params, method)?;
    let mut round_params = Value::Object(base.clone());
    // The first send honours a caller-supplied id; retries must not, because
    // "the JSON-RPC id MUST be different between the initial request and the
    // retry".
    let mut round_options = options;

    for round in 1..=MAX_INPUT_ROUNDS {
        let result = send_message_with_options(
            read,
            write,
            method,
            Some(round_params),
            round_options.clone(),
        )
        .await?;

        let Some(required) = InputRequired::from_result(&result)? else {
            return Ok(result);
        };

        // "Servers MUST NOT send InputRequiredResult responses on any other
        // client requests." Retrying anyway would send input responses to a
        // method with no notion of them.
        if !supports_input_required(method) {
            return Err(McpError::validation(format!(
                "the server answered `{method}` with an input_required result, \
                 which the specification permits only on tools/call, \
                 resources/read and prompts/get"
            )));
        }

        if round == MAX_INPUT_ROUNDS {
            return Err(McpError::validation(format!(
                "`{method}` still required input after {MAX_INPUT_ROUNDS} rounds; \
                 giving up rather than looping"
            )));
        }

        let handler = handler.ok_or_else(|| {
            McpError::validation(format!(
                "the server needs input to complete `{method}`, but this client \
                 has no input handler; set one with `Connect::input_handler`"
            ))
        })?;

        let mut responses = InputResponses::new();
        for (key, request) in &required.input_requests {
            match answer(handler, request).await? {
                Some(value) => responses.insert(key.clone(), value),
                // Left out rather than faked. A server SHOULD re-ask for what
                // is missing, and a fabricated answer would be worse than the
                // extra round trip.
                None => tracing::warn!(
                    "no answer for input request {key:?} ({}); omitting it",
                    request.method
                ),
            }
        }

        round_params = Value::Object(retry_params(&base, &responses, &required));
        // Force a fresh id for every retry.
        round_options.message_id = None;
    }

    // The loop returns or errors on every path; the bound check above fires on
    // the final round.
    unreachable!("the input round loop always terminates within MAX_INPUT_ROUNDS")
}

/// The original params, plus the answers and the echoed state.
fn retry_params(
    base: &Map<String, Value>,
    responses: &InputResponses,
    required: &InputRequired,
) -> Map<String, Value> {
    let mut params = base.clone();

    if !responses.is_empty() {
        params.insert(FIELD_INPUT_RESPONSES.to_string(), responses.to_value());
    }

    match &required.request_state {
        // "Clients MUST echo back the exact value of that field."
        Some(state) => {
            params.insert(
                FIELD_REQUEST_STATE.to_string(),
                Value::String(state.echo().to_string()),
            );
        }
        // "If the InputRequiredResult does not contain a requestState field,
        // the client MUST NOT include one in the retry" — including one left
        // over from a previous round.
        None => {
            params.remove(FIELD_REQUEST_STATE);
        }
    }

    params
}

/// Request params must be an object for anything to be attached to them.
fn as_object(params: Value, method: &str) -> Result<Map<String, Value>, McpError> {
    match params {
        Value::Null => Ok(Map::new()),
        Value::Object(map) => Ok(map),
        other => Err(McpError::validation(format!(
            "`{method}` params must be an object, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::mrtr::{ElicitResult, InputRequired, RequestState};
    use serde_json::json;

    fn base() -> Map<String, Value> {
        json!({"name": "get_weather", "arguments": {"location": "New York"}})
            .as_object()
            .unwrap()
            .clone()
    }

    fn answers() -> InputResponses {
        let mut content = Map::new();
        content.insert("name".into(), json!("octocat"));
        let mut responses = InputResponses::new();
        responses.insert_elicit("github_login", &ElicitResult::accept(content));
        responses
    }

    #[test]
    fn the_retry_matches_the_specification_example() {
        let required = InputRequired {
            input_requests: Default::default(),
            request_state: Some(RequestState::new("eyJsb2NhdGlvbiI6Ik5ldyBZb3JrIn0...")),
        };
        let params = retry_params(&base(), &answers(), &required);

        assert_eq!(
            Value::Object(params),
            json!({
                "name": "get_weather",
                "arguments": {"location": "New York"},
                "inputResponses": {
                    "github_login": {"action": "accept", "content": {"name": "octocat"}},
                },
                "requestState": "eyJsb2NhdGlvbiI6Ik5ldyBZb3JrIn0...",
            })
        );
    }

    #[test]
    fn no_state_from_the_server_means_none_on_the_retry() {
        let required = InputRequired {
            input_requests: Default::default(),
            request_state: None,
        };
        let params = retry_params(&base(), &answers(), &required);
        assert!(params.get(FIELD_REQUEST_STATE).is_none());
    }

    #[test]
    fn stale_state_is_dropped_rather_than_carried_forward() {
        // Round one supplied state, round two did not: continuing to send the
        // old value would be inventing state the server did not ask for.
        let mut carried = base();
        carried.insert(FIELD_REQUEST_STATE.to_string(), json!("from-round-one"));

        let required = InputRequired {
            input_requests: Default::default(),
            request_state: None,
        };
        let params = retry_params(&carried, &answers(), &required);
        assert!(params.get(FIELD_REQUEST_STATE).is_none());
    }

    #[test]
    fn state_alone_retries_with_no_responses() {
        let required = InputRequired {
            input_requests: Default::default(),
            request_state: Some(RequestState::new("opaque")),
        };
        let params = retry_params(&base(), &InputResponses::new(), &required);
        assert!(params.get(FIELD_INPUT_RESPONSES).is_none());
        assert_eq!(params[FIELD_REQUEST_STATE], json!("opaque"));
    }

    #[test]
    fn params_must_be_an_object() {
        assert_eq!(as_object(Value::Null, "tools/call").unwrap(), Map::new());
        assert!(as_object(json!({"a": 1}), "tools/call").is_ok());
        assert!(as_object(json!("nope"), "tools/call").is_err());
        assert!(as_object(json!([1, 2]), "tools/call").is_err());
    }
}
