//! Shared helpers for the modern result envelope.
//!
//! Every `2026-07-28` result is wrapped in an envelope whose `resultType` says
//! whether the operation ran to completion. A legacy result carries no
//! `resultType`; it normalises upward to [`RESULT_TYPE_COMPLETE`] so a caller
//! reads the same shape whichever era produced the result (design note D4).

/// `resultType` of a result that ran to completion.
pub const RESULT_TYPE_COMPLETE: &str = "complete";

/// Serde default for a `result_type` field: an absent `resultType` (a legacy
/// result) normalises to `"complete"`.
pub(crate) fn default_result_type() -> String {
    RESULT_TYPE_COMPLETE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::completions::CompletionResult;
    use crate::protocol::messages::prompts::GetPromptResult;
    use crate::protocol::messages::resources::ReadResourceResult;
    use serde_json::json;

    #[test]
    fn absent_result_type_normalises_to_complete_across_types() {
        let read: ReadResourceResult = serde_json::from_value(json!({"contents": []})).unwrap();
        assert_eq!(read.result_type, RESULT_TYPE_COMPLETE);

        let prompt: GetPromptResult = serde_json::from_value(json!({})).unwrap();
        assert_eq!(prompt.result_type, RESULT_TYPE_COMPLETE);

        let completion: CompletionResult =
            serde_json::from_value(json!({"values": ["a", "b"]})).unwrap();
        assert_eq!(completion.result_type, RESULT_TYPE_COMPLETE);
    }

    #[test]
    fn present_result_type_is_preserved() {
        let read: ReadResourceResult =
            serde_json::from_value(json!({"contents": [], "resultType": "incomplete"})).unwrap();
        assert_eq!(read.result_type, "incomplete");
    }

    #[test]
    fn value_flattens_each_type() {
        // resource: single text content -> its text
        let read: ReadResourceResult = serde_json::from_value(json!({
            "contents": [{"uri": "x://y", "text": "hello"}]
        }))
        .unwrap();
        assert_eq!(read.value(), json!("hello"));

        // prompt: messages if present
        let prompt: GetPromptResult = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]
        }))
        .unwrap();
        assert_eq!(prompt.value().as_array().map(|a| a.len()), Some(1));
        // prompt with no messages -> description
        let desc: GetPromptResult =
            serde_json::from_value(json!({"description": "a prompt"})).unwrap();
        assert_eq!(desc.value(), json!("a prompt"));

        // completion: the values
        let completion: CompletionResult =
            serde_json::from_value(json!({"values": ["x", "y"]})).unwrap();
        assert_eq!(completion.value(), json!(["x", "y"]));
    }
}
