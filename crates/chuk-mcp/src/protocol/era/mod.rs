//! Protocol era — which generation of the MCP wire protocol a peer speaks.
//!
//! The `2026-07-28` revision made MCP stateless: it removed the `initialize`
//! handshake, `notifications/initialized`, `ping`, and the `Mcp-Session-Id`
//! header, and moved protocol version and client capabilities into per-request
//! `_meta`. Servers speaking any earlier revision still need the legacy
//! stateful lifecycle. A client therefore has to know, per peer, which of the
//! two it is talking to.
//!
//! Era is a property of the `(endpoint, credential context)` tuple — not of the
//! client, and not of the transport. One process can talk to a modern server
//! and a legacy one simultaneously, the same host can serve different eras to
//! different principals, and a server can be upgraded underneath a running
//! client. [`EraCache`] holds that decision with a TTL; see [`cache`].
//!
//! Detection is transport-specific and the two algorithms are **not**
//! interchangeable — see [`detect`].

mod cache;
mod detect;

pub use cache::{EndpointKey, EraCache, DEFAULT_ERA_TTL};
pub use detect::{
    classify_http_error_body, classify_probe_error, classify_probe_result, Detection,
};

use serde_json::{Map, Value};

use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::errors::{McpError, INVALID_REQUEST};
use crate::protocol::types::info::ServerInfo;
use crate::protocol::versioning;

/// Which generation of the MCP wire protocol a peer speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolEra {
    /// `2025-06-18` and earlier: `initialize` handshake, protocol sessions,
    /// server-initiated requests, `ping`.
    Legacy,
    /// `2026-07-28` and later: stateless requests, `server/discover`, MRTR.
    Modern,
}

impl ProtocolEra {
    pub fn is_modern(self) -> bool {
        matches!(self, ProtocolEra::Modern)
    }

    pub fn is_legacy(self) -> bool {
        matches!(self, ProtocolEra::Legacy)
    }

    /// The protocol version a client declares when operating in this era.
    pub fn default_protocol_version(self) -> &'static str {
        match self {
            ProtocolEra::Modern => versioning::FIRST_MODERN_VERSION,
            ProtocolEra::Legacy => versioning::LATEST_LEGACY_VERSION,
        }
    }

    /// Classify a protocol version string into its era.
    pub fn from_protocol_version(version: &str) -> Self {
        if versioning::is_modern_version(version) {
            ProtocolEra::Modern
        } else {
            ProtocolEra::Legacy
        }
    }
}

impl std::fmt::Display for ProtocolEra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolEra::Legacy => f.write_str("legacy"),
            ProtocolEra::Modern => f.write_str(versioning::FIRST_MODERN_VERSION),
        }
    }
}

/// The configured era policy for a connection.
///
/// Pinning exists for tests and for deployments behind gateways that rewrite or
/// swallow the `400` body that HTTP detection depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EraMode {
    /// Detect the peer's era. The default.
    #[default]
    Auto,
    /// Always use the legacy stateful lifecycle; never probe.
    Legacy,
    /// Always use the stateless protocol; never probe.
    Modern,
}

impl EraMode {
    /// Whether this mode needs a detection step at all.
    pub fn requires_detection(self) -> bool {
        matches!(self, EraMode::Auto)
    }

    /// Resolve the era to use. `detected` is consulted **only** in
    /// [`EraMode::Auto`]; a pinned mode ignores it entirely, which is the whole
    /// point of pinning.
    pub fn resolve(self, detected: Option<ProtocolEra>) -> Option<ProtocolEra> {
        match self {
            EraMode::Auto => detected,
            EraMode::Legacy => Some(ProtocolEra::Legacy),
            EraMode::Modern => Some(ProtocolEra::Modern),
        }
    }

    /// The canonical configuration string for this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            EraMode::Auto => "auto",
            EraMode::Legacy => "legacy",
            EraMode::Modern => versioning::FIRST_MODERN_VERSION,
        }
    }
}

impl std::str::FromStr for EraMode {
    type Err = McpError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "auto" {
            Ok(EraMode::Auto)
        } else if s == "legacy" {
            Ok(EraMode::Legacy)
        } else if s == versioning::V2026_07_28 {
            Ok(EraMode::Modern)
        } else {
            Err(McpError::validation(format!(
                "Unknown protocol era mode {s:?}; expected \"auto\", \"legacy\", or {:?}",
                versioning::V2026_07_28
            )))
        }
    }
}

