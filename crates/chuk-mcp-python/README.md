# chuk-mcp-rs (Python bindings)

Model Context Protocol client and server, powered by the `chuk-mcp` Rust core.

```python
import asyncio
from chuk_mcp_rs import StdioParameters, connect_to_server

async def main():
    params = StdioParameters(command="python", args=["server.py"])
    async with await connect_to_server(params) as client:
        tools = await client.list_tools()
        result = await client.call_tool("greet", {"name": "World"})
        print(result["content"][0]["text"])

asyncio.run(main())
```

Build with [maturin](https://github.com/PyO3/maturin): `maturin develop` from
`crates/chuk-mcp-python`.
