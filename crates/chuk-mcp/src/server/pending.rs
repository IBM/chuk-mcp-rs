//! Requests this server has asked the client, waiting for their answers.
//!
//! A server request travels out on one connection and its answer arrives on
//! another — the client POSTs the response as a fresh HTTP request. Something
//! has to hold the two ends together, and this is it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::oneshot;

use crate::protocol::json_rpc::{JsonRpcMessage, RequestId};

/// Prefixes the ids this server generates.
///
/// Client and server number their requests independently, so both may hold an
/// id `1`. The prefix keeps a log readable about which side asked.
const SERVER_ID_PREFIX: &str = "srv-";

/// What a server request eventually produced.
pub type Answer = Result<Value, String>;

/// The requests this server is waiting on.
#[derive(Debug, Default)]
pub struct PendingRequests {
    next: AtomicU64,
    waiting: Mutex<HashMap<String, oneshot::Sender<Answer>>>,
}

impl PendingRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a request about to be sent, yielding its id and the receiver
    /// that will hold the answer.
    pub fn issue(&self) -> (RequestId, oneshot::Receiver<Answer>) {
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        let id = format!("{SERVER_ID_PREFIX}{number}");
        let (sender, receiver) = oneshot::channel();

        self.lock().insert(id.clone(), sender);
        (RequestId::Str(id), receiver)
    }

    /// Deliver a client's answer to whoever is waiting for it.
    ///
    /// Returns whether anybody was: a response to an id this server never
    /// issued is the client's confusion, not ours, and the caller needs to
    /// know it has an unclaimed message rather than a delivered one.
    pub fn resolve(&self, message: &JsonRpcMessage) -> bool {
        let Some(id) = message.id() else {
            return false;
        };
        let Some(sender) = self.lock().remove(&id.to_string()) else {
            return false;
        };

        let answer = match message.error() {
            Some(error) => Err(error.message.clone()),
            None => Ok(message.result().cloned().unwrap_or(Value::Null)),
        };
        // A receiver dropped before the answer arrived means the caller gave
        // up — the send failing is that, and nothing to report.
        let _ = sender.send(answer);
        true
    }

    /// Give up on a request, so a caller that timed out leaves nothing behind.
    pub fn forget(&self, id: &RequestId) {
        self.lock().remove(&id.to_string());
    }

    /// How many requests are outstanding.
    pub fn outstanding(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Answer>>> {
        self.waiting
            .lock()
            .expect("the pending-request lock is never held across a panic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::{create_error_response, create_response};
    use serde_json::json;

    fn response_to(id: &RequestId, result: Value) -> JsonRpcMessage {
        JsonRpcMessage::Response(create_response(id.clone(), Some(result)))
    }

    #[tokio::test]
    async fn an_answer_reaches_whoever_asked() {
        let pending = PendingRequests::new();
        let (id, receiver) = pending.issue();
        assert_eq!(pending.outstanding(), 1);

        assert!(pending.resolve(&response_to(&id, json!({"ok": true}))));
        assert_eq!(receiver.await.expect("an answer"), Ok(json!({"ok": true})));
        // Answered once, and no longer waited on.
        assert_eq!(pending.outstanding(), 0);
    }

    #[tokio::test]
    async fn an_error_answer_arrives_as_an_error() {
        let pending = PendingRequests::new();
        let (id, receiver) = pending.issue();

        let refused = JsonRpcMessage::Error(create_error_response(id, -1, "no sampling", None));
        assert!(pending.resolve(&refused));
        assert_eq!(
            receiver.await.expect("an answer"),
            Err("no sampling".to_string())
        );
    }

    #[test]
    fn every_request_gets_its_own_id() {
        let pending = PendingRequests::new();
        let (first, _a) = pending.issue();
        let (second, _b) = pending.issue();

        assert_ne!(first, second);
        assert!(first.to_string().starts_with(SERVER_ID_PREFIX));
        assert_eq!(pending.outstanding(), 2);
    }

    #[test]
    fn an_answer_to_something_never_asked_is_not_claimed() {
        let pending = PendingRequests::new();
        let stray = response_to(&RequestId::Str("srv-99".into()), json!({}));
        assert!(!pending.resolve(&stray));

        // Nor is a message with no id at all.
        let notification = JsonRpcMessage::Notification(
            crate::protocol::json_rpc::create_notification("notifications/message", None),
        );
        assert!(!pending.resolve(&notification));
    }

    #[test]
    fn a_forgotten_request_is_no_longer_waited_on() {
        let pending = PendingRequests::new();
        let (id, _receiver) = pending.issue();
        pending.forget(&id);

        assert_eq!(pending.outstanding(), 0);
        assert!(!pending.resolve(&response_to(&id, json!({}))));
    }

    #[tokio::test]
    async fn a_caller_that_gave_up_does_not_break_the_answer() {
        let pending = PendingRequests::new();
        let (id, receiver) = pending.issue();
        drop(receiver);

        // Claimed, even though nobody is left to hear it.
        assert!(pending.resolve(&response_to(&id, json!({}))));
    }
}