impl std::fmt::Display for EraMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a peer told us about itself, however we learned it.
///
/// Built from a `server/discover` result under the modern era, or from an
/// `initialize` result under the legacy one. Callers consume the same struct
/// either way — era never leaks into a caller-facing shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerProfile {
    pub era: ProtocolEra,
    /// The version selected for this peer.
    pub protocol_version: String,
    /// Every version the peer advertised. A legacy `initialize` reports one.
    pub supported_versions: Vec<String>,
    pub capabilities: ServerCapabilities,
    pub server_info: Option<ServerInfo>,
    /// Extension declarations lifted from `capabilities.extensions`.
    pub extensions: Map<String, Value>,
}

impl ServerProfile {
    /// Build a profile from a `server/discover` result.
    ///
    /// Accepts either `protocolVersions` (an array, which is what a server
    /// advertising multiple revisions returns) or a single `protocolVersion`,
    /// and negotiates the best mutually supported version from it.
    pub fn from_discover(value: &Value) -> Result<Self, McpError> {
        let obj = value.as_object().ok_or_else(|| {
            McpError::protocol(INVALID_REQUEST, "server/discover result is not an object")
        })?;

        let advertised: Vec<String> = match obj.get("protocolVersions") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => obj
                .get("protocolVersion")
                .and_then(Value::as_str)
                .map(|s| vec![s.to_string()])
                .unwrap_or_default(),
        };

        if advertised.is_empty() {
            return Err(McpError::protocol(
                INVALID_REQUEST,
                "server/discover result advertised no protocol versions",
            ));
        }

        let advertised_refs: Vec<&str> = advertised.iter().map(String::as_str).collect();
        let protocol_version =
            versioning::negotiate_version(versioning::SUPPORTED_VERSIONS, &advertised_refs)?;

