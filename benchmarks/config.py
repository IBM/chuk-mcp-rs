"""Names, paths and defaults shared by the end-to-end benchmark harness.

Nothing here is tuning advice — these are the values that have to agree
between the orchestrator and the drivers. A driver reading a flag the
orchestrator does not send would measure a different workload while still
reporting a healthy-looking number, so both sides read the spellings from
this module.
"""

from __future__ import annotations

from pathlib import Path

# --- Layout ---------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
BENCHMARKS_DIR = REPO_ROOT / "benchmarks"
DRIVERS_DIR = BENCHMARKS_DIR / "drivers"
PYTHON_DRIVER = DRIVERS_DIR / "python_client.py"

#: Scratch space for the virtualenvs and wheels the harness provisions.
#: Git-ignored; safe to delete between runs at the cost of a rebuild.
WORK_DIR = REPO_ROOT / ".bench"
VENV_DIR = WORK_DIR / "venvs"
WHEEL_DIR = WORK_DIR / "wheels"
RESULTS_DIR = BENCHMARKS_DIR / "results"

CARGO_MANIFEST = REPO_ROOT / "Cargo.toml"
BINDINGS_MANIFEST = REPO_ROOT / "crates" / "chuk-mcp-python" / "Cargo.toml"

#: Benchmarks are meaningless against a debug build.
CARGO_PROFILE_FLAG = "--release"
CARGO_TARGET_DIR = REPO_ROOT / "target" / "release"

# --- Binaries -------------------------------------------------------------

#: The server every driver connects to. Holding the server constant is what
#: makes the drivers comparable: the difference between runs is client cost.
SERVER_BINARY_NAME = "chuk-mcp-demo-server"
RUST_DRIVER_BINARY_NAME = "chuk-mcp-bench-client"

# --- Modules --------------------------------------------------------------

#: The PyO3 extension built from this repo.
BINDINGS_MODULE = "chuk_mcp_rs"
#: The historical pure-Python package, used as the baseline.
PURE_PYTHON_MODULE = "chuk_mcp"
#: The last pure-Python release of chuk-mcp. Every later release delegates to
#: the Rust core, so pinning is what keeps this a Rust-versus-Python
#: comparison rather than a comparison of Rust with itself.
PURE_PYTHON_REQUIREMENT = "chuk-mcp==0.9.4"

#: Build backend for the bindings wheel, installed into the bindings venv.
MATURIN_REQUIREMENT = "maturin>=1.7,<2.0"

# --- Workload -------------------------------------------------------------

#: The tool every driver calls. `greet` is the cheapest tool the demo server
#: exposes: the closer the handler is to free, the more of each measurement is
#: protocol and transport cost, which is what is under test.
WORKLOAD_TOOL = "greet"
WORKLOAD_ARGUMENT_NAME = "name"
WORKLOAD_ARGUMENT_VALUE = "bench"

DEFAULT_ITERATIONS = 1000
#: Discarded calls, run before timing starts. Covers interpreter warm-up,
#: connection pool priming and first-call allocation in every driver.
DEFAULT_WARMUP = 100

# --- Driver contract ------------------------------------------------------


class Flag:
    """Command-line flags every driver accepts. See `benchmarks/README.md`."""

    SERVER_COMMAND = "--server-command"
    SERVER_ARG = "--server-arg"
    ITERATIONS = "--iterations"
    WARMUP = "--warmup"
    TOOL = "--tool"
    ARGUMENT_NAME = "--argument-name"
    ARGUMENT_VALUE = "--argument-value"
    MODULE = "--module"


class ResultKey:
    """Keys in the JSON object a driver prints on stdout."""

    RUNNER = "runner"
    HANDSHAKE_SECONDS = "handshake_seconds"
    CALL_SECONDS = "call_seconds"


# --- Reporting ------------------------------------------------------------

MICROSECONDS_PER_SECOND = 1_000_000
MILLISECONDS_PER_SECOND = 1_000

#: Latency percentiles reported for every runner.
REPORTED_PERCENTILES = (50, 95, 99)
