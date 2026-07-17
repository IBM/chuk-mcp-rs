//! High-level MCP client, mirroring `chuk_mcp.client`.

use serde_json::Value;

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
pub struct McpClient {
    transport: Box<dyn Transport>,
    streams: Option<(ReadStream, WriteStream)>,
    /// Server info from initialization, if initialized.
    pub server_info: Option<ServerInfo>,
    /// Server capabilities from initialization, if initialized.
    pub capabilities: Option<ServerCapabilities>,
}

impl McpClient {
    /// Wrap a started transport. Call [`McpClient::initialize`] (or use
    /// [`connect_to_server`]) before issuing requests.
    pub fn new(transport: impl Transport + 'static) -> Self {
        McpClient {
            transport: Box::new(transport),
            streams: None,
            server_info: None,
            capabilities: None,
        }
    }

    /// Whether the initialization handshake has completed.
    pub fn initialized(&self) -> bool {
        self.streams.is_some()
    }

    /// Perform the MCP initialization handshake (idempotent).
    pub async fn initialize(&mut self) -> Result<InitializeResult, McpError> {
        let (read, write) = self.transport.get_streams().await?;
        let result = send_initialize(&read, &write).await?;

        self.server_info = Some(result.server_info.clone());
        self.capabilities = Some(result.capabilities.clone());
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
