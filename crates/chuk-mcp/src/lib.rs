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
//! ```no_run
//! use chuk_mcp::client::connect_to_server;
//! use chuk_mcp::transports::stdio::StdioParameters;
//!
//! # async fn run() -> Result<(), chuk_mcp::protocol::types::errors::McpError> {
//! let params = StdioParameters::new("python", ["server.py"]);
//! let client = connect_to_server(params).await?;
//! let tools = client.list_tools().await?;
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod protocol;
pub mod server;
pub mod transports;

pub use client::{connect_to_server, McpClient};
pub use protocol::json_rpc::JsonRpcMessage;
pub use protocol::types::capabilities::{ClientCapabilities, ServerCapabilities};
pub use protocol::types::errors::McpError;
pub use protocol::types::info::{ClientInfo, ServerInfo};
pub use server::McpServer;
pub use transports::stdio::StdioParameters;
