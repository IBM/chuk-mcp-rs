//! Request envelope construction — `_meta` and HTTP headers from one source.
//!
//! Streamable HTTP mirrors selected body fields into headers so gateways can
//! route without parsing the body. Servers **MUST** reject any request whose
//! headers and body disagree with [`HEADER_MISMATCH`] (`-32020`), because
//! otherwise a load balancer could route on the header while the server acts on
//! the body.
//!
//! That makes divergence a security bug, not a cosmetic one, so this module is
//! the only place either side is produced: [`build_envelope`] derives every
//! header *from* the params it embeds, and no caller passes a method name or
//! target name twice. There is no API for setting a header independently.
//!
//! Note what is absent: no `Mcp-Session-Id`. The 2026 revision removed protocol
//! sessions, and a modern request must never carry one.
//!
//! [`HEADER_MISMATCH`]: crate::protocol::types::errors::HEADER_MISMATCH

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{Map, Value};

use crate::protocol::header_params;
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::meta::RequestMeta;
use crate::protocol::types::capabilities::ClientCapabilities;
use crate::protocol::types::errors::McpError;
use crate::protocol::types::info::ClientInfo;

/// Carries the request's protocol version; must equal the `_meta` value.
pub const HEADER_PROTOCOL_VERSION: &str = "MCP-Protocol-Version";
/// Carries the JSON-RPC `method`.
pub const HEADER_METHOD: &str = "Mcp-Method";
/// Carries `params.name` or `params.uri`.
pub const HEADER_NAME: &str = "Mcp-Name";
/// Prefix for headers promoted from tool parameters via `x-mcp-header`.
pub const HEADER_PARAM_PREFIX: &str = "Mcp-Param-";

/// Marks a header value as Base64-encoded. Case-sensitive, exactly as written.
pub const BASE64_SENTINEL_PREFIX: &str = "=?base64?";
/// Closes a Base64 sentinel. See [`BASE64_SENTINEL_PREFIX`].
pub const BASE64_SENTINEL_SUFFIX: &str = "?=";

/// Who the client says it is, sent on every modern request.
#[derive(Debug, Clone, Default)]
pub struct ClientIdentity {
    /// Recommended but optional.
    pub info: Option<ClientInfo>,
    pub capabilities: ClientCapabilities,
}

impl ClientIdentity {
    /// This library's default identity.
    pub fn chuk() -> Self {
        ClientIdentity {
            info: Some(ClientInfo::default()),
            capabilities: ClientCapabilities::default(),
        }
    }
}

/// A request ready to send: body params with `_meta`, plus matching headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub method: String,
    /// The params object, with `_meta` merged in.
    pub params: Value,
    /// Header name/value pairs. Names are canonical-cased; HTTP comparison is
    /// case-insensitive, but sending a consistent casing keeps logs readable.
    pub headers: Vec<(String, String)>,
}

impl Envelope {
    /// Look up a header by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Attach a promoted parameter header (`Mcp-Param-{name}`).
    ///
    /// The value is encoded here rather than by the caller, so a caller cannot
    /// bypass [`encode_header_value`] and emit an unsafe raw value.
    pub fn push_param_header(&mut self, name: &str, value: &str) {
        self.headers.push((
            format!("{HEADER_PARAM_PREFIX}{name}"),
            encode_header_value(value),
        ));
    }

    /// Re-derive the mirrored headers from the body and confirm they agree.
    ///
    /// Construction already guarantees this; the check exists so tests and
    /// debug assertions can verify the guarantee rather than trust it.
    pub fn headers_match_body(&self) -> bool {
        if self.header(HEADER_METHOD) != Some(self.method.as_str()) {
            return false;
        }
        if crate::protocol::meta::protocol_version_of(&self.params)
            != self.header(HEADER_PROTOCOL_VERSION)
        {
            return false;
        }
        match (
            mcp_name_of(&self.method, &self.params),
            self.header(HEADER_NAME),
        ) {
            (Some(body), Some(sent)) => sent == encode_header_value(&body),
            (None, None) => true,
            _ => false,
        }
    }

