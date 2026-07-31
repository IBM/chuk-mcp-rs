# Protocol eras

MCP has two generations on the wire, and a client has to know which one each
peer speaks. [`connect`](../README.md#your-first-client) handles that for you;
this page is what it is doing, for when the detail matters.

## What `2026-07-28` changed

The revision made MCP **stateless**. Gone:

- the `initialize` request and `notifications/initialized`
- protocol sessions and the `Mcp-Session-Id` header
- `ping`
- server-initiated requests over the legacy lifecycle

In their place, every request carries what a server needs to serve it:

| Legacy | `2026-07-28` |
| --- | --- |
| `initialize` declares the version once | `_meta` declares it on every request |
| `initialize` declares client capabilities | `_meta` declares them on every request |
| `initialize` returns server info + capabilities | `server/discover` returns them, any time |
| Session id ties requests together | Nothing does; each request stands alone |

`ProtocolEra::Legacy` covers `2025-11-25` and earlier; `ProtocolEra::Modern` is
`2026-07-28` and later. `versioning::SUPPORTED_VERSIONS` lists every version
this library negotiates.

## Era is a property of the endpoint, not the client

One process can talk to a modern server and a legacy one at the same time. The
same host can serve different eras to different principals — which is why the
cache key is `(endpoint, credential context)` and not just the URL:

```rust
use chuk_mcp::transports::http_dual::DualEraHttpParameters;

let params = DualEraHttpParameters::new("https://example.com/mcp")?
    .with_bearer_token(token)
    // An opaque, stable identity for the credential — never a raw token.
    .with_credential_context(tenant_id);
```

A server can also be upgraded underneath a running client, so the decision is
held with a TTL (`EraCache`, `DEFAULT_ERA_TTL`) rather than forever.

## How detection works

The two algorithms are **not** interchangeable, because the transports give
different evidence.

### Stdio

Probe with `server/discover`:

- **a result** → modern
- **a protocol-level rejection** → legacy; fall back to `initialize`
- **a timeout, a crash, a broken pipe** → *nothing was learned*

That last case matters. A crashed process is not a legacy server, and treating
it as one would send an `initialize` to something that never answered — so the
probe returns an error rather than a verdict.

```rust
use chuk_mcp::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};

let connection = stdio_client_dual(params, StdioDualOptions::default()).await?;
match connection.era() {
    era if era.is_modern() => println!("stateless"),
    _ => println!("legacy handshake"),
}
```

### HTTP

There is no probe: the first response classifies the peer.

| Response | Verdict | Why |
| --- | --- | --- |
| `2xx` | Modern | Only a modern server answers a modern request. |
| `400` with a modern protocol error | Modern | It understood the request and rejected its contents. |
| `400`/`404`/`405` otherwise | Legacy | It rejected the request before processing it — so re-sending through the legacy path is safe. |
| `401`, `5xx`, timeout | **Unknown** | Proves nothing; must not be cached, or a briefly unhealthy server gets pinned to an era it never claimed. |

`DualEraHttpTransport::era()` returns `None` under `EraMode::Auto` until the
first response lands. HTTP cannot know sooner.

## Pinning

When you already know, skip the probe entirely:

```rust
use chuk_mcp::protocol::era::EraMode;

let params = DualEraHttpParameters::new(url)?.with_mode(EraMode::Modern);
```

`EraMode::Legacy` and `EraMode::Modern` never probe and never consult the
cache. In Python the same values are the strings `"legacy"` and `"2026-07-28"`,
with `"auto"` for detection.

## What a modern request carries

`build_envelope` produces the body and the headers together, and the headers
are derived from the body — never passed in — so they cannot disagree:

```rust
use chuk_mcp::protocol::envelope::{build_envelope, ClientIdentity};
use chuk_mcp::protocol::versioning;

let envelope = build_envelope(
    "tools/call",
    Some(json!({"name": "execute_sql", "arguments": {"region": "emea"}})),
    versioning::FIRST_MODERN_VERSION,
    &ClientIdentity::chuk(),
)?;
```

**In `params._meta`** (keys are namespaced —
`io.modelcontextprotocol/protocolVersion` and friends; use the constants in
`protocol::meta`):

| Key | Required | |
| --- | --- | --- |
| `protocolVersion` | yes | Must equal the `MCP-Protocol-Version` header. |
| `clientCapabilities` | yes | |
| `clientInfo` | recommended | |
| `logLevel` | no | Absent means "emit no log notifications for this request". |
| `progressToken` | no | |

**As headers**, mirroring the body:

| Header | Value |
| --- | --- |
| `MCP-Protocol-Version` | `_meta.protocolVersion` |
| `Mcp-Method` | the JSON-RPC method |
| `Mcp-Name` | `params.name`, or `params.uri` when there is no name |
| `Mcp-Param-*` | promoted tool parameters, below |

`envelope.headers_match_body()` re-derives them and confirms they agree.

## Parameter promotion

A tool schema can mark a parameter with `x-mcp-header`, and its value is
promoted to a header so a gateway can route on it without parsing the body:

```rust
let schema = json!({
    "type": "object",
    "properties": {
        "region": {"type": "string", "x-mcp-header": "Region"},
    },
});
envelope.promote_tool_params(&schema, &arguments)?;
// -> Mcp-Param-Region: emea
```

A value that is not plain-header-safe — non-ASCII, or containing a line break —
is wrapped in a Base64 sentinel (`=?base64?...?=`). Encoding happens inside
`push_param_header`, so a caller cannot bypass it and emit a header that would
split the request.

## Results in either era

Modern results are wrapped in an envelope whose `resultType` says whether the
operation ran to completion. A legacy result has no `resultType` and normalises
upward to `"complete"`, so one caller reads both:

```rust
let result = client.call_tool("greet", args).await?;
result.result_type;         // "complete", whichever era answered
result.text();              // flattened text content
result.value();             // the result as a single JSON value
result.structured_content(); // Some(..) when the server sent structured output
result.server_identity();   // Some(..) when the server identified itself
```

## What is not built yet

The **server** speaks the legacy lifecycle only — there is no `server/discover`
handler and no stateless request path. The in-repo conformance suite reflects
that honestly: its matrix has modern client rules and legacy server rules, and
no modern server rules at all. See [testing.md](testing.md).
