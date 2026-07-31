//! The legacy half of the bridge.
//!
//! A `2025-11-25`-and-earlier server that needs input pushes a JSON-RPC
//! *request* at the client in the middle of handling one — `elicitation/create`,
//! `sampling/createMessage` or `roots/list` — and waits for a response before it
//! can finish. The modern era does the same job by returning
//! [`InputRequired`](crate::protocol::mrtr::InputRequired) and having the client
//! retry.
//!
//! Two protocols, one question. This adapts the pushed form onto the same
//! [`InputHandler`] the modern driver uses, which is what makes the phase gate
//! — *identical caller code drives both eras* — true rather than aspirational.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::protocol::messages::send_message::InboundRequestHandler;
use crate::protocol::mrtr::InputRequest;

use super::{answer, InputHandler};

/// Serves pushed server requests from an [`InputHandler`].
pub(crate) struct PushedRequestBridge {
    handler: Arc<dyn InputHandler>,
}

impl PushedRequestBridge {
    pub(crate) fn new(handler: Arc<dyn InputHandler>) -> Self {
        PushedRequestBridge { handler }
    }
}

#[async_trait]
impl InboundRequestHandler for PushedRequestBridge {
    async fn handle(&self, method: &str, params: Value) -> Option<Value> {
        // A pushed request carries the same method and params an embedded one
        // would, so it can be answered by exactly the same code.
        let request = InputRequest {
            method: method.to_string(),
            params,
        };

        match answer(self.handler.as_ref(), &request).await {
            Ok(answered) => answered,
            // A malformed request from the server is reported to the server as
            // unsupported rather than raised to the caller, whose own call is
            // unrelated and may still succeed.
            Err(error) => {
                tracing::warn!("could not answer pushed `{method}`: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::input::DeclineAll;
    use crate::protocol::messages::method::MessageMethod;
    use serde_json::json;

    #[tokio::test]
    async fn a_pushed_elicitation_is_answered_like_an_embedded_one() {
        let bridge = PushedRequestBridge::new(Arc::new(DeclineAll));
        let answered = bridge
            .handle(
                MessageMethod::ELICITATION_CREATE,
                json!({"message": "who are you?"}),
            )
            .await;
        assert_eq!(answered, Some(json!({"action": "decline"})));
    }

    #[tokio::test]
    async fn an_unsupported_push_is_declined_rather_than_invented() {
        let bridge = PushedRequestBridge::new(Arc::new(DeclineAll));
        for method in [
            MessageMethod::SAMPLING_CREATE_MESSAGE,
            MessageMethod::ROOTS_LIST,
            "something/else",
        ] {
            assert_eq!(bridge.handle(method, json!({})).await, None);
        }
    }

    #[tokio::test]
    async fn a_malformed_push_does_not_escape_to_the_caller() {
        let bridge = PushedRequestBridge::new(Arc::new(DeclineAll));
        // No `message`, so it cannot decode as an ElicitRequest.
        let answered = bridge
            .handle(MessageMethod::ELICITATION_CREATE, json!({"mode": "form"}))
            .await;
        assert_eq!(answered, None);
    }
}
