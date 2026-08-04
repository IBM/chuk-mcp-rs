//! The diagnostic tools the `2026-07-28` scenarios call.
//!
//! These exercise obligations that only exist in the stateless era: refusing a
//! capability the client never declared, staying silent unless the request
//! asked for logs, answering with `input_required` instead of turning the
//! connection around, and pushing list-changed notifications to whoever is
//! listening.
//!
//! Kept apart from [`super::tools`] because the older scenarios must keep
//! seeing exactly the fixtures they always did — a tool added for one era
//! should not change what the other observes.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use chuk_mcp::server::{CallContext, Listeners, LogLevel, McpServer};

/// The pause between notifications a scenario expects to arrive separately.
const STEP: Duration = Duration::from_millis(50);

/// A schema for a tool that takes nothing.
fn no_arguments() -> Value {
    json!({"type": "object", "properties": {}})
}

pub fn register(server: &mut McpServer) {
    register_capability_gate(server);
    register_mrtr_prompt(server);
    register_logging(server);
    register_mrtr(server);
    register_schema(server);
    register_headers(server);
    register_change_trigger(server);
}

/// A tool that cannot run without a capability, for the `-32021` rule.
///
/// It never actually samples: the point is that the refusal happens *before*
/// the handler, so a client that did not declare `sampling` gets a protocol
/// error rather than a tool result reporting a failure after the fact.
fn register_capability_gate(server: &mut McpServer) {
    server.register_tool_requiring(
        "test_missing_capability",
        no_arguments(),
        "Requires the sampling capability, and is refused without it",
        &["sampling"],
        |_, _context: CallContext| async move {
            Ok(json!("The client declared sampling, so this ran."))
        },
    );
}

/// `prompts/get` answering with `input_required`.
///
/// One of the three requests that may be answered this way, and the one that
/// proves the mechanism is not tool-specific.
fn register_mrtr_prompt(server: &mut McpServer) {
    server.register_raw_prompt(
        "test_input_required_result_prompt",
        "A prompt that asks for context before it renders",
        vec![],
        |_arguments, context: CallContext| async move {
            if context.input_responses().is_none() {
                return Ok(asking(
                    vec![elicit("prompt_context", "What context should I use?")],
                    STATE_ROUND_1,
                ));
            }
            Ok(json!({
                "description": "A prompt rendered once its context arrived",
                "messages": [{
                    "role": "user",
                    "content": {"type": "text", "text": "Context received; here is the prompt."},
                }],
            }))
        },
    );
}

/// A tool that logs, for the "no logs unless asked" rule.
///
/// Whether any of these are actually sent is not this tool's decision: a
/// request whose `_meta` omitted `io.modelcontextprotocol/logLevel` silences
/// them in the context itself. The tool logs unconditionally precisely so that
/// the silencing is what is being tested.
fn register_logging(server: &mut McpServer) {
    server.register_interactive_tool(
        "test_logging_tool",
        no_arguments(),
        "Logs while it runs, whether or not anyone asked",
        |_, context: CallContext| async move {
            let level = LogLevel::Info.as_str();
            context.log(level, json!("Tool execution started"));
            tokio::time::sleep(STEP).await;
            context.log(level, json!("Tool execution completed"));
            Ok(json!("Logging tool completed"))
        },
    );
}

/// An `elicitation/create` input request under `key`.
fn elicit(key: &str, message: &str) -> (String, Value) {
    (
        key.to_string(),
        json!({
            "method": "elicitation/create",
            "params": {
                "message": message,
                "requestedSchema": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"],
                },
            },
        }),
    )
}

/// A `sampling/createMessage` input request under `key`.
fn sample(key: &str, prompt: &str) -> (String, Value) {
    (
        key.to_string(),
        json!({
            "method": "sampling/createMessage",
            "params": {
                "messages": [{"role": "user", "content": {"type": "text", "text": prompt}}],
                "maxTokens": 100,
            },
        }),
    )
}

/// A `roots/list` input request under `key`.
fn list_roots(key: &str) -> (String, Value) {
    (
        key.to_string(),
        json!({"method": "roots/list", "params": {}}),
    )
}

/// An `input_required` result asking for `requests`, carrying `state`.
fn asking(requests: Vec<(String, Value)>, state: &str) -> Value {
    let map: serde_json::Map<String, Value> = requests.into_iter().collect();
    json!({
        "resultType": "input_required",
        "inputRequests": Value::Object(map),
        "requestState": state,
    })
}

