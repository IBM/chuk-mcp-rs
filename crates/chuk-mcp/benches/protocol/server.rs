//! The server-side per-request costs.
//!
//! Everything here is paid by an ordinary request on an ordinary server, so
//! these are the figures that multiply by request count on the serving side —
//! the mirror of what the client-side groups measure. Nothing here starts a
//! runtime: dispatch itself is async and belongs in the end-to-end harness.

use criterion::{black_box, Criterion};

use chuk_mcp::server::completion::CompletionRequest;
use chuk_mcp::server::http::origin::{check, AllowedHosts};
use chuk_mcp::server::http::sse;
use chuk_mcp::server::logging::LogLevel;
use chuk_mcp::server::resources::UriTemplate;
use chuk_mcp::server::tools::content;

use crate::fixtures;

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("server");

    // Shaping a handler's return value. Paid once per `tools/call`, and the
    // three arms cost quite differently — the rendering one has to serialize.
    let text = fixtures::tool_handler_text();
    group.bench_function("result_from_text", |b| {
        b.iter(|| content::call_result(black_box(text.clone())))
    });

    let block = fixtures::tool_handler_content_block();
    group.bench_function("result_from_content_block", |b| {
        b.iter(|| content::call_result(black_box(block.clone())))
    });

    let data = fixtures::tool_handler_data();
    group.bench_function("result_from_data", |b| {
        b.iter(|| content::call_result(black_box(data.clone())))
    });

    group.bench_function("result_from_failure", |b| {
        b.iter(|| content::error_result(black_box(fixtures::TOOL_FAILURE)))
    });

    // Host validation, paid by every HTTP request before anything else looks
    // at it — so its cost is a floor under the whole serving path.
    let allowed = AllowedHosts::default();
    let loopback = fixtures::loopback_headers();
    group.bench_function("check_host_allowed", |b| {
        b.iter(|| check(black_box(&loopback), black_box(&allowed)).is_ok())
    });

    let hostile = fixtures::rebinding_headers();
    group.bench_function("check_host_refused", |b| {
        b.iter(|| check(black_box(&hostile), black_box(&allowed)).is_err())
    });

    // Framing one message as an event. Paid per notification, per server
    // request, and once more for the result of every streamed call.
    let message = fixtures::progress_notification();
    group.bench_function("frame_sse_event", |b| {
        b.iter(|| sse::event(black_box(&message), None))
    });

    // Matching a templated resource URI. Paid per `resources/read` that no
    // exact registration answered, once per registered template.
    let template = UriTemplate::parse(fixtures::RESOURCE_TEMPLATE);
    group.bench_function("match_template_hit", |b| {
        b.iter(|| template.match_uri(black_box(fixtures::RESOURCE_URI_MATCHING)))
    });
    group.bench_function("match_template_miss", |b| {
        b.iter(|| template.match_uri(black_box(fixtures::RESOURCE_URI_UNMATCHING)))
    });

    // The two small parses on their own request paths.
    group.bench_function("parse_log_level", |b| {
        b.iter(|| LogLevel::parse(black_box("warning")))
    });

    let completion = fixtures::completion_params();
    group.bench_function("read_completion_request", |b| {
        b.iter(|| CompletionRequest::from_params(black_box(&completion)))
    });

    group.finish();
}
