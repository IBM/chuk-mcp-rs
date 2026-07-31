//! The opaque state a server asks the client to carry between round trips.

use serde::{Deserialize, Serialize};

/// What `Debug` prints instead of the state itself.
///
/// The value is frequently an AEAD-protected blob binding the authenticated
/// principal and a TTL; a debug log is exactly where one should not end up.
const REDACTED: &str = "RequestState(<opaque>)";

/// Server state echoed back verbatim on the retry.
///
/// The specification is explicit that clients **MUST NOT** inspect, parse,
/// modify, or make any assumptions about the contents. There is deliberately no
/// way to read it as anything but the bytes to send back: no `Deref` to `str`,
/// no `Display`, and a `Debug` that redacts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestState(String);

impl RequestState {
    /// Wrap a value received from a server.
    pub fn new(raw: impl Into<String>) -> Self {
        RequestState(raw.into())
    }

    /// The value to echo on the retry. The only legitimate use.
    pub(crate) fn echo(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RequestState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_verbatim() {
        // Echoing "the exact value" is the whole contract, so anything that
        // normalises or re-encodes it is a bug.
        let raw = "eyJsb2NhdGlvbiI6Ik5ldyBZb3JrIn0...==";
        let state = RequestState::new(raw);
        assert_eq!(state.echo(), raw);
        assert_eq!(serde_json::to_value(&state).unwrap(), json!(raw));

        let decoded: RequestState = serde_json::from_value(json!(raw)).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn debug_does_not_leak_the_blob() {
        let state = RequestState::new("principal=alice;exp=1234;sig=deadbeef");
        let rendered = format!("{state:?}");
        assert_eq!(rendered, REDACTED);
        assert!(!rendered.contains("alice"));
    }
}
