# chuk-mcp-rs

A Model Context Protocol (MCP) client and server library in Rust, with Python
bindings — speaking **both** protocol generations: the stateless `2026-07-28`
revision and the legacy `initialize`-handshake era, working out which one a
server wants so you do not have to.

---

## Quickstart

**Rust**

```bash
cargo add chuk-mcp tokio serde_json
```

```rust
use chuk_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), McpError> {
    // A URL or a command line — it picks the transport and the protocol era.
    let mut client = connect("./my-mcp-server").await?;

    println!("connected to {}", client.server_info().unwrap().name);
    for tool in client.list_tools().await? {
        println!("  {}", tool.name);
    }

    let result = client.call_tool("greet", json!({"name": "World"})).await?;
    println!("{}", result.text());

    client.close().await
}
```

**Python**

```bash
pip install chuk-mcp-rs
```

```python
import asyncio
from chuk_mcp_rs import connect

async def main():
    async with await connect("./my-mcp-server") as client:
        print("connected to", client.server_info.name)
        for tool in await client.list_tools():
            print(" ", tool.name)

        result = await client.call_tool("greet", {"name": "World"})
        print(result.text)

asyncio.run(main())
```

That is the whole API for most uses. `connect` takes a URL (`https://…/mcp`) or
a command line (`python server.py`), spawns or dials it, detects whether the
server speaks `2026-07-28` or the legacy protocol, completes the right
handshake, and hands back a client that works the same either way.

### Run it right now

No server of your own needed — the repo ships one:

```bash
git clone https://github.com/IBM/chuk-mcp-rs && cd chuk-mcp-rs
cargo build --bin chuk-mcp-demo-server
cargo run --example interop_client -- ./target/debug/chuk-mcp-demo-server
```

```text
connected: chuk-mcp-demo v0.1.0 over 2026-07-28
ping: true
tools: ["add", "greet"]
greet: Hello, RustClient!
add: {
  "sum": 42.0
}
```

### Where to go next

