//! One way to connect to anything.
//!
//! [`connect`] takes a URL or a command line, works out which transport that
//! implies, detects which protocol era the peer speaks, completes whichever
//! handshake that era needs, and hands back a ready [`McpClient`]. The typed
//! APIs underneath — [`crate::transports`], [`crate::client::connect_to_server`]
//! — remain available for when you need to say exactly what you mean.
//!
//! ```no_run
//! # async fn run() -> Result<(), chuk_mcp::McpError> {
//! let client = chuk_mcp::connect("https://example.com/mcp").await?;
//! let client = chuk_mcp::connect("python server.py").await?;
//! # Ok(()) }
//! ```
//!
//! For options, [`Connect`] is the same thing with the knobs exposed.

mod builder;
mod settle;
mod target;

pub use builder::Connect;
pub use target::Target;

use crate::client::McpClient;
use crate::protocol::types::errors::McpError;

/// Connect to an MCP server, whatever it is and whichever era it speaks.
///
/// - `http://…` or `https://…` — Streamable HTTP, era detected per endpoint.
/// - anything else — a command line to spawn, era probed on connect.
///
/// The returned client is ready: the handshake its era requires has already
/// happened.
pub async fn connect(target: impl AsRef<str>) -> Result<McpClient, McpError> {
    Connect::to(target).connect().await
}
