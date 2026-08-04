# Python

The `chuk_mcp_rs` extension exposes the whole Rust surface: the high-level
client and server, the low-level `send_*` API, transports, the protocol
handler, typed results and the error hierarchy. The
[`chuk-mcp`](https://github.com/chrishayuk/chuk-mcp) package re-exports it, so
existing `import chuk_mcp` code keeps working unchanged.

```bash
pip install chuk-mcp-rs
```

## High-level client

`connect` takes a URL or a command line, picks the transport, detects the
protocol era and completes its handshake:

```python
import asyncio
from chuk_mcp_rs import connect

async def main():
    async with await connect("https://example.com/mcp") as client:
        # …or: await connect("python server.py")
        print(client.era, client.protocol_version)
        result = await client.call_tool("greet", {"name": "World"})
        print(result.text)

asyncio.run(main())
```

With options — `bearer_token` and `headers` apply to HTTP targets, `env` to
subprocess targets, and `era` pins the generation instead of detecting it:

```python
client = await connect(
    "https://example.com/mcp",
    era="auto",                        # "auto" | "legacy" | "2026-07-28"
    bearer_token=token,
    headers={"X-Tenant": "acme"},
    timeout=10.0,
    credential_context="tenant-acme",  # opaque; never a raw token
)
```

The full client surface:

```python
import asyncio
from chuk_mcp_rs import connect

async def main():
    async with await connect("python server.py") as client:
        client.server_info          # ServerInfo | None
        client.capabilities         # ServerCapabilities | None
        client.era                  # "legacy" | "2026-07-28"
        client.protocol_version     # str | None

        tools = await client.list_tools()             # [Tool(...)]
        result = await client.call_tool("greet", {"name": "World"})
        resources = await client.list_resources()
        contents = await client.read_resource("demo://motd")
        prompts = await client.list_prompts()
        prompt = await client.get_prompt("summary", {"topic": "mcp"})
        alive = await client.ping()                   # bool

asyncio.run(main())
```

`connect` is a coroutine returning an async context manager, hence
`async with await`.

Two older entry points remain for compatibility, both stdio-only:
`connect_to_server(params)` performs the legacy handshake and nothing else, and
`connect_dual_stdio(params, mode="auto")` detects the era. Prefer `connect`.

## Answering a server that asks for input

A server can need something from the user mid-call — a username, a
confirmation, consent to open a URL. Pass a coroutine and it is answered in
either era: the `2026-07-28` revision returns the question as a result and the
call is retried with your answer attached, while a legacy server pushes the
question mid-call. Your handler does not see the difference.

```python
from chuk_mcp_rs import connect, ElicitResult

async def on_elicit(request):
    print(request.message)          # why it is being asked
    print(request.mode)             # "form" or "url"
    print(request.requestedSchema)  # form mode: the shape of the answer
    print(request.url)              # url mode: where to send the user
    return ElicitResult.accept({"name": "octocat"})

client = await connect("https://example.com/mcp", on_elicit=on_elicit)
result = await client.call_tool("open_pull_request", {"repo": "chuk-mcp-rs"})
```

Return `ElicitResult.accept(content)`, `.decline()` or `.cancel()` — or a plain
dict of the same shape. A user who says no is not an error. Returning `None`,
or raising, is read as a cancellation: neither is a decision the user made.

Setting `on_elicit` also declares the `elicitation` capability, because a server
must not ask a client that has not said it can answer. Pass `url_mode=True`
only if you can actually open a URL for the user — a server is then permitted
to send URL-mode requests, and a handler that cannot service them will simply
have to decline every one.

## Typed results

Results are objects, not dicts. Attribute names follow the wire where the wire
is camelCase:

```python
result = await client.call_tool("query", {"sql": "select 1"})

result.text                # flattened text content
result.value               # the result as a single Python value
result.content             # the raw content blocks
result.isError             # bool
result.resultType          # "complete" — normalised across both eras
result.structuredContent   # structured output, or None
result.serverIdentity      # who answered, or None
result.to_dict()           # everything, as a dict
```

`Tool`, `Resource`, `Prompt`, `ServerInfo`, `ServerCapabilities`,
`ReadResourceResult` and `GetPromptResult` are typed the same way —
`tool.name`, `caps.tools`, `resource.uri`.

## High-level server

Handlers are async and receive the tool's arguments as keyword arguments:

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

async def motd() -> str:
    return "hello"

server.register_resource("demo://motd", "motd", "Message of the day", "text/plain", motd)

asyncio.run(server.run_stdio())
```

### Argument order

`register_tool` and `register_resource` accept their arguments in **any order**.
The historical `chuk_mcp` convention puts the handler second and the Rust core
puts it last; rather than make one of them wrong, both work — a callable can
only be the handler, a dict can only be the schema, a string can only be a
description. All four of these are the same registration:

```python
server.register_tool("greet", greet, SCHEMA, "Greet someone")     # chuk_mcp order
server.register_tool("greet", SCHEMA, "Greet someone", greet)     # Rust-core order
server.register_tool("greet", schema=SCHEMA, handler=greet)       # keywords
server.register_resource("demo://motd", "motd", "MOTD", "text/plain", motd)
```

Anything that is not a callable, a dict or a string is a `TypeError` naming what
arrived, rather than being silently read as the wrong argument.

Custom methods go through the protocol handler:

```python
server.protocol_handler.register_method("my/method", handler)
```

## Low-level API

For direct control, drive the stream pair yourself:

```python
from chuk_mcp_rs import stdio_client, send_initialize, send_tools_call

async with stdio_client(params) as (read, write):
    init = await send_initialize(read, write)
    result = await send_tools_call(read, write, "greet", {"name": "World"})
```

Available: `send_initialize`, `send_initialized_notification`, `send_tools_list`,
`send_tools_call`, `send_resources_list`, `send_resources_read`,
`send_resources_subscribe`, `send_resources_unsubscribe`, `send_prompts_list`,
`send_prompts_get`, `send_ping`, `send_roots_list`, `send_message`, and the
`send_progress_notification` / `send_cancelled_notification` /
`send_roots_list_changed_notification` notifications. Most take an optional
`timeout=` and, where the operation paginates, `cursor=`.

`client.raw_streams()` gives the same pair from an already-connected client —
the era is settled and `_meta` injection lives in the transport, so the same
`send_*` calls work whichever era was negotiated.

## Serving `2026-07-28` from Python

`chuk-mcp-server` brings its own HTTP serving and registries; what it needs
from here is the wire contract, so there is one implementation of it rather
than two. Everything below takes and returns plain dicts, so it can be used a
message at a time.

```python
from chuk_mcp_rs import (
    discover_result, is_modern_request, check_version,
    missing_required_meta, is_removed_method,
    stamp_result_type, stamp_cache_hints,
    input_required_result, elicit_request,
    subscription_filter, subscription_acknowledgement,
)

async def handle(message):
    modern = is_modern_request(message)

    # A modern request carries its version and the client's capabilities on
    # every call. Missing either is malformed — answer -32602, and 400 on HTTP.
    if modern and (why := missing_required_meta(message)):
        return error(-32602, why)

    # An unspeakable version is answered with the list this server can speak,
    # and the version that was asked for, so the client can renegotiate.
    if rejection := check_version(message):
        return error(rejection["code"], rejection["message"], rejection["data"])

    method = message["method"]

    # The RPCs this revision removed answer -32601 (404 on HTTP) — but only
    # for a modern request. A legacy one still gets them.
    if modern and is_removed_method(method):
        return error(-32601, f"Method not found: {method}")

    result = await dispatch(method, message.get("params", {}))

    if modern:
        # Caching hints first: they belong on a `complete` result, which is
        # what an unstamped one is. Only the six cacheable operations are
        # touched, so this is safe to call on every result.
        result = stamp_cache_hints(result, method, ttl_ms=60_000, scope="private")
        result = stamp_result_type(result)
    return result
```

`discover_result(name, version, capabilities, instructions)` builds the
`server/discover` answer — versions under `supportedVersions`, identity in
`_meta`, not shaped like the old `initialize` result.

**Asking for more input.** A modern server does not send the client a request;
it returns one and waits to be retried:

```python
return input_required_result(
    input_requests={"user_name": elicit_request("What is your name?", {
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
    })},
    request_state="opaque-token-only-this-server-understands",
)
```

The client retries the original request with `inputResponses` and the
`requestState` echoed back. Verify that state before acting on it — with no
session, the client has been holding what the server needs to remember.

**Subscriptions.** `subscriptions/listen` replaced the HTTP `GET` endpoint and
`resources/subscribe`. The acknowledgement must be the stream's first message,
and every message on it carries the subscription id:

```python
ack = subscription_acknowledgement(request["id"], message["params"])
await stream.send(ack)

agreed = subscription_filter(message["params"])
if agreed.get("toolsListChanged"):
    ...  # and nothing the client did not ask for
```

## Streamable HTTP

```python
from chuk_mcp_rs import StreamableHTTPParameters, StreamableHTTPTransport

params = StreamableHTTPParameters(
    url="http://localhost:3000/mcp",
    timeout=30.0,
    bearer_token=token,
    enable_streaming=True,
    max_concurrent_requests=10,
)
async with StreamableHTTPTransport(params) as transport:
    read, write = await transport.get_streams()
    transport.get_session_id()
```

## Errors and constants

```python
from chuk_mcp_rs import (
    McpError, RetryableError, NonRetryableError,
    VersionMismatchError, ValidationError,
    CURRENT_VERSION, LATEST_LEGACY_VERSION,
    supported_versions, core_version, get_default_environment,
)
```

`supported_versions()` lists every version the core negotiates, newest first;
`core_version()` reports the Rust crate version behind the bindings.

## Building from source

```bash
cd crates/chuk-mcp-python
maturin develop            # into the active virtualenv
maturin build --release    # a wheel in target/wheels
```

Wheels are abi3, so one build covers Python 3.9 and later.