| I want to… | Go to |
| --- | --- |
| Set a token, a header, or pin the protocol era | [Options](#options) |
| Build a server | [Your first server](#your-first-server) |
| Understand the two protocol eras | [docs/protocol-eras.md](docs/protocol-eras.md) |
| Answer a server that asks the user a question | [docs/python.md](docs/python.md#answering-a-server-that-asks-for-input) |
| Use Python specifically | [docs/python.md](docs/python.md) |
| Pick a transport myself | [docs/transports.md](docs/transports.md) |

---

## Options

`Connect` is `connect` with the knobs exposed:

```rust
let client = Connect::to("https://example.com/mcp")
    .bearer_token(token)
    .header("X-Tenant", "acme")
    .era(EraMode::Legacy)               // pin instead of detecting
    .timeout(Duration::from_secs(10))
    .input_handler(handler)             // answer questions from the server
    .connect()
    .await?;
```

```python
client = await connect(
    "https://example.com/mcp",
    bearer_token=token,
    headers={"X-Tenant": "acme"},
    era="legacy",                       # "auto" | "legacy" | "2026-07-28"
    timeout=10.0,
    on_elicit=handler,
)
```

`client.era()` and `client.protocol_version()` (`.era` / `.protocol_version` in
Python) report what it settled on.

---

## The crates

| Crate | What it is |
| --- | --- |
| [`crates/chuk-mcp`](crates/chuk-mcp) | The core Rust library — protocol types, JSON-RPC, transports, client, server. On crates.io as [`chuk-mcp`](https://crates.io/crates/chuk-mcp). |
| [`crates/chuk-mcp-python`](crates/chuk-mcp-python) | PyO3/maturin bindings exposing the core to Python as `chuk_mcp_rs` (on PyPI as `chuk-mcp-rs`). |

The Python package [`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp)
re-exports these bindings, so `import chuk_mcp` keeps working unchanged while
being powered by Rust — [about 6× more tool calls per
second](benchmarks/README.md) than the last pure-Python release, with no code
changes.

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

The server speaks **both** eras from the one registration: a legacy client gets
the `initialize` lifecycle, a `2026-07-28` client gets `server/discover` and
stateless requests, and the era is decided per request rather than per server.

### Tools that talk while they work

Most tools answer and stop. One that needs to report progress, log what it is
doing, sample a model, or ask the user something registers differently, and is
handed a `CallContext`:

```rust
server.register_interactive_tool(
    "summarise",
    json!({"type": "object", "properties": {"text": {"type": "string"}}}),
    "Summarise some text",
    |args, ctx| async move {
        ctx.log("info", json!("starting"));
        ctx.progress(50.0, Some(100.0));

        // Ask the client's model, and wait for its answer.
        let reply = ctx.sample(json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": args["text"]}}],
            "maxTokens": 100,
        })).await?;

        Ok(json!(reply["content"]["text"]))
    },
);
```

Over Streamable HTTP the POST that carried the call answers as an event stream:
what the tool says arrives before its result, and a question it asks is
answered by a separate POST that the server matches back to the waiting call.
The distinction between the two registration calls is not cosmetic — the
transport has to know *before* the call whether the answer needs a stream.

`CallContext::request` fails rather than hangs when the transport has no way
back to the client (plain stdio) or when the client does not answer in time.

### Serving somewhere other than loopback

`serve_http` answers `localhost`, `127.0.0.0/8` and `::1` only. That is the
defence against [DNS rebinding][rebinding]: without it, any web page the user
visits can reach a local MCP server through a hostname that resolves to
`127.0.0.1`. A server that is meant to be reachable from elsewhere says so:

```rust
use chuk_mcp::server::http::{serve_http_with, HttpOptions};

serve_http_with(
    server,
    "0.0.0.0:3000".parse().unwrap(),
    HttpOptions::new().allow_host("mcp.example.com"),
).await
```

`allow_any_host()` disables the check entirely, which is only correct when
something else already establishes who is calling — TLS with authentication,
or a network the server is not reachable from.

[rebinding]: https://github.com/modelcontextprotocol/typescript-sdk/security/advisories/GHSA-w48q-cv73-mx4w

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

Pin the era with `.era(...)` — see [Options](#options) — when you already know,
and no probe is sent.

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
100 warm-up calls, on an Apple M3 Max (macOS 15.7.4, rustc 1.95.0, CPython
3.12.2). Reproduce with `python3 -m benchmarks`:

| Client | Mean/call | Calls/sec | p50 | p95 | p99 | Handshake |
| --- | --- | --- | --- | --- | --- | --- |
| **rust-native** — the `chuk-mcp` crate | **38.6 µs** | **25,907** | 37.5 µs | 45.2 µs | 54.2 µs | 3.7 ms |
| **python-bindings** — `chuk_mcp_rs` via PyO3 | 72.4 µs | 13,818 | 70.6 µs | 82.0 µs | 98.2 µs | 2.9 ms |
| **pure-python** — `chuk-mcp==0.9.4` | 427.8 µs | 2,338 | 426.2 µs | 445.6 µs | 470.7 µs | 13.9 ms |

A Python caller that switches to the Rust-backed package gets **6× more tool
calls per second without changing a line of code**. Dropping Python entirely
buys another 1.9× — that gap is the PyO3 boundary and the event loop, since the
protocol work is already the same code.

The baseline is pinned to `chuk-mcp==0.9.4` deliberately: it is the last release
before the Rust core landed, so every later version would be measuring this
library against itself.

Per-message protocol costs, from `cargo bench -p chuk-mcp`:

| Client side | | Server side | |
| --- | --- | --- | --- |
| Parse a `tools/call` request | 2.29 µs | Validate `Host`/`Origin` | 102 ns |
| Serialize a request | 1.35 µs | Shape a text result | 665 ns |
| Build a `2026-07-28` envelope | 1.77 µs | Shape a rendered result | 1.64 µs |
| …and promote `x-mcp-header` params | 2.80 µs | Frame one SSE event | 645 ns |
| Classify an HTTP response as modern | 0.80 ns | Match a resource template | 148 ns |
| Decode a tool result | 2.28 µs | Read a completion request | 427 ns |
| Negotiate a protocol version | 32.0 ns | Parse a log level | 2.6 ns |

The rebinding check costs ~100 ns on every HTTP request — a floor under the
serving path, and about 4% of what parsing the request itself costs.

Method and caveats: [benchmarks/README.md](benchmarks/README.md).

---

## Conformance

Two suites, both blocking in CI — `./scripts/run-conformance.sh`:

**Our own rule suite**, spec requirements expressed as data and run against this
client and this server in both eras. 42 of 42 rules hold:

| Era | Subject | Rules | Covers |
| --- | --- | --- | --- |
| legacy | client | 6 | `initialize` first, required params, `notifications/initialized`, unique ids, `tools/call` shape, version pushed to the transport |
| `2026-07-28` | client | 10 | `_meta` version + capabilities, the three mirrored headers, parameter promotion, header encoding, no session header, `server/discover`, opaque `requestState` |
| legacy | server | 10 | `initialize` result, version negotiation, id echo, `tools/list` shape, tool errors, `-32601`, `ping`, notifications unanswered, `resources/read` |
| `2026-07-28` | server | 5 | `server/discover` shape and statelessness, `resultType` on every result, legacy requests left alone, unsupported versions rejected with the list |
| `2026-07-28` | protocol | 3 | `input_required` decoding, the methods that may carry it, at-least-one-field |
| both | protocol | 8 | `resultType` defaulting and preservation, `isError`, batch parsing, era classification, negotiation failure, elicitation modes and actions |

Every era-and-subject pair the implementation covers has rules. An area with
none would show as an absent row, which is where an unimplemented one belongs —
not hidden behind rules nobody wrote.

**The official `@modelcontextprotocol/conformance` suite**, driving our client
and our server as black boxes. **Every client scenario it offers at a version
we support passes** — `initialize` and `tools_call` at both `2025-06-18` and
`2025-11-25`, plus `elicitation-sep1034-client-defaults` and `sse-retry` at
`2025-11-25`.

Server-side reference scenarios run too, against the HTTP serving mode, and
**all 39 checks across 30 scenarios pass** — the lifecycle, logging,
completion, subscriptions, URI templates, binary resources, every content type,
and the server-initiated exchanges (progress, sampling, elicitation). Blocking,
like the client scenarios.

The upstream draft (`2026-07-28`) client scenarios are auth-only, which is why
the modern era is covered by the in-repo suite instead.

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
- **Server** — `McpServer` with tool/resource/prompt registration, resource templates and binary bodies, subscriptions, logging levels and completions, a protocol handler, and in-memory sessions, served over **stdio or Streamable HTTP**. Answers **both** eras: `server/discover`, per-request version checking and `resultType` stamping for modern callers, the `initialize` lifecycle for legacy ones. A tool may talk while it works — progress, logs, sampling and elicitation — answered on an event stream.
- **Hardening** — `Host`/`Origin` validation against DNS rebinding, bounded request bodies and message buffers, and a timeout on anything the server asks of the client.

Wire compatibility is verified in both directions against the Python
`chuk_mcp` implementation, and against the official
`@modelcontextprotocol/conformance` client **and server** scenarios in CI —
all of both, blocking.

Known gaps: a subscription is recorded but nothing yet emits
`notifications/resources/updated` when a resource changes, and the same is true
of the `listChanged` notifications. `scripts/run-conformance.sh` prints the
current state.

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
