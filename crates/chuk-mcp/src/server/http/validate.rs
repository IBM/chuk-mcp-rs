//! Checking that a request's headers and its body say the same thing.
//!
//! Streamable HTTP mirrors the method, the target name and the protocol version
//! into headers so a gateway can route without parsing the body. That is only
//! safe if the two agree: a load balancer routing on `Mcp-Method: tools/list`
//! while the server executes a `tools/call` in the body is a confused-deputy
//! bug, not a cosmetic inconsistency. [SEP-2243] therefore makes the server's
//! side of the bargain mandatory — it **MUST** reject any disagreement with
//! `400 Bad Request` and [`HEADER_MISMATCH`].
//!
//! What this module does *not* do is guess. Every check runs only against a
//! request that presents itself as a 2026-era one, because a legacy client
//! sends none of these headers and rejecting it for their absence would break
//! the era it is entitled to speak. See [`presents_as_modern`] for how that is
//! decided and why the decision is made from the version *value* rather than
//! from the header's presence.
//!
//! [SEP-2243]: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2243
//! [`HEADER_MISMATCH`]: crate::protocol::types::errors::HEADER_MISMATCH

use hyper::HeaderMap;
use serde_json::{json, Value};

use crate::protocol::envelope::{
    decode_header_value, mcp_name_of, requires_name, HEADER_METHOD, HEADER_NAME,
    HEADER_PARAM_PREFIX, HEADER_PROTOCOL_VERSION,
};
use crate::protocol::header_params;
use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::meta;
use crate::protocol::types::errors::{HEADER_MISMATCH, INVALID_PARAMS};
use crate::protocol::versioning;
use crate::server::{modern, McpServer, FIELD_ARGUMENTS, FIELD_NAME};

/// A reason to refuse a request before it is ever dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rejection {
    /// The JSON-RPC error code to answer with.
    pub code: i64,
    /// What was wrong, in terms the client can act on.
    pub message: String,
}

impl Rejection {
    fn mismatch(message: impl Into<String>) -> Self {
        Rejection {
            code: HEADER_MISMATCH,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Rejection {
            code: INVALID_PARAMS,
            message: message.into(),
        }
    }
}

/// Read a header, trimming the optional whitespace HTTP allows around a value.
///
/// RFC 9110 §5.5 requires a recipient to exclude leading and trailing
/// whitespace before interpreting a field value, so ` tools/list ` and
/// `tools/list` are the same header. Comparing without trimming would reject a
/// perfectly legal request.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
}

/// Whether this request is asking to be judged by the 2026-era rules.
///
/// Decided by the *value* of `MCP-Protocol-Version`, not by its presence: the
/// header has existed since `2025-03-26`, so a legacy client sends it too, and
/// treating presence as modernity would hold legacy clients to rules their
/// revision never had.
///
/// `Mcp-Method` is the fallback signal. Only a modern client sends it, so a
/// request carrying one is modern even if the version header went missing —
/// and if it went missing, saying so is more useful than silently serving it
/// as legacy.
///
/// A version that is neither modern nor a known legacy one — a typo, a future
/// revision, a fabricated string — counts as modern. It cannot be served as
/// legacy in any case, and the rejection it earns is a 2026-era one: `400`
/// with the list of versions this server does speak. Treating it as legacy
/// would answer `200` and leave the client with nothing to renegotiate from.
pub(crate) fn presents_as_modern(headers: &HeaderMap) -> bool {
    match header(headers, HEADER_PROTOCOL_VERSION) {
        Some(version) => !versioning::LEGACY_VERSIONS.contains(&version),
        None => headers.contains_key(HEADER_METHOD),
    }
}

