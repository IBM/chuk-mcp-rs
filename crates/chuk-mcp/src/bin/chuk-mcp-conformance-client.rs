//! Client runner for the MCP conformance suite
//! (`@modelcontextprotocol/conformance client --command "<this> ..."`).
//!
//! The harness starts an HTTP server and invokes this binary with the server
//! URL as the final argument. We connect over Streamable HTTP, run the core
//! client flow — initialise, then best-effort list + call a tool — and exit 0.
//! The harness observes our requests and judges conformance; a scenario that
//! only exercises the handshake simply ignores the tool traffic, so the tool
//! calls are best-effort and never fail the run on their own.

use serde_json::json;

use chuk_mcp::client::McpClient;
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};
use chuk_mcp::McpError;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // The URL is the last argument the harness appends.
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: chuk-mcp-conformance-client <server-url>");
        return std::process::ExitCode::from(2);
    };

    match run(&url).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("conformance client error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(url: &str) -> Result<(), McpError> {
    let transport = StreamableHttpTransport::start(StreamableHttpParameters::new(url)?)?;
    let mut client = McpClient::new(transport);

    // Required: the handshake must succeed.
    client.initialize().await?;

    // Best-effort: exercise a tool if the scenario offers one. Errors here are
    // deliberately swallowed so a handshake-only scenario still exits cleanly.
    if let Ok(tools) = client.list_tools().await {
        if let Some(tool) = tools.first() {
            let _ = client.call_tool(&tool.name, json!({})).await;
        }
    }

    client.close().await
}
