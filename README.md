# chuk-mcp-rs

A Rust port of [`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp): a
Model Context Protocol (MCP) client and server library, with Python bindings so
the existing `chuk_mcp` API can be powered by a native Rust core.

The workspace has two crates:

| Crate | What it is |
| --- | --- |
| [`crates/chuk-mcp`](crates/chuk-mcp) | The core Rust library — protocol types, JSON-RPC, transports, client, server. Published to crates.io as [`chuk-mcp`](https://crates.io/crates/chuk-mcp). |
| [`crates/chuk-mcp-python`](crates/chuk-mcp-python) | PyO3/maturin bindings exposing the core to Python as the `chuk_mcp_rs` module (published to PyPI as `chuk-mcp-rs`). |

The bindings are what the existing [`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp)
Python package re-exports, so `import chuk_mcp` keeps working while being powered
by Rust.

## Status

Full-parity port of the Python package's public surface:

- **Protocol layer** — JSON-RPC 2.0 messages (request/notification/response/error + batches), MCP types (content, capabilities, tools, info, elicitation), version negotiation across both protocol eras (`2026-07-28` / `2025-06-18` / `2025-03-26` / `2024-11-05`), and version-gated batching.
- **Message layer** — `initialize`, `tools/*`, `resources/*`, `prompts/*`, `ping`, `logging/setLevel`, `completion/complete`, `sampling/createMessage`, `roots/*`, and all notifications, with cancellation and progress support in `send_message`.
- **Transports** — stdio (subprocess), Streamable HTTP in both shapes — stateless
  `2026-07-28` and the legacy stateful revision, with a dual-era transport that
  picks between them — and the deprecated HTTP+SSE transport.
- **2026-07-28 support** — per-request `_meta`, the mirrored `MCP-Protocol-Version`
  / `Mcp-Method` / `Mcp-Name` headers, `x-mcp-header` parameter promotion,
  `server/discover`, era detection and caching, and new-request-ID retry on a
  broken response stream. See `ROADMAP.md` in the `chuk-mcp` repo for what is
  still outstanding (typed results, MRTR, catalogue caching).
- **Client** — high-level `McpClient` (initialize / list & call tools / read resources / get prompts / ping).
- **Server** — `McpServer` with tool/resource registration, a protocol handler, and in-memory sessions.

The Python bindings expose this whole surface: the high-level client and server,
the low-level `stdio_client`/`send_*` API, the Streamable HTTP transport, the
`ProtocolHandler` (`register_method`/`create_response`/`handle_message`), typed
result objects (`tool.name`, `result.text`, `caps.tools`), and the error
hierarchy — so downstream consumers keep the exact same import paths.

Wire compatibility is verified in both directions: the Rust client talks to the
Python `chuk_mcp` server, and the Python `chuk_mcp` client talks to the Rust
server.

## Rust usage

```rust
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

Building a server:

```rust
use chuk_mcp::server::McpServer;
use serde_json::{json, Value};

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
// server.run_stdio().await?;
```

## Python usage

```python
import asyncio
from chuk_mcp_rs import StdioParameters, connect_to_server

async def main():
    params = StdioParameters(command="python", args=["server.py"])
    async with await connect_to_server(params) as client:
        tools = await client.list_tools()      # -> [Tool(name=...), ...]
        result = await client.call_tool("greet", {"name": "World"})
        print(result.text)                     # typed result

asyncio.run(main())
```

## Building & testing

```bash
# Rust core + workspace tests
cargo test --workspace

# Lints
cargo clippy --workspace --all-targets

# Python extension (from crates/chuk-mcp-python)
maturin develop        # into the active virtualenv
maturin build --release
```

## License

Apache-2.0, matching the upstream `chuk-mcp` package.
