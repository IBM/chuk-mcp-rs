//! Pluggable transport implementations, mirroring `chuk_mcp.transports`.

pub mod http;
pub mod limits;
pub mod sse;
pub mod stdio;

use async_trait::async_trait;

use crate::protocol::messages::send_message::{ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// Base transport interface for MCP communication.
///
/// A transport is started (spawning its I/O tasks), hands out a stream pair
/// for JSON-RPC communication, and is closed when dropped or via
/// [`Transport::close`].
#[async_trait]
pub trait Transport: Send + Sync {
    /// Get read/write streams for message communication.
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError>;

    /// Set the negotiated protocol version (used e.g. for batching rules).
    fn set_protocol_version(&self, _version: &str) {}

    /// Shut the transport down, terminating any subprocess/connections.
    async fn close(&mut self) -> Result<(), McpError> {
        Ok(())
    }
}
