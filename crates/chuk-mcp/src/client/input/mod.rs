//! Answering a server that asks for more.
//!
//! A server needing user input says so differently in each era — the
//! `2026-07-28` revision returns an
//! [`InputRequired`](crate::protocol::mrtr::InputRequired) result and expects
//! the request retried, while a legacy server pushes an `elicitation/create`
//! request mid-call. Both are answered by one [`InputHandler`], which is what
//! lets identical caller code drive either.
//!
//! ```no_run
//! use async_trait::async_trait;
//! use chuk_mcp::client::input::InputHandler;
//! use chuk_mcp::protocol::mrtr::{ElicitRequest, ElicitResult};
//! use serde_json::{Map, json};
//!
//! struct AlwaysOctocat;
//!
//! #[async_trait]
//! impl InputHandler for AlwaysOctocat {
//!     async fn elicit(&self, request: ElicitRequest) -> ElicitResult {
//!         let mut content = Map::new();
//!         content.insert("name".into(), json!("octocat"));
//!         ElicitResult::accept(content)
//!     }
//! }
//! ```

mod driver;
mod legacy;

pub(crate) use driver::call_with_input;
pub use driver::MAX_INPUT_ROUNDS;
pub(crate) use legacy::PushedRequestBridge;

use async_trait::async_trait;
use serde_json::Value;

use crate::protocol::mrtr::{ElicitRequest, ElicitResult, InputRequest};

/// Answers the requests a server embeds in an `input_required` result, or
/// pushes at a legacy client mid-call.
///
/// Only [`elicit`](InputHandler::elicit) must be implemented. Sampling and
/// roots default to unsupported, which is honest: a client that has not
/// declared those capabilities must not be sent them in the first place, and a
/// server that does anyway gets a clear answer rather than a fabricated one.
#[async_trait]
pub trait InputHandler: Send + Sync {
    /// Ask the user, and say what they did.
    ///
    /// Returning [`ElicitResult::decline`] or [`ElicitResult::cancel`] is
    /// always valid — a user who says no is not an error.
    async fn elicit(&self, request: ElicitRequest) -> ElicitResult;

    /// Answer a `sampling/createMessage` request. `None` means unsupported.
    async fn sample(&self, _params: Value) -> Option<Value> {
        None
    }

    /// Answer a `roots/list` request. `None` means unsupported.
    async fn roots(&self) -> Option<Value> {
        None
    }

    /// Whether this handler supports URL-mode elicitation.
    ///
    /// Declared to the server in `_meta`, and a server **MUST NOT** send a mode
    /// the client has not declared — so a handler that cannot open a browser
    /// should say so here rather than decline every URL request it is sent.
    fn supports_url_mode(&self) -> bool {
        false
    }
}

/// A handler that declines everything.
///
/// Useful as a placeholder and in tests: it exercises the whole round-trip
/// path — the server still gets a well-formed answer and can complete or
/// re-ask — without needing a user.
pub struct DeclineAll;

#[async_trait]
impl InputHandler for DeclineAll {
    async fn elicit(&self, _request: ElicitRequest) -> ElicitResult {
        ElicitResult::decline()
    }
}

/// Answer one embedded request, whatever kind it is.
///
/// `None` means this handler cannot answer that kind, which the driver reports
/// as a missing answer rather than inventing one.
pub(crate) async fn answer(
    handler: &dyn InputHandler,
    request: &InputRequest,
) -> Result<Option<Value>, crate::McpError> {
    use crate::protocol::messages::method::MessageMethod;

    match request.method.as_str() {
        MessageMethod::ELICITATION_CREATE => {
            let elicit = request
                .as_elicitation()
                .expect("method matched elicitation/create")?;
            let result = handler.elicit(elicit).await;
            Ok(Some(serde_json::to_value(result)?))
        }
        MessageMethod::SAMPLING_CREATE_MESSAGE => Ok(handler.sample(request.params.clone()).await),
        MessageMethod::ROOTS_LIST => Ok(handler.roots().await),
        // A server may only embed those three. Anything else is a server bug,
        // and answering it would be inventing a protocol.
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::method::MessageMethod;
    use serde_json::json;

    #[tokio::test]
    async fn decline_all_answers_elicitations_and_nothing_else() {
        let handler = DeclineAll;

        let elicitation = InputRequest {
            method: MessageMethod::ELICITATION_CREATE.to_string(),
            params: json!({"message": "who are you?"}),
        };
        let answered = answer(&handler, &elicitation).await.unwrap().unwrap();
        assert_eq!(answered, json!({"action": "decline"}));

        for method in [
            MessageMethod::SAMPLING_CREATE_MESSAGE,
            MessageMethod::ROOTS_LIST,
            MessageMethod::TOOLS_LIST,
        ] {
            let request = InputRequest {
                method: method.to_string(),
                params: json!({}),
            };
            assert_eq!(answer(&handler, &request).await.unwrap(), None);
        }

        assert!(!handler.supports_url_mode());
    }

    #[tokio::test]
    async fn a_malformed_elicitation_is_reported_not_declined() {
        let handler = DeclineAll;
        let request = InputRequest {
            method: MessageMethod::ELICITATION_CREATE.to_string(),
            params: json!({"mode": "form"}), // no message
        };
        assert!(answer(&handler, &request).await.is_err());
    }

    #[tokio::test]
    async fn sampling_and_roots_can_be_supported() {
        struct Full;

        #[async_trait]
        impl InputHandler for Full {
            async fn elicit(&self, _request: ElicitRequest) -> ElicitResult {
                ElicitResult::cancel()
            }
            async fn sample(&self, _params: Value) -> Option<Value> {
                Some(json!({"role": "assistant"}))
            }
            async fn roots(&self) -> Option<Value> {
                Some(json!({"roots": []}))
            }
            fn supports_url_mode(&self) -> bool {
                true
            }
        }

        let handler = Full;
        assert!(handler.supports_url_mode());
        assert_eq!(
            answer(
                &handler,
                &InputRequest {
                    method: MessageMethod::SAMPLING_CREATE_MESSAGE.to_string(),
                    params: json!({}),
                }
            )
            .await
            .unwrap(),
            Some(json!({"role": "assistant"}))
        );
        assert_eq!(
            answer(
                &handler,
                &InputRequest {
                    method: MessageMethod::ROOTS_LIST.to_string(),
                    params: json!({}),
                }
            )
            .await
            .unwrap(),
            Some(json!({"roots": []}))
        );
    }
}
