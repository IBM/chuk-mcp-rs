//! Reserved `_meta` keys and the per-request protocol metadata block.
//!
//! The 2026-07-28 revision is stateless: instead of establishing the protocol
//! version, client identity and capabilities once through an `initialize`
//! handshake, every request carries them in `params._meta`. A server **MUST
//! NOT** infer them from earlier requests on the same connection.
//!
//! A request missing a required field is malformed: the server rejects it with
//! [`INVALID_PARAMS`] (`-32602`), and on HTTP that is a `400`. Required fields
//! are therefore non-optional in [`RequestMeta`] rather than merely documented.
//!
//! [`INVALID_PARAMS`]: crate::protocol::types::errors::INVALID_PARAMS

use serde_json::{Map, Value};

use crate::protocol::types::capabilities::ClientCapabilities;
use crate::protocol::types::errors::McpError;
use crate::protocol::types::info::{ClientInfo, ServerInfo};

/// Protocol version for this request. **Required** on every modern request.
pub const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
/// Client capabilities relevant to this request. **Required**.
///
/// A server must not rely on a capability the client did not declare here; if
/// it needs one, it returns `-32021` with `data.requiredCapabilities`.
pub const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
/// Client name and version. Clients **SHOULD** send it on every request.
pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
/// Minimum log level the server should emit for this request.
///
/// This replaces `logging/setLevel`, which the 2026 revision removed. A server
/// **MUST NOT** emit `notifications/message` for a request that omitted it, so
/// leaving this unset silences logging rather than defaulting it.
pub const LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
/// Server name and version. Servers set this on results, not clients.
pub const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
/// Correlates a notification with its originating `subscriptions/listen`.
pub const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
/// Opts a request into progress notifications.
pub const PROGRESS_TOKEN: &str = "progressToken";

/// W3C trace context. Deliberately unprefixed: these three keys are an
/// explicit exception to the reverse-DNS prefix rule, kept bare for
/// compatibility with OpenTelemetry semantic conventions.
pub const TRACEPARENT: &str = "traceparent";
/// See [`TRACEPARENT`].
pub const TRACESTATE: &str = "tracestate";
/// See [`TRACEPARENT`].
pub const BAGGAGE: &str = "baggage";

/// The `_meta` block a modern request carries in its params.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestMeta {
    /// Required. Must equal the `MCP-Protocol-Version` header on HTTP.
    pub protocol_version: String,
    /// Required.
    pub client_capabilities: ClientCapabilities,
    /// Recommended.
    pub client_info: Option<ClientInfo>,
    /// Omitted means "emit no log notifications for this request".
    pub log_level: Option<String>,
    pub progress_token: Option<Value>,
    /// Merged verbatim: trace context, extension keys, anything else.
    pub extra: Map<String, Value>,
}

impl RequestMeta {
    /// A metadata block with only the required fields populated.
    pub fn new(protocol_version: impl Into<String>, capabilities: ClientCapabilities) -> Self {
        RequestMeta {
            protocol_version: protocol_version.into(),
            client_capabilities: capabilities,
            client_info: None,
            log_level: None,
            progress_token: None,
            extra: Map::new(),
        }
    }

    pub fn with_client_info(mut self, info: ClientInfo) -> Self {
        self.client_info = Some(info);
        self
    }

    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = Some(level.into());
        self
    }

    pub fn with_progress_token(mut self, token: Value) -> Self {
        self.progress_token = Some(token);
        self
    }

    /// Render as a `_meta` object.
    ///
    /// `extra` is written first so reserved keys always win: an extension
    /// cannot accidentally override the protocol version or capabilities.
    pub fn to_map(&self) -> Result<Map<String, Value>, McpError> {
        let mut map = self.extra.clone();
        map.insert(
            PROTOCOL_VERSION.to_string(),
            Value::String(self.protocol_version.clone()),
        );
        map.insert(
            CLIENT_CAPABILITIES.to_string(),
            serde_json::to_value(&self.client_capabilities)?,
        );
        if let Some(info) = &self.client_info {
            map.insert(CLIENT_INFO.to_string(), serde_json::to_value(info)?);
        }
        if let Some(level) = &self.log_level {
            map.insert(LOG_LEVEL.to_string(), Value::String(level.clone()));
        }
        if let Some(token) = &self.progress_token {
            map.insert(PROGRESS_TOKEN.to_string(), token.clone());
        }
        Ok(map)
    }
}

/// Borrow the `_meta` object out of a params value, if present.
pub fn meta_of(params: &Value) -> Option<&Map<String, Value>> {
    params.get("_meta").and_then(Value::as_object)
}

/// Read the protocol version a request declared.
pub fn protocol_version_of(params: &Value) -> Option<&str> {
    meta_of(params)?.get(PROTOCOL_VERSION)?.as_str()
}

