//! Decoding results into typed values, and the accessors callers reach for
//! afterwards. Paid once per response, on top of the JSON-RPC parse.

use criterion::{black_box, Criterion, Throughput};

use chuk_mcp::protocol::messages::tools::{ListToolsResult, ToolResult};

use crate::fixtures;

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("results");

    let result_value = fixtures::tools_call_result_value();
    group.bench_function("decode_tool_result", |b| {
        b.iter(|| serde_json::from_value::<ToolResult>(black_box(result_value.clone())).unwrap())
    });

    let decoded: ToolResult =
        serde_json::from_value(result_value).expect("the tool result fixture must decode");
    group.bench_function("tool_result_value", |b| {
        b.iter(|| black_box(&decoded).value())
    });
    group.bench_function("tool_result_text", |b| {
        b.iter(|| black_box(&decoded).text())
    });

    // Reported per tool, so the figure stays comparable if CATALOGUE_SIZE
    // ever changes.
    let catalogue = fixtures::tools_list_result();
    group.throughput(Throughput::Elements(fixtures::CATALOGUE_SIZE as u64));
    group.bench_function("decode_tools_list", |b| {
        b.iter(|| serde_json::from_value::<ListToolsResult>(black_box(catalogue.clone())).unwrap())
    });

    group.finish();
}
