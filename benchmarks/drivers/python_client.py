#!/usr/bin/env python3
"""End-to-end benchmark driver for a Python MCP client.

Standalone by design: this file is executed by whichever interpreter has the
package under test installed, so it imports nothing from the benchmark
harness. The module to drive is named by ``--module``, which is what lets the
same driver measure the Rust-backed bindings (``chuk_mcp_rs``) and the
pure-Python baseline (``chuk_mcp``) without either one getting a code path of
its own.

Prints one JSON object of raw timings on stdout; statistics are the
orchestrator's job. See ``benchmarks/README.md`` for the contract.
"""

from __future__ import annotations

import argparse
import asyncio
import importlib
import inspect
import json
import sys
import time
from typing import Any

#: Exit status for a workload that could not be run at all, as opposed to one
#: that ran and was slow.
EXIT_DRIVER_ERROR = 1

#: Submodules searched when a symbol is missing from the top-level package.
#: chuk-mcp 0.9.4 leaves ``connect_to_server`` bound to ``None`` at package
#: level — its ``__init__`` swallows an ImportError — while exporting it
#: perfectly well from ``chuk_mcp.client``. Searching rather than special-casing
#: the version keeps the driver honest: it drives whatever the package's public
#: API is, and fails loudly when there isn't one.
FALLBACK_SUBMODULES = ("client", "transports")

#: The two symbols the driver needs from the module under test.
CONNECT_SYMBOL = "connect_to_server"
PARAMETERS_SYMBOL = "StdioParameters"


def resolve(module_name: str, symbol: str) -> Any:
    """Find ``symbol`` on the named package or one of its usual submodules."""
    root = importlib.import_module(module_name)
    found = getattr(root, symbol, None)
    if found is not None:
        return found

    for submodule in FALLBACK_SUBMODULES:
        try:
            candidate = importlib.import_module(f"{module_name}.{submodule}")
        except ImportError:
            continue
        found = getattr(candidate, symbol, None)
        if found is not None:
            return found

    raise AttributeError(f"{module_name} exports no usable {symbol}")


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--module", required=True, help="MCP client module to import")
    parser.add_argument("--server-command", required=True)
    parser.add_argument("--server-arg", action="append", default=[])
    parser.add_argument("--iterations", type=int, required=True)
    parser.add_argument("--warmup", type=int, default=0)
    parser.add_argument("--tool", required=True)
    parser.add_argument("--argument-name")
    parser.add_argument("--argument-value")
    return parser.parse_args()


def tool_arguments(options: argparse.Namespace) -> dict[str, Any]:
    if options.argument_name is None or options.argument_value is None:
        return {}
    return {options.argument_name: options.argument_value}


async def open_client(connect: Any, parameters: Any) -> Any:
    """Enter ``connect_to_server`` for either calling convention.

    The bindings expose it as a coroutine returning an async context manager
    (``async with await connect_to_server(...)``); the pure-Python baseline
    exposes it as an ``@asynccontextmanager`` (``async with
    connect_to_server(...)``). Awaiting only when the result is awaitable
    covers both without asking the caller which is which.
    """
    connection = connect(parameters)
    if inspect.isawaitable(connection):
        connection = await connection
    return connection


async def run(options: argparse.Namespace) -> dict[str, Any]:
    connect = resolve(options.module, CONNECT_SYMBOL)
    stdio_parameters = resolve(options.module, PARAMETERS_SYMBOL)
    parameters = stdio_parameters(
        command=options.server_command, args=options.server_arg
    )
    arguments = tool_arguments(options)

    # The handshake span covers spawning the server as well as the protocol
    # exchange — the same span the Rust driver reports.
    handshake_started = time.perf_counter()
    connection = await open_client(connect, parameters)
    async with connection as client:
        handshake_seconds = time.perf_counter() - handshake_started

        for _ in range(options.warmup):
            await client.call_tool(options.tool, arguments)

        call_seconds = []
        for _ in range(options.iterations):
            started = time.perf_counter()
            await client.call_tool(options.tool, arguments)
            call_seconds.append(time.perf_counter() - started)

    return {
        "runner": options.module,
        "handshake_seconds": handshake_seconds,
        "call_seconds": call_seconds,
    }


def main() -> int:
    options = parse_arguments()
    try:
        report = asyncio.run(run(options))
    except Exception as error:  # noqa: BLE001 - reported verbatim to the orchestrator
        print(f"{options.module}: {error!r}", file=sys.stderr)
        return EXIT_DRIVER_ERROR
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
