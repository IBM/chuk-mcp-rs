//! High-level MCP client, mirroring `chuk_mcp.client`.

use serde_json::Value;

use crate::protocol::era::{ProtocolEra, ServerProfile};
use crate::protocol::messages::initialize::{send_initialize, InitializeResult};
use crate::protocol::messages::ping::send_ping;
use crate::protocol::messages::prompts::{
    send_prompts_get, send_prompts_list, GetPromptResult, Prompt,
};
use crate::protocol::messages::resources::{
    send_resources_list, send_resources_read, ReadResourceResult, Resource,
};
use crate::protocol::messages::send_message::{ReadStream, WriteStream};
use crate::protocol::messages::tools::{send_tools_call, send_tools_list, Tool, ToolResult};
use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::errors::McpError;
use crate::protocol::types::info::ServerInfo;
use crate::transports::stdio::{StdioParameters, StdioTransport};
use crate::transports::Transport;

/// High-level MCP client over any [`Transport`].
///
/// What the connection settled on — the server's identity and capabilities, the
/// era, the version — is read through accessors rather than fields. All four
/// are decided by the handshake and meaningless to assign afterwards, so none
/// of them is exposed as something you can set.
pub struct McpClient {
    transport: Box<dyn Transport>,
    streams: Option<(ReadStream, WriteStream)>,
    server_info: Option<ServerInfo>,
    capabilities: Option<ServerCapabilities>,
    era: Option<ProtocolEra>,
    protocol_version: Option<String>,
}

impl McpClient {
    /// Wrap a started transport. Call [`McpClient::initialize`] (or use
    /// [`connect`](crate::connect)) before issuing requests.
    pub fn new(transport: impl Transport + 'static) -> Self {
        McpClient {
            transport: Box::new(transport),
            streams: None,
            server_info: None,
            capabilities: None,
            era: None,
            protocol_version: None,
        }
    }

    /// Build a client from an already-settled connection.
    ///
    /// The era has already been detected and the handshake (legacy `initialize`
    /// or modern `server/discover`) already completed by the dual-era transport,
    /// so the streams and server profile are supplied directly and no further
    /// handshake is issued. Modern `_meta` injection, if any, lives in the
    /// transport, so the ordinary `send_*`-backed operations work unchanged.
    #[deprecated(
        since = "0.1.0",
        note = "use `from_profile`, which also carries the era and protocol version through; \
                a client built here reports `era() == None` even though the era is known"
    )]
    pub fn from_settled(
        transport: impl Transport + 'static,
        read: ReadStream,
        write: WriteStream,
        server_info: Option<ServerInfo>,
        capabilities: Option<ServerCapabilities>,
    ) -> Self {
        McpClient {
            transport: Box::new(transport),
            streams: Some((read, write)),
            server_info,
            capabilities,
            era: None,
            protocol_version: None,
        }
    }

    /// Build a client from an already-settled connection and the profile the
    /// peer reported.
    ///
    /// Prefer this to [`McpClient::from_settled`]: it carries the era and
    /// version through, so [`McpClient::era`] and
    /// [`McpClient::protocol_version`] can answer afterwards.
    pub fn from_profile(
        transport: impl Transport + 'static,
        read: ReadStream,
        write: WriteStream,
        profile: ServerProfile,
    ) -> Self {
        McpClient {
            transport: Box::new(transport),
            streams: Some((read, write)),
            server_info: profile.server_info,
            capabilities: Some(profile.capabilities),
            era: Some(profile.era),
            protocol_version: Some(profile.protocol_version),
        }
    }

    /// Whether the initialization handshake has completed.
    pub fn initialized(&self) -> bool {
        self.streams.is_some()
    }

    /// Which protocol generation this connection settled on, once it has.
    ///
    /// `None` only before a connection is established — every path that
    /// completes a handshake records it.
    pub fn era(&self) -> Option<ProtocolEra> {
        self.era
    }

    /// The protocol version in use, once negotiated.
    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    /// Who the server says it is, once the handshake has run.
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    /// What the server said it can do, once the handshake has run.
    pub fn capabilities(&self) -> Option<&ServerCapabilities> {
        self.capabilities.as_ref()
    }

    /// Perform the MCP initialization handshake (idempotent).
    pub async fn initialize(&mut self) -> Result<InitializeResult, McpError> {
        let (read, write) = self.transport.get_streams().await?;
        let result = send_initialize(&read, &write).await?;

        self.server_info = Some(result.server_info.clone());
        self.capabilities = Some(result.capabilities.clone());
        // `initialize` exists only in the legacy era, so reaching here settles
        // the question without a separate probe.
        self.era = Some(ProtocolEra::Legacy);
        self.protocol_version = Some(result.protocol_version.clone());
        self.transport
            .set_protocol_version(&result.protocol_version);
        self.streams = Some((read, write));

        tracing::info!("Initialized connection to {}", result.server_info.name);
        Ok(result)
    }

    fn streams(&self) -> Result<&(ReadStream, WriteStream), McpError> {
        self.streams.as_ref().ok_or_else(|| {
            McpError::Transport("Client not initialized - call initialize() first".into())
        })
    }

    /// List available tools.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpError> {
        let (read, write) = self.streams()?;
        Ok(send_tools_list(read, write, None).await?.tools)
    }

    /// Call a tool with JSON object arguments.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<ToolResult, McpError> {
        let (read, write) = self.streams()?;
        send_tools_call(read, write, name, arguments).await
    }

    /// List available resources.
    pub async fn list_resources(&self) -> Result<Vec<Resource>, McpError> {
        let (read, write) = self.streams()?;
        Ok(send_resources_list(read, write, None).await?.resources)
    }

    /// Read a resource by URI.
    pub async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, McpError> {
        let (read, write) = self.streams()?;
        send_resources_read(read, write, uri).await
    }

    /// List available prompts.
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>, McpError> {
        let (read, write) = self.streams()?;
        Ok(send_prompts_list(read, write, None).await?.prompts)
    }

    /// Get a prompt by name with optional arguments.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<GetPromptResult, McpError> {
        let (read, write) = self.streams()?;
        send_prompts_get(read, write, name, arguments).await
    }

    /// Ping the server. Returns `false` on failure rather than erroring.
    pub async fn ping(&self) -> bool {
        match self.streams() {
            Ok((read, write)) => send_ping(read, write).await,
            Err(_) => false,
        }
    }

    /// The raw stream pair, for lower-level `send_*` calls.
    pub fn raw_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        let (read, write) = self.streams()?;
        Ok((read.clone(), write.clone()))
    }

    /// Shut down the underlying transport.
    pub async fn close(&mut self) -> Result<(), McpError> {
        self.transport.close().await
    }
}

/// Connect to an MCP server over stdio with automatic initialization,
/// mirroring the Python `connect_to_server` context manager.
///
/// Legacy era only: it goes straight to `initialize` without asking whether the
/// peer is a `2026-07-28` server, which will fail against one.
#[deprecated(
    since = "0.1.0",
    note = "use `connect(\"command args\")` or `Connect::to_command(..)`, which detect the \
            protocol era instead of assuming the legacy handshake"
)]
pub async fn connect_to_server(parameters: StdioParameters) -> Result<McpClient, McpError> {
    let transport = StdioTransport::start(parameters).await?;
    connect_with_transport(transport).await
}

/// Connect over any started transport with automatic initialization.
pub async fn connect_with_transport(
    transport: impl Transport + 'static,
) -> Result<McpClient, McpError> {
    let mut client = McpClient::new(transport);
    client.initialize().await?;
    Ok(client)
}
