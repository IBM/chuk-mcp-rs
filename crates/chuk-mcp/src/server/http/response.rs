//! Turning what the server decided into an HTTP response.

use std::convert::Infallible;

use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::{Response, StatusCode};
use tokio::sync::mpsc;

use crate::protocol::json_rpc::JsonRpcMessage;

use super::sse;

/// The content type for a complete answer in one message.
pub(crate) const JSON: &str = "application/json";

/// The content type for a plain-text explanation.
const TEXT: &str = "text/plain";

/// The header carrying a legacy session, assigned on `initialize`.
pub(crate) const SESSION_HEADER: &str = "Mcp-Session-Id";

/// The body type every response here uses.
///
/// Boxed because an answer is either one buffered message or a stream of them,
/// and the choice is made per request.
pub(crate) type Body = BoxBody<Bytes, Infallible>;

/// One buffered body.
fn full(payload: impl Into<Bytes>) -> Body {
    Full::new(payload.into()).boxed()
}

/// A JSON-RPC response, as `application/json`.
///
/// `modern` says whether the request was a 2026-era one, because the statuses
/// below are that revision's rules. Applying them to a legacy exchange would
/// change answers that revision defined as `200`.
pub(crate) fn json(
    message: &JsonRpcMessage,
    session: Option<&str>,
    modern: bool,
) -> Response<Body> {
    json_with_status(message, session, status_for(message, modern))
}

/// A JSON-RPC response with an explicit status.
pub(crate) fn json_with_status(
    message: &JsonRpcMessage,
    session: Option<&str>,
    code: StatusCode,
) -> Response<Body> {
    with_session(
        Response::builder().status(code).header(CONTENT_TYPE, JSON),
        session,
    )
    .body(full(message.to_json()))
    .expect("a response with a valid status and headers")
}

/// The HTTP status an answer should carry.
///
/// The 2026-07-28 revision ties three JSON-RPC errors to a transport status, so
/// an intermediary can act on them without parsing the body: a request that
/// misdescribes itself or names an unspeakable version is a `400`, and a method
/// the server does not implement — including the ones this revision removed —
/// is a `404`.
///
/// Deliberately narrow beyond those. `-32602` is *not* here: "unknown tool" and
/// "unknown resource" use it too, and those are answers to a well-formed
/// request rather than a malformed one. The envelope checks that do warrant a
/// `400` set their status explicitly, before dispatch.
fn status_for(message: &JsonRpcMessage, modern: bool) -> StatusCode {
    use crate::protocol::types::errors::{
        HEADER_MISMATCH, METHOD_NOT_FOUND, MISSING_REQUIRED_CLIENT_CAPABILITY,
        UNSUPPORTED_PROTOCOL_VERSION,
    };

    if !modern {
        return StatusCode::OK;
    }
    match message {
        JsonRpcMessage::Error(error) => match error.error.code {
            HEADER_MISMATCH | UNSUPPORTED_PROTOCOL_VERSION | MISSING_REQUIRED_CLIENT_CAPABILITY => {
                StatusCode::BAD_REQUEST
            }
            METHOD_NOT_FOUND => StatusCode::NOT_FOUND,
            _ => StatusCode::OK,
        },
        _ => StatusCode::OK,
    }
}

/// An event stream fed by `messages`, ending when the sender is dropped.
///
/// This is what a call that talks while it works is answered with: each
/// message becomes an event as it is produced, rather than everything waiting
/// for the result.
pub(crate) fn event_stream(
    messages: mpsc::UnboundedReceiver<JsonRpcMessage>,
    session: Option<&str>,
) -> Response<Body> {
    // Each message becomes a frame as it arrives; the stream ends when the
    // sender is dropped, which is how the handler says it has finished.
    let frames = futures::stream::unfold(messages, |mut messages| async move {
        let message = messages.recv().await?;
        let frame = Frame::data(Bytes::from(sse::event(&message.to_value(), None)));
        Some((Ok::<_, Infallible>(frame), messages))
    });

    with_session(
        Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, sse::EVENT_STREAM)
            // Nothing about a live exchange is worth a cache's while, and a
            // proxy that buffered it would defeat the point of streaming.
            .header(hyper::header::CACHE_CONTROL, "no-cache"),
        session,
    )
    .body(BoxBody::new(StreamBody::new(frames)))
    .expect("a response with a valid status and headers")
}

