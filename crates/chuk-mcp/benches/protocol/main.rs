//! Micro-benchmarks for the per-message hot paths.
//!
//! Each of these runs on the critical path of a single MCP request, so they
//! are the costs that multiply by request count. Anything touching I/O belongs
//! in the end-to-end harness under `benchmarks/` instead — nothing here starts
//! a runtime, opens a socket, or spawns a process.
//!
//! Run with `cargo bench -p chuk-mcp`; add `-- <group>` for one group.

mod envelope;
mod era;
mod fixtures;
mod json_rpc;
mod results;
mod versioning;

use criterion::{criterion_group, criterion_main};

criterion_group!(
    benches,
    json_rpc::benches,
    envelope::benches,
    versioning::benches,
    era::benches,
    results::benches,
);
criterion_main!(benches);
