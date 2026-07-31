//! Serving [`McpServer`] over Streamable HTTP.
//!
//! The mirror image of [`crate::transports::http`]: that dials a server, this
//! answers. Both eras arrive on the same endpoint and are told apart per
//! request, exactly as [`crate::server::modern`] describes — a legacy client
//! `initialize`s and is given a session, a `2026-07-28` client declares its
//! version in `_meta` and is given nothing to remember.
//!
//! ```no_run
//! use chuk_mcp::server::{http::serve_http, McpServer};
//!
//! # async fn run() -> Result<(), chuk_mcp::McpError> {
//! let server = McpServer::new("my-server", "1.0.0", None);
//! serve_http(server, "127.0.0.1:3000".parse().unwrap()).await
//! # }
//! ```

mod handler;
mod response;

pub use handler::MCP_PATH;

use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::protocol::types::errors::McpError;
use crate::server::McpServer;

/// Serve `server` over Streamable HTTP on `address`, until the process ends.
///
/// Every connection is handled concurrently; the server itself is shared
/// rather than cloned, because a modern exchange keeps nothing per connection
/// and a legacy one keeps its state in the session store.
pub async fn serve_http(server: McpServer, address: SocketAddr) -> Result<(), McpError> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|error| McpError::Transport(format!("cannot bind {address}: {error}")))?;

    serve_on(server, listener).await
}

/// [`serve_http`] on a listener the caller already bound.
///
/// Useful when the port must be known before serving starts — binding to port
/// 0 and reading back the assignment, as tests do.
pub async fn serve_on(server: McpServer, listener: TcpListener) -> Result<(), McpError> {
    let server = Arc::new(server);
    tracing::info!("serving MCP over HTTP on {:?}", listener.local_addr().ok());

    loop {
        let (stream, _peer) = listener.accept().await.map_err(accept_failed)?;
        tokio::spawn(serve_connection(server.clone(), stream));
    }
}

/// The listener itself failing is fatal: unlike one connection dropping, there
/// is nothing left to serve anybody on.
fn accept_failed(error: std::io::Error) -> McpError {
    McpError::Transport(format!("accept failed: {error}"))
}

/// Serve one connection to completion.
///
/// A connection that fails is one client's problem: it is logged rather than
/// propagated, because taking the listener down with it would let any client
/// stop the server for everyone.
async fn serve_connection(server: Arc<McpServer>, stream: tokio::net::TcpStream) {
    let service = service_fn(move |request| handler::handle(server.clone(), request));
    if let Err(error) = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await
    {
        tracing::debug!("connection ended: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_accept_is_a_transport_error_naming_the_cause() {
        // Unreachable from a test without exhausting the process's file
        // descriptors, so the mapping is checked directly.
        let error = accept_failed(std::io::Error::other("too many open files"));
        assert!(matches!(error, McpError::Transport(_)));
        assert!(error.to_string().contains("too many open files"));
    }
}
