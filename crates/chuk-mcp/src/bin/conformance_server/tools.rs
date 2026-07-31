//! The tools the reference conformance scenarios call.
//!
//! Each is named by the scenario that calls it and returns exactly what that
//! scenario describes. They exist to exercise the server, not to be useful.

use std::time::Duration;

use serde_json::{json, Value};

use chuk_mcp::server::{CallContext, LogLevel, McpServer};

use super::media;

/// The pause between the notifications a scenario expects to arrive
/// separately. Sending them back-to-back would not test that a client can
/// receive several during one call.
const STEP: Duration = Duration::from_millis(50);

/// A schema for a tool that takes nothing.
fn no_arguments() -> Value {
    json!({"type": "object", "properties": {}})
}

/// A schema for a tool taking one required string.
fn one_string(name: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {name: {"type": "string", "description": description}},
        "required": [name],
    })
}

/// The text a tool was given under `name`.
fn argument(arguments: &Value, name: &str) -> String {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn register(server: &mut McpServer) {
    register_content(server);
    register_behaviour(server);
    register_interactive(server);
}

/// The tools that return one content shape each.
fn register_content(server: &mut McpServer) {
    server.register_tool(
        "test_simple_text",
        no_arguments(),
        "Returns simple text content",
        |_| async { Ok(json!("This is a simple text response for testing.")) },
    );

    server.register_tool(
        "test_image_content",
        no_arguments(),
        "Returns image content",
        |_| async {
            Ok(json!({
                "type": "image",
                "data": media::RED_PIXEL_PNG,
                "mimeType": media::IMAGE_PNG,
            }))
        },
    );

    server.register_tool(
        "test_audio_content",
        no_arguments(),
        "Returns audio content",
        |_| async {
            Ok(json!({
                "type": "audio",
                "data": media::SILENT_WAV,
                "mimeType": media::AUDIO_WAV,
            }))
        },
    );

    server.register_tool(
        "test_embedded_resource",
        no_arguments(),
        "Returns an embedded resource",
        |_| async {
            Ok(json!({
                "type": "resource",
                "resource": {
                    "uri": "test://embedded-resource",
                    "mimeType": media::TEXT_PLAIN,
                    "text": "This is an embedded resource content.",
                },
            }))
        },
    );

    server.register_tool(
        "test_multiple_content_types",
        no_arguments(),
        "Returns several content types at once",
        |_| async {
            Ok(json!([
                {"type": "text", "text": "Multiple content types test:"},
                {
                    "type": "image",
                    "data": media::RED_PIXEL_PNG,
                    "mimeType": media::IMAGE_PNG,
                },
                {
                    "type": "resource",
                    "resource": {
                        "uri": "test://mixed-content-resource",
                        "mimeType": media::APPLICATION_JSON,
                        "text": json!({"test": "data", "value": 123}).to_string(),
                    },
                },
            ]))
        },
    );
}

/// The tools that demonstrate something other than content.
fn register_behaviour(server: &mut McpServer) {
    server.register_tool(
        "test_error_handling",
        no_arguments(),
        "Always fails",
        |_| async { Err("This tool intentionally returns an error for testing".to_string()) },
    );
}

/// The tools that talk to the client while they run.
fn register_interactive(server: &mut McpServer) {
    server.register_interactive_tool(
        "test_tool_with_logging",
        no_arguments(),
        "Logs while it runs",
        |_, context: CallContext| async move {
            let level = LogLevel::Info.as_str();
            context.log(level, json!("Tool execution started"));
            tokio::time::sleep(STEP).await;
            context.log(level, json!("Tool processing data"));
            tokio::time::sleep(STEP).await;
            context.log(level, json!("Tool execution completed"));
            Ok(json!("Tool with logging completed"))
        },
    );

    server.register_interactive_tool(
        "test_tool_with_progress",
        no_arguments(),
        "Reports progress while it runs",
        |_, context: CallContext| async move {
            const TOTAL: f64 = 100.0;
            for step in [0.0, 50.0, TOTAL] {
                context.progress(step, Some(TOTAL));
                tokio::time::sleep(STEP).await;
            }
            Ok(json!("Tool with progress completed"))
        },
    );

    server.register_interactive_tool(
        "test_sampling",
        one_string("prompt", "The prompt to send to the LLM"),
        "Asks the client to sample a model",
        |arguments, context: CallContext| async move {
            let answer = context
                .sample(json!({
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": argument(&arguments, "prompt")},
                    }],
                    "maxTokens": 100,
                }))
                .await?;
            Ok(json!(format!("LLM response: {}", sampled_text(&answer))))
        },
    );

    register_elicitation(server);
}

