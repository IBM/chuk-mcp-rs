//! # chuk-mcp
//!
//! A Model Context Protocol (MCP) client and server library — the Rust port of
//! the Python [`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp) package.
//!
//! Layers mirror the Python package:
//! - [`protocol`] — JSON-RPC messages, MCP types, versioning, feature message APIs
//! - [`transports`] — stdio, Streamable HTTP, and (legacy) SSE transports
//! - [`client`] — high-level [`client::McpClient`]
//! - [`server`] — high-level [`server::McpServer`] and protocol handler
//!
//! ## Quick start
//!
//! [`connect`] takes a URL or a command line, picks the transport, detects the
//! protocol era and completes its handshake:
//!
//! ```no_run
//! # async fn run() -> Result<(), chuk_mcp::McpError> {
//! let client = chuk_mcp::connect("https://example.com/mcp").await?;
//! let tools = client.list_tools().await?;
//! # Ok(())
//! # }
//! ```
//!
//! With options, or to say exactly which transport you want:
//!
//! ```no_run
//! use chuk_mcp::Connect;
//! # async fn run() -> Result<(), chuk_mcp::McpError> {
//! let client = Connect::to_command("python", ["server.py"])
//!     .timeout(std::time::Duration::from_secs(10))
//!     .connect()
//!     .await?;
//! # Ok(())
//! # }
//! ```

/// OAuth 2.1 authorization for HTTP transports.
///
/// Behind the default-on `auth` feature.
#[cfg(feature = "auth")]
pub mod auth;
pub mod client;
pub mod connect;
pub mod protocol;
pub mod server;
pub mod transports;

pub use client::McpClient;
// Re-exported for compatibility; `connect` supersedes it.
#[allow(deprecated)]
pub use client::connect_to_server;
pub use connect::{connect, Connect, Target};
pub use protocol::envelope::{build_envelope, ClientIdentity, Envelope};
pub use protocol::era::{
    renegotiate, Detection, EndpointKey, EraCache, EraMode, ProtocolEra, ServerProfile,
};
pub use protocol::header_params::HeaderParam;
pub use protocol::json_rpc::JsonRpcMessage;
pub use protocol::meta::RequestMeta;
pub use protocol::types::capabilities::{ClientCapabilities, ServerCapabilities};
pub use protocol::types::errors::McpError;
pub use protocol::types::info::{ClientInfo, ServerInfo};
pub use server::McpServer;
pub use transports::stdio::StdioParameters;

/// The names most programs need, in one import.
///
/// ```no_run
/// use chuk_mcp::prelude::*;
///
/// # async fn run() -> Result<(), McpError> {
/// let client = connect("https://example.com/mcp").await?;
/// let result = client.call_tool("greet", json!({"name": "World"})).await?;
/// println!("{}", result.text());
/// # Ok(()) }
/// ```
///
/// Everything here is also available from the crate root; the prelude exists so
/// a first program needs one `use` line rather than six.
pub mod prelude {
    pub use crate::client::McpClient;
    pub use crate::connect::{connect, Connect};
    pub use crate::protocol::era::{EraMode, ProtocolEra};
    pub use crate::protocol::messages::prompts::{GetPromptResult, Prompt};
    pub use crate::protocol::messages::resources::{ReadResourceResult, Resource};
    pub use crate::protocol::messages::tools::{Tool, ToolResult};
    pub use crate::protocol::types::capabilities::{ClientCapabilities, ServerCapabilities};
    pub use crate::protocol::types::errors::McpError;
    pub use crate::protocol::types::info::{ClientInfo, ServerInfo};
    pub use crate::server::McpServer;
    pub use crate::transports::stdio::StdioParameters;
    pub use serde_json::{json, Value};
}
