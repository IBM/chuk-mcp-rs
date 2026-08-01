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

Apple M3 Max, macOS 15.7.4, rustc 1.95.0, CPython 3.12.2 — 1000 calls to
`greet` after 100 warm-up calls:

| Client | Handshake | Mean/call | Calls/sec | vs slowest | p50 | p95 | p99 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| rust-native | 3.7 ms | 38.6 µs | 25,907 | 11.1× | 37.5 µs | 45.2 µs | 54.2 µs |
| python-bindings | 2.9 ms | 72.4 µs | 13,818 | 5.9× | 70.6 µs | 82.0 µs | 98.2 µs |
| pure-python | 13.9 ms | 427.8 µs | 2,338 | 1.0× | 426.2 µs | 445.6 µs | 470.7 µs |

Reading it: a Python caller that switches to the Rust-backed package gets
roughly **6× more tool calls per second** without changing a line of code.
Dropping Python entirely buys another 1.9×, which is the PyO3 boundary and
the event loop — the protocol work is already identical.

Measured after MRTR landed, so these include the `input_required` check every
`tools/call` now makes on its way through the retry driver. It does not show:
the cost is one field lookup against a result already parsed.

Handshake covers spawning the server *and* the protocol exchange, so it is
dominated by process start-up; treat it as a rough figure, not a protocol
measurement. It is also the noisiest column here — a cold binary and a warm one
differ by more than the protocol work being measured.

**These figures were taken on a different machine from the previous revision**
(Apple M2 Pro, rustc 1.97.1, CPython 3.11.11), so the change against those
numbers is the hardware, not this library. The ratios between rows are the
part worth comparing across runs, and they have barely moved.

## Scope

Legacy era (`initialize` handshake) over stdio, because that is what the demo
server this harness drives speaks. The library's server now answers
`2026-07-28` and serves over Streamable HTTP as well, but measuring either
would be a different workload with a different server binary rather than
another row in this table — what varies here is deliberately only the client.

Until such a row exists, the modern per-request cost is covered by the
`envelope` group in the criterion benches, and the serving-side costs by the
`server` group.

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