/// Read `io.modelcontextprotocol/serverInfo` out of a result's `_meta`.
///
/// Self-reported and unverified: use for display, logging and debugging only,
/// never for behavioural or security decisions.
pub fn server_info_of(result: &Value) -> Option<ServerInfo> {
    let info = meta_of(result)?.get(SERVER_INFO)?;
    serde_json::from_value(info.clone()).ok()
}

/// Read `io.modelcontextprotocol/subscriptionId` off a notification.
pub fn subscription_id_of(params: &Value) -> Option<&str> {
    meta_of(params)?.get(SUBSCRIPTION_ID)?.as_str()
}

/// Read the log level a request asked to be served at.
///
/// Absent means the request wants no log notifications at all, which is a
/// different thing from wanting them at a default level — see [`LOG_LEVEL`].
pub fn log_level_of(params: &Value) -> Option<&str> {
    meta_of(params)?.get(LOG_LEVEL)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta() -> RequestMeta {
        RequestMeta::new("2026-07-28", ClientCapabilities::default())
    }

    #[test]
    fn required_fields_are_always_present() {
        let map = meta().to_map().unwrap();
        assert_eq!(map[PROTOCOL_VERSION], json!("2026-07-28"));
        assert!(map.contains_key(CLIENT_CAPABILITIES));
        // Optional fields stay absent rather than serialising as null: a
        // present-but-null logLevel would read as "log at level null".
        assert!(!map.contains_key(CLIENT_INFO));
        assert!(!map.contains_key(LOG_LEVEL));
        assert!(!map.contains_key(PROGRESS_TOKEN));
    }

    #[test]
    fn optional_fields_round_trip() {
        let map = meta()
            .with_client_info(ClientInfo::default())
            .with_log_level("debug")
            .with_progress_token(json!("tok-1"))
            .to_map()
            .unwrap();

        assert_eq!(map[CLIENT_INFO]["name"], json!("chuk-mcp-client"));
        assert_eq!(map[LOG_LEVEL], json!("debug"));
        assert_eq!(map[PROGRESS_TOKEN], json!("tok-1"));
    }

    #[test]
    fn extras_cannot_override_reserved_keys() {
        let mut m = meta();
        m.extra
            .insert(TRACEPARENT.to_string(), json!("00-abc-def-01"));
        // A hostile or buggy extension trying to rewrite the version.
        m.extra
            .insert(PROTOCOL_VERSION.to_string(), json!("1999-01-01"));

        let map = m.to_map().unwrap();
        assert_eq!(map[TRACEPARENT], json!("00-abc-def-01"));
        assert_eq!(map[PROTOCOL_VERSION], json!("2026-07-28"));
    }

    #[test]
    fn reserved_key_names_match_the_specification() {
        // These strings are wire format; a typo is silently non-conforming
        // because a server treats an unknown _meta key as opaque.
        assert_eq!(PROTOCOL_VERSION, "io.modelcontextprotocol/protocolVersion");
        assert_eq!(
            CLIENT_CAPABILITIES,
            "io.modelcontextprotocol/clientCapabilities"
        );
        assert_eq!(CLIENT_INFO, "io.modelcontextprotocol/clientInfo");
        assert_eq!(LOG_LEVEL, "io.modelcontextprotocol/logLevel");
        assert_eq!(SERVER_INFO, "io.modelcontextprotocol/serverInfo");
        assert_eq!(SUBSCRIPTION_ID, "io.modelcontextprotocol/subscriptionId");
        // Unprefixed by design.
        assert_eq!(PROGRESS_TOKEN, "progressToken");
        assert_eq!(TRACEPARENT, "traceparent");
        assert_eq!(TRACESTATE, "tracestate");
        assert_eq!(BAGGAGE, "baggage");
    }

    #[test]
    fn accessors_read_what_to_map_writes() {
        let params = json!({"_meta": meta().to_map().unwrap()});
        assert_eq!(protocol_version_of(&params), Some("2026-07-28"));
        assert!(meta_of(&params).is_some());

        // A server's result carrying its identity.
        let result = json!({
            "resultType": "complete",
            "_meta": {SERVER_INFO: {"name": "demo", "version": "1.0"}},
        });
        assert_eq!(server_info_of(&result).unwrap().name, "demo");

        let notification = json!({"_meta": {SUBSCRIPTION_ID: "sub-7"}});
        assert_eq!(subscription_id_of(&notification), Some("sub-7"));
    }

    #[test]
    fn accessors_tolerate_absent_and_malformed_meta() {
        for params in [json!({}), json!({"_meta": "nope"}), json!(null), json!([])] {
            assert!(meta_of(&params).is_none());
            assert!(protocol_version_of(&params).is_none());
            assert!(server_info_of(&params).is_none());
            assert!(subscription_id_of(&params).is_none());
        }
        // Present _meta, wrong types inside.
        let bad = json!({"_meta": {PROTOCOL_VERSION: 42, SERVER_INFO: "nope"}});
        assert!(protocol_version_of(&bad).is_none());
        assert!(server_info_of(&bad).is_none());
    }
}