/// The state a tool mints on its first round.
///
/// Opaque to the client and meaningful only here, which is the whole point:
/// with no session to hold it, everything the server must remember between
/// rounds travels through the client.
pub const STATE_ROUND_1: &str = "chuk-mrtr-round-1";
pub const STATE_ROUND_2: &str = "chuk-mrtr-round-2";

/// The multi round-trip tools, one per shape the scenarios exercise.
fn register_mrtr(server: &mut McpServer) {
    // The basic shape, once per input method. Asked cold each returns its
    // request for input; asked again with the answer attached, each completes.
    server.register_interactive_tool(
        "test_input_required_result_elicitation",
        no_arguments(),
        "Returns an InputRequiredResult asking for an elicitation",
        |_, context: CallContext| async move {
            match context.input_response("user_name") {
                Some(answer) => Ok(json!(format!("Hello, {answer}"))),
                None => Ok(asking(
                    vec![elicit("user_name", "What is your name?")],
                    STATE_ROUND_1,
                )),
            }
        },
    );

    server.register_interactive_tool(
        "test_input_required_result_sampling",
        no_arguments(),
        "Returns an InputRequiredResult asking for a sampling round",
        |_, context: CallContext| async move {
            match context.input_response("capital_question") {
                Some(answer) => Ok(json!(format!("The model said: {answer}"))),
                None => Ok(asking(
                    vec![sample("capital_question", "What is the capital of France?")],
                    STATE_ROUND_1,
                )),
            }
        },
    );

    server.register_interactive_tool(
        "test_input_required_result_list_roots",
        no_arguments(),
        "Returns an InputRequiredResult asking for the client's roots",
        |_, context: CallContext| async move {
            match context.input_response("client_roots") {
                Some(answer) => Ok(json!(format!("Roots: {answer}"))),
                None => Ok(asking(vec![list_roots("client_roots")], STATE_ROUND_1)),
            }
        },
    );

    // The state round-trip, checked by its own scenario: the completed result
    // must say "state-ok" to prove the server saw the state come back.
    server.register_interactive_tool(
        "test_input_required_result_request_state",
        no_arguments(),
        "Returns an InputRequiredResult whose requestState must come back",
        |_, context: CallContext| async move {
            if context.input_responses().is_none() {
                return Ok(asking(
                    vec![elicit("confirmation", "Please confirm")],
                    STATE_ROUND_1,
                ));
            }
            match context.request_state() {
                Some(STATE_ROUND_1) => Ok(json!("state-ok: the requestState came back intact")),
                other => Err(format!("requestState was not echoed: {other:?}")),
            }
        },
    );

    // Three different input methods in one result, answered together.
    server.register_interactive_tool(
        "test_input_required_result_multiple_inputs",
        no_arguments(),
        "Asks for an elicitation, a sampling and the roots at once",
        |_, context: CallContext| async move {
            if context.input_responses().is_none() {
                return Ok(asking(
                    vec![
                        elicit("who", "Who are you?"),
                        sample("greeting", "Say hello"),
                        list_roots("roots"),
                    ],
                    STATE_ROUND_1,
                ));
            }
            Ok(json!("All three inputs arrived"))
        },
    );

    // Two rounds of asking, each with its own state, so a client that reuses
    // the first round's state on the second is visibly wrong.
    server.register_interactive_tool(
        "test_input_required_result_multi_round",
        no_arguments(),
        "Asks twice, with a different requestState each time",
        |_, context: CallContext| async move {
            match context.request_state() {
                None => Ok(asking(
                    vec![elicit("step1", "First: your name?")],
                    STATE_ROUND_1,
                )),
                Some(STATE_ROUND_1) => Ok(asking(
                    vec![elicit("step2", "Second: your favourite colour?")],
                    STATE_ROUND_2,
                )),
                Some(STATE_ROUND_2) => Ok(json!("Both rounds answered")),
                Some(other) => Err(format!("unrecognised requestState: {other}")),
            }
        },
    );

    // Only ask for what the client can actually answer. This one declares no
    // required capability of its own precisely so it is reached, and then
    // filters — a server that asked for an elicitation from a client that
    // never declared one would be asking a question that cannot be answered.
    server.register_interactive_tool(
        "test_input_required_result_capabilities",
        no_arguments(),
        "Asks only for the input methods the client declared",
        |_, context: CallContext| async move {
            if context.input_responses().is_some() {
                return Ok(json!("Answered"));
            }
            let mut requests = Vec::new();
            if context.client_supports("sampling") {
                requests.push(sample("model_question", "What is the capital of France?"));
            }
            if context.client_supports("elicitation") {
                requests.push(elicit("user_question", "What is your name?"));
            }
            if context.client_supports("roots") {
                requests.push(list_roots("client_roots"));
            }
            if requests.is_empty() {
                return Ok(json!("The client declared nothing that could be asked"));
            }
            Ok(asking(requests, STATE_ROUND_1))
        },
    );

    // State whose integrity is checked. The scenario edits the state and
    // requires the retry to be refused rather than served — which the server's
    // registered validator does before this handler is ever reached, so by the
    // time it runs the state is known good.
    server.register_interactive_tool(
        "test_input_required_result_tampered_state",
        no_arguments(),
        "Refuses a requestState that was edited in transit",
        |_, context: CallContext| async move {
            if context.input_responses().is_none() {
                return Ok(asking(
                    vec![elicit("confirmation", "Please confirm")],
                    STATE_ROUND_1,
                ));
            }
            Ok(json!("state-ok"))
        },
    );

    // The same idea over a *stream*: the scenario watches the response stream
    // and requires that everything on it is either a notification or the
    // result. A server-initiated `elicitation/create` request appearing here
    // would be the pre-2026 way, and is exactly what MRTR replaced.
    server.register_tool_requiring(
        "test_streaming_elicitation",
        no_arguments(),
        "Streams progress, then asks for input without turning the connection around",
        &["elicitation"],
        |_, context: CallContext| async move {
            context.progress(50.0, Some(100.0));
            tokio::time::sleep(STEP).await;

            match context.input_responses() {
                Some(responses) => Ok(json!(format!("Got input: {responses}"))),
                None => Ok(asking(
                    vec![elicit("user_name", "What is your name?")],
                    STATE_ROUND_1,
                )),
            }
        },
    );
}

