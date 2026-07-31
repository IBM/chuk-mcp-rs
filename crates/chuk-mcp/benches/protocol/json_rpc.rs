//! Parsing and serialising JSON-RPC frames — the cost paid on every message
//! in either direction, in either era.

use criterion::{black_box, Criterion};

use chuk_mcp::protocol::json_rpc::{create_request, parse_message_str, JsonRpcMessage, RequestId};

use crate::fixtures;

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_rpc");

    group.bench_function("parse_request", |b| {
        b.iter(|| parse_message_str(black_box(fixtures::TOOLS_CALL_REQUEST)).unwrap())
    });

    group.bench_function("parse_response", |b| {
        b.iter(|| parse_message_str(black_box(fixtures::TOOLS_CALL_RESPONSE)).unwrap())
    });

    group.bench_function("parse_batch", |b| {
        b.iter(|| parse_message_str(black_box(fixtures::LIST_BATCH)).unwrap())
    });

    let request = JsonRpcMessage::Request(create_request(
        fixtures::CALL_METHOD,
        Some(fixtures::tools_call_params()),
        Some(RequestId::Str(fixtures::REQUEST_ID.to_string())),
        None,
    ));
    group.bench_function("serialize_request", |b| {
        b.iter(|| black_box(&request).to_json())
    });

    group.finish();
}