        let capabilities = Self::capabilities_of(obj)?;
        Ok(ServerProfile {
            era: ProtocolEra::from_protocol_version(&protocol_version),
            protocol_version,
            supported_versions: advertised,
            extensions: Self::extensions_of(&capabilities),
            server_info: Self::server_info_of(obj),
            capabilities,
        })
    }

    /// Build a profile from a legacy `initialize` result.
    ///
    /// The era is forced to [`ProtocolEra::Legacy`]: arriving here means the
    /// `initialize` handshake succeeded, which is definitionally the legacy
    /// lifecycle regardless of what version string the server echoed back.
    pub fn from_initialize(value: &Value) -> Result<Self, McpError> {
        let obj = value.as_object().ok_or_else(|| {
            McpError::protocol(INVALID_REQUEST, "initialize result is not an object")
        })?;

        let protocol_version = obj
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                McpError::protocol(INVALID_REQUEST, "initialize result has no protocolVersion")
            })?
            .to_string();

        let capabilities = Self::capabilities_of(obj)?;
        Ok(ServerProfile {
            era: ProtocolEra::Legacy,
            supported_versions: vec![protocol_version.clone()],
            protocol_version,
            extensions: Self::extensions_of(&capabilities),
            server_info: Self::server_info_of(obj),
            capabilities,
        })
    }

    fn capabilities_of(obj: &Map<String, Value>) -> Result<ServerCapabilities, McpError> {
        match obj.get("capabilities") {
            Some(v) => Ok(serde_json::from_value(v.clone())?),
            None => Ok(ServerCapabilities::default()),
        }
    }

    fn server_info_of(obj: &Map<String, Value>) -> Option<ServerInfo> {
        obj.get("serverInfo")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    fn extensions_of(capabilities: &ServerCapabilities) -> Map<String, Value> {
        capabilities
            .extra
            .get("extensions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::str::FromStr;

    #[test]
    fn era_from_version() {
        assert_eq!(
            ProtocolEra::from_protocol_version("2026-07-28"),
            ProtocolEra::Modern
        );
        assert_eq!(
            ProtocolEra::from_protocol_version("2025-06-18"),
            ProtocolEra::Legacy
        );
        // Unsupported and malformed both fall back to legacy, never modern.
        assert_eq!(
            ProtocolEra::from_protocol_version("2025-11-25"),
            ProtocolEra::Legacy
        );
        assert_eq!(
            ProtocolEra::from_protocol_version("garbage"),
            ProtocolEra::Legacy
        );
    }

    #[test]
    fn default_versions_match_era() {
        assert_eq!(
            ProtocolEra::Modern.default_protocol_version(),
            versioning::FIRST_MODERN_VERSION
        );
        assert_eq!(
            ProtocolEra::Legacy.default_protocol_version(),
            versioning::LATEST_LEGACY_VERSION
        );
        assert!(!versioning::is_modern_version(
            ProtocolEra::Legacy.default_protocol_version()
        ));
    }

    #[test]
    fn mode_parses_the_three_documented_values() {
        assert_eq!(EraMode::from_str("auto").unwrap(), EraMode::Auto);
        assert_eq!(EraMode::from_str("legacy").unwrap(), EraMode::Legacy);
        assert_eq!(EraMode::from_str("2026-07-28").unwrap(), EraMode::Modern);
        assert!(EraMode::from_str("modern").is_err());
        assert!(EraMode::from_str("").is_err());
        assert_eq!(EraMode::default(), EraMode::Auto);
    }

    #[test]
    fn mode_roundtrips_through_its_string() {
        for mode in [EraMode::Auto, EraMode::Legacy, EraMode::Modern] {
            assert_eq!(EraMode::from_str(mode.as_str()).unwrap(), mode);
        }
    }

    #[test]
    fn pinning_ignores_detection() {
        // The point of pinning: a wrong or absent detection cannot override it.
        for detected in [None, Some(ProtocolEra::Modern), Some(ProtocolEra::Legacy)] {
            assert_eq!(
                EraMode::Legacy.resolve(detected),
                Some(ProtocolEra::Legacy),
                "legacy pin overridden by {detected:?}"
            );
            assert_eq!(
                EraMode::Modern.resolve(detected),
                Some(ProtocolEra::Modern),
                "modern pin overridden by {detected:?}"
            );
        }
        assert!(!EraMode::Legacy.requires_detection());
        assert!(!EraMode::Modern.requires_detection());
    }

    #[test]
    fn auto_defers_to_detection() {
        assert!(EraMode::Auto.requires_detection());
        assert_eq!(EraMode::Auto.resolve(None), None);
        assert_eq!(
            EraMode::Auto.resolve(Some(ProtocolEra::Modern)),
            Some(ProtocolEra::Modern)
        );
        assert_eq!(
            EraMode::Auto.resolve(Some(ProtocolEra::Legacy)),
            Some(ProtocolEra::Legacy)
        );
    }

    #[test]
    fn profile_from_discover() {
        let profile = ServerProfile::from_discover(&json!({
            "protocolVersions": ["2026-07-28", "2025-06-18"],
            "capabilities": {
                "tools": {"listChanged": true},
                "extensions": {"io.modelcontextprotocol/tasks": {}}
            },
            "serverInfo": {"name": "demo", "version": "1.2.3"}
        }))
        .unwrap();

        assert_eq!(profile.era, ProtocolEra::Modern);
        assert_eq!(profile.protocol_version, "2026-07-28");
        assert_eq!(profile.supported_versions.len(), 2);
        assert_eq!(profile.server_info.unwrap().name, "demo");
        assert!(profile.capabilities.tools.is_some());
        assert!(profile
            .extensions
            .contains_key("io.modelcontextprotocol/tasks"));
    }

    #[test]
    fn discover_negotiates_down_to_a_shared_version() {
        // A server that only speaks legacy versions but implements discover.
        let profile = ServerProfile::from_discover(&json!({
            "protocolVersions": ["2025-03-26", "2024-11-05"]
        }))
        .unwrap();
        assert_eq!(profile.protocol_version, "2025-03-26");
        assert_eq!(profile.era, ProtocolEra::Legacy);
    }

    #[test]
    fn discover_rejects_unusable_results() {
        assert!(ServerProfile::from_discover(&json!("nope")).is_err());
        assert!(ServerProfile::from_discover(&json!({})).is_err());
        assert!(ServerProfile::from_discover(&json!({"protocolVersions": []})).is_err());
        // No mutually supported version.
        assert!(
            ServerProfile::from_discover(&json!({"protocolVersions": ["1999-01-01"]})).is_err()
        );
    }

    #[test]
    fn profile_from_initialize_is_always_legacy() {
        let profile = ServerProfile::from_initialize(&json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"resources": {"subscribe": true}},
            "serverInfo": {"name": "old", "version": "0.1"}
        }))
        .unwrap();

        assert_eq!(profile.era, ProtocolEra::Legacy);
        assert_eq!(profile.protocol_version, "2025-06-18");
        assert!(profile.capabilities.resources.is_some());

        // Even a server echoing a modern version through the legacy handshake
        // stays legacy — it completed an `initialize`, so it is legacy.
        let confused =
            ServerProfile::from_initialize(&json!({"protocolVersion": "2026-07-28"})).unwrap();
        assert_eq!(confused.era, ProtocolEra::Legacy);
    }

    #[test]
    fn initialize_rejects_missing_version() {
        assert!(ServerProfile::from_initialize(&json!({})).is_err());
        assert!(ServerProfile::from_initialize(&json!([])).is_err());
    }
}
