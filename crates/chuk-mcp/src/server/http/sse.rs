//! Framing messages as Server-Sent Events.
//!
//! A tool that speaks while it works needs its words to reach the client
//! before its result does, which a single JSON body cannot do. SSE is the
//! specification's answer: the POST that carried the call stays open, each
//! message arrives as an event, and the result is the last one.

use serde_json::Value;

/// The media type of an event stream.
pub const EVENT_STREAM: &str = "text/event-stream";

/// The event name every MCP message is sent under.
const EVENT_MESSAGE: &str = "message";

/// Field prefixes, per the SSE grammar.
const FIELD_EVENT: &str = "event: ";
const FIELD_DATA: &str = "data: ";
const FIELD_ID: &str = "id: ";

/// Ends a line; two of them end an event.
const LINE_END: &str = "\n";

/// One event carrying `payload`, optionally identified for resumption.
///
/// The payload is written on one `data:` line. JSON serialisation never emits
/// a raw newline, so it cannot accidentally end the event early.
pub fn event(payload: &Value, id: Option<&str>) -> String {
    let mut frame = String::new();
    if let Some(id) = id {
        frame.push_str(FIELD_ID);
        frame.push_str(id);
        frame.push_str(LINE_END);
    }
    frame.push_str(FIELD_EVENT);
    frame.push_str(EVENT_MESSAGE);
    frame.push_str(LINE_END);
    frame.push_str(FIELD_DATA);
    frame.push_str(&payload.to_string());
    // A blank line is what tells the client the event is complete.
    frame.push_str(LINE_END);
    frame.push_str(LINE_END);
    frame
}

/// Whether an `Accept` header allows an event stream.
///
/// A client that did not ask for one is sent a plain JSON body instead, which
/// is what any client written before streaming existed expects.
pub fn accepts_event_stream(accept: Option<&str>) -> bool {
    accept.is_some_and(|accept| {
        accept.split(',').any(|entry| {
            entry
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim() == EVENT_STREAM)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_event_is_named_and_terminated_by_a_blank_line() {
        let frame = event(&json!({"jsonrpc": "2.0", "id": 1}), None);

        assert!(frame.starts_with("event: message\n"));
        assert!(frame.contains("data: {"));
        assert!(frame.ends_with("\n\n"), "an event ends with a blank line");
        assert!(!frame.contains(FIELD_ID));
    }

    #[test]
    fn an_identified_event_carries_its_id_first() {
        let frame = event(&json!({"ok": true}), Some("event-1"));
        assert!(frame.starts_with("id: event-1\n"));
        assert!(frame.contains("event: message\n"));
    }

    #[test]
    fn the_payload_stays_on_one_line() {
        // Text with newlines in it must not end the event early: JSON escapes
        // them, and this is the property that relies on it.
        let frame = event(&json!({"text": "first\nsecond"}), None);
        let data_lines: Vec<&str> = frame
            .lines()
            .filter(|line| line.starts_with(FIELD_DATA))
            .collect();
        assert_eq!(data_lines.len(), 1);
        assert!(data_lines[0].contains("first\\nsecond"));
    }

    #[test]
    fn an_accept_header_naming_the_stream_allows_it() {
        assert!(accepts_event_stream(Some(EVENT_STREAM)));
        assert!(accepts_event_stream(Some(
            "application/json, text/event-stream"
        )));
        // Parameters after the type are not part of the type.
        assert!(accepts_event_stream(Some("text/event-stream;q=0.9")));
        assert!(accepts_event_stream(Some(" text/event-stream ")));
    }

    #[test]
    fn an_accept_header_without_it_does_not() {
        assert!(!accepts_event_stream(Some("application/json")));
        assert!(!accepts_event_stream(Some("*/*")));
        assert!(!accepts_event_stream(None));
        // A prefix of the type is not the type.
        assert!(!accepts_event_stream(Some("text/event-stream-plus")));
    }
}
