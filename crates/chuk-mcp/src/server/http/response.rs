//! Turning what the server decided into an HTTP response.

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::{Response, StatusCode};

use crate::protocol::json_rpc::JsonRpcMessage;

/// The content type this server answers with. Every response is a complete
/// JSON-RPC message; nothing is streamed, so there is no event stream to
/// negotiate.
pub(crate) const JSON: &str = "application/json";

/// The header carrying a legacy session, assigned on `initialize`.
pub(crate) const SESSION_HEADER: &str = "Mcp-Session-Id";

/// The body type every response here uses.
pub(crate) type Body = Full<Bytes>;

/// A JSON-RPC response, as `application/json`.
pub(crate) fn json(message: &JsonRpcMessage, session: Option<&str>) -> Response<Body> {
    let payload = message.to_json();
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, JSON);

    // Only the legacy era has sessions; a modern exchange never assigns one,
    // and echoing one back would invite a client to start sending it.
    if let Some(session) = session {
        if let Ok(value) = HeaderValue::from_str(session) {
            response = response.header(SESSION_HEADER, value);
        }
    }

    response
        .body(Full::new(Bytes::from(payload)))
        .expect("a response with a valid status and headers")
}

/// `202 Accepted` with no body — the answer to a notification, which by
/// definition has no reply.
pub(crate) fn accepted() -> Response<Body> {
    Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(Full::new(Bytes::new()))
        .expect("a valid empty response")
}

/// A bare status, for the cases where there is nothing useful to say.
pub(crate) fn status(code: StatusCode) -> Response<Body> {
    Response::builder()
        .status(code)
        .body(Full::new(Bytes::new()))
        .expect("a valid empty response")
}

/// A plain-text error, for failures that are not JSON-RPC's to describe —
/// a malformed body, or a request to a path this server does not serve.
pub(crate) fn text_error(code: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from(message.to_string())))
        .expect("a valid text response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::{create_response, RequestId};
    use serde_json::json;

    fn response_message() -> JsonRpcMessage {
        JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"ok": true})),
        ))
    }

    #[test]
    fn a_json_response_carries_the_session_only_when_there_is_one() {
        let with = json(&response_message(), Some("session-1"));
        assert_eq!(with.status(), StatusCode::OK);
        assert_eq!(with.headers()[CONTENT_TYPE], JSON);
        assert_eq!(with.headers()[SESSION_HEADER], "session-1");

        // A modern exchange has no session, and inventing a header would
        // invite the client to start sending one back.
        let without = json(&response_message(), None);
        assert!(without.headers().get(SESSION_HEADER).is_none());
    }

    #[test]
    fn a_session_that_cannot_be_a_header_is_dropped_rather_than_panicking() {
        let response = json(&response_message(), Some("bad\nvalue"));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(SESSION_HEADER).is_none());
    }

    #[test]
    fn a_notification_is_accepted_with_no_body() {
        let response = accepted();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(response.headers().get(CONTENT_TYPE).is_none());
    }

    #[test]
    fn errors_and_statuses_carry_what_they_should() {
        let error = text_error(StatusCode::BAD_REQUEST, "nope");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error.headers()[CONTENT_TYPE], "text/plain");

        assert_eq!(
            status(StatusCode::NOT_FOUND).status(),
            StatusCode::NOT_FOUND
        );
    }
}
