//! Multi Round-Trip Requests — how a `2026-07-28` server asks for more.
//!
//! The revision removed server-initiated requests entirely. A server that needs
//! `elicitation/create`, `sampling/createMessage` or `roots/list` answered no
//! longer sends the client a request; it *returns* one, as an
//! [`InputRequired`] result, and the client answers by **retrying the original
//! request** with the answers attached:
//!
//! ```text
//!   client ──▶ tools/call (id: 1)
//!   server ──▶ resultType: "input_required", inputRequests, requestState
//!   client ──▶ tools/call (id: 2, same params, + inputResponses + requestState)
//!   server ──▶ resultType: "complete"
//! ```
//!
//! The point of the shape is that the server needs no session: everything it
//! must remember is encoded into the opaque [`RequestState`] the client carries
//! back. Which is also why the client may not look inside it.
//!
//! Only `tools/call`, `resources/read` and `prompts/get` may be answered this
//! way — see [`supports_input_required`].
//!
//! The legacy era does the same job with a pushed `elicitation/create` request
//! mid-call. Both are driven by one caller-supplied handler; see
//! [`crate::client::input`].

mod elicitation;
mod exchange;
mod result;
mod state;

pub use elicitation::{ElicitAction, ElicitMode, ElicitRequest, ElicitResult};
pub use exchange::{InputRequest, InputRequests, InputResponses};
pub use result::{input_required_result, InputRequired, RESULT_TYPE_INPUT_REQUIRED};
pub use state::RequestState;

use crate::protocol::messages::method::MessageMethod;

/// The client requests a server may answer with an [`InputRequired`].
///
/// "Servers **MUST NOT** send `InputRequiredResult` responses on any other
/// client requests", so an `input_required` arriving on anything else is a
/// server error rather than something to drive a retry loop from.
pub const INPUT_REQUIRED_METHODS: [&str; 3] = [
    MessageMethod::TOOLS_CALL,
    MessageMethod::RESOURCES_READ,
    MessageMethod::PROMPTS_GET,
];

/// Whether `method` is one a server may answer with an [`InputRequired`].
pub fn supports_input_required(method: &str) -> bool {
    INPUT_REQUIRED_METHODS.contains(&method)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_specified_methods_support_input_required() {
        for method in INPUT_REQUIRED_METHODS {
            assert!(
                supports_input_required(method),
                "{method} should support it"
            );
        }
        for method in [
            MessageMethod::TOOLS_LIST,
            MessageMethod::RESOURCES_LIST,
            MessageMethod::PROMPTS_LIST,
            MessageMethod::PING,
            MessageMethod::SERVER_DISCOVER,
            MessageMethod::COMPLETION_COMPLETE,
        ] {
            assert!(
                !supports_input_required(method),
                "{method} must not support it"
            );
        }
    }
}
