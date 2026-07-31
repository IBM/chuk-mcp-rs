# chuk-mcp-rs

A Model Context Protocol (MCP) client and server library in Rust, with Python
bindings — speaking **both** protocol generations: the stateless `2026-07-28`
revision and the legacy `initialize`-handshake era, with automatic detection
between them.

| Crate | What it is |
| --- | --- |
| [`crates/chuk-mcp`](crates/chuk-mcp) | The core Rust library — protocol types, JSON-RPC, transports, client, server. On crates.io as [`chuk-mcp`](https://crates.io/crates/chuk-mcp). |
| [`crates/chuk-mcp-python`](crates/chuk-mcp-python) | PyO3/maturin bindings exposing the core to Python as `chuk_mcp_rs` (on PyPI as `chuk-mcp-rs`). |

The Python package [`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp)
re-exports these bindings, so `import chuk_mcp` keeps working unchanged while
being powered by Rust — [about 5.8× more tool calls per
second](benchmarks/README.md) than the last pure-Python release, with no code
changes.

---

## Install

**Rust**

```bash
cargo add chuk-mcp tokio serde_json
```

**Python**

```bash
pip install chuk-mcp-rs      # the bindings directly
pip install chuk-mcp         # or the familiar package, Rust-powered
```

---

## Your first client

One call. Give it a URL or a command line; it picks the transport, works out
which protocol generation the server speaks, and completes that generation's
handshake:

```rust
use chuk_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    let mut client = connect("https://example.com/mcp").await?;
    //  …or: connect("python server.py").await?

    for tool in client.list_tools().await? {
        println!("{}", tool.name);
    }

    let result = client.call_tool("greet", json!({"name": "World"})).await?;
    println!("{}", result.text());

    client.close().await
}
```

The same thing in Python:

```python
import asyncio
from chuk_mcp_rs import connect

async def main():
    async with await connect("https://example.com/mcp") as client:
        # …or: await connect("python server.py")
        for tool in await client.list_tools():
            print(tool.name)
        result = await client.call_tool("greet", {"name": "World"})
        print(result.text)

asyncio.run(main())
```

`client.era()` and `client.protocol_version()` (`.era` / `.protocol_version` in
Python) report what it settled on.

When you need options, the builder is the same thing with the knobs exposed:

```rust
let client = Connect::to("https://example.com/mcp")
    .bearer_token(token)
    .header("X-Tenant", "acme")
    .era(EraMode::Legacy)          // pin instead of detecting
    .timeout(Duration::from_secs(10))
    .connect()
    .await?;
```

```python
client = await connect(
    "https://example.com/mcp",
    bearer_token=token,
    headers={"X-Tenant": "acme"},
    era="legacy",
    timeout=10.0,
)
```

The typed layer underneath — the transports, the `send_*` helpers — is still
there for when you want to say exactly what you mean.

Try it against the bundled demo server:

```bash
cargo build --bin chuk-mcp-demo-server
cargo run --example interop_client -- ./target/debug/chuk-mcp-demo-server
```

---

## Your first server

```rust
use chuk_mcp::server::McpServer;
use serde_json::{json, Value};

#[tokio::main]
async fn main() -> Result<(), chuk_mcp::McpError> {
    let mut server = McpServer::new("my-server", "1.0.0", None);

    server.register_tool(
        "greet",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"],
        }),
        "Greet someone",
        |args| async move {
            let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );

    server.run_stdio().await
}
```

In Python, with async handlers taking the tool's arguments as keywords:

```python
import asyncio
from chuk_mcp_rs import MCPServer

server = MCPServer("my-server", "1.0.0")

async def greet(name: str) -> str:
    return f"Hello, {name}!"

