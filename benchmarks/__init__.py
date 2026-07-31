"""End-to-end benchmark harness for chuk-mcp-rs.

Measures the cost of driving a real MCP server over stdio from three clients —
the native Rust client, the PyO3 bindings, and the last pure-Python release of
chuk-mcp — against the same server binary and the same workload. See
``README.md`` in this directory.
"""