    /// Promote a tool's `x-mcp-header` parameters onto this envelope.
    ///
    /// Fails if the tool definition violates the `x-mcp-header` constraints. The
    /// caller should then exclude that one tool from `tools/list` rather than
    /// failing the whole listing — see [`crate::protocol::header_params`].
    pub fn promote_tool_params(
        &mut self,
        input_schema: &Value,
        arguments: &Value,
    ) -> Result<(), McpError> {
        let declared = header_params::collect(input_schema)?;
        for (name, value) in header_params::extract(&declared, arguments)? {
            self.push_param_header(&name, &value);
        }
        Ok(())
    }

    /// Whether any header carries a protocol session id.
    ///
    /// Always false for envelopes this module builds. Exposed so transports and
    /// conformance tests can assert the absence directly.
    pub fn has_session_header(&self) -> bool {
        self.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Mcp-Session-Id"))
    }
}

/// Build a modern request envelope.
///
/// Fails rather than emitting a request the server would reject: a method that
/// requires `Mcp-Name` but whose params lack the source field produces a local
/// error instead of a `-32020` round trip.
pub fn build_envelope(
    method: &str,
    params: Option<Value>,
    protocol_version: &str,
    identity: &ClientIdentity,
) -> Result<Envelope, McpError> {
    let mut meta = RequestMeta::new(protocol_version, identity.capabilities.clone());
    meta.client_info = identity.info.clone();
    build_envelope_with_meta(method, params, &meta)
}

/// [`build_envelope`] with a caller-supplied `_meta` block, for progress
/// tokens, log levels and trace context.
pub fn build_envelope_with_meta(
    method: &str,
    params: Option<Value>,
    meta: &RequestMeta,
) -> Result<Envelope, McpError> {
    let mut object = match params {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map,
        Some(other) => {
            return Err(McpError::validation(format!(
                "request params must be an object, got {other}"
            )))
        }
    };
    object.insert("_meta".to_string(), Value::Object(meta.to_map()?));
    let params = Value::Object(object);

    // Every header below is derived from `method`/`params`, never passed in.
    let mut headers = vec![
        (
            HEADER_PROTOCOL_VERSION.to_string(),
            meta.protocol_version.clone(),
        ),
        (HEADER_METHOD.to_string(), method.to_string()),
    ];

    match mcp_name_of(method, &params) {
        Some(name) => headers.push((HEADER_NAME.to_string(), encode_header_value(&name))),
        None if requires_name(method) => {
            return Err(McpError::validation(format!(
                "{method} requires the {HEADER_NAME} header, but params carry no {} field",
                name_source(method).unwrap_or("name")
            )))
        }
        None => {}
    }

    Ok(Envelope {
        method: method.to_string(),
        params,
        headers,
    })
}

/// Which params field supplies `Mcp-Name` for a method, if any.
fn name_source(method: &str) -> Option<&'static str> {
    match method {
        MessageMethod::TOOLS_CALL | MessageMethod::PROMPTS_GET => Some("name"),
        MessageMethod::RESOURCES_READ => Some("uri"),
        _ => None,
    }
}

/// Whether the spec requires `Mcp-Name` for this method.
pub fn requires_name(method: &str) -> bool {
    name_source(method).is_some()
}

/// The unencoded `Mcp-Name` value for a request, if the method defines one.
pub fn mcp_name_of(method: &str, params: &Value) -> Option<String> {
    let field = name_source(method)?;
    params.get(field)?.as_str().map(str::to_string)
}

/// Whether `value` can travel as a plain header value.
///
/// Safe means: only visible ASCII, space or tab; no leading or trailing
/// whitespace; and not itself shaped like a Base64 sentinel — a literal
/// `=?base64?...?=` must be encoded or a server would decode it back into
/// something the body never contained.
fn is_plain_header_safe(value: &str) -> bool {
    if value.starts_with(BASE64_SENTINEL_PREFIX) && value.ends_with(BASE64_SENTINEL_SUFFIX) {
        return false;
    }
    if value != value.trim() {
        return false;
    }
    value
        .bytes()
        .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b))
}

