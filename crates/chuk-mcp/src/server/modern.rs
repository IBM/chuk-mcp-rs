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
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use crate::protocol::meta;
use crate::protocol::types::errors::{
    MISSING_REQUIRED_CLIENT_CAPABILITY, UNSUPPORTED_PROTOCOL_VERSION,
};
use crate::protocol::versioning;
use crate::server::caching::CachePolicy;

/// The `resultType` field a modern result must carry.
const FIELD_RESULT_TYPE: &str = "resultType";
/// Where an unsupported-version error lists what the server *can* speak.
const FIELD_SUPPORTED: &str = "supported";
/// Where it echoes back the version that was asked for.
const FIELD_REQUESTED: &str = "requested";

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

/// The RPCs the 2026-07-28 revision removed.
///
/// A server that still answers these in the modern era is not being generous,
/// it is lying about which revision it speaks: a client that gets a result
/// from `ping` learns the server is pre-2026 and may then assume sessions,
/// `initialize` and the rest of the stateful lifecycle exist too. Answering
/// `Method not found` is the honest reply, and the one the specification
/// requires.
///
/// `resources/subscribe` and its opposite are here because
/// [`subscriptions/listen`](crate::protocol::messages::method::MessageMethod::SUBSCRIPTIONS_LISTEN)
/// replaced them wholesale. The same request over a *legacy* connection is
/// still served — era is a property of the request, not of the server.
pub const REMOVED_METHODS: &[&str] = &[
    MessageMethod::INITIALIZE,
    MessageMethod::NOTIFICATION_INITIALIZED,
    MessageMethod::PING,
    MessageMethod::LOGGING_SET_LEVEL,
    MessageMethod::RESOURCES_SUBSCRIBE,
    MessageMethod::RESOURCES_UNSUBSCRIBE,
    MessageMethod::NOTIFICATION_ROOTS_LIST_CHANGED,
];

/// Whether `method` was removed by the 2026-07-28 revision.
pub fn is_removed_method(method: &str) -> bool {
    REMOVED_METHODS.contains(&method)
}

