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
