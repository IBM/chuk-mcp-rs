# Transports

Most programs never name a transport: [`connect`](../README.md#your-first-client)
picks one from the target string. This page is for when you want to hold the
transport yourself.

A transport is started, hands out a `(read, write)` stream pair of JSON-RPC
messages, and is closed on drop or via `close()`. Everything above it —
`McpClient`, the `send_*` helpers — is transport-agnostic.

| Module | Era | Shape |
| --- | --- | --- |
| `stdio` | Legacy | Subprocess, newline-delimited JSON-RPC. |
| `stdio_dual` | Both | Stdio, era probed on connect. |
| `http` | Legacy | Streamable HTTP with `Mcp-Session-Id`. |
| `http_modern` | Modern | Stateless Streamable HTTP. |
| `http_dual` | Both | Streamable HTTP, era detected per endpoint. |
| `sse` | Legacy | The deprecated HTTP+SSE transport. |

## Stdio

```rust
use chuk_mcp::transports::stdio::{StdioParameters, StdioTransport};

let params = StdioParameters::new("python", ["server.py"])
    .with_env(env);                        // defaults to get_default_environment()

let transport = StdioTransport::start(params).await?;
```

`get_default_environment()` returns a conservative environment for spawned
subprocesses, rather than inheriting the parent's wholesale.

### Dual-era stdio

```rust
use chuk_mcp::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};
use chuk_mcp::protocol::era::EraMode;

let options = StdioDualOptions {
    mode: EraMode::Auto,           // or Legacy / Modern to pin
    ..StdioDualOptions::default()  // timeout, identity, limits
};
let connection = stdio_client_dual(params, options).await?;
```

`StdioConnection` carries the settled transport, its streams and the peer's
profile. Hand it to a client with `McpClient::from_profile` — the handshake has
already happened, so the client must not repeat it, and passing the profile is
what makes `client.era()` answerable afterwards.

## Streamable HTTP — legacy

```rust
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};

let params = StreamableHttpParameters::new("http://localhost:3000/mcp")?
    .with_bearer_token(token)
    .with_headers(headers);

let transport = StreamableHttpTransport::start(params)?;
transport.get_session_id();     // Some(..) once the server assigns one
```

Handles both immediate JSON responses and SSE-streamed ones. `enable_streaming`,
`timeout` and `max_concurrent_requests` are fields on the parameters.

## Streamable HTTP — modern

```rust
use chuk_mcp::transports::http_modern::{ModernHttpParameters, ModernHttpTransport};

let params = ModernHttpParameters::new("https://example.com/mcp")?
    .with_bearer_token(token)
    .with_identity(identity)          // sent in every request's _meta
    .with_max_stream_retries(retries);

let transport = ModernHttpTransport::start(params)?;
```

Deliberately has **no** `session_id`: there are no protocol sessions to rejoin.
Any `Mcp-Session-Id` a caller supplies in `with_headers` is dropped — forwarding
it could make a dual-era server select legacy semantics for a request that is
otherwise modern.

A response stream that dies before delivering a result causes the request to be
re-issued under a **new** request id, up to `max_stream_retries`.

## Streamable HTTP — dual-era

```rust
use chuk_mcp::transports::http_dual::{DualEraHttpParameters, DualEraHttpTransport};

let params = DualEraHttpParameters::new(url)?
    .with_bearer_token(token)
    .with_credential_context(tenant)   // opaque identity, never a raw token
    .with_mode(EraMode::Auto);

let transport = DualEraHttpTransport::start(params)?;
transport.era();                       // None until the first response classifies
```

Detection rules and what each response proves: see
[protocol-eras.md](protocol-eras.md).

## SSE (deprecated)

```rust
use chuk_mcp::transports::sse::{SseParameters, SseTransport};

let params = SseParameters::new("http://localhost:3000")?;   // sse_endpoint defaults to /sse
let transport = SseTransport::start(params).await?;
```

`start` waits, up to the configured timeout, for the server's `endpoint` event
announcing where to POST. Present for compatibility with servers that have not
migrated; prefer Streamable HTTP for anything new.

## Limits

Every transport bounds how much undelimited data a peer can send before it
gives up, rather than buffering it all:

```rust
use chuk_mcp::transports::limits::{TransportLimits, DEFAULT_MAX_BUFFER_SIZE};

let limits = TransportLimits::default().with_max_buffer_size(1024 * 1024);
let transport = StreamableHttpTransport::start_with_limits(params, limits)?;
```

The default is 10 MB — comfortably above any normal JSON-RPC message, and still
a bound on a hostile one. `0` disables the cap. Every transport has a
`start_with_limits` alongside `start`; the server side takes
`McpServer::with_max_buffer_size`.

## Serving

```rust
use chuk_mcp::server::http::{serve_http, serve_http_with, HttpOptions};

serve_http(server, "127.0.0.1:3000".parse()?).await?;              // loopback only
serve_http_with(server, address, HttpOptions::new()
    .allow_host("mcp.example.com")).await?;                        // and this name
```

Both eras arrive on the same `/mcp` endpoint and are told apart per request.

**`Host` and `Origin` are validated before anything else** — before the path,
the method, or reading the body. The default answers `localhost`,
`127.0.0.0/8` and `::1` only, which is what stops a web page the user happens
to visit from reaching a loopback server through a name that resolves to
`127.0.0.1` ([GHSA-w48q-cv73-mx4w][rebinding]). A refused request gets `403`
naming the host. `HttpOptions::allow_any_host()` turns the check off, which is
only correct behind TLS with authentication or on a network the server is not
reachable from.

[rebinding]: https://github.com/modelcontextprotocol/typescript-sdk/security/advisories/GHSA-w48q-cv73-mx4w

### When a POST answers with a stream

A call to a tool registered with `register_interactive_tool` is answered as
`text/event-stream` rather than one JSON body: the tool's notifications and any
question it asks arrive as events, and its result is the last one. Everything
else is answered with a single `application/json` body, as before.

Two things have to be true for the stream: the tool was registered as
interactive, and the client's `Accept` includes `text/event-stream`. A client
that did not ask for a stream gets the buffered answer, because anything else
would be unreadable to it.

A question the server asks is answered by a **separate POST** carrying a
JSON-RPC response. The server matches it back to the waiting call by id and
answers `202`; an answer nobody is waiting for is dropped rather than mistaken
for a request.

## Writing your own

Implement `Transport`:

```rust
#[async_trait]
impl Transport for MyTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> { .. }
    fn set_protocol_version(&self, version: &str) {}   // gates batching
    async fn close(&mut self) -> Result<(), McpError> { Ok(()) }
}
```

Build the stream pair with `protocol::messages::send_message::message_channel`.
`connect_with_transport(transport)` then gives you a client over it.
