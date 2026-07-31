//! What a tool handler can say while it is still running.
//!
//! Most tools answer and stop. Some need to speak first: report progress on
//! something slow, log what they are doing, ask the client to sample a model,
//! or ask the user a question. All four are messages sent *before* the result,
//! which is what separates this from simply returning a value.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::protocol::json_rpc::{create_notification, create_request, JsonRpcMessage};
use crate::protocol::messages::method::MessageMethod;

use super::pending::PendingRequests;

/// How long a server request waits for the client before giving up.
///
/// Without a limit a client that never answers holds the tool — and the HTTP
/// response it is streaming on — open for the life of the process.
pub const DEFAULT_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Notification parameter names.
const FIELD_PROGRESS_TOKEN: &str = "progressToken";
const FIELD_PROGRESS: &str = "progress";
const FIELD_TOTAL: &str = "total";
const FIELD_LEVEL: &str = "level";
const FIELD_DATA: &str = "data";
const FIELD_LOGGER: &str = "logger";

/// Handed to a tool handler so it can reach the client mid-call.
///
/// Cloneable and cheap: a handler may pass it to whatever does the work.
#[derive(Clone)]
pub struct CallContext {
    outbound: Option<mpsc::UnboundedSender<JsonRpcMessage>>,
    pending: Arc<PendingRequests>,
    progress_token: Option<Value>,
    timeout: Duration,
}

impl CallContext {
    /// A context that can reach the client.
    pub fn new(
        outbound: mpsc::UnboundedSender<JsonRpcMessage>,
        pending: Arc<PendingRequests>,
        progress_token: Option<Value>,
        timeout: Duration,
    ) -> Self {
        CallContext {
            outbound: Some(outbound),
            pending,
            progress_token,
            timeout,
        }
    }

    /// A context with nowhere to send.
    ///
    /// What a tool gets on a transport with no way back — a plain stdio
    /// exchange, or a direct `handle_message`. Notifications are dropped and
    /// requests fail rather than hanging: a handler must be able to tell that
    /// asking is not possible here.
    pub fn detached() -> Self {
        CallContext {
            outbound: None,
            pending: Arc::new(PendingRequests::new()),
            progress_token: None,
            timeout: DEFAULT_CLIENT_TIMEOUT,
        }
    }

    /// Whether anything sent here can actually reach the client.
    pub fn is_connected(&self) -> bool {
        self.outbound.is_some()
    }

    /// The token the caller asked progress to be reported under, if any.
    ///
    /// Progress is only reported when the client asked for it, so a handler
    /// that wants to skip the work of measuring can check first.
    pub fn progress_token(&self) -> Option<&Value> {
        self.progress_token.as_ref()
    }

    /// Send a notification to the client. Nothing is expected back.
    pub fn notify(&self, method: &str, params: Value) {
        let Some(outbound) = &self.outbound else {
            tracing::debug!("dropped {method}: this call has no way back to the client");
            return;
        };
        let message = JsonRpcMessage::Notification(create_notification(method, Some(params)));
        // A closed channel means the client hung up; the call will find that
        // out when it tries to answer.
        let _ = outbound.send(message);
    }

    /// Report progress against the caller's token.
    ///
    /// Does nothing when the caller did not ask for progress — an unsolicited
    /// token is one the client cannot match to anything.
    pub fn progress(&self, progress: f64, total: Option<f64>) {
        let Some(token) = self.progress_token.clone() else {
            return;
        };
        let mut params = json!({FIELD_PROGRESS_TOKEN: token, FIELD_PROGRESS: progress});
        if let Some(total) = total {
            params[FIELD_TOTAL] = json!(total);
        }
        self.notify(MessageMethod::NOTIFICATION_PROGRESS, params);
    }

    /// Send a log message to the client.
    pub fn log(&self, level: &str, data: Value) {
        self.notify(
            MessageMethod::NOTIFICATION_MESSAGE,
            json!({FIELD_LEVEL: level, FIELD_DATA: data}),
        );
    }

    /// Send a log message attributed to a named logger.
    pub fn log_from(&self, level: &str, logger: &str, data: Value) {
        self.notify(
            MessageMethod::NOTIFICATION_MESSAGE,
            json!({FIELD_LEVEL: level, FIELD_LOGGER: logger, FIELD_DATA: data}),
        );
    }

    /// Ask the client something and wait for its answer.
    ///
    /// Errors when there is no way back to the client, when the client refuses,
    /// or when it does not answer within the timeout — a handler needs to tell
    /// those apart from an answer of "no".
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let Some(outbound) = &self.outbound else {
            return Err(format!(
                "cannot ask the client for {method} on this transport"
            ));
        };

        let (id, receiver) = self.pending.issue();
        let request =
            JsonRpcMessage::Request(create_request(method, Some(params), Some(id.clone()), None));

        if outbound.send(request).is_err() {
            self.pending.forget(&id);
            return Err(format!("the client hung up before {method} could be sent"));
        }

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(answer)) => answer,
            // The sender was dropped without answering.
            Ok(Err(_)) => {
                self.pending.forget(&id);
                Err(format!("the client never answered {method}"))
            }
            Err(_) => {
                self.pending.forget(&id);
                Err(format!(
                    "the client did not answer {method} within {:?}",
                    self.timeout
                ))
            }
        }
    }

    /// Ask the client to sample a model.
    pub async fn sample(&self, params: Value) -> Result<Value, String> {
        self.request(MessageMethod::SAMPLING_CREATE_MESSAGE, params)
            .await
    }

    /// Ask the client to put a question to its user.
    pub async fn elicit(&self, params: Value) -> Result<Value, String> {
        self.request(MessageMethod::ELICITATION_CREATE, params)
            .await
    }
}