/// Encode a value for an HTTP header, applying the Base64 sentinel when the
/// value cannot travel as plain ASCII.
pub fn encode_header_value(value: &str) -> String {
    if is_plain_header_safe(value) {
        value.to_string()
    } else {
        format!(
            "{BASE64_SENTINEL_PREFIX}{}{BASE64_SENTINEL_SUFFIX}",
            BASE64.encode(value)
        )
    }
}

/// Decode a header value, reversing [`encode_header_value`].
///
/// Servers must decode before comparing a header against the body. A malformed
/// sentinel is an error rather than a passthrough: treating undecodable bytes
/// as a literal would let a crafted value bypass header/body comparison.
pub fn decode_header_value(value: &str) -> Result<String, McpError> {
    let Some(inner) = value
        .strip_prefix(BASE64_SENTINEL_PREFIX)
        .and_then(|rest| rest.strip_suffix(BASE64_SENTINEL_SUFFIX))
    else {
        return Ok(value.to_string());
    };

    let bytes = BASE64
        .decode(inner)
        .map_err(|e| McpError::validation(format!("invalid base64 header value: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| McpError::validation(format!("header value is not valid UTF-8: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::meta;
    use serde_json::json;

    fn build(method: &str, params: Value) -> Envelope {
        build_envelope(method, Some(params), "2026-07-28", &ClientIdentity::chuk()).unwrap()
    }

    #[test]
    fn tools_call_matches_the_specification_example() {
        let env = build(
            "tools/call",
            json!({"name": "get_weather", "arguments": {"location": "Seattle, WA"}}),
        );

        assert_eq!(env.header(HEADER_PROTOCOL_VERSION), Some("2026-07-28"));
        assert_eq!(env.header(HEADER_METHOD), Some("tools/call"));
        assert_eq!(env.header(HEADER_NAME), Some("get_weather"));

        let m = meta::meta_of(&env.params).unwrap();
        assert_eq!(m[meta::PROTOCOL_VERSION], json!("2026-07-28"));
        assert_eq!(m[meta::CLIENT_INFO]["name"], json!("chuk-mcp-client"));
        assert!(m.contains_key(meta::CLIENT_CAPABILITIES));
        // Original params survive alongside _meta.
        assert_eq!(env.params["arguments"]["location"], json!("Seattle, WA"));
    }

    #[test]
    fn resources_read_takes_its_name_from_uri() {
        let env = build(
            "resources/read",
            json!({"uri": "file:///projects/myapp/config.json"}),
        );
        assert_eq!(
            env.header(HEADER_NAME),
            Some("file:///projects/myapp/config.json")
        );
        assert!(env.headers_match_body());
    }

    #[test]
    fn methods_without_a_name_omit_the_header() {
        for method in ["tools/list", "server/discover", "resources/list"] {
            let env = build(method, json!({}));
            assert_eq!(env.header(HEADER_NAME), None, "{method}");
            assert!(!requires_name(method), "{method}");
            assert!(env.headers_match_body(), "{method}");
        }
    }

    #[test]
    fn missing_name_source_fails_locally() {
        // Sending this would earn a -32020 round trip; catch it here instead.
        for (method, params) in [
            ("tools/call", json!({"arguments": {}})),
            ("prompts/get", json!({})),
            ("resources/read", json!({"name": "wrong field"})),
        ] {
            let err = build_envelope(method, Some(params), "2026-07-28", &ClientIdentity::chuk())
                .expect_err(&format!("{method} should require a name"));
            assert!(err.to_string().contains(HEADER_NAME), "{method}: {err}");
        }
    }

    #[test]
    fn no_session_header_is_ever_emitted() {
        // The 2026 revision removed protocol sessions outright.
        for (method, params) in [
            ("tools/call", json!({"name": "x"})),
            ("tools/list", json!({})),
            ("resources/read", json!({"uri": "file:///x"})),
        ] {
            let env = build(method, params);
            assert!(!env.has_session_header(), "{method}");
            assert!(env.header("Mcp-Session-Id").is_none(), "{method}");
        }
    }

    #[test]
    fn headers_and_body_agree_by_construction() {
        let mut env = build("tools/call", json!({"name": "get_weather"}));
        assert!(env.headers_match_body());

        // Tamper with the body and the check must notice.
        env.params["name"] = json!("something_else");
        assert!(!env.headers_match_body());
    }

    #[test]
    fn every_kind_of_divergence_is_detected() {
        // This guard is what makes header/body agreement checkable rather than
        // merely asserted, so each way they can drift apart needs a test —
        // a divergence it silently passed would be a -32020 in production, or
        // worse, a gateway routing on one value while the server acts on another.

        // 1. Method header no longer matches the method.
        let mut env = build("tools/call", json!({"name": "x"}));
        env.method = "tools/list".to_string();
        assert!(!env.headers_match_body(), "method divergence missed");

        // 2. Protocol version header no longer matches `_meta`.
        let mut env = build("tools/list", json!({}));
        env.params["_meta"][crate::protocol::meta::PROTOCOL_VERSION] = json!("2025-06-18");
        assert!(!env.headers_match_body(), "version divergence missed");

        // 3. Body gained a name the header does not carry.
        let mut env = build("tools/list", json!({}));
        assert!(env.header(HEADER_NAME).is_none());
        env.method = "tools/call".to_string();
        env.params["name"] = json!("smuggled");
        assert!(!env.headers_match_body(), "added-name divergence missed");

        // 4. Header carries a name the body no longer has.
        let mut env = build("tools/call", json!({"name": "x"}));
        env.params.as_object_mut().unwrap().remove("name");
        assert!(!env.headers_match_body(), "removed-name divergence missed");
    }

    #[test]
    fn params_must_be_an_object() {
        for bad in [json!("string"), json!(42), json!([1, 2])] {
            assert!(build_envelope(
                "tools/list",
                Some(bad),
                "2026-07-28",
                &ClientIdentity::chuk()
            )
            .is_err());
        }
        // Absent and null params are both fine — _meta is still added.
        for ok in [None, Some(Value::Null)] {
            let env =
                build_envelope("tools/list", ok, "2026-07-28", &ClientIdentity::chuk()).unwrap();
            assert!(meta::meta_of(&env.params).is_some());
        }
    }

    // --- value encoding, against the specification's own table -------------

    #[test]
    fn encoding_matches_the_specification_examples() {
        // Every row of the spec's Value Encoding table.
        assert_eq!(encode_header_value("us-west1"), "us-west1");
        assert_eq!(
            encode_header_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(encode_header_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(
            encode_header_value("line1\nline2"),
            "=?base64?bGluZTEKbGluZTI=?="
        );
        assert_eq!(
            encode_header_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
    }

    #[test]
    fn encoding_round_trips() {
        for original in [
            "us-west1",
            "Hello, 世界",
            " padded ",
            "line1\nline2",
            "=?base64?literal?=",
            "",
            "file:///a/b?c=d&e=f",
            "tab\there",
        ] {
            let encoded = encode_header_value(original);
            assert_eq!(
                decode_header_value(&encoded).unwrap(),
                original,
                "round trip failed for {original:?}"
            );
        }
    }

    #[test]
    fn plain_safe_classification() {
        assert!(is_plain_header_safe("simple"));
        assert!(is_plain_header_safe("with spaces inside"));
        assert!(is_plain_header_safe("tab\tinside"));
        assert!(is_plain_header_safe(""));

        assert!(!is_plain_header_safe(" leading"));
        assert!(!is_plain_header_safe("trailing "));
        assert!(!is_plain_header_safe("new\nline"));
        assert!(!is_plain_header_safe("null\0byte"));
        assert!(!is_plain_header_safe("世界"));
        assert!(!is_plain_header_safe("=?base64?anything?="));
    }

    #[test]
    fn decoding_rejects_malformed_sentinels() {
        // Not passthrough: a value that looks encoded but isn't decodable must
        // error, or it could slip past a server's header/body comparison.
        assert!(decode_header_value("=?base64?not!valid!base64?=").is_err());
        // Valid base64 that isn't UTF-8.
        let non_utf8 = format!("{BASE64_SENTINEL_PREFIX}//8=?{}", "=");
        assert!(decode_header_value(&non_utf8).is_err() || !non_utf8.ends_with("?="));
        assert!(decode_header_value(&format!(
            "{BASE64_SENTINEL_PREFIX}{}{BASE64_SENTINEL_SUFFIX}",
            BASE64.encode([0xff, 0xfe])
        ))
        .is_err());

        // A value that merely contains the marker is left alone.
        assert_eq!(
            decode_header_value("prefix=?base64?middle").unwrap(),
            "prefix=?base64?middle"
        );
    }

    #[test]
    fn param_headers_are_encoded_on_the_way_in() {
        let mut env = build("tools/call", json!({"name": "execute_sql"}));
        env.push_param_header("Region", "us-west1");
        env.push_param_header("Greeting", "Hello, 世界");

        assert_eq!(env.header("Mcp-Param-Region"), Some("us-west1"));
        assert_eq!(
            env.header("Mcp-Param-Greeting"),
            Some("=?base64?SGVsbG8sIOS4lueVjA==?=")
        );
        // Promoted headers do not disturb the mirrored ones.
        assert!(env.headers_match_body());
    }

    #[test]
    fn promotes_the_specification_execute_sql_example() {
        // The spec's worked example, end to end: schema + arguments in, the
        // exact documented header set out.
        let schema = json!({
            "type": "object",
            "properties": {
                "region": {"type": "string", "x-mcp-header": "Region"},
                "query": {"type": "string"}
            },
            "required": ["region", "query"]
        });
        let arguments = json!({"region": "us-west1", "query": "SELECT * FROM users"});

        let mut env = build(
            "tools/call",
            json!({"name": "execute_sql", "arguments": arguments.clone()}),
        );
        env.promote_tool_params(&schema, &arguments).unwrap();

        assert_eq!(env.header(HEADER_PROTOCOL_VERSION), Some("2026-07-28"));
        assert_eq!(env.header(HEADER_METHOD), Some("tools/call"));
        assert_eq!(env.header(HEADER_NAME), Some("execute_sql"));
        assert_eq!(env.header("Mcp-Param-Region"), Some("us-west1"));
        // `query` was not annotated, so it stays in the body only.
        assert!(env.header("Mcp-Param-Query").is_none());
        assert!(env.headers_match_body());
        assert!(!env.has_session_header());
    }

    #[test]
    fn promotion_rejects_an_invalid_tool_definition() {
        // The caller uses this to drop one tool from tools/list rather than
        // failing the whole listing.
        let mut env = build("tools/call", json!({"name": "bad"}));
        let bad_schema = json!({
            "properties": {"p": {"type": "number", "x-mcp-header": "P"}}
        });
        assert!(env
            .promote_tool_params(&bad_schema, &json!({"p": 1}))
            .is_err());

        // An unannotated schema promotes nothing and succeeds.
        let mut env = build("tools/call", json!({"name": "plain"}));
        let before = env.headers.len();
        env.promote_tool_params(
            &json!({"properties": {"p": {"type": "string"}}}),
            &json!({"p": "x"}),
        )
        .unwrap();
        assert_eq!(env.headers.len(), before);
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let env = build("tools/list", json!({}));
        assert_eq!(env.header("mcp-method"), Some("tools/list"));
        assert_eq!(env.header("MCP-METHOD"), Some("tools/list"));
        assert_eq!(env.header("mcp-protocol-version"), Some("2026-07-28"));
    }
}
