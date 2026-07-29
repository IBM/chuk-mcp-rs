# chuk-mcp

[![crates.io](https://img.shields.io/crates/v/chuk-mcp.svg)](https://crates.io/crates/chuk-mcp)
[![docs.rs](https://img.shields.io/docsrs/chuk-mcp)](https://docs.rs/chuk-mcp)
[![License](https://img.shields.io/crates/l/chuk-mcp.svg)](https://github.com/chrishayuk/chuk-mcp-rs)

A native Rust implementation of the [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) — client and server, with stdio, Streamable HTTP, and (legacy) SSE
transports.

It is the core of [`chuk-mcp-rs`](https://github.com/chrishayuk/chuk-mcp-rs) and
also powers the [`chuk-mcp`](https://pypi.org/project/chuk-mcp/) Python package
through PyO3 bindings.

## Features

- **Protocol** — JSON-RPC 2.0 (requests, notifications, responses, errors,
  batches), the MCP type system (content, capabilities, tools, info,
  elicitation), version negotiation across both protocol eras (`2026-07-28` /
  `2025-06-18` / `2025-03-26` / `2024-11-05`),
  and version-gated batching.
- **Messages** — `initialize`, `tools/*`, `resources/*`, `prompts/*`, `ping`,
  `logging/setLevel`, `completion/complete`, `sampling/createMessage`,
  `roots/*`, and notifications, with request cancellation and progress support.
- **Transports** — stdio (subprocess), Streamable HTTP (2025-03-26), and the
  legacy SSE transport.
- **Client** — high-level [`McpClient`]: initialize, list/call tools, read
  resources, get prompts, ping.
- **Server** — [`McpServer`] with tool/resource registration, a protocol
  handler for custom methods, and in-memory sessions.

Async on [Tokio](https://tokio.rs); JSON via [serde](https://serde.rs).

## Install

```toml
[dependencies]
chuk-mcp = "0.1"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

## Client

```rust,no_run
use chuk_mcp::client::connect_to_server;
use chuk_mcp::transports::stdio::StdioParameters;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), chuk_mcp::McpError> {
    let params = StdioParameters::new("python", ["server.py"]);
    let mut client = connect_to_server(params).await?;

    for tool in client.list_tools().await? {
        println!("{}", tool.name);
    }

    let result = client.call_tool("greet", json!({"name": "World"})).await?;
    println!("{}", result.text());

    client.close().await
}
```

## Server

```rust,no_run
use chuk_mcp::server::McpServer;
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<(), chuk_mcp::McpError> {
    let mut server = McpServer::new("my-server", "1.0.0", None);

    server.register_tool(
        "greet",
        json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        "Greet someone",
        |args| async move {
            let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );

    // Serve newline-delimited JSON-RPC on stdin/stdout until EOF.
    server.run_stdio().await
}
```

## Transports

`connect_to_server` uses stdio. For other transports, build the transport and
wrap it with `McpClient::new`:

```rust,no_run
use chuk_mcp::client::McpClient;
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};

# async fn run() -> Result<(), chuk_mcp::McpError> {
let params = StreamableHttpParameters::new("http://localhost:3000/mcp")?;
let transport = StreamableHttpTransport::start(params)?;
let mut client = McpClient::new(transport);
client.initialize().await?;
# Ok(())
# }
```

## License

Apache-2.0.