/// A tool whose schema uses JSON Schema 2020-12 keywords beyond the subset
/// earlier revisions allowed.
fn register_schema(server: &mut McpServer) {
    server.register_tool(
        "json_schema_2020_12_tool",
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "$defs": {
                "address": {
                    "$anchor": "addressDef",
                    "type": "object",
                    "properties": {
                        "street": {"type": "string"},
                        "city": {"type": "string"},
                    },
                },
            },
            "properties": {
                "name": {"type": "string"},
                "address": {"$ref": "#/$defs/address"},
                "contactMethod": {"type": "string", "enum": ["phone", "email"]},
                "phone": {"type": "string"},
                "email": {"type": "string"},
            },
            "allOf": [
                {"anyOf": [{"required": ["phone"]}, {"required": ["email"]}]},
            ],
            "if": {
                "properties": {"contactMethod": {"const": "phone"}},
                "required": ["contactMethod"],
            },
            "then": {"required": ["phone"]},
            "else": {"required": ["email"]},
            "additionalProperties": false,
        }),
        "Tool with JSON Schema 2020-12 features",
        |arguments| async move { Ok(json!(format!("Validated: {arguments}"))) },
    );
}

/// A tool with `x-mcp-header` annotations, so the custom-header rules have
/// something to be checked against.
///
/// Only primitive, statically reachable properties may be promoted, which is
/// what these three are.
fn register_headers(server: &mut McpServer) {
    server.register_tool(
        "test_custom_headers",
        json!({
            "type": "object",
            "properties": {
                "region": {
                    "type": "string",
                    "description": "Where to run the query",
                    "x-mcp-header": "Region",
                },
                "tenant": {
                    "type": "string",
                    "description": "Which tenant is asking",
                    "x-mcp-header": "Tenant",
                },
                "query": {"type": "string", "description": "The query itself"},
            },
            "required": ["region", "query"],
        }),
        "Mirrors two of its parameters into Mcp-Param-* headers",
        |arguments| async move { Ok(json!(format!("Ran with: {arguments}"))) },
    );
}

/// A tool that makes the lists change, so a subscription has something to
/// deliver.
///
/// The scenarios open a `subscriptions/listen` stream and then call this; what
/// they are checking is that the notification arrives tagged, and that a
/// stream which asked for one type is not sent the other.
fn register_change_trigger(server: &mut McpServer) {
    let listeners: Arc<Listeners> = server.listeners();
    server.register_tool(
        "test_trigger_tool_change",
        no_arguments(),
        "Announces that the tool and prompt lists changed",
        move |_| {
            let listeners = listeners.clone();
            async move {
                listeners.tools_list_changed();
                listeners.prompts_list_changed();
                listeners.resources_list_changed();
                Ok(json!("Announced the list changes"))
            }
        },
    );
}
