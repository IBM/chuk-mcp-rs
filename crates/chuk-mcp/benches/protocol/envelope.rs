//! Building the modern request envelope: `_meta` construction, the mirrored
//! headers, and `x-mcp-header` parameter promotion. This is the whole of what
//! `2026-07-28` adds to a request's client-side cost.

use criterion::{black_box, Criterion};

use chuk_mcp::protocol::envelope::{build_envelope, encode_header_value, ClientIdentity};
use chuk_mcp::protocol::versioning;

use crate::fixtures;

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope");

    let identity = ClientIdentity::chuk();
    let params = fixtures::tools_call_params();

    group.bench_function("build", |b| {
        b.iter(|| {
            build_envelope(
                black_box(fixtures::CALL_METHOD),
                Some(params.clone()),
                versioning::FIRST_MODERN_VERSION,
                &identity,
            )
            .unwrap()
        })
    });

    // Promotion re-scans the tool's schema for `x-mcp-header` on every call,
    // so this measures build plus scan plus encode, not promotion alone.
    let schema = fixtures::tools_call_schema();
    let arguments = fixtures::tools_call_arguments();
    group.bench_function("build_and_promote", |b| {
        b.iter(|| {
            let mut envelope = build_envelope(
                fixtures::CALL_METHOD,
                Some(params.clone()),
                versioning::FIRST_MODERN_VERSION,
                &identity,
            )
            .unwrap();
            envelope
                .promote_tool_params(black_box(&schema), black_box(&arguments))
                .unwrap();
            envelope
        })
    });

    // The two encoding paths diverge sharply: one returns the input, the other
    // allocates and Base64-encodes.
    group.bench_function("encode_header_plain", |b| {
        b.iter(|| encode_header_value(black_box(fixtures::PLAIN_HEADER_VALUE)))
    });
    group.bench_function("encode_header_base64", |b| {
        b.iter(|| encode_header_value(black_box(fixtures::ENCODED_HEADER_VALUE)))
    });

    group.finish();
}
