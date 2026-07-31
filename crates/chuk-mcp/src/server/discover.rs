//! Answering `server/discover`.
//!
//! The `2026-07-28` revision removed `initialize`, and with it the handshake
//! that used to tell a client what a server is and what it can do. A server
//! **MUST** implement `server/discover` in its place — and unlike `initialize`
//! it establishes nothing: it is an ordinary stateless request that may be
//! asked at any time, by anyone, as often as they like.
//!
//! The result is deliberately *not* shaped like the old `initialize` one. The
//! versions live under `supportedVersions` rather than a single negotiated
//! `protocolVersion`, and the server's identity sits in a reserved `_meta` key
//! rather than beside `capabilities` — a difference that has already caused one
//! bug in this codebase, when the client's reader was written from a guess.

use serde_json::{json, Map, Value};

use crate::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use crate::protocol::meta::SERVER_INFO;
use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::info::ServerInfo;
use crate::protocol::versioning;

/// Field names of a `DiscoverResult`, so the producer and the client's reader
/// cannot drift apart.
const FIELD_RESULT_TYPE: &str = "resultType";
const FIELD_SUPPORTED_VERSIONS: &str = "supportedVersions";
const FIELD_CAPABILITIES: &str = "capabilities";
const FIELD_INSTRUCTIONS: &str = "instructions";
const FIELD_META: &str = "_meta";

/// Build the result of a `server/discover` request.
///
/// `instructions` is optional natural-language guidance for a model on how to
/// use this server; omitted rather than sent empty when there is none to give.
pub fn discover_result(
    server_info: &ServerInfo,
    capabilities: &ServerCapabilities,
    instructions: Option<&str>,
) -> Value {
    let mut meta = Map::new();
    meta.insert(
        SERVER_INFO.to_string(),
        serde_json::to_value(server_info).expect("server info serializes"),
    );

    let mut result = Map::new();
    result.insert(FIELD_RESULT_TYPE.to_string(), json!(RESULT_TYPE_COMPLETE));
    // Every version this build can speak, newest first — the client picks.
    result.insert(
        FIELD_SUPPORTED_VERSIONS.to_string(),
        json!(versioning::SUPPORTED_VERSIONS),
    );
    result.insert(
        FIELD_CAPABILITIES.to_string(),
        serde_json::to_value(capabilities).expect("capabilities serialize"),
    );
    if let Some(instructions) = instructions {
        result.insert(FIELD_INSTRUCTIONS.to_string(), json!(instructions));
    }
    result.insert(FIELD_META.to_string(), Value::Object(meta));

    Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::era::ServerProfile;
    use crate::protocol::types::capabilities::ToolsCapability;

    fn info() -> ServerInfo {
        ServerInfo {
            name: "discover-test".to_string(),
            version: "1.2.3".to_string(),
            title: None,
            extra: Default::default(),
        }
    }

    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn the_result_is_shaped_as_the_specification_describes() {
        let result = discover_result(&info(), &capabilities(), None);

        assert_eq!(result[FIELD_RESULT_TYPE], json!(RESULT_TYPE_COMPLETE));
        assert_eq!(
            result[FIELD_SUPPORTED_VERSIONS],
            json!(versioning::SUPPORTED_VERSIONS)
        );
        assert_eq!(
            result[FIELD_CAPABILITIES]["tools"]["listChanged"],
            json!(true)
        );

        // Identity lives in the reserved `_meta` key, *not* beside
        // capabilities — the difference from `initialize`.
        assert_eq!(
            result[FIELD_META][SERVER_INFO]["name"],
            json!("discover-test")
        );
        assert!(
            result.get("serverInfo").is_none(),
            "identity must not be a sibling of capabilities, as it was in initialize"
        );
    }

    #[test]
    fn our_own_client_can_read_it() {
        // The strongest check available: the reader on the other side of this
        // codebase parses it without special-casing.
        let profile = ServerProfile::from_discover(&discover_result(
            &info(),
            &capabilities(),
            Some("Use the greet tool first."),
        ))
        .expect("the client reads our discover result");

        assert!(profile.era.is_modern());
        assert_eq!(profile.protocol_version, versioning::FIRST_MODERN_VERSION);
        assert_eq!(
            profile.server_info.as_ref().map(|info| info.name.as_str()),
            Some("discover-test")
        );
        assert_eq!(
            profile.instructions.as_deref(),
            Some("Use the greet tool first.")
        );
    }

    #[test]
    fn instructions_are_omitted_rather_than_sent_empty() {
        let result = discover_result(&info(), &capabilities(), None);
        assert!(result.get(FIELD_INSTRUCTIONS).is_none());
    }
}
