# Testing, conformance and benchmarks

Four things run against this repo, answering four different questions.

| | Question | Command |
| --- | --- | --- |
| Tests | Does the code work? | `cargo test --workspace` |
| Conformance | Does it obey the specification? | `./scripts/run-conformance.sh` |
| Micro-benchmarks | What does one message cost? | `cargo bench -p chuk-mcp` |
| End-to-end benchmarks | What does one tool call cost? | `python3 -m benchmarks` |

## Tests

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

CI holds every file in the core crate at **≥90% line coverage individually**
(`scripts/coverage-gate.py`), rather than a crate total that one badly covered
file could hide behind. The `bin/` targets are excluded: they are subprocess
fixtures `llvm-cov` cannot observe.

## Conformance

Two suites, run by one script.

### The in-repo rule suite

`crates/chuk-mcp/tests/conformance/` expresses specification requirements as
**data** — id, era, subject, the requirement in one line, and a check — so the
suite can report a coverage matrix rather than a pass count:

```bash
cargo test -p chuk-mcp --test conformance -- --nocapture
```

```text
RULE                                         ERA         SUBJECT   RESULT
legacy.client.initialize-first               legacy      client    pass
modern.client.header-mirrors-version         2026-07-28  client    pass
legacy.server.unknown-method                 legacy      server    pass
both.protocol.result-type-default            both        protocol  pass
...
Coverage:
  legacy      client    6 rules
  legacy      server    10 rules
  2026-07-28  client    9 rules
  both        protocol  6 rules
```

There are deliberately **no modern server rules**: this crate's server speaks
the legacy lifecycle only. An unimplemented era belongs in the matrix as an
absence, not hidden behind rules that were never written.

Rules assert against two fixtures, in `tests/conformance/harness/`:

- **`recorder`** — a transport that records what our client actually put on the
  wire. Client conformance is a question about bytes, so asserting on the
  arguments a method was called with would not answer it.
- **`server`** — an in-process `McpServer` driven one message at a time through
  `handle_message`, with no transport and no subprocess, so no timing artefact
  can be mistaken for a protocol answer.

To add a rule, add it to the relevant module under `tests/conformance/rules/`
and it appears in the matrix. Nothing else needs touching.

### The official suite

`@modelcontextprotocol/conformance` drives our client as a black box:

```bash
./scripts/run-conformance.sh          # blocking checks
./scripts/run-conformance.sh --gaps   # also re-check known gaps
```

Blocking today: `initialize` and `tools_call` for `2025-06-18` and `2025-11-25`.

Known gaps, reported but never fatal — they need client features that do not
exist yet, and the script tells you if one starts passing:

| Scenario | Needs |
| --- | --- |
| `sse-retry` | GET reconnection after a graceful SSE stream close |
| `elicitation-sep1034-client-defaults` | Handling server-initiated `elicitation/create` |

Server-side reference scenarios cannot run: the official suite drives a server
over `--url`, and this crate's server has no HTTP serving mode. The in-repo
suite covers the server's behaviour meanwhile. The upstream draft
(`2026-07-28`) client scenarios are auth-only, which is why the modern era is
covered in-repo rather than upstream.

## Micro-benchmarks

`crates/chuk-mcp/benches/protocol/` — criterion, over the costs that multiply
by request count. Nothing here starts a runtime, opens a socket or spawns a
process.

```bash
cargo bench -p chuk-mcp
cargo bench -p chuk-mcp -- envelope     # one group
```

Groups: `json_rpc` (parse/serialise), `envelope` (`_meta` + mirrored headers +
parameter promotion), `versioning`, `era` (response classification), `results`
(decoding and accessors). Fixtures live in one place —
`benches/protocol/fixtures.rs` — so a number from one group is comparable with
another's.

Apple M2 Pro, rustc 1.97.1:

| | |
| --- | --- |
| Parse a `tools/call` request | 1.77 µs |
| Parse a `tools/call` response | 3.46 µs |
| Serialize a request | 1.25 µs |
| Build a modern envelope | 1.29 µs |
| Build and promote parameters | 2.13 µs |
| Encode a header value (plain / Base64) | 31.6 ns / 176 ns |
| Negotiate a protocol version | 38.8 ns |
| Classify an HTTP response as modern | 1.2 ns |
| Decode a tool result | 1.66 µs |
| Decode a 32-tool catalogue | 43.7 µs |

CI compiles the benchmarks but does not run them — the numbers would be noise
on a shared runner.

## End-to-end benchmarks

`benchmarks/` — the same workload through three clients against one server
binary. See [benchmarks/README.md](../benchmarks/README.md) for the numbers,
the driver contract, and how to add a client.

```bash
python3 -m benchmarks
python3 -m benchmarks --runner rust-native --iterations 5000
```
