//! Server runner for the MCP conformance suite
//! (`@modelcontextprotocol/conformance server --url <this>`).
//!
//! Serves an [`McpServer`] over Streamable HTTP on a port the caller chooses,
//! registering the fixtures the reference scenarios require — each named by
//! the scenario that calls it. Prints the URL it bound to on stdout so a
//! harness can read it.

mod media;
mod modern;
mod prompts;
mod resources;
mod tools;

use chuk_mcp::protocol::types::capabilities::{
    CompletionCapability, LoggingCapability, PromptsCapability, ResourcesCapability,
    ServerCapabilities, ToolsCapability,
};
use chuk_mcp::server::http::serve_on;
use chuk_mcp::server::McpServer;

/// Bound when no port is given, so a caller that wants a fixed one can say so
/// and a caller that does not gets whatever is free.
const DEFAULT_BIND: &str = "127.0.0.1:0";

/// What this server tells a client it can do. Every one of these is backed by
/// a handler; declaring more than is implemented earns requests that can only
/// be refused.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        resources: Some(ResourcesCapability {
            subscribe: Some(true),
            list_changed: Some(true),
            ..Default::default()
        }),
        prompts: Some(PromptsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        logging: Some(LoggingCapability::default()),
        completion: Some(CompletionCapability::default()),
        ..Default::default()
    }
}

fn conformance_server() -> McpServer {
    let mut server = McpServer::new(
        "chuk-mcp-conformance",
        env!("CARGO_PKG_VERSION"),
        Some(capabilities()),
    )
    .with_instructions("A server used to exercise the MCP conformance suite.")
    // Every `requestState` this server mints is one of two known constants, so
    // anything else came back edited — which is exactly what the tampered-state
    // scenario sends, and what a server MUST refuse.
    .with_request_state_validator(|state| {
        matches!(state, modern::STATE_ROUND_1 | modern::STATE_ROUND_2)
    });

    tools::register(&mut server);
    modern::register(&mut server);
    resources::register(&mut server);
    prompts::register(&mut server);

    // Something to suggest, so the completion scenario has more than an empty
    // list to look at.
    server.register_completion(|_reference, _name, value| async move {
        ["paris", "park", "party"]
            .into_iter()
            .filter(|candidate| candidate.starts_with(&value))
            .map(String::from)
            .collect()
    });

    server
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("cannot bind {bind}: {error}");
            return std::process::ExitCode::from(2);
        }
    };

    match listener.local_addr() {
        Ok(address) => println!("http://{address}/mcp"),
        Err(error) => eprintln!("bound, but could not read the address: {error}"),
    }

    if let Err(error) = serve_on(conformance_server(), listener).await {
        eprintln!("server stopped: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}
