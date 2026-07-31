//! The standalone server-to-client stream.
//!
//! Streamable HTTP has two directions. Responses come back on the POST that
//! carried the request, but anything a server originates — a pushed
//! `elicitation/create`, `sampling/createMessage` or `roots/list`, and
//! notifications unrelated to any request — travels on a **`GET`** to the same
//! endpoint, held open as an event stream.
//!
//! Without one, a legacy server that needs to ask the client something has
//! nowhere to say so: it waits, the client waits, and the call times out
//! looking like a slow tool.
//!
//! Only the legacy era needs this. The `2026-07-28` revision removed
//! server-initiated requests in favour of [MRTR](crate::protocol::mrtr) and
//! replaced this stream with `subscriptions/listen`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};

use crate::protocol::json_rpc::JsonRpcMessage;
use crate::transports::http::{stream_sse_response, StreamableHttpParameters};

/// Header advertising that we want an event stream back.
const ACCEPT_HEADER: &str = "Accept";
const EVENT_STREAM: &str = "text/event-stream";
const SESSION_HEADER: &str = "Mcp-Session-Id";
/// Where a reconnecting client says how far it got, so the server can resume
/// it rather than replay or skip.
const LAST_EVENT_ID_HEADER: &str = "Last-Event-ID";

/// How long to wait before reopening a stream the server closed cleanly, when
/// the server did not say.
///
/// A graceful close is the server saying "nothing more for now", not an error,
/// so reconnecting is expected — but immediately would spin against a server
/// that closes at once. A `retry:` field in the stream overrides this: that is
/// the server's own instruction, and honouring it is what keeps a fleet of
/// clients from reconnecting in lockstep.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);

/// How long to keep waiting for the session id the handshake assigns.
///
/// The stream is opened after `initialize` so it can carry the session id; a
/// server that never assigns one is still worth listening to, so this bounds
/// the wait rather than requiring one.
const SESSION_WAIT: Duration = Duration::from_secs(5);
const SESSION_POLL: Duration = Duration::from_millis(25);

/// Statuses that mean "this server has no such stream", as opposed to a
/// transient failure. Reconnecting against these would loop forever.
const UNSUPPORTED: [u16; 3] = [404, 405, 501];

/// How long a caller will wait for the stream to be established before
/// carrying on without it.
///
/// Bounded because a server that never answers the `GET` must not stop the
/// client working; the stream is how a server *may* ask for input, not
/// something every exchange needs.
pub(crate) const READY_TIMEOUT: Duration = Duration::from_secs(2);

/// What the streams have told us about resuming them, shared between the POST
/// path and the listener.
///
/// A `retry:` or an event id can arrive on *any* stream — including the SSE
/// response to a POST — and applies to the connection, not to the stream that
/// carried it. Keeping them here is what lets a request whose stream ended
/// early be resumed by the listener rather than lost.
#[derive(Default)]
pub(crate) struct StreamHints {
    inner: std::sync::Mutex<Hints>,
    resume: Notify,
}

#[derive(Default, Clone)]
struct Hints {
    last_event_id: Option<String>,
    retry: Option<Duration>,
}

impl StreamHints {
    /// Record what a finished stream said, and ask for a resume if it ended
    /// without delivering the response it was opened for.
    pub(crate) fn record(&self, stream: &crate::transports::http::SseStream) {
        {
            let mut hints = self.inner.lock().expect("hints lock");
            if stream.last_event_id.is_some() {
                hints.last_event_id = stream.last_event_id.clone();
            }
            if stream.retry.is_some() {
                hints.retry = stream.retry;
            }
        }
        if !stream.answered {
            self.resume.notify_waiters();
        }
    }

    fn snapshot(&self) -> Hints {
        self.inner.lock().expect("hints lock").clone()
    }

    /// Resolve when a stream ends unanswered and wants resuming.
    async fn resume_requested(&self) {
        self.resume.notified().await;
    }
}

