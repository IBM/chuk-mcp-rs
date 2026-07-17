//! Roots feature messages, mirroring `chuk_mcp.protocol.messages.roots`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::json_rpc::{create_response, JsonRpcMessage, RequestId};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

pub use crate::protocol::messages::notifications::send_roots_list_changed_notification;

/// A root directory or file the client grants the server access to.
/// URIs must start with `file://`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Root {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Root {
    /// Create a root, validating the `file://` prefix.
    pub fn new(uri: impl Into<String>, name: Option<String>) -> Result<Self, McpError> {
        let uri = uri.into();
        if !uri.starts_with("file://") {
            return Err(McpError::validation(format!(
                "Root URI must start with 'file://', got: {uri}"
            )));
        }
        Ok(Root {
            uri,
            name,
            extra: Map::new(),
        })
    }
}

/// Result of `roots/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListRootsResult {
    pub roots: Vec<Root>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Send a `roots/list` request (server → client feature; useful for tests and
/// server implementations driving a client).
pub async fn send_roots_list(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
) -> Result<ListRootsResult, McpError> {
    let response = send_message(read_stream, write_stream, MessageMethod::ROOTS_LIST, None).await?;
    Ok(serde_json::from_value(response)?)
}

/// Build the response message for an incoming `roots/list` request.
pub fn handle_roots_list_request(roots: &[Root], request_id: RequestId) -> JsonRpcMessage {
    let result = ListRootsResult {
        roots: roots.to_vec(),
        extra: Map::new(),
    };
    JsonRpcMessage::Response(create_response(
        request_id,
        Some(serde_json::to_value(result).expect("serialize roots")),
    ))
}

/// Convert a filesystem path to a `file://` root.
pub fn create_file_root(path: &std::path::Path, name: Option<String>) -> Result<Root, McpError> {
    let abs = std::path::absolute(path)?;
    let name = name.or_else(|| {
        abs.file_name()
            .map(|n| n.to_string_lossy().to_string())
    });
    let uri = format!("file://{}", abs.to_string_lossy());
    Root::new(uri, name)
}

/// Extract the filesystem path from a `file://` root.
pub fn parse_file_root(root: &Root) -> Result<std::path::PathBuf, McpError> {
    let path = root.uri.strip_prefix("file://").ok_or_else(|| {
        McpError::validation(format!("Not a file URI: {}", root.uri))
    })?;
    Ok(std::path::PathBuf::from(path))
}

/// Client-side manager for the roots list.
#[derive(Default)]
pub struct RootsManager {
    roots: std::collections::BTreeMap<String, Root>,
    write_stream: Option<WriteStream>,
}

impl RootsManager {
    pub fn new(write_stream: Option<WriteStream>) -> Self {
        RootsManager {
            roots: Default::default(),
            write_stream,
        }
    }

    pub async fn add_root(&mut self, root: Root) {
        self.roots.insert(root.uri.clone(), root);
        self.notify_changed().await;
    }

    pub async fn remove_root(&mut self, uri: &str) {
        if self.roots.remove(uri).is_some() {
            self.notify_changed().await;
        }
    }

    pub fn get_roots(&self) -> Vec<Root> {
        self.roots.values().cloned().collect()
    }

    pub async fn clear(&mut self) {
        if !self.roots.is_empty() {
            self.roots.clear();
            self.notify_changed().await;
        }
    }

    /// Build the response for an incoming `roots/list` request.
    pub fn handle_list_request(&self, request_id: RequestId) -> JsonRpcMessage {
        handle_roots_list_request(&self.get_roots(), request_id)
    }

    async fn notify_changed(&self) {
        if let Some(stream) = &self.write_stream {
            if let Err(e) = send_roots_list_changed_notification(stream).await {
                tracing::error!("Failed to send roots list changed notification: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_requires_file_uri() {
        assert!(Root::new("file:///tmp/x", None).is_ok());
        assert!(Root::new("http://example.com", None).is_err());
    }

    #[test]
    fn file_root_roundtrip() {
        let root = create_file_root(std::path::Path::new("/tmp/project"), None).unwrap();
        assert_eq!(root.uri, "file:///tmp/project");
        assert_eq!(root.name.as_deref(), Some("project"));
        assert_eq!(
            parse_file_root(&root).unwrap(),
            std::path::PathBuf::from("/tmp/project")
        );
    }
}