/// Check a modern request's headers against its body.
///
/// `None` means the request may proceed. The caller is responsible for having
/// established that this is a modern request; see [`presents_as_modern`].
pub(crate) fn check(
    server: &McpServer,
    headers: &HeaderMap,
    message: &JsonRpcMessage,
) -> Option<Rejection> {
    let Some(method) = message.method() else {
        // A response flowing back to the server carries no method to mirror,
        // and none of these rules describe it.
        return None;
    };

    // --- Mcp-Method ------------------------------------------------------
    //
    // Required, and compared byte-for-byte after trimming: method names are
    // case-sensitive, so `TOOLS/LIST` names nothing and must not be accepted
    // as a spelling of `tools/list`.
    match header(headers, HEADER_METHOD) {
        None => {
            return Some(Rejection::mismatch(format!(
                "{HEADER_METHOD} is required on a {} request",
                versioning::CURRENT_VERSION
            )))
        }
        Some(sent) if sent != method => {
            return Some(Rejection::mismatch(format!(
                "{HEADER_METHOD} is {sent:?} but the body method is {method:?}"
            )))
        }
        Some(_) => {}
    }

    // --- MCP-Protocol-Version --------------------------------------------
    //
    // When the body declares a version too, the two must agree. A gateway that
    // routed on one while the server honoured the other would be applying the
    // wrong revision's rules to the request.
    if let (Some(sent), Some(params)) = (
        header(headers, HEADER_PROTOCOL_VERSION),
        message.params().as_ref(),
    ) {
        if let Some(declared) = meta::protocol_version_of(params) {
            if sent != declared {
                return Some(Rejection::mismatch(format!(
                    "{HEADER_PROTOCOL_VERSION} is {sent:?} but params._meta declares {declared:?}"
                )));
            }
        }
    }

    // --- Mcp-Name --------------------------------------------------------
    //
    // Carried only by the methods that name a target, and required for those.
    // The header value may be Base64-wrapped, so it is decoded before being
    // compared with the body — comparing the encoded forms would reject a
    // request whose name merely needed encoding.
    let params = message.params();
    let body_name = params
        .as_ref()
        .and_then(|params| mcp_name_of(method, params));
    match (header(headers, HEADER_NAME), &body_name) {
        (Some(sent), Some(expected)) => {
            let decoded = match decode_header_value(sent) {
                Ok(decoded) => decoded,
                Err(error) => {
                    return Some(Rejection::mismatch(format!(
                        "{HEADER_NAME} could not be decoded: {error}"
                    )))
                }
            };
            if &decoded != expected {
                return Some(Rejection::mismatch(format!(
                    "{HEADER_NAME} is {decoded:?} but the body names {expected:?}"
                )));
            }
        }
        (None, Some(expected)) => {
            return Some(Rejection::mismatch(format!(
                "{method} requires {HEADER_NAME}, and the body names {expected:?}"
            )))
        }
        // A method that must name a target but whose body names none is
        // malformed in the body, not in the headers.
        (_, None) if requires_name(method) => {
            return Some(Rejection::invalid(format!(
                "{method} requires a target name in its params"
            )))
        }
        _ => {}
    }

    // --- Mcp-Param-* ------------------------------------------------------
    if let Some(rejection) = check_promoted_params(server, method, params, headers) {
        return Some(rejection);
    }

    // --- params._meta -----------------------------------------------------
    //
    // Last, because a request whose headers misdescribe it is wrong in a way
    // the client should hear about first.
    modern::missing_required_meta(message).map(Rejection::invalid)
}