/// Attach the session header when there is a session to attach.
///
/// Only the legacy era has sessions; a modern exchange never assigns one, and
/// echoing one back would invite a client to start sending it.
fn with_session(
    builder: hyper::http::response::Builder,
    session: Option<&str>,
) -> hyper::http::response::Builder {
    match session.and_then(|session| HeaderValue::from_str(session).ok()) {
        Some(value) => builder.header(SESSION_HEADER, value),
        None => builder,
    }
}

/// `202 Accepted` with no body — the answer to a notification, which by
/// definition has no reply.
pub(crate) fn accepted() -> Response<Body> {
    status(StatusCode::ACCEPTED)
}

/// A bare status, for the cases where there is nothing useful to say.
pub(crate) fn status(code: StatusCode) -> Response<Body> {
    Response::builder()
        .status(code)
        .body(full(Bytes::new()))
        .expect("a valid empty response")
}

/// A plain-text error, for failures that are not JSON-RPC's to describe —
/// a malformed body, or a request to a path this server does not serve.
pub(crate) fn text_error(code: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(code)
        .header(CONTENT_TYPE, TEXT)
        .body(full(message.to_string()))
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
        let with = json(&response_message(), Some("session-1"), true);
        assert_eq!(with.status(), StatusCode::OK);
        assert_eq!(with.headers()[CONTENT_TYPE], JSON);
        assert_eq!(with.headers()[SESSION_HEADER], "session-1");

        // A modern exchange has no session, and inventing a header would
        // invite the client to start sending one back.
        let without = json(&response_message(), None, true);
        assert!(without.headers().get(SESSION_HEADER).is_none());
    }

    #[test]
    fn a_session_that_cannot_be_a_header_is_dropped_rather_than_panicking() {
        let response = json(&response_message(), Some("bad\nvalue"), true);
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
        assert_eq!(error.headers()[CONTENT_TYPE], TEXT);

        assert_eq!(
            status(StatusCode::NOT_FOUND).status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_json_body_carries_the_message_it_was_given() {
        let body = read(json(&response_message(), None, true)).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(parsed["result"]["ok"], json!(true));
    }

    #[tokio::test]
    async fn an_event_stream_carries_one_event_per_message_and_ends_with_the_sender() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let response = event_stream(receiver, None);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], sse::EVENT_STREAM);
        assert_eq!(response.headers()[hyper::header::CACHE_CONTROL], "no-cache");

        sender.send(response_message()).expect("the stream is open");
        sender
            .send(JsonRpcMessage::Notification(
                crate::protocol::json_rpc::create_notification("notifications/message", None),
            ))
            .expect("the stream is open");
        // Dropping the sender is what ends the body; without it, reading it
        // whole would never return.
        drop(sender);

        let body = read(response).await;
        assert_eq!(body.matches("event: message").count(), 2);
        assert!(body.contains("\"result\""));
        assert!(body.contains("notifications/message"));
    }

    #[tokio::test]
    async fn a_streamed_response_carries_the_session_when_there_is_one() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let response = event_stream(receiver, Some("session-1"));
        assert_eq!(response.headers()[SESSION_HEADER], "session-1");
        drop(sender);
    }

    /// Read a whole body to a string.
    async fn read(response: Response<Body>) -> String {
        let collected = response
            .into_body()
            .collect()
            .await
            .expect("a body that cannot fail")
            .to_bytes();
        String::from_utf8(collected.to_vec()).expect("UTF-8")
    }
}
