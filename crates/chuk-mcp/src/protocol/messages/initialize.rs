//! The MCP initialization handshake, mirroring
//! `chuk_mcp.protocol.messages.initialize`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::json_rpc::create_notification;
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{
    send_message_with_options, ReadStream, SendMessageOptions, WriteStream,
};
use crate::protocol::types::capabilities::{ClientCapabilities, ServerCapabilities};
use crate::protocol::types::errors::{McpError, INVALID_PARAMS};
use crate::protocol::types::info::{ClientInfo, ServerInfo};
use crate::protocol::versioning::SUPPORTED_VERSIONS;

/// Parameters for the `initialize` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Result of the `initialize` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Options controlling version negotiation in [`send_initialize_with_options`].
#[derive(Debug, Clone, Default)]
pub struct InitializeOptions {
    /// Timeout for the handshake (default 60s).
    pub timeout: Option<std::time::Duration>,
    /// Supported protocol versions, preferred first. Defaults to
    /// [`SUPPORTED_VERSIONS`].
    pub supported_versions: Option<Vec<String>>,
    /// Preferred protocol version to propose.
    pub preferred_version: Option<String>,
    /// Client info to advertise. Defaults to [`ClientInfo::default`].
    pub client_info: Option<ClientInfo>,
    /// Client capabilities to advertise. Defaults to
    /// [`ClientCapabilities::default`].
    pub capabilities: Option<ClientCapabilities>,
}

/// Perform the full MCP initialization handshake with default options.
///
/// Proposes the latest supported version, accepts a supported counter-proposal
/// from the server, sends `notifications/initialized` on success, and returns
/// the server's [`InitializeResult`].
pub async fn send_initialize(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
) -> Result<InitializeResult, McpError> {
    send_initialize_with_options(read_stream, write_stream, InitializeOptions::default()).await
}

/// [`send_initialize`] with explicit version negotiation options.
pub async fn send_initialize_with_options(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    options: InitializeOptions,
) -> Result<InitializeResult, McpError> {
    let supported_versions: Vec<String> = options
        .supported_versions
        .unwrap_or_else(|| SUPPORTED_VERSIONS.iter().map(|v| v.to_string()).collect());

    let proposed_version = options
        .preferred_version
        .filter(|v| supported_versions.contains(v))
        .unwrap_or_else(|| supported_versions[0].clone());

    let params = InitializeParams {
        protocol_version: proposed_version.clone(),
        capabilities: options.capabilities.unwrap_or_default(),
        client_info: options.client_info.unwrap_or_default(),
        extra: Map::new(),
    };

    let response = send_message_with_options(
        read_stream,
        write_stream,
        MessageMethod::INITIALIZE,
        Some(serde_json::to_value(&params)?),
        SendMessageOptions {
            timeout: options.timeout,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| classify_init_error(e, &proposed_version))?;

    let result: InitializeResult = serde_json::from_value(response)?;

    // Version negotiation: exact match, or accept a supported counter-proposal.
    if result.protocol_version != proposed_version
        && !supported_versions.contains(&result.protocol_version)
    {
        return Err(McpError::VersionMismatch {
            requested: proposed_version,
            supported: vec![result.protocol_version],
        });
    }

    // Complete the handshake.
    send_initialized_notification(write_stream).await?;

    Ok(result)
}

/// Map an INVALID_PARAMS error mentioning "protocol version" to a
/// VersionMismatch, matching the Python behavior.
fn classify_init_error(error: McpError, proposed_version: &str) -> McpError {
    if error.code() == Some(INVALID_PARAMS)
        && error.to_string().to_lowercase().contains("protocol version")
    {
        McpError::VersionMismatch {
            requested: proposed_version.to_string(),
            supported: vec!["unknown".to_string()],
        }
    } else {
        error
    }
}

/// Send the `notifications/initialized` notification completing the handshake.
pub async fn send_initialized_notification(write_stream: &WriteStream) -> Result<(), McpError> {
    write_stream
        .send(JsonRpcMessage::Notification(create_notification(
            MessageMethod::NOTIFICATION_INITIALIZED,
            Some(json!({})),
        )))
        .await
        .map_err(|_| McpError::Transport("write stream closed".into()))
}

/// All protocol versions this library supports (preferred first).
pub fn get_supported_versions() -> Vec<String> {
    SUPPORTED_VERSIONS.iter().map(|v| v.to_string()).collect()
}

/// The latest supported protocol version.
pub fn get_current_version() -> &'static str {
    crate::protocol::versioning::CURRENT_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::create_response;
    use crate::protocol::messages::send_message::message_channel;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn handshake_negotiates_and_sends_initialized() {
        let (inject, read) = message_channel(16);
        let (write, mut sent) = mpsc::channel(16);
        let read2 = read.clone();

        let client = tokio::spawn(async move { send_initialize(&read2, &write).await });

        let request = sent.recv().await.unwrap();
        assert_eq!(request.method(), Some("initialize"));
        let params = request.params().unwrap();
        assert_eq!(params["protocolVersion"], SUPPORTED_VERSIONS[0]);
        assert_eq!(params["clientInfo"]["name"], "chuk-mcp-client");

        inject
            .send(JsonRpcMessage::Response(create_response(
                request.id().unwrap().clone(),
                Some(json!({
                    "protocolVersion": SUPPORTED_VERSIONS[0],
                    "serverInfo": {"name": "test-server", "version": "1.0"},
                    "capabilities": {"tools": {}},
                })),
            )))
            .await
            .unwrap();

        let result = client.await.unwrap().unwrap();
        assert_eq!(result.server_info.name, "test-server");

        let initialized = sent.recv().await.unwrap();
        assert_eq!(
            initialized.method(),
            Some(MessageMethod::NOTIFICATION_INITIALIZED)
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_counter_proposal() {
        let (inject, read) = message_channel(16);
        let (write, mut sent) = mpsc::channel(16);
        let read2 = read.clone();

        let client = tokio::spawn(async move { send_initialize(&read2, &write).await });

        let request = sent.recv().await.unwrap();
        inject
            .send(JsonRpcMessage::Response(create_response(
                request.id().unwrap().clone(),
                Some(json!({
                    "protocolVersion": "1999-01-01",
                    "serverInfo": {"name": "old", "version": "0"},
                    "capabilities": {},
                })),
            )))
            .await
            .unwrap();

        let err = client.await.unwrap().unwrap_err();
        assert!(matches!(err, McpError::VersionMismatch { .. }));
    }
}
