//! What a `2026-07-28` request obliges a server to do differently.
//!
//! Three things, all of them per-request rather than per-connection:
//!
//! - the client declares its protocol version in `_meta` on **every** request,
//!   and a server that cannot speak it must say so with the versions it can;
//! - every result carries a `resultType`, where a legacy result carried none;
//! - nothing is remembered between requests, so there is no session to check.
//!
//! A legacy request reaching the same server is left exactly as it was: the
//! era is a property of the request, not of the server, and one server answers
//! both.

use serde_json::{json, Map, Value};

use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use crate::protocol::meta;
use crate::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION;
use crate::protocol::versioning;

/// The `resultType` field a modern result must carry.
const FIELD_RESULT_TYPE: &str = "resultType";
/// Where an unsupported-version error lists what the server *can* speak.
const FIELD_SUPPORTED: &str = "supported";

/// The protocol version a request declared, if it declared one.
///
/// Only the modern era declares per-request; a legacy request carries its
/// version once, in `initialize`.
pub fn declared_version(message: &JsonRpcMessage) -> Option<String> {
    let params = message.params()?;
    meta::protocol_version_of(params).map(str::to_string)
}

/// Whether this request is a modern one.
pub fn is_modern_request(message: &JsonRpcMessage) -> bool {
    declared_version(message)
        .map(|version| versioning::is_modern_version(&version))
        .unwrap_or(false)
}

/// The error a server owes a client whose declared version it cannot speak.
///
/// Carries the supported list, because a bare rejection leaves the client
/// nothing to retry with — and renegotiating from that list is exactly what
/// our own client does.
pub fn unsupported_version_error(version: &str) -> (i64, String, Option<Value>) {
    (
        UNSUPPORTED_PROTOCOL_VERSION,
        format!("unsupported protocol version: {version}"),
        Some(json!({ FIELD_SUPPORTED: versioning::SUPPORTED_VERSIONS })),
    )
}

/// Check a declared version, returning the error to send if it is unusable.
pub fn reject_unsupported_version(
    message: &JsonRpcMessage,
) -> Option<(i64, String, Option<Value>)> {
    let declared = declared_version(message)?;
    if versioning::is_supported(&declared) {
        None
    } else {
        Some(unsupported_version_error(&declared))
    }
}

/// Stamp `resultType: "complete"` on a result that has none.
///
/// Applied only to modern responses: adding it to a legacy one would send a
/// field that revision never defined. A result that already declares its type
/// — an `input_required`, say — is left alone.
pub fn stamp_result_type(result: &mut Value) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    object
        .entry(FIELD_RESULT_TYPE)
        .or_insert_with(|| json!(RESULT_TYPE_COMPLETE));
}

/// Apply the modern result rules to a response message, if the request that
/// produced it was modern.
pub fn finish_response(response: &mut JsonRpcMessage, modern: bool) {
    if !modern {
        return;
    }
    if let JsonRpcMessage::Response(response) = response {
        stamp_result_type(&mut response.result);
    }
}

/// Params with `_meta` declaring a protocol version, for tests and fixtures.
pub fn params_with_version(version: &str, params: Value) -> Value {
    let mut object = match params {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let mut meta_map = Map::new();
    meta_map.insert(meta::PROTOCOL_VERSION.to_string(), json!(version));
    object.insert("_meta".to_string(), Value::Object(meta_map));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::{create_request, create_response, RequestId};

    fn request(params: Value) -> JsonRpcMessage {
        JsonRpcMessage::Request(create_request(
            "tools/list",
            Some(params),
            Some(RequestId::Str("r1".into())),
            None,
        ))
    }

    #[test]
    fn a_declared_modern_version_marks_the_request_modern() {
        let message = request(params_with_version(
            versioning::FIRST_MODERN_VERSION,
            json!({}),
        ));
        assert_eq!(
            declared_version(&message).as_deref(),
            Some(versioning::FIRST_MODERN_VERSION)
        );
        assert!(is_modern_request(&message));
    }

    #[test]
    fn a_legacy_request_declares_nothing_per_request() {
        // No `_meta` at all: the legacy era declares its version once, in
        // `initialize`, and never again.
        let message = request(json!({}));
        assert_eq!(declared_version(&message), None);
        assert!(!is_modern_request(&message));

        // A legacy version declared in `_meta` is still not a modern request.
        let declared_legacy = request(params_with_version(versioning::V2025_06_18, json!({})));
        assert!(!is_modern_request(&declared_legacy));
    }

    #[test]
    fn an_unsupported_version_is_rejected_with_the_supported_list() {
        let message = request(params_with_version("1999-01-01", json!({})));
        let (code, text, data) = reject_unsupported_version(&message).expect("must be rejected");

        assert_eq!(code, UNSUPPORTED_PROTOCOL_VERSION);
        assert!(text.contains("1999-01-01"));
        // Without the list the client has nothing to renegotiate from.
        assert_eq!(
            data.expect("data")[FIELD_SUPPORTED],
            json!(versioning::SUPPORTED_VERSIONS)
        );
    }

    #[test]
    fn a_supported_version_is_not_rejected() {
        for version in versioning::SUPPORTED_VERSIONS {
            let message = request(params_with_version(version, json!({})));
            assert!(
                reject_unsupported_version(&message).is_none(),
                "{version} must be accepted"
            );
        }
        // Nothing declared, nothing to reject.
        assert!(reject_unsupported_version(&request(json!({}))).is_none());
    }

    #[test]
    fn modern_results_are_stamped_and_legacy_ones_left_alone() {
        let mut modern = JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"tools": []})),
        ));
        finish_response(&mut modern, true);
        assert_eq!(
            modern.result().unwrap()[FIELD_RESULT_TYPE],
            json!(RESULT_TYPE_COMPLETE)
        );

        let mut legacy = JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"tools": []})),
        ));
        finish_response(&mut legacy, false);
        assert!(
            legacy.result().unwrap().get(FIELD_RESULT_TYPE).is_none(),
            "a legacy result must not carry a field its revision never defined"
        );
    }

    #[test]
    fn a_result_that_declares_its_type_is_not_overwritten() {
        // An `input_required` must survive: stamping it "complete" would tell
        // the client the call had finished when it had not.
        let mut result = json!({"resultType": "input_required", "requestState": "s"});
        stamp_result_type(&mut result);
        assert_eq!(result[FIELD_RESULT_TYPE], json!("input_required"));

        // A non-object result is left entirely alone.
        let mut scalar = json!("not an object");
        stamp_result_type(&mut scalar);
        assert_eq!(scalar, json!("not an object"));
    }
}
