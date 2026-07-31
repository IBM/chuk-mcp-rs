//! Turning what a tool handler returned into a `tools/call` result.
//!
//! A handler may return three quite different things, and the difference
//! matters: a value to render, content blocks it has already arranged, or a
//! whole result envelope it built itself. Guessing wrong turns a picture into
//! a paragraph about a picture, or a question into an answer.

use serde_json::{json, Value};

use crate::protocol::mrtr::RESULT_TYPE_INPUT_REQUIRED;

/// Result and content-block field names, so a producer and a reader here
/// cannot drift apart.
const FIELD_TYPE: &str = "type";
const FIELD_TEXT: &str = "text";
const FIELD_CONTENT: &str = "content";
const FIELD_IS_ERROR: &str = "isError";
const FIELD_RESULT_TYPE: &str = "resultType";

/// The content block types the specification defines.
const TYPE_TEXT: &str = "text";
const TYPE_IMAGE: &str = "image";
const TYPE_AUDIO: &str = "audio";
const TYPE_RESOURCE: &str = "resource";
const TYPE_RESOURCE_LINK: &str = "resource_link";

/// A handler returning one of these has described its own content and is taken
/// at its word; anything else is a value to render.
const CONTENT_TYPES: [&str; 5] = [
    TYPE_TEXT,
    TYPE_IMAGE,
    TYPE_AUDIO,
    TYPE_RESOURCE,
    TYPE_RESOURCE_LINK,
];

/// A text content block.
pub fn text_block(text: impl Into<String>) -> Value {
    json!({FIELD_TYPE: TYPE_TEXT, FIELD_TEXT: text.into()})
}

/// Whether a handler's value is already a complete result rather than content
/// to wrap.
///
/// A tool that needs more input returns an `input_required` result; wrapping it
/// in content blocks would turn a question into a paragraph of JSON the client
/// would read as an answer.
fn is_input_required(value: &Value) -> bool {
    value
        .get(FIELD_RESULT_TYPE)
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == RESULT_TYPE_INPUT_REQUIRED)
}

/// Whether a value is already a content block rather than data to render.
///
/// Only the specified types count. A tool returning `{"type": "object", ...}`
/// — a schema, say — means the word "object", not a content block, and
/// guessing otherwise would corrupt an ordinary result.
fn is_content_block(value: &Value) -> bool {
    value
        .get(FIELD_TYPE)
        .and_then(Value::as_str)
        .is_some_and(|kind| CONTENT_TYPES.contains(&kind))
}

/// Whether a handler's value is a whole `tools/call` result rather than the
/// content of one.
///
/// A tool that builds its own envelope — content blocks it has already
/// arranged, `isError`, `structuredContent` — has said something wrapping
/// would destroy.
fn is_complete_result(value: &Value) -> bool {
    is_input_required(value) || value.get(FIELD_CONTENT).is_some_and(Value::is_array)
}

/// The `tools/call` result for whatever a handler returned.
pub fn call_result(value: Value) -> Value {
    if is_complete_result(&value) {
        return value;
    }
    json!({FIELD_CONTENT: format_content(&value)})
}

/// The `tools/call` result for a handler that failed.
///
/// The failure is the content, and `isError` is what tells the client the
/// difference between this and an answer.
pub fn error_result(message: &str) -> Value {
    json!({
        FIELD_CONTENT: [text_block(message)],
        FIELD_IS_ERROR: true,
    })
}

/// Format a tool handler's value as MCP content blocks, matching the Python
/// `MCPServer._format_content`.
fn format_content(result: &Value) -> Vec<Value> {
    match result {
        Value::String(text) => vec![text_block(text)],
        Value::Array(items) => items.iter().flat_map(format_content).collect(),
        // An image, some audio, an embedded resource: already a content block,
        // and pretty-printing it would turn the picture into a paragraph
        // describing the picture.
        Value::Object(_) if is_content_block(result) => vec![result.clone()],
        Value::Object(_) => vec![text_block(
            serde_json::to_string_pretty(result).expect("a value that was already parsed"),
        )],
        other => vec![text_block(other.to_string())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_becomes_one_text_block() {
        let result = call_result(json!("hello"));
        assert_eq!(result[FIELD_CONTENT][0][FIELD_TYPE], json!(TYPE_TEXT));
        assert_eq!(result[FIELD_CONTENT][0][FIELD_TEXT], json!("hello"));
    }

    #[test]
    fn a_plain_object_is_rendered_as_text() {
        let result = call_result(json!({"total": 3}));
        let text = result[FIELD_CONTENT][0][FIELD_TEXT]
            .as_str()
            .expect("rendered text");
        assert!(text.contains("total"));
        assert!(text.contains('3'));
    }

    #[test]
    fn a_content_block_is_passed_through_untouched() {
        let image = json!({FIELD_TYPE: TYPE_IMAGE, "data": "aGk=", "mimeType": "image/png"});
        let result = call_result(image.clone());
        assert_eq!(result[FIELD_CONTENT][0], image);
    }

    #[test]
    fn every_specified_content_type_is_recognised() {
        for kind in CONTENT_TYPES {
            assert!(
                is_content_block(&json!({FIELD_TYPE: kind})),
                "{kind} is a content block"
            );
        }
        // A type the specification does not define is data, not content.
        assert!(!is_content_block(&json!({FIELD_TYPE: "object"})));
        assert!(!is_content_block(&json!({"total": 1})));
    }

    #[test]
    fn an_array_flattens_into_its_blocks() {
        let result = call_result(json!([
            {FIELD_TYPE: TYPE_TEXT, FIELD_TEXT: "look:"},
            {FIELD_TYPE: TYPE_IMAGE, "data": "aGk=", "mimeType": "image/png"},
        ]));
        assert_eq!(result[FIELD_CONTENT][0][FIELD_TYPE], json!(TYPE_TEXT));
        assert_eq!(result[FIELD_CONTENT][1][FIELD_TYPE], json!(TYPE_IMAGE));
    }

    #[test]
    fn a_scalar_that_is_not_a_string_is_still_rendered() {
        assert_eq!(
            call_result(json!(42))[FIELD_CONTENT][0][FIELD_TEXT],
            json!("42")
        );
        assert_eq!(
            call_result(json!(true))[FIELD_CONTENT][0][FIELD_TEXT],
            json!("true")
        );
        assert_eq!(
            call_result(json!(null))[FIELD_CONTENT][0][FIELD_TEXT],
            json!("null")
        );
    }

    #[test]
    fn a_self_built_envelope_is_left_alone() {
        let own = json!({
            FIELD_CONTENT: [text_block("no")],
            FIELD_IS_ERROR: true,
            "structuredContent": {"reason": "refused"},
        });
        assert_eq!(call_result(own.clone()), own);
    }

    #[test]
    fn an_input_required_survives_rather_than_being_wrapped() {
        let asking = json!({
            FIELD_RESULT_TYPE: RESULT_TYPE_INPUT_REQUIRED,
            "requestState": "s",
        });
        let result = call_result(asking.clone());
        assert_eq!(result, asking);
        assert!(result.get(FIELD_CONTENT).is_none());
    }

    #[test]
    fn a_failure_is_content_flagged_as_an_error() {
        let result = error_result("the kettle is empty");
        assert_eq!(result[FIELD_IS_ERROR], json!(true));
        assert_eq!(
            result[FIELD_CONTENT][0][FIELD_TEXT],
            json!("the kettle is empty")
        );
    }
}
