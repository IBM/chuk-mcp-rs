//! Era detection.
//!
//! **The two transports do not share an algorithm.** Using one for the other is
//! a correctness bug, and on HTTP it also costs a wasted round trip on every
//! connection.
//!
//! ## stdio — probe
//!
//! stdio is the only transport that probes. Issue `server/discover`; a result
//! means modern, and so does a rejection carrying a recognised modern error
//! code. Anything else — above all [`METHOD_NOT_FOUND`], which is what a legacy
//! server answers an unknown method with — means legacy, and the client falls
//! back to the `initialize` handshake.
//!
//! ## Streamable HTTP — never probe
//!
//! The first real request *is* the probe. Issue it in modern form. On `400`,
//! inspect the body: a recognised modern JSON-RPC error identifies a modern
//! server, anything else means legacy and the call is retried under the legacy
//! driver. Probing here would add a round trip to every connection for
//! information the first call already yields.
//!
//! [`METHOD_NOT_FOUND`]: crate::protocol::types::errors::METHOD_NOT_FOUND

use serde_json::Value;

use crate::protocol::types::errors::{is_modern_protocol_error, McpError};

/// The conclusion of a detection step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// The peer positively identified itself as speaking the 2026-era protocol.
    Modern,
    /// The peer is not modern; use the legacy stateful lifecycle.
    Legacy,
    /// Nothing was learned — the failure was below the protocol. **Must not be
    /// cached**, and must not be treated as legacy.
    Undetermined,
}

impl Detection {
    /// Whether this conclusion is safe to store in the era cache.
    pub fn is_conclusive(self) -> bool {
        !matches!(self, Detection::Undetermined)
    }
}

/// Classify the outcome of a **stdio** `server/discover` probe.
pub fn classify_probe_result(result: &Result<Value, McpError>) -> Detection {
    match result {
        Ok(_) => Detection::Modern,
        Err(err) => classify_probe_error(err),
    }
}

/// Classify a failed **stdio** `server/discover` probe.
///
/// A recognised modern error still identifies a *modern* server: an
/// `UnsupportedProtocolVersion` means "I speak the 2026 protocol and dislike
/// your version", not "I am legacy".
pub fn classify_probe_error(err: &McpError) -> Detection {
    // Transport-level failures say nothing about the peer's protocol. Calling
    // them legacy would pin — and cache — the wrong era for an endpoint that
    // merely timed out or had its connection dropped.
    if matches!(
        err,
        McpError::Timeout(_)
            | McpError::Transport(_)
            | McpError::Io(_)
            | McpError::Cancelled(_)
            | McpError::Json(_)
    ) {
        return Detection::Undetermined;
    }

    match err.code() {
        Some(code) if is_modern_protocol_error(code) => Detection::Modern,
        _ => Detection::Legacy,
    }
}

/// Classify the body of a **Streamable HTTP** `400` response.
///
/// Only call this for a `400`. A body that is not JSON, or is JSON without a
/// recognised modern error code, means legacy — including the HTML error pages
/// that proxies and gateways substitute for the real response.
pub fn classify_http_error_body(body: &str) -> Detection {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Detection::Legacy;
    };

    match error_code_of(&value) {
        Some(code) if is_modern_protocol_error(code) => Detection::Modern,
        _ => Detection::Legacy,
    }
}