/// What a `sampling/createMessage` result said, however the client shaped it.
fn sampled_text(answer: &Value) -> String {
    answer
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| answer.to_string())
}

/// How an elicitation turned out, in the shape the scenarios read.
fn elicited(answer: &Value) -> String {
    let action = answer
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let content = answer.get("content").cloned().unwrap_or(json!({}));
    format!("action={action}, content={content}")
}

fn register_elicitation(server: &mut McpServer) {
    server.register_interactive_tool(
        "test_elicitation",
        one_string("message", "The message to show the user"),
        "Asks the client to put a question to its user",
        |arguments, context: CallContext| async move {
            let answer = context
                .elicit(json!({
                    "message": argument(&arguments, "message"),
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "username": {"type": "string", "description": "User's response"},
                            "email": {"type": "string", "description": "User's email address"},
                        },
                        "required": ["username", "email"],
                    },
                }))
                .await?;
            Ok(json!(format!("User response: {}", elicited(&answer))))
        },
    );

    // SEP-1034: every primitive type carries a default the client may offer.
    server.register_interactive_tool(
        "test_elicitation_sep1034_defaults",
        no_arguments(),
        "Elicits a schema whose fields all have defaults",
        |_, context: CallContext| async move {
            let answer = context
                .elicit(json!({
                    "message": "Please confirm these defaults",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "default": "John Doe"},
                            "age": {"type": "integer", "default": 30},
                            "score": {"type": "number", "default": 95.5},
                            "status": {
                                "type": "string",
                                "enum": ["active", "inactive", "pending"],
                                "default": "active",
                            },
                            "verified": {"type": "boolean", "default": true},
                        },
                    },
                }))
                .await?;
            Ok(json!(format!(
                "Elicitation completed: {}",
                elicited(&answer)
            )))
        },
    );

    // SEP-1330: all five ways a schema may offer a choice.
    server.register_interactive_tool(
        "test_elicitation_sep1330_enums",
        no_arguments(),
        "Elicits a schema using every enum variant",
        |_, context: CallContext| async move {
            let answer = context
                .elicit(json!({
                    "message": "Please choose",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "untitledSingle": {
                                "type": "string",
                                "enum": ["option1", "option2", "option3"],
                            },
                            "titledSingle": {
                                "type": "string",
                                "oneOf": [
                                    {"const": "value1", "title": "First Option"},
                                    {"const": "value2", "title": "Second Option"},
                                    {"const": "value3", "title": "Third Option"},
                                ],
                            },
                            "legacyEnum": {
                                "type": "string",
                                "enum": ["opt1", "opt2", "opt3"],
                                "enumNames": ["Option One", "Option Two", "Option Three"],
                            },
                            "untitledMulti": {
                                "type": "array",
                                "items": {
                                    "type": "string",
                                    "enum": ["option1", "option2", "option3"],
                                },
                            },
                            "titledMulti": {
                                "type": "array",
                                "items": {
                                    "anyOf": [
                                        {"const": "value1", "title": "First Choice"},
                                        {"const": "value2", "title": "Second Choice"},
                                        {"const": "value3", "title": "Third Choice"},
                                    ],
                                },
                            },
                        },
                    },
                }))
                .await?;
            Ok(json!(format!(
                "Elicitation completed: {}",
                elicited(&answer)
            )))
        },
    );
}
