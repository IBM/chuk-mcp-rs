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
  2026-07-28  client    10 rules
  2026-07-28  server    5 rules
  2026-07-28  protocol  3 rules
  both        protocol  8 rules
```

Every era-and-subject pair the implementation covers has rules. An area with
none shows as an absent row, which is where an unimplemented one belongs —
not hidden behind rules that were never written.

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

Blocking today: `initialize` and `tools_call` for `2025-06-18` and
`2025-11-25`, plus `elicitation-sep1034-client-defaults` and `sse-retry` at
`2025-11-25`. **Every client scenario the suite offers at a version we support
now passes**, so the known-gaps list is empty; the script still prints the
section, so a new entry is visible the moment one appears.

Server-side reference scenarios run against `chuk-mcp-conformance-server` over
the HTTP serving mode, and **all 39 checks across 30 scenarios pass** — so the
stage blocks, and anything that stops passing is a regression rather than news.
That covers logging, completion, subscriptions, URI templates, binary
resources, every content type, and the three server-initiated exchanges
(progress, sampling and elicitation).

The fixtures each scenario expects live in
`crates/chuk-mcp/src/bin/conformance_server/`, one module per kind. A scenario
that fails with `Unknown tool`/`Unknown prompt`/`Unknown resource` is missing
its fixture, not a protocol feature — worth checking before concluding
anything deeper is wrong.

The upstream draft (`2026-07-28`) client scenarios are auth-only, which is why
the modern era is covered in-repo rather than upstream.

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
| Parse a `tools/call` request | 1.76 µs |
| Parse a `tools/call` response | 3.47 µs |
| Serialize a request | 1.24 µs |
| Build a modern envelope | 1.27 µs |
| Build and promote parameters | 2.11 µs |
| Encode a header value (plain / Base64) | 31.6 ns / 177 ns |
| Negotiate a protocol version | 38.7 ns |
| Classify an HTTP response as modern | 1.2 ns |
| Decode a tool result | 1.66 µs |
| Decode a 32-tool catalogue | 43.8 µs |

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
