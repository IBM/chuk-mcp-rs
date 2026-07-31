//! Era classification. A dual-era HTTP client classifies every response until
//! the endpoint's era is cached, so this runs per request during the window
//! that matters most — the first requests to a new endpoint.

use criterion::{black_box, Criterion};

use chuk_mcp::protocol::era::{classify_http_response, ProtocolEra};
use chuk_mcp::protocol::versioning;

use crate::fixtures::{self, status};

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("era");

    // A success needs no body inspection at all.
    group.bench_function("classify_success", |b| {
        b.iter(|| classify_http_response(black_box(status::OK), black_box("{}")))
    });

    // A 400 is the expensive verdict: the body has to be parsed and its error
    // code checked before the peer can be called modern.
    let error_body = fixtures::unsupported_version_error_body();
    group.bench_function("classify_protocol_error", |b| {
        b.iter(|| classify_http_response(black_box(status::BAD_REQUEST), black_box(&error_body)))
    });

    // A non-JSON 404 short-circuits to legacy on the failed parse.
    group.bench_function("classify_not_found", |b| {
        b.iter(|| {
            classify_http_response(
                black_box(status::NOT_FOUND),
                black_box(fixtures::NOT_FOUND_BODY),
            )
        })
    });

    group.bench_function("era_from_version", |b| {
        b.iter(|| ProtocolEra::from_protocol_version(black_box(versioning::FIRST_MODERN_VERSION)))
    });

    group.finish();
}
