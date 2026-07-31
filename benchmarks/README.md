# End-to-end benchmarks

Measures what it costs to *drive* an MCP server: three clients, one server
binary, one workload.

| | |
| --- | --- |
| `rust-native` | The `chuk-mcp` crate, release build. No Python involved. |
| `python-bindings` | `chuk_mcp_rs` — the same Rust core, called through PyO3. |
| `pure-python` | `chuk-mcp==0.9.4`, the last release before the Rust core landed. |

Every runner connects to the **same** `chuk-mcp-demo-server` binary over
stdio and calls the same tool the same number of times. Holding the server
constant is the point: what varies between rows is client cost.

For the per-message protocol costs underneath this — envelope construction,
JSON-RPC parsing, era classification — see the criterion benches in
`crates/chuk-mcp/benches/`, run with `cargo bench -p chuk-mcp`.

## Running

```bash
python3 -m benchmarks                                  # all runners
python3 -m benchmarks --iterations 5000 --warmup 500
python3 -m benchmarks --runner rust-native             # one runner
python3 -m benchmarks --json benchmarks/results/latest.json
```

The harness provisions what it needs on first run: `cargo build --release`
for the two Rust binaries, and a virtualenv per Python runner under `.bench/`
(git-ignored) — one with a wheel built from this working tree, one with the
pinned release from PyPI. Later runs reuse both.

Requires [`uv`](https://docs.astral.sh/uv/) for the Python runners. Without
it those runners are skipped with a message and the Rust row still prints.

## Results

Apple M2 Pro, macOS 26.5.2, rustc 1.97.1, CPython 3.11.11 — 1000 calls to
`greet` after 100 warm-up calls:

| Client | Handshake | Mean/call | Calls/sec | vs slowest | p50 | p95 | p99 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| rust-native | 4.7 ms | 55.2 µs | 18,107 | 9.1× | 57.0 µs | 72.6 µs | 85.9 µs |
| python-bindings | 12.7 ms | 86.3 µs | 11,585 | 5.8× | 78.6 µs | 128.3 µs | 158.7 µs |
| pure-python | 17.9 ms | 500.6 µs | 1,998 | 1.0× | 492.6 µs | 552.5 µs | 648.4 µs |

Reading it: a Python caller that switches to the Rust-backed package gets
roughly **5.8× more tool calls per second** without changing a line of code.
Dropping Python entirely buys another 1.6×, which is the PyO3 boundary and
the event loop — the protocol work is already identical.

Handshake covers spawning the server *and* the protocol exchange, so it is
dominated by process start-up; treat it as a rough figure, not a protocol
measurement.

## Scope

Legacy era (`initialize` handshake) only, because the demo server is a legacy
server. The `2026-07-28` client path has no server here to talk to yet; its
per-request cost is covered by the `envelope` group in the criterion benches.

## Adding a client

Runners live in `runners/`. A runner prepares its own environment and returns
a command line; the orchestrator handles timing, statistics and reporting, so
no runner can measure itself more favourably than its neighbours.

Every driver — `drivers/python_client.py` and the Rust
`chuk-mcp-bench-client` binary — obeys one contract:

**In**, as flags: `--server-command`, `--server-arg` (repeatable),
`--iterations`, `--warmup`, `--tool`, `--argument-name`, `--argument-value`.
The Python driver also takes `--module`.

**Out**, one JSON object on stdout:

```json
{
  "runner": "rust-native",
  "handshake_seconds": 0.0097,
  "call_seconds": [0.0000485, 0.0000512, "..."]
}
```

Raw per-call timings, not summaries: every runner's numbers then go through
the same statistics code in `summaries.py`, so a difference in the table is a
difference in the client and never a difference in how it was measured.
Anything on stderr is diagnostic. A non-zero exit skips the runner.