/// The error a server owes a client whose declared version it cannot speak.
///
/// Carries the supported list, because a bare rejection leaves the client
/// nothing to retry with — and renegotiating from that list is exactly what
/// our own client does. It also echoes the version that was asked for: a
/// client with several requests in flight cannot otherwise tell which one this
/// rejection belongs to, since the error says nothing else about the request.
pub fn unsupported_version_error(version: &str) -> (i64, String, Option<Value>) {
    (
        UNSUPPORTED_PROTOCOL_VERSION,
        format!("unsupported protocol version: {version}"),
        Some(json!({
            FIELD_SUPPORTED: versioning::SUPPORTED_VERSIONS,
            FIELD_REQUESTED: version,
        })),
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

/// Why this request's `_meta` is not one a modern server can act on, if it is
/// not.
///
/// The 2026-07-28 revision carries the protocol version and the client's
/// capabilities on **every** request, and a server **MUST NOT** infer either
/// from an earlier request on the same connection — there is no connection to
/// infer along any more. A request missing them is therefore malformed rather
/// than merely terse, and the answer is [`INVALID_PARAMS`].
///
/// `clientInfo` is deliberately not required: the specification makes it a
/// SHOULD, and rejecting a request for omitting it would turn a recommendation
/// into a barrier.
///
/// Only meaningful for a request already known to be modern. A legacy request
/// carries none of this and is not asking to be judged by these rules — which
/// is why the caller supplies the era rather than this function guessing at it.
///
/// [`INVALID_PARAMS`]: crate::protocol::types::errors::INVALID_PARAMS
pub fn missing_required_meta(message: &JsonRpcMessage) -> Option<String> {
    // A notification is not answerable, so there is nothing to reject it with.
    message.id()?;

    let params = message.params();
    let Some(meta) = params.and_then(meta::meta_of) else {
        return Some(format!(
            "a {} request must carry params._meta",
            versioning::CURRENT_VERSION
        ));
    };

    for required in [meta::PROTOCOL_VERSION, meta::CLIENT_CAPABILITIES] {
        if !meta.contains_key(required) {
            return Some(format!("params._meta is missing {required}"));
        }
    }
    None
}

/// The client capabilities a request declared, as raw names.
///
/// Empty for a legacy request, which declares its capabilities once through
/// `initialize` rather than on every call.
pub fn declared_capabilities(message: &JsonRpcMessage) -> Vec<String> {
    let Some(params) = message.params() else {
        return Vec::new();
    };
    let Some(meta) = meta::meta_of(params) else {
        return Vec::new();
    };
    meta.get(meta::CLIENT_CAPABILITIES)
        .and_then(Value::as_object)
        .map(|declared| declared.keys().cloned().collect())
        .unwrap_or_default()
}

/// The error a server owes a client that asked for something needing a
/// capability it never declared.
///
/// `data.requiredCapabilities` is a `ClientCapabilities` *object* — the shape
/// the client would have had to send — rather than a list of names, so the
/// client can compare it against what it declared without reformatting it.
pub fn missing_capability_error(required: &[String]) -> (i64, String, Option<Value>) {
    let mut shape = serde_json::Map::new();
    for name in required {
        shape.insert(name.clone(), json!({}));
    }
    (
        MISSING_REQUIRED_CLIENT_CAPABILITY,
        format!(
            "this request requires client capabilities: {}",
            required.join(", ")
        ),
        Some(json!({ "requiredCapabilities": Value::Object(shape) })),
    )
}

/// Which of `required` the request did not declare.
pub fn undeclared(message: &JsonRpcMessage, required: &[String]) -> Vec<String> {
    let declared = declared_capabilities(message);
    required
        .iter()
        .filter(|name| !declared.contains(name))
        .cloned()
        .collect()
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
///
/// `method` is the method of the request being answered, which decides whether
/// the result is a cacheable one — the caching hints are keyed by operation,
/// not by result shape, so the response alone cannot say.
pub fn finish_response(
    response: &mut JsonRpcMessage,
    modern: bool,
    method: Option<&str>,
    caching: &CachePolicy,
) {
    if !modern {
        return;
    }
    if let JsonRpcMessage::Response(response) = response {
        // Hints first: they apply to a `complete` result, and a result that
        // has not been stamped yet is exactly that.
        if let Some(method) = method {
            caching.stamp(method, &mut response.result);
        }
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
        let policy = CachePolicy::default();
        let listed = Some(crate::protocol::messages::method::MessageMethod::TOOLS_LIST);

        let mut modern = JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"tools": []})),
        ));
        finish_response(&mut modern, true, listed, &policy);
        assert_eq!(
            modern.result().unwrap()[FIELD_RESULT_TYPE],
            json!(RESULT_TYPE_COMPLETE)
        );
        // A cacheable modern result carries its hints too.
        assert!(modern.result().unwrap().get("ttlMs").is_some());
        assert!(modern.result().unwrap().get("cacheScope").is_some());

        let mut legacy = JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"tools": []})),
        ));
        finish_response(&mut legacy, false, listed, &policy);
        let result = legacy.result().unwrap();
        assert!(
            result.get(FIELD_RESULT_TYPE).is_none(),
            "a legacy result must not carry a field its revision never defined"
        );
        assert!(
            result.get("ttlMs").is_none() && result.get("cacheScope").is_none(),
            "caching hints arrived with 2026-07-28 and do not belong on a legacy result"
        );
    }

    /// A result whose operation is not one of the six cacheable ones is
    /// stamped with its `resultType` and nothing else.
    #[test]
    fn a_non_cacheable_modern_result_gets_no_hints() {
        let mut answer = JsonRpcMessage::Response(create_response(
            RequestId::Str("r1".into()),
            Some(json!({"content": []})),
        ));
        finish_response(
            &mut answer,
            true,
            Some(crate::protocol::messages::method::MessageMethod::TOOLS_CALL),
            &CachePolicy::default(),
        );
        let result = answer.result().unwrap();
        assert_eq!(result[FIELD_RESULT_TYPE], json!(RESULT_TYPE_COMPLETE));
        assert!(result.get("ttlMs").is_none());
    }

    #[test]
    fn declared_capabilities_are_read_off_the_meta_block() {
        let declaring = request(json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {"sampling": {}, "roots": {}},
        }}));
        let mut declared = declared_capabilities(&declaring);
        declared.sort();
        assert_eq!(declared, vec!["roots".to_string(), "sampling".to_string()]);

        // A legacy request declares once, through initialize, so there is
        // nothing per-request to read.
        assert!(declared_capabilities(&request(json!({}))).is_empty());
    }

    #[test]
    fn undeclared_names_only_what_is_missing() {
        let declaring = request(json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {"sampling": {}},
        }}));
        let required = vec!["sampling".to_string(), "elicitation".to_string()];
        assert_eq!(undeclared(&declaring, &required), vec!["elicitation"]);
        assert!(undeclared(&declaring, &["sampling".to_string()]).is_empty());
    }

    /// `requiredCapabilities` is the shape the client would have had to send,
    /// not a list of names — so it can be compared with what it did send.
    #[test]
    fn the_missing_capability_error_names_the_shape_not_a_list() {
        let (code, message, data) = missing_capability_error(&["sampling".to_string()]);

        assert_eq!(code, MISSING_REQUIRED_CLIENT_CAPABILITY);
        assert!(message.contains("sampling"), "{message}");
        let data = data.expect("the error carried no data");
        assert_eq!(data["requiredCapabilities"], json!({"sampling": {}}));
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
