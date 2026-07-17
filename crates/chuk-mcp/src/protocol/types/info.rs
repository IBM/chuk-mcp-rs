//! Client and server identification types, mirroring `chuk_mcp.protocol.types.info`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Information about the server implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// The programmatic name of the server.
    pub name: String,
    /// Version of the server implementation.
    pub version: String,
    /// Human-readable title for UI display; fall back to `name` if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ServerInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        ServerInfo {
            name: name.into(),
            version: version.into(),
            title: None,
            extra: Map::new(),
        }
    }
}

/// Information about the client implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// The programmatic name of the client.
    pub name: String,
    /// Version of the client implementation.
    pub version: String,
    /// Human-readable title for UI display; fall back to `name` if absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for ClientInfo {
    /// Matches the Python defaults (`chuk-mcp-client` / `0.3`).
    fn default() -> Self {
        ClientInfo {
            name: "chuk-mcp-client".to_string(),
            version: "0.3".to_string(),
            title: None,
            extra: Map::new(),
        }
    }
}