/// Pull a JSON-RPC error code out of a response body, looking inside batches.
fn error_code_of(value: &Value) -> Option<i64> {
    match value {
        Value::Array(items) => items.iter().find_map(error_code_of),
        _ => value
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(Value::as_i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::errors::{
        HEADER_MISMATCH, INTERNAL_ERROR, METHOD_NOT_FOUND, MISSING_REQUIRED_CLIENT_CAPABILITY,
        UNSUPPORTED_PROTOCOL_VERSION,
    };
    use serde_json::json;
    use std::time::Duration;

    fn rpc(code: i64) -> McpError {
        McpError::from_json_rpc(code, "boom", None)
    }

    // --- stdio ------------------------------------------------------------

    #[test]
    fn probe_success_is_modern() {
        assert_eq!(
            classify_probe_result(&Ok(json!({"protocolVersions": ["2026-07-28"]}))),
            Detection::Modern
        );
    }

    #[test]
    fn probe_rejection_with_a_modern_code_is_still_modern() {
        // The subtle one: these are rejections, but only a modern server can
        // produce them, so they identify the era just as well as a success.
        for code in [
            UNSUPPORTED_PROTOCOL_VERSION,
            HEADER_MISMATCH,
            MISSING_REQUIRED_CLIENT_CAPABILITY,
        ] {
            assert_eq!(
                classify_probe_error(&rpc(code)),
                Detection::Modern,
                "code {code} should identify a modern peer"
            );
        }
    }

    #[test]
    fn probe_method_not_found_is_legacy() {
        // What a legacy server actually answers `server/discover` with.
        assert_eq!(
            classify_probe_error(&rpc(METHOD_NOT_FOUND)),
            Detection::Legacy
        );
        assert_eq!(
            classify_probe_error(&rpc(INTERNAL_ERROR)),
            Detection::Legacy
        );
        assert_eq!(classify_probe_error(&rpc(-32000)), Detection::Legacy);
    }

    #[test]
    fn transport_failures_are_undetermined_not_legacy() {
        // Regression guard: a timeout must never pin an endpoint to legacy.
        let cases = [
            McpError::Timeout(Duration::from_secs(1)),
            McpError::Transport("connection reset".into()),
            McpError::Cancelled("req-1".into()),
            McpError::Io(std::io::Error::other("broken pipe")),
        ];
        for err in &cases {
            assert_eq!(
                classify_probe_error(err),
                Detection::Undetermined,
                "{err} should be undetermined"
            );
            assert!(!classify_probe_error(err).is_conclusive());
        }
    }

    // --- Streamable HTTP --------------------------------------------------

    #[test]
    fn http_400_with_a_modern_code_is_modern() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": UNSUPPORTED_PROTOCOL_VERSION, "message": "unsupported"}
        })
        .to_string();
        assert_eq!(classify_http_error_body(&body), Detection::Modern);
    }

    #[test]
    fn http_400_finds_a_modern_code_inside_a_batch() {
        let body = json!([
            {"jsonrpc": "2.0", "id": 1, "result": {}},
            {"jsonrpc": "2.0", "id": 2, "error": {"code": HEADER_MISMATCH, "message": "no"}}
        ])
        .to_string();
        assert_eq!(classify_http_error_body(&body), Detection::Modern);
    }

    #[test]
    fn http_400_otherwise_is_legacy() {
        // A legacy server's own JSON-RPC error.
        let legacy = json!({
            "jsonrpc": "2.0", "id": 1,
            "error": {"code": METHOD_NOT_FOUND, "message": "unknown"}
        })
        .to_string();
        assert_eq!(classify_http_error_body(&legacy), Detection::Legacy);

        // A gateway that swallowed the body and substituted its own page.
        assert_eq!(
            classify_http_error_body("<html><body>400 Bad Request</body></html>"),
            Detection::Legacy
        );
        assert_eq!(classify_http_error_body(""), Detection::Legacy);
        assert_eq!(classify_http_error_body("{}"), Detection::Legacy);
        assert_eq!(
            classify_http_error_body(r#"{"error": {}}"#),
            Detection::Legacy
        );
    }

    #[test]
    fn http_detection_never_returns_undetermined() {
        // HTTP has no probe to fail: a 400 is always conclusive one way or the
        // other, because the first real call already reached the server.
        for body in ["", "{}", "not json", r#"{"error":{"code":-32022}}"#] {
            assert!(classify_http_error_body(body).is_conclusive());
        }
    }
}