/// Signals that the server-to-client stream has been settled — either opened,
/// or established as unavailable.
///
/// A server may only send a request on a stream that exists, so one that is
/// asked to work before the stream is up will decline to ask at all. Waiting
/// makes the difference deterministic instead of a race the client usually
/// loses.
#[derive(Default)]
pub(crate) struct Ready {
    settled: AtomicBool,
    changed: Notify,
}

impl Ready {
    /// Mark the stream settled and release anything waiting.
    fn signal(&self) {
        self.settled.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    /// Wait for the stream to settle, giving up after [`READY_TIMEOUT`].
    pub(crate) async fn wait(&self) {
        if self.settled.load(Ordering::SeqCst) {
            return;
        }
        let notified = self.changed.notified();
        if self.settled.load(Ordering::SeqCst) {
            return;
        }
        let _ = tokio::time::timeout(READY_TIMEOUT, notified).await;
    }
}

/// Hold a `GET` stream open, routing everything the server sends to `incoming`.
///
/// Runs until the transport drops the incoming channel, or the server says it
/// does not offer the stream.
pub(crate) async fn listen(
    client: reqwest::Client,
    parameters: StreamableHttpParameters,
    session: Arc<std::sync::Mutex<Option<String>>>,
    incoming: mpsc::Sender<JsonRpcMessage>,
    max_buffer_size: usize,
    ready: Arc<Ready>,
    hints: Arc<StreamHints>,
) {
    await_session(&session).await;

    loop {
        // Read fresh each round: a POST stream that ended since the last
        // attempt may have moved the resume point on.
        let last_event_id = hints.snapshot().last_event_id;
        if incoming.is_closed() {
            ready.signal();
            return;
        }

        let mut request = client
            .get(&parameters.url)
            .header(ACCEPT_HEADER, EVENT_STREAM);

        for (key, value) in parameters.effective_headers() {
            if !key.eq_ignore_ascii_case(ACCEPT_HEADER) {
                request = request.header(key, value);
            }
        }
        if let Some(session_id) = session.lock().expect("session lock").clone() {
            request = request.header(SESSION_HEADER, session_id);
        }
        if let Some(event_id) = &last_event_id {
            request = request.header(LAST_EVENT_ID_HEADER, event_id);
        }

        match request.send().await {
            Ok(response) if UNSUPPORTED.contains(&response.status().as_u16()) => {
                tracing::debug!(
                    "server does not offer a GET stream ({}); not retrying",
                    response.status()
                );
                ready.signal();
                return;
            }
            Ok(response) if response.status().is_success() => {
                // Established: anything waiting on the stream may proceed.
                ready.signal();
                // Ends when the server closes the stream, which is ordinary:
                // reconnect and keep listening.
                // Race the stream against a resume request: a POST stream that
                // ended unanswered wants this one reopened from its event id,
                // and waiting for a server that is holding this stream open
                // would never get there.
                tokio::select! {
                    stream = stream_sse_response(response, &incoming, max_buffer_size) => {
                        hints.record(&stream);
                    }
                    _ = hints.resume_requested() => {
                        tracing::debug!("resuming the server-to-client stream");
                    }
                }
            }
            Ok(response) => {
                tracing::debug!("GET stream returned {}; retrying", response.status());
            }
            Err(error) => {
                tracing::debug!("GET stream failed: {error}; retrying");
            }
        }

        // Read the delay *after* the stream ended, not before it started: the
        // `retry:` that governs this reconnection usually arrives on the very
        // stream that just closed, so a value sampled earlier is the previous
        // instruction rather than the current one.
        let delay = hints.snapshot().retry.unwrap_or(RECONNECT_DELAY);
        tokio::time::sleep(delay).await;
    }
}

/// Wait, briefly, for the handshake to assign a session id.
async fn await_session(session: &Arc<std::sync::Mutex<Option<String>>>) {
    let deadline = tokio::time::Instant::now() + SESSION_WAIT;
    while tokio::time::Instant::now() < deadline {
        if session.lock().expect("session lock").is_some() {
            return;
        }
        tokio::time::sleep(SESSION_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::transports::http::SseStream;

    fn stream(id: Option<&str>, retry: Option<u64>, answered: bool) -> SseStream {
        SseStream {
            last_event_id: id.map(str::to_string),
            retry: retry.map(Duration::from_millis),
            answered,
        }
    }

    #[test]
    fn hints_accumulate_and_never_regress_to_nothing() {
        let hints = StreamHints::default();
        assert!(hints.snapshot().last_event_id.is_none());
        assert!(hints.snapshot().retry.is_none());

        hints.record(&stream(Some("event-2"), Some(500), true));
        assert_eq!(hints.snapshot().last_event_id.as_deref(), Some("event-2"));
        assert_eq!(hints.snapshot().retry, Some(Duration::from_millis(500)));

        // A later stream that says nothing must not erase where we got to:
        // resuming from the start would replay everything already seen.
        hints.record(&stream(None, None, true));
        assert_eq!(hints.snapshot().last_event_id.as_deref(), Some("event-2"));
        assert_eq!(hints.snapshot().retry, Some(Duration::from_millis(500)));

        // A newer position does replace the old one.
        hints.record(&stream(Some("event-9"), None, true));
        assert_eq!(hints.snapshot().last_event_id.as_deref(), Some("event-9"));
    }

    #[tokio::test]
    async fn an_unanswered_stream_asks_to_be_resumed() {
        let hints = Arc::new(StreamHints::default());

        // Nothing is waiting yet, so this must not deadlock a later waiter.
        hints.record(&stream(Some("event-1"), None, true));

        let waiting = hints.clone();
        let resumed = tokio::spawn(async move { waiting.resume_requested().await });
        tokio::task::yield_now().await;

        // Answered: no resume. Unanswered: resume.
        hints.record(&stream(Some("event-2"), None, true));
        tokio::task::yield_now().await;
        assert!(
            !resumed.is_finished(),
            "an answered stream asked for a resume"
        );

        hints.record(&stream(Some("event-3"), None, false));
        tokio::time::timeout(Duration::from_secs(1), resumed)
            .await
            .expect("an unanswered stream must ask to be resumed")
            .expect("the waiter completes");
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_is_settled_only_once_and_releases_waiters() {
        let ready = Arc::new(Ready::default());
        let waiting = ready.clone();
        let released = tokio::spawn(async move { waiting.wait().await });
        tokio::task::yield_now().await;

        ready.signal();
        released.await.expect("the waiter is released");

        // Already settled: a later wait returns at once rather than waiting out
        // the timeout.
        ready.wait().await;
    }

    #[test]
    fn unsupported_statuses_are_the_ones_that_mean_no_stream() {
        // A 404/405/501 is the server saying it has no GET endpoint. Anything
        // else — including 500 — may be transient and is worth retrying.
        for status in UNSUPPORTED {
            assert!(UNSUPPORTED.contains(&status));
        }
        for status in [200u16, 429, 500, 502, 503] {
            assert!(
                !UNSUPPORTED.contains(&status),
                "{status} must not stop the listener"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_for_a_session_gives_up_rather_than_blocking_forever() {
        // A server that never assigns a session id must not keep the stream
        // from opening at all.
        let session = Arc::new(std::sync::Mutex::new(None));
        await_session(&session).await;
        assert!(session.lock().unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_stops_as_soon_as_the_session_arrives() {
        let session = Arc::new(std::sync::Mutex::new(None));
        let assigned = session.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SESSION_POLL).await;
            *assigned.lock().unwrap() = Some("session-1".to_string());
        });

        await_session(&session).await;
        assert_eq!(
            session.lock().unwrap().clone(),
            Some("session-1".to_string())
        );
    }
}