server.register_tool(
    "greet",
    greet,
    {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
    "Greet someone",
)

asyncio.run(server.run_stdio())
```

The server speaks the legacy lifecycle. A `2026-07-28` server is not built yet —
see [Status](#status).

---

## Two protocol eras

The `2026-07-28` revision made MCP **stateless**. It removed `initialize`,
`notifications/initialized`, `ping` and the `Mcp-Session-Id` header, and moved
the protocol version and client capabilities into per-request `_meta` mirrored
by HTTP headers. Servers on any earlier revision still need the legacy stateful
lifecycle.

So a client has to know, per peer, which one it is talking to — and the era is a
property of the `(endpoint, credential)` pair, not of your process. One program
can talk to both at once.

```
                      ┌── server/discover succeeds ──▶  modern (2026-07-28)
  connect ──▶ probe ──┤
                      └── rejected / unknown ────────▶  legacy (initialize)
```

`connect` already does all of this. What it does under the hood:

**Stdio** — probe with `server/discover` before anything else is sent.
**HTTP** — no free probe exists, so the first request *is* the probe; the
verdict is then cached per `(endpoint, credential)` with a TTL.

Reach for the transports directly only when you want to hold one yourself:

```rust
use chuk_mcp::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};

let connection = stdio_client_dual(params, StdioDualOptions::default()).await?;
println!("peer speaks {}", connection.era());
```

Everything after connecting is identical in both eras — `list_tools`,
`call_tool`, `read_resource` and the rest have one shape, and legacy results
normalise upward, so `result.result_type` (`result.resultType` in Python) reads
`"complete"` whichever peer produced it.

Pin the era with `.era(EraMode::Legacy)` or `EraMode::Modern` (`era="legacy"` /
`"2026-07-28"` in Python) when you already know, and no probe is sent.

**Deeper:** [docs/protocol-eras.md](docs/protocol-eras.md) — detection rules,
caching, `_meta` and header mirroring, parameter promotion.

---

## Choosing a transport

`connect` chooses for you — a URL means HTTP, anything else means a subprocess.
Name one yourself when you need to:

| Transport | Era | Use it for |
| --- | --- | --- |
| `transports::stdio` | Legacy | A server you spawn as a subprocess. |
| `transports::stdio_dual` | Both | Same, but detect the era first. |
| `transports::http` | Legacy | Streamable HTTP with sessions. |
| `transports::http_modern` | Modern | Stateless Streamable HTTP. |
| `transports::http_dual` | Both | Streamable HTTP, era detected per endpoint. |
| `transports::sse` | Legacy | The deprecated HTTP+SSE transport. |

**Deeper:** [docs/transports.md](docs/transports.md).

---

## Performance

Same MCP server binary, same workload, three clients — 1000 `greet` calls after
100 warm-up calls, on an Apple M2 Pro (macOS 26.5.2, rustc 1.97.1, CPython
3.11.11). Reproduce with `python3 -m benchmarks`:

| Client | Mean/call | Calls/sec | p50 | p95 | p99 | Handshake |
| --- | --- | --- | --- | --- | --- | --- |
| **rust-native** — the `chuk-mcp` crate | **55.2 µs** | **18,107** | 57.0 µs | 72.6 µs | 85.9 µs | 4.7 ms |
| **python-bindings** — `chuk_mcp_rs` via PyO3 | 86.3 µs | 11,585 | 78.6 µs | 128.3 µs | 158.7 µs | 12.7 ms |
| **pure-python** — `chuk-mcp==0.9.4` | 500.6 µs | 1,998 | 492.6 µs | 552.5 µs | 648.4 µs | 17.9 ms |

A Python caller that switches to the Rust-backed package gets **5.8× more tool
calls per second without changing a line of code**. Dropping Python entirely
buys another 1.6× — that gap is the PyO3 boundary and the event loop, since the
protocol work is already the same code.

The baseline is pinned to `chuk-mcp==0.9.4` deliberately: it is the last release
before the Rust core landed, so every later version would be measuring this
library against itself.

Per-message protocol costs, from `cargo bench -p chuk-mcp`:

| | |
| --- | --- |
| Parse a `tools/call` request | 1.77 µs |
| Serialize a request | 1.25 µs |
| Build a `2026-07-28` envelope (`_meta` + mirrored headers) | 1.29 µs |
| …and promote `x-mcp-header` parameters | 2.13 µs |
| Classify an HTTP response as modern | 1.2 ns |
| Decode a tool result | 1.66 µs |
| Negotiate a protocol version | 38.8 ns |

Method and caveats: [benchmarks/README.md](benchmarks/README.md).

---

## Conformance

Two suites, both blocking in CI — `./scripts/run-conformance.sh`:

**Our own rule suite**, spec requirements expressed as data and run against this
client and this server in both eras. 37 of 37 rules hold:

| Era | Subject | Rules | Covers |
| --- | --- | --- | --- |
| legacy | client | 6 | `initialize` first, required params, `notifications/initialized`, unique ids, `tools/call` shape, version pushed to the transport |
| `2026-07-28` | client | 10 | `_meta` version + capabilities, the three mirrored headers, parameter promotion, header encoding, no session header, `server/discover`, opaque `requestState` |
| legacy | server | 10 | `initialize` result, version negotiation, id echo, `tools/list` shape, tool errors, `-32601`, `ping`, notifications unanswered, `resources/read` |
| `2026-07-28` | protocol | 3 | `input_required` decoding, the methods that may carry it, at-least-one-field |
| both | protocol | 8 | `resultType` defaulting and preservation, `isError`, batch parsing, era classification, negotiation failure, elicitation modes and actions |

There are deliberately **no modern-server rules**: this crate's server is legacy
only, and an unimplemented era belongs in the matrix as an absence rather than
hidden behind rules nobody wrote.

**The official `@modelcontextprotocol/conformance` suite**, driving our client
as a black box. **Every client scenario it offers at a version we support
passes** — `initialize` and `tools_call` at both `2025-06-18` and `2025-11-25`,
plus `elicitation-sep1034-client-defaults` and `sse-retry` at `2025-11-25`.

Server-side reference scenarios cannot run at all — the suite drives servers
over `--url` and this crate has no HTTP serving mode yet. The upstream draft
(`2026-07-28`) client scenarios are auth-only, which is why the modern era is
covered by the in-repo suite instead.

Details: [docs/testing.md](docs/testing.md).

---

## Beyond the basics

- **[docs/protocol-eras.md](docs/protocol-eras.md)** — how detection works, and what `2026-07-28` changes.
- **[docs/transports.md](docs/transports.md)** — configuring each transport, limits, auth.
- **[docs/python.md](docs/python.md)** — the full Python surface, including the low-level `send_*` API.
- **[docs/testing.md](docs/testing.md)** — tests, the conformance suite, the benchmarks.
- **[docs/comparison.md](docs/comparison.md)** — how this compares with the official MCP SDKs.

---

## Status

Full-parity port of the Python package's public surface.

- **Protocol** — JSON-RPC 2.0 (request / notification / response / error / batch), MCP types, version negotiation across `2026-07-28`, `2025-11-25`, `2025-06-18`, `2025-03-26` and `2024-11-05`, and version-gated batching.
- **Messages** — `initialize`, `tools/*`, `resources/*`, `prompts/*`, `ping`, `logging/setLevel`, `completion/complete`, `sampling/createMessage`, `roots/*`, all notifications, with cancellation and progress.
- **Transports** — stdio, Streamable HTTP in both shapes, the dual-era transports, and the deprecated HTTP+SSE transport.
- **`2026-07-28`** — per-request `_meta`, mirrored `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` headers, `x-mcp-header` parameter promotion, `server/discover`, era detection and caching, new-request-ID retry on a broken response stream, and the typed result envelope.
- **Client** — `McpClient`, on any transport, in either era.
- **Server** — `McpServer` with tool/resource registration, a protocol handler, and in-memory sessions. **Legacy era only**; a modern server is outstanding.

Wire compatibility is verified in both directions against the Python
`chuk_mcp` implementation, and against the official
`@modelcontextprotocol/conformance` client scenarios in CI.

Known gaps, all tracked: a modern-era server, and an HTTP serving mode for it.
`scripts/run-conformance.sh` prints the current state.

---

## Building & testing

```bash
cargo test --workspace                  # Rust core + bindings
cargo clippy --workspace --all-targets  # lints
./scripts/run-conformance.sh            # protocol conformance, both eras
cargo bench -p chuk-mcp                 # protocol micro-benchmarks
python3 -m benchmarks                   # end-to-end: Rust vs PyO3 vs pure Python

cd crates/chuk-mcp-python && maturin develop   # build the extension into a venv
```

---

## License

Apache-2.0, matching the upstream `chuk-mcp` package.
