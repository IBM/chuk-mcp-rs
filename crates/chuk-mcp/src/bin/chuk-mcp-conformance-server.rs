//! Server runner for the MCP conformance suite
//! (`@modelcontextprotocol/conformance server --url <this>`).
//!
//! Serves an [`McpServer`] over Streamable HTTP on a port the caller chooses,
//! with enough surface for the suite's scenarios to have something to exercise.
//! Prints the URL it bound to on stdout so a harness can read it.

use serde_json::{json, Value};

use chuk_mcp::protocol::types::capabilities::{
    PromptsCapability, ResourcesCapability, ServerCapabilities, ToolsCapability,
};
use chuk_mcp::server::http::serve_on;
use chuk_mcp::server::McpServer;

/// Bound when no port is given, so a caller that wants a fixed one can say so
/// and a caller that does not gets whatever is free.
const DEFAULT_BIND: &str = "127.0.0.1:0";

fn conformance_server() -> McpServer {
    let capabilities = ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        resources: Some(ResourcesCapability::default()),
        prompts: Some(PromptsCapability::default()),
        ..Default::default()
    };
    let mut server = McpServer::new(
        "chuk-mcp-conformance",
        env!("CARGO_PKG_VERSION"),
        Some(capabilities),
    )
    .with_instructions("A server used to exercise the MCP conformance suite.");

    server.register_tool(
        "greet",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string", "description": "Who to greet"}},
            "required": ["name"],
        }),
        "Greet someone by name",
        |arguments| async move {
            let name = arguments
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );

    server.register_tool(
        "add",
        json!({
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
            "required": ["a", "b"],
        }),
        "Add two numbers",
        |arguments| async move {
            let a = arguments
                .get("a")
                .and_then(Value::as_f64)
                .ok_or("missing a")?;
            let b = arguments
                .get("b")
                .and_then(Value::as_f64)
                .ok_or("missing b")?;
            Ok(json!({"sum": a + b}))
        },
    );

    server.register_resource(
        "conformance://motd",
        "motd",
        "Message of the day",
        "text/plain",
        || async { Ok("chuk-mcp conformance server".to_string()) },
    );

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