/// Check the headers a tool's `x-mcp-header` parameters were promoted into.
///
/// The promotion exists so an intermediary can route on an argument without
/// parsing the body, which only holds if the header and the argument agree.
/// A missing header for a parameter that *is* present in the body is the
/// dangerous case: a gateway would see no value where the server sees one, and
/// route accordingly.
///
/// A tool whose own schema is malformed is not the caller's fault, so it is not
/// the caller's rejection: the promotion is skipped and the call proceeds.
fn check_promoted_params(
    server: &McpServer,
    method: &str,
    params: Option<&Value>,
    headers: &HeaderMap,
) -> Option<Rejection> {
    if method != MessageMethod::TOOLS_CALL {
        return None;
    }
    let params = params?;
    let name = params.get(FIELD_NAME)?.as_str()?;
    let schema = server.tool_schema(name)?;
    let arguments = params.get(FIELD_ARGUMENTS).cloned().unwrap_or(json!({}));

    let declared = header_params::collect(schema).ok()?;
    let expected = header_params::extract(&declared, &arguments).ok()?;

    for (param, value) in expected {
        let header_name = format!("{HEADER_PARAM_PREFIX}{param}");
        let Some(sent) = header(headers, &header_name) else {
            return Some(Rejection::mismatch(format!(
                "{header_name} is required: the body carries {param} but no header mirrors it"
            )));
        };
        // Decoded before comparison, and a value that will not decode is a
        // rejection rather than a literal — treating undecodable bytes as text
        // would let a crafted header slip past this comparison entirely.
        let decoded = match decode_header_value(sent) {
            Ok(decoded) => decoded,
            Err(error) => {
                return Some(Rejection::mismatch(format!(
                    "{header_name} could not be decoded: {error}"
                )))
            }
        };
        if decoded != value {
            return Some(Rejection::mismatch(format!(
                "{header_name} is {decoded:?} but the body carries {value:?}"
            )));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::parse_message;
    /// A server with one tool that promotes a parameter into a header.
    fn server() -> McpServer {
        let mut server = McpServer::new("validate-test", "1.0.0", None);
        server.register_tool(
            "get_weather",
            json!({
                "type": "object",
                "properties": {
                    "region": {"type": "string", "x-mcp-header": "Region"},
                },
            }),
            "Test tool",
            |_| async { Ok(json!("ok")) },
        );
        server
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                hyper::header::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    /// A well-formed modern `_meta` block.
    fn meta_block() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        })
    }

    fn request(method: &str, params: Value) -> JsonRpcMessage {
        parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": method, "params": params
        }))
        .unwrap()
    }

    fn listing() -> JsonRpcMessage {
        request("tools/list", json!({"_meta": meta_block()}))
    }

    fn good_headers(method: &str) -> Vec<(&'static str, String)> {
        vec![
            ("MCP-Protocol-Version", "2026-07-28".to_string()),
            ("Mcp-Method", method.to_string()),
        ]
    }

    fn as_pairs<'a>(owned: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
        owned.iter().map(|(k, v)| (*k, v.as_str())).collect()
    }

    #[test]
    fn a_matching_request_passes() {
        let owned = good_headers("tools/list");
        assert_eq!(
            check(&server(), &headers(&as_pairs(&owned)), &listing()),
            None
        );
    }

    #[test]
    fn a_method_header_that_disagrees_with_the_body_is_a_mismatch() {
        let owned = good_headers("tools/call");
        let rejection = check(&server(), &headers(&as_pairs(&owned)), &listing()).unwrap();
        assert_eq!(rejection.code, HEADER_MISMATCH);
    }

    #[test]
    fn a_missing_method_header_is_a_mismatch() {
        let map = headers(&[("MCP-Protocol-Version", "2026-07-28")]);
        assert_eq!(
            check(&server(), &map, &listing()).unwrap().code,
            HEADER_MISMATCH
        );
    }

    /// Method names are case-sensitive, so this names nothing at all.
    #[test]
    fn an_uppercased_method_value_is_a_mismatch() {
        let owned = good_headers("TOOLS/LIST");
        assert_eq!(
            check(&server(), &headers(&as_pairs(&owned)), &listing())
                .unwrap()
                .code,
            HEADER_MISMATCH
        );
    }

    /// Header *names* are case-insensitive even though their values are not.
    #[test]
    fn header_names_are_matched_case_insensitively() {
        let map = headers(&[
            ("mcp-protocol-version", "2026-07-28"),
            ("MCP-METHOD", "tools/list"),
        ]);
        assert_eq!(check(&server(), &map, &listing()), None);
    }

    /// RFC 9110 §5.5: whitespace around a field value is not part of it.
    #[test]
    fn surrounding_whitespace_in_a_header_value_is_ignored() {
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "  tools/list  "),
        ]);
        assert_eq!(check(&server(), &map, &listing()), None);
    }

    #[test]
    fn a_name_header_must_match_the_body() {
        let call = request(
            "tools/call",
            json!({"name": "get_weather", "_meta": meta_block()}),
        );
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "something_else"),
        ]);
        assert_eq!(check(&server(), &map, &call).unwrap().code, HEADER_MISMATCH);
    }

    #[test]
    fn a_missing_name_header_is_a_mismatch_when_the_body_names_a_target() {
        let call = request(
            "tools/call",
            json!({"name": "get_weather", "_meta": meta_block()}),
        );
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
        ]);
        assert_eq!(check(&server(), &map, &call).unwrap().code, HEADER_MISMATCH);
    }

    /// A name needing encoding must still compare equal to the body's plain
    /// value, or every non-ASCII tool name would be unreachable.
    #[test]
    fn a_base64_encoded_name_is_decoded_before_comparison() {
        let call = request("tools/call", json!({"name": "café", "_meta": meta_block()}));
        let encoded = crate::protocol::envelope::encode_header_value("café");
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", &encoded),
        ]);
        assert_eq!(check(&server(), &map, &call), None);
    }

    #[test]
    fn resources_read_takes_its_name_from_the_uri() {
        let read = request(
            "resources/read",
            json!({"uri": "file:///x", "_meta": meta_block()}),
        );
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "resources/read"),
            ("Mcp-Name", "file:///x"),
        ]);
        assert_eq!(check(&server(), &map, &read), None);
    }

    #[test]
    fn a_version_header_that_disagrees_with_the_body_is_a_mismatch() {
        let listing = request(
            "tools/list",
            json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "v999.0.0",
                "io.modelcontextprotocol/clientCapabilities": {},
            }}),
        );
        let owned = good_headers("tools/list");
        let rejection = check(&server(), &headers(&as_pairs(&owned)), &listing).unwrap();
        assert_eq!(rejection.code, HEADER_MISMATCH);
    }

    #[test]
    fn missing_meta_fields_are_invalid_params() {
        let owned = good_headers("tools/list");
        let map = headers(&as_pairs(&owned));

        for params in [
            json!({}),
            json!({"_meta": {"io.modelcontextprotocol/clientCapabilities": {}}}),
            json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
        ] {
            let rejection = check(&server(), &map, &request("tools/list", params.clone())).unwrap();
            assert_eq!(
                rejection.code, INVALID_PARAMS,
                "{params} should be rejected as invalid params"
            );
        }
    }

    /// `clientInfo` is a SHOULD. Requiring it would turn a recommendation into
    /// a barrier.
    #[test]
    fn omitting_client_info_is_allowed() {
        let owned = good_headers("tools/list");
        assert_eq!(
            check(&server(), &headers(&as_pairs(&owned)), &listing()),
            None
        );
    }

    #[test]
    fn era_is_decided_by_the_version_value_not_the_header_being_present() {
        // A legacy client sends this header too — it has existed since
        // 2025-03-26 — so its presence alone must not pull the request into
        // the modern rules.
        assert!(!presents_as_modern(&headers(&[(
            "MCP-Protocol-Version",
            "2025-06-18"
        )])));
        assert!(presents_as_modern(&headers(&[(
            "MCP-Protocol-Version",
            "2026-07-28"
        )])));
        // No version header, but only a modern client sends Mcp-Method.
        assert!(presents_as_modern(&headers(&[(
            "Mcp-Method",
            "tools/list"
        )])));
        assert!(!presents_as_modern(&headers(&[])));
    }

    /// A notification cannot be answered, so there is no rejection to send.
    #[test]
    fn a_notification_missing_meta_is_not_rejected() {
        let notification = parse_message(&json!({
            "jsonrpc": "2.0", "method": "notifications/progress", "params": {}
        }))
        .unwrap();
        let map = headers(&[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "notifications/progress"),
        ]);
        assert_eq!(check(&server(), &map, &notification), None);
    }
}
