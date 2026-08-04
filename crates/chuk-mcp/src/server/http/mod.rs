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
pub mod origin;
mod response;
pub mod sse;
mod validate;

pub use handler::MCP_PATH;
pub use origin::AllowedHosts;

use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::protocol::types::errors::McpError;
use crate::server::McpServer;

/// How the HTTP server treats a request beyond the protocol itself.
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// Which `Host` and `Origin` values are answered. Loopback only by
    /// default; see [`AllowedHosts`].
    pub allowed_hosts: AllowedHosts,
}

impl HttpOptions {
    /// The defaults: loopback only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also answer requests naming this host.
    pub fn allow_host(mut self, host: impl Into<String>) -> Self {
        match &mut self.allowed_hosts {
            AllowedHosts::Also(names) => names.push(host.into()),
            AllowedHosts::Loopback => {
                self.allowed_hosts = AllowedHosts::Also(vec![host.into()]);
            }
            AllowedHosts::Any => {}
        }
        self
    }

    /// Answer regardless of `Host` and `Origin`. See [`AllowedHosts::Any`] for
    /// when that is safe.
    pub fn allow_any_host(mut self) -> Self {
        self.allowed_hosts = AllowedHosts::Any;
        self
    }
}

/// Serve `server` over Streamable HTTP on `address`, until the process ends.
///
/// Every connection is handled concurrently; the server itself is shared
/// rather than cloned, because a modern exchange keeps nothing per connection
/// and a legacy one keeps its state in the session store.
///
/// Answers loopback requests only. A server reachable from elsewhere wants
/// [`serve_http_with`] and an [`AllowedHosts`] that says so.
pub async fn serve_http(server: McpServer, address: SocketAddr) -> Result<(), McpError> {
    serve_http_with(server, address, HttpOptions::default()).await
}

/// [`serve_http`] with the host policy spelled out.
pub async fn serve_http_with(
    server: McpServer,
    address: SocketAddr,
    options: HttpOptions,
) -> Result<(), McpError> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|error| McpError::Transport(format!("cannot bind {address}: {error}")))?;

    serve_on_with(server, listener, options).await
}

/// [`serve_http`] on a listener the caller already bound.
///
/// Useful when the port must be known before serving starts — binding to port
/// 0 and reading back the assignment, as tests do.
pub async fn serve_on(server: McpServer, listener: TcpListener) -> Result<(), McpError> {
    serve_on_with(server, listener, HttpOptions::default()).await
}

/// [`serve_on`] with the host policy spelled out.
pub async fn serve_on_with(
    server: McpServer,
    listener: TcpListener,
    options: HttpOptions,
) -> Result<(), McpError> {
    let server = Arc::new(server);
    let options = Arc::new(options);
    tracing::info!("serving MCP over HTTP on {:?}", listener.local_addr().ok());

    loop {
        let (stream, _peer) = listener.accept().await.map_err(accept_failed)?;
        tokio::spawn(serve_connection(server.clone(), options.clone(), stream));
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
async fn serve_connection(
    server: Arc<McpServer>,
    options: Arc<HttpOptions>,
    stream: tokio::net::TcpStream,
) {
    let service =
        service_fn(move |request| handler::handle(server.clone(), options.clone(), request));
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

    #[test]
    fn the_default_options_answer_loopback_only() {
        assert_eq!(HttpOptions::new().allowed_hosts, AllowedHosts::Loopback);
    }

    #[test]
    fn allowing_a_host_keeps_loopback_alongside_it() {
        let options = HttpOptions::new().allow_host("mcp.example.com");
        assert_eq!(
            options.allowed_hosts,
            AllowedHosts::Also(vec!["mcp.example.com".into()])
        );

        // A second name joins the first rather than replacing it.
        let options = options.allow_host("other.example.com");
        assert_eq!(
            options.allowed_hosts,
            AllowedHosts::Also(vec!["mcp.example.com".into(), "other.example.com".into()])
        );
    }

    #[test]
    fn allowing_any_host_is_not_narrowed_by_naming_one() {
        // Having said "anything", naming a host cannot mean "only that one" —
        // that would quietly tighten a policy the caller widened on purpose.
        let options = HttpOptions::new()
            .allow_any_host()
            .allow_host("example.com");
        assert_eq!(options.allowed_hosts, AllowedHosts::Any);
    }

    #[test]
    fn allowing_any_host_overrides_a_narrower_policy() {
        let options = HttpOptions::new()
            .allow_host("example.com")
            .allow_any_host();
        assert_eq!(options.allowed_hosts, AllowedHosts::Any);
    }

    #[tokio::test]
    async fn binding_a_port_already_taken_reports_which_one() {
        let held = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
        let address = held.local_addr().expect("an address");

        let error = serve_http(McpServer::new("t", "1.0.0", None), address)
            .await
            .expect_err("the port is held");
        assert!(error.to_string().contains(&address.port().to_string()));
    }
}