impl std::fmt::Debug for CallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallContext")
            .field("connected", &self.is_connected())
            .field("progress_token", &self.progress_token)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context wired to a channel the test can read.
    fn wired(token: Option<Value>) -> (CallContext, mpsc::UnboundedReceiver<JsonRpcMessage>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let context = CallContext::new(
            sender,
            Arc::new(PendingRequests::new()),
            token,
            Duration::from_millis(50),
        );
        (context, receiver)
    }

    #[tokio::test]
    async fn a_notification_reaches_the_client() {
        let (context, mut receiver) = wired(None);
        assert!(context.is_connected());

        context.log("info", json!("started"));
        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(sent.method(), Some(MessageMethod::NOTIFICATION_MESSAGE));
        assert_eq!(sent.params().unwrap()[FIELD_LEVEL], json!("info"));
        assert_eq!(sent.params().unwrap()[FIELD_DATA], json!("started"));
    }

    #[tokio::test]
    async fn a_named_logger_is_carried_alongside_the_level() {
        let (context, mut receiver) = wired(None);
        context.log_from("warning", "kettle", json!("empty"));

        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(sent.params().unwrap()[FIELD_LOGGER], json!("kettle"));
    }

    #[tokio::test]
    async fn progress_is_reported_only_when_it_was_asked_for() {
        let (context, mut receiver) = wired(Some(json!("token-1")));
        assert_eq!(context.progress_token(), Some(&json!("token-1")));

        context.progress(50.0, Some(100.0));
        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(sent.method(), Some(MessageMethod::NOTIFICATION_PROGRESS));
        let params = sent.params().unwrap();
        assert_eq!(params[FIELD_PROGRESS_TOKEN], json!("token-1"));
        assert_eq!(params[FIELD_PROGRESS], json!(50.0));
        assert_eq!(params[FIELD_TOTAL], json!(100.0));

        // A total is optional; the field is absent rather than invented.
        context.progress(60.0, None);
        let sent = receiver.recv().await.expect("a notification");
        assert!(sent.params().unwrap().get(FIELD_TOTAL).is_none());
    }

    #[tokio::test]
    async fn progress_without_a_token_is_not_sent() {
        let (context, mut receiver) = wired(None);
        context.progress(10.0, Some(100.0));

        drop(context);
        // Nothing was sent: an unsolicited token matches nothing at the client.
        assert!(receiver.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_request_is_answered_by_the_client() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(PendingRequests::new());
        let context = CallContext::new(sender, pending.clone(), None, Duration::from_secs(5));

        let asking = tokio::spawn(async move { context.sample(json!({"maxTokens": 10})).await });

        let sent = receiver.recv().await.expect("a request");
        assert_eq!(sent.method(), Some(MessageMethod::SAMPLING_CREATE_MESSAGE));
        let id = sent.id().expect("a request carries an id").clone();

        pending.resolve(&JsonRpcMessage::Response(
            crate::protocol::json_rpc::create_response(id, Some(json!({"role": "assistant"}))),
        ));

        assert_eq!(
            asking.await.expect("the task finished"),
            Ok(json!({"role": "assistant"}))
        );
    }

    #[tokio::test]
    async fn a_client_that_never_answers_times_out_rather_than_hanging() {
        let (context, _receiver) = wired(None);
        let outcome = context.elicit(json!({"message": "your name?"})).await;

        let error = outcome.expect_err("no answer came");
        assert!(error.contains(MessageMethod::ELICITATION_CREATE), "{error}");
    }

    #[tokio::test]
    async fn a_detached_context_drops_notifications_and_refuses_requests() {
        let context = CallContext::detached();
        assert!(!context.is_connected());

        // Dropped, not panicked: a tool must work on a transport with no way
        // back, it just cannot be heard.
        context.log("info", json!("nobody is listening"));
        context.progress(1.0, None);

        let error = context
            .sample(json!({}))
            .await
            .expect_err("there is nowhere to ask");
        assert!(
            error.contains(MessageMethod::SAMPLING_CREATE_MESSAGE),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_client_that_hung_up_fails_the_request_at_once() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(PendingRequests::new());
        let context = CallContext::new(sender, pending.clone(), None, Duration::from_secs(60));
        drop(receiver);

        let error = context
            .sample(json!({}))
            .await
            .expect_err("nowhere to send");
        assert!(error.contains("hung up"), "{error}");
        // Nothing is left waiting for an answer that cannot come.
        assert_eq!(pending.outstanding(), 0);
    }

    #[tokio::test]
    async fn a_refusal_from_the_client_is_an_error_not_an_answer() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let pending = Arc::new(PendingRequests::new());
        let context = CallContext::new(sender, pending.clone(), None, Duration::from_secs(5));

        let asking = tokio::spawn(async move { context.elicit(json!({})).await });
        let id = receiver
            .recv()
            .await
            .expect("a request")
            .id()
            .expect("an id")
            .clone();

        pending.resolve(&JsonRpcMessage::Error(
            crate::protocol::json_rpc::create_error_response(id, -32601, "no elicitation", None),
        ));

        assert_eq!(
            asking.await.expect("the task finished"),
            Err("no elicitation".to_string())
        );
    }

    #[test]
    fn the_debug_view_says_whether_anyone_is_listening() {
        assert!(format!("{:?}", CallContext::detached()).contains("connected: false"));
    }
}
