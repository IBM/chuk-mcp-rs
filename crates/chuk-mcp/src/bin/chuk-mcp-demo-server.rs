//! A small MCP server over stdio, used by integration tests and as a demo.

use serde_json::{json, Value};

use chuk_mcp::protocol::types::capabilities::{
    ResourcesCapability, ServerCapabilities, ToolsCapability,
};
use chuk_mcp::server::McpServer;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let capabilities = ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        resources: Some(ResourcesCapability::default()),
        ..Default::default()
    };
    let mut server = McpServer::new(
        "chuk-mcp-demo",
        env!("CARGO_PKG_VERSION"),
        Some(capabilities),
    );

    server.register_tool(
        "greet",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string", "description": "Who to greet"}},
            "required": ["name"],
        }),
        "Greet someone by name",
        |args| async move {
            let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
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
        |args| async move {
            let a = args.get("a").and_then(Value::as_f64).ok_or("missing a")?;
            let b = args.get("b").and_then(Value::as_f64).ok_or("missing b")?;
            Ok(json!({"sum": a + b}))
        },
    );

    server.register_resource(
        "demo://motd",
        "motd",
        "Message of the day",
        "text/plain",
        || async { Ok("chuk-mcp demo server says hi".to_string()) },
    );

    if let Err(e) = server.run_stdio().await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}
