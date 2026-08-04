# chuk-mcp

[![crates.io](https://img.shields.io/crates/v/chuk-mcp.svg)](https://crates.io/crates/chuk-mcp)
[![docs.rs](https://img.shields.io/docsrs/chuk-mcp)](https://docs.rs/chuk-mcp)
[![License](https://img.shields.io/crates/l/chuk-mcp.svg)](https://github.com/IBM/chuk-mcp-rs)

A native Rust implementation of the [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) — client and server, speaking **both** protocol generations: the
stateless `2026-07-28` revision and the legacy `initialize`-handshake era,
working out which one a server wants so you do not have to.

It is the core of [`chuk-mcp-rs`](https://github.com/IBM/chuk-mcp-rs) and
also powers the [`chuk-mcp`](https://pypi.org/project/chuk-mcp/) Python package
through PyO3 bindings.

## Features

- **Protocol** — JSON-RPC 2.0 (requests, notifications, responses, errors,
  batches), the MCP type system (content, capabilities, tools, info,
  elicitation), version negotiation across `2026-07-28`, `2025-11-25`,
  `2025-06-18`, `2025-03-26` and `2024-11-05`, and version-gated batching.
- **`2026-07-28`** — per-request `_meta`, the mirrored `Mcp-Method` /
  `Mcp-Name` headers and `x-mcp-header` parameter promotion, `server/discover`,
  `subscriptions/listen`, `ttlMs` / `cacheScope` caching hints, multi
  round-trip requests, and era detection. Serving it means enforcing it:
  headers checked against the body, `_meta` required, removed methods answered
  `404`.
- **Transports** — stdio (subprocess), Streamable HTTP in both shapes, the
  dual-era transports that detect which, and the deprecated SSE transport.
- **Client** — high-level [`McpClient`]: list/call tools, read resources, get
  prompts, on any transport in either era.
- **Server** — [`McpServer`] with tool/resource/prompt registration, resource
  templates, subscriptions, completions, a protocol handler for custom
  methods, served over stdio or Streamable HTTP — answering both eras from one
  registration.
- **Authorization** — OAuth 2.1: metadata discovery, Client ID Metadata
  Documents / pre-registration / Dynamic Client Registration, PKCE `S256`,
  RFC 9207 `iss` validation, scope step-up and refresh. Default-on `auth`
  feature.

Verified against the official `@modelcontextprotocol/conformance` suite —
client *and* server, at `2025-11-25` and `2026-07-28`, all blocking in CI.

Async on [Tokio](https://tokio.rs); JSON via [serde](https://serde.rs).

## Install

```toml
[dependencies]
chuk-mcp = "0.2"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

## Client

`connect` takes a URL or a command line, works out the transport and the
protocol era, completes whichever handshake that era needs, and hands back a
client that works the same either way.

```rust,no_run
use chuk_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let mut client = connect("./my-mcp-server").await?;

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

## Authorizing

A server that answers `401` is asking for a token. Supply an `Auth` and the
client handles discovery, registration, PKCE, the exchange and refresh — you
supply only the step it cannot do, which is putting the authorization page in
front of a person.

```rust,no_run
# use std::sync::Arc;
# use chuk_mcp::auth::Auth;
# use chuk_mcp::connect::Connect;
# async fn run(handler: Arc<dyn chuk_mcp::auth::AuthorizationHandler>) -> Result<(), chuk_mcp::McpError> {
let client = Connect::to("https://example.com/mcp")
    .authorization(Auth::new().handler(handler))
    .connect()
    .await?;
# Ok(())
# }
```

## Documentation

Full guides — the two protocol eras, transports, the Python bindings, the
conformance suite and the benchmarks — live in the
[repository](https://github.com/IBM/chuk-mcp-rs), along with the
[changelog](https://github.com/IBM/chuk-mcp-rs/blob/main/CHANGELOG.md).

## License

Apache-2.0.
