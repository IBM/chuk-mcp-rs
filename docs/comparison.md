# How this compares with the official MCP SDKs

Written against the official SDKs as of July 2026: the Rust SDK
[`rmcp`](https://github.com/modelcontextprotocol/rust-sdk), the Python SDK
[`mcp`](https://github.com/modelcontextprotocol/python-sdk), and the
[TypeScript SDK v2](https://github.com/modelcontextprotocol/typescript-sdk),
all of which implement the `2026-07-28` specification.

This is a comparison of *developer experience*, not of correctness or maturity.
The official SDKs are the reference implementations and are further along in
several areas named below.

## At a glance

| | chuk-mcp-rs | rmcp (official Rust) | mcp (official Python) | TS SDK v2 |
| --- | --- | --- | --- | --- |
| Connect to a server | `connect("url or command")` | `().serve(transport)` | `Client(...)` per transport | per transport |
| Transport chosen for you | ✅ from the target string | ❌ name it | ❌ name it | ❌ name it |
| Era detected per peer | ✅ automatic, cached | ⚙️ `serve_with_lifecycle(Discover)` | ✅ | ✅ |
| Define a tool (server) | explicit JSON Schema | `#[tool]` macro, schema derived | `@mcp.tool()`, schema from type hints | `registerTool` + Standard Schema |
| Stateless HTTP **server** | ❌ not built | ✅ default | ✅ | ✅ |
| One library, two languages | ✅ same core | ❌ Rust only | ❌ Python only | ❌ TS only |

## Connecting

The official Rust SDK asks you to choose and construct the transport, then
serve over it:

```rust
// rmcp
let client = ().serve(TokioChildProcess::new(Command::new("npx").configure(|cmd| {
    cmd.arg("-y").arg("@modelcontextprotocol/server-everything");
}))?).await?;
```

Here the target string carries that information, so there is nothing to choose:

```rust
// chuk-mcp-rs
let client = connect("npx -y @modelcontextprotocol/server-everything").await?;
let client = connect("https://example.com/mcp").await?;
```

`().serve(...)` is idiomatic Rust but reads oddly on first contact — the unit
value is the client handler. The trade is real, though: rmcp's shape is what
lets you supply a *client* handler (for sampling, roots, elicitation) in the
same call, which this library has no equivalent of.

On the era, both detect. rmcp exposes it as an explicit lifecycle choice
(`serve_with_lifecycle` with a `Discover` mode); here detection is the default
and `EraMode` pins it when you already know. Neither is more capable — but the
default differs, and defaults are most of DX.

## Defining tools

This is where the official SDKs are clearly ahead. They derive the schema:

```rust
// rmcp — schema from the parameter type
#[tool(description = "Add two numbers")]
fn add(&self, Parameters(AddParams { a, b }): Parameters<AddParams>) -> String {
    (a + b).to_string()
}
```

```python
# mcp — schema from the type hints and docstring
@mcp.tool()
def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b
```

Here you write the schema out:

```rust
server.register_tool(
    "add",
    json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}),
    "Add two numbers",
    |args| async move { /* … */ },
);
```

More typing, more to get wrong, and no compile-time link between the schema and
what the handler actually reads. A `#[tool]`-style macro deriving `inputSchema`
from a `JsonSchema` parameter type is the single largest DX gap in this
library, and it is a gap on the *server* side only.

## Client results

Typed results with era normalisation are a genuine advantage here. A tool result
reads the same whichever protocol generation answered:

```rust
result.text();               // flattened text
result.value();              // the whole result as one value
result.structured_content(); // structured output, when present
result.result_type;          // "complete" — legacy results normalise upward
result.server_identity();    // who answered
```

The official SDKs expose the wire shape more directly; the Python SDK's
`result.structured_content` is the closest equivalent. Normalising a legacy
result up to the modern envelope means dual-era callers do not branch, which is
the whole point of supporting both.

## Two languages, one implementation

The official SDKs are separate implementations per language, each tracking the
spec independently. Here the Python package is the Rust core through PyO3, so
protocol behaviour cannot drift between them — and Python callers get [5.8×
the throughput](../benchmarks/README.md) of the last pure-Python release without
touching their code.

The cost is Python-side ergonomics: no decorator-based server, and the surface
is shaped by the historical `chuk_mcp` API rather than designed fresh.
`register_tool` accepts its arguments in either order precisely because two
conventions had to be honoured at once — a compatibility tax the official SDKs
do not pay.

## Where the official SDKs are simply ahead

Stated plainly, because a comparison that only flatters is not useful:

- **Stateless HTTP serving.** rmcp's `StreamableHttpService` serves `2026-07-28`
  clients without sessions, by default. This library has no HTTP server
  transport at all — its server is stdio and legacy-only.
- **Schema derivation.** Covered above.
- **Client-side features.** Sampling, roots and elicitation handlers are
  first-class in the official SDKs. Here the message types exist but there is no
  handler surface, which is why the official conformance suite's
  `elicitation-sep1034-client-defaults` scenario does not pass.
- **Ecosystem.** Reference servers, auth helpers and OAuth flows ship with the
  official SDKs.

## When this library fits

- You have Python code on `chuk_mcp` and want the throughput without a rewrite.
- You must talk to a mixed fleet where some servers are `2026-07-28` and some
  are not, and you would rather not think about which.
- You want one protocol implementation behind both your Rust and Python code.

## When to reach for the official SDK

- You are **building a server**, especially one served over HTTP.
- You want schemas derived from types rather than written by hand.
- You need sampling, roots, elicitation or the OAuth helpers.
- You want the reference implementation, tracked by the people who write the
  spec.
