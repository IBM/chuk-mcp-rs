//! Multi Round-Trip Requests.
//!
//! The wire-level behaviour — a retry under a new id carrying the echoed state
//! — is driven end to end in `tests/mrtr_e2e.rs` against scripted servers of
//! both eras. What lives here are the requirements that hold of the *shapes*
//! regardless of transport, and that a future change could break silently.

use serde_json::json;

use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::mrtr::{
    supports_input_required, ElicitAction, ElicitMode, ElicitRequest, ElicitResult, InputRequired,
    RequestState, RESULT_TYPE_INPUT_REQUIRED,
};

use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

/// The spec's worked example, used wherever a well-formed result is needed.
const EXAMPLE_STATE: &str = "AEAD-protected blob";
const EXAMPLE_KEY: &str = "github_login";

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::sync(
            "modern.protocol.input-required-decodes",
            Era::Modern,
            Subject::Protocol,
            "An `input_required` result carries `inputRequests` and `requestState`",
            input_required_decodes,
        ),
        Rule::sync(
            "modern.protocol.input-required-needs-content",
            Era::Modern,
            Subject::Protocol,
            "A server includes at least one of `inputRequests` or `requestState`",
            input_required_needs_content,
        ),
        Rule::sync(
            "modern.protocol.input-required-methods",
            Era::Modern,
            Subject::Protocol,
            "Only tools/call, resources/read and prompts/get may be answered with it",
            only_three_methods_carry_input_required,
        ),
        Rule::sync(
            "modern.client.request-state-opaque",
            Era::Modern,
            Subject::Client,
            "`requestState` is never inspected, and never rendered in logs",
            request_state_is_opaque,
        ),
        Rule::sync(
            "both.protocol.elicit-default-mode",
            Era::Both,
            Subject::Protocol,
            "An elicitation request with no `mode` is a form request",
            absent_mode_is_form,
        ),
        Rule::sync(
            "both.protocol.elicit-three-actions",
            Era::Both,
            Subject::Protocol,
            "Elicitation answers are accept, decline or cancel, and only accept carries content",
            elicit_results_use_three_actions,
        ),
    ]
}

/// The `InputRequiredResult` example, verbatim from the specification.
fn example_result() -> serde_json::Value {
    json!({
        "resultType": RESULT_TYPE_INPUT_REQUIRED,
        "inputRequests": {
            EXAMPLE_KEY: {
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
        "requestState": EXAMPLE_STATE,
    })
}

fn input_required_decodes() -> Verdict {
    let required = InputRequired::from_result(&example_result())
        .map_err(|error| format!("the specification example did not decode: {error}"))?
        .ok_or("the specification example was not recognised as input_required")?;

    expect_eq(
        "inputRequests keys",
        required.input_requests.keys().cloned().collect::<Vec<_>>(),
        vec![EXAMPLE_KEY.to_string()],
    )?;
    expect_eq(
        "the embedded request method",
        required.input_requests[EXAMPLE_KEY].method.as_str(),
        MessageMethod::ELICITATION_CREATE,
    )?;
    expect(
        required.request_state.is_some(),
        "requestState was dropped during decoding",
    )?;

    // An ordinary result must not be mistaken for one.
    expect(
        InputRequired::from_result(&json!({"resultType": "complete", "content": []}))
            .map_err(|error| error.to_string())?
            .is_none(),
        "a completed result was read as input_required",
    )
}

fn input_required_needs_content() -> Verdict {
    let outcome = InputRequired::from_result(&json!({"resultType": RESULT_TYPE_INPUT_REQUIRED}));
    expect(
        outcome.is_err(),
        "a result with neither inputRequests nor requestState was accepted; \
         retrying it would send an identical request and expect a different answer",
    )
}

fn only_three_methods_carry_input_required() -> Verdict {
    for method in [
        MessageMethod::TOOLS_CALL,
        MessageMethod::RESOURCES_READ,
        MessageMethod::PROMPTS_GET,
    ] {
        expect(
            supports_input_required(method),
            format!("{method} must accept an input_required result"),
        )?;
    }
    for method in [
        MessageMethod::TOOLS_LIST,
        MessageMethod::RESOURCES_LIST,
        MessageMethod::PROMPTS_LIST,
        MessageMethod::COMPLETION_COMPLETE,
        MessageMethod::SERVER_DISCOVER,
    ] {
        expect(
            !supports_input_required(method),
            format!("{method} must not accept an input_required result"),
        )?;
    }
    Ok(())
}

fn request_state_is_opaque() -> Verdict {
    let secret = "principal=alice;exp=1730000000;sig=deadbeef";
    let state = RequestState::new(secret);

    // The client has no way to read it apart from echoing it, and a debug log
    // must not become the way it leaks.
    let rendered = format!("{state:?}");
    expect(
        !rendered.contains("alice") && !rendered.contains("deadbeef"),
        format!("Debug rendered the opaque state: {rendered}"),
    )?;

    // It must survive a round trip untouched: "clients MUST echo back the
    // exact value".
    let encoded = serde_json::to_value(&state).map_err(|error| error.to_string())?;
    expect_eq("serialized requestState", encoded, json!(secret))
}

fn absent_mode_is_form() -> Verdict {
    let request: ElicitRequest = serde_json::from_value(json!({"message": "who are you?"}))
        .map_err(|error| format!("a mode-less elicitation did not decode: {error}"))?;
    expect_eq(
        "mode of a mode-less request",
        request.mode,
        ElicitMode::Form,
    )
}

fn elicit_results_use_three_actions() -> Verdict {
    let mut content = serde_json::Map::new();
    content.insert("name".into(), json!("octocat"));

    let accepted =
        serde_json::to_value(ElicitResult::accept(content)).map_err(|e| e.to_string())?;
    expect_eq(
        "accepted action",
        accepted.get("action").and_then(|v| v.as_str()),
        Some("accept"),
    )?;
    expect(
        accepted.get("content").is_some(),
        "an accepted form answer carried no content",
    )?;

    // Everything else omits content rather than sending an empty object: a
    // server distinguishes "no data" from "declined".
    for (result, action) in [
        (ElicitResult::decline(), ElicitAction::Decline),
        (ElicitResult::cancel(), ElicitAction::Cancel),
        (ElicitResult::accept_url(), ElicitAction::Accept),
    ] {
        let value = serde_json::to_value(&result).map_err(|e| e.to_string())?;
        expect(
            value.get("content").is_none(),
            format!("{action:?} carried content: {value}"),
        )?;
    }
    Ok(())
}
