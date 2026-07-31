//! Version negotiation and classification. Cheap individually, but on the
//! connection path for every peer, and `is_modern_version` is consulted per
//! request by the dual-era transports.

use criterion::{black_box, Criterion};

use chuk_mcp::protocol::versioning;

pub fn benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("versioning");

    // The worst realistic case: a server offering only older revisions, so the
    // scan walks most of our supported list before matching.
    let server_versions = [versioning::V2025_06_18, versioning::V2024_11_05];
    group.bench_function("negotiate", |b| {
        b.iter(|| {
            versioning::negotiate_version(
                black_box(versioning::SUPPORTED_VERSIONS),
                black_box(&server_versions),
            )
            .unwrap()
        })
    });

    group.bench_function("is_modern", |b| {
        b.iter(|| versioning::is_modern_version(black_box(versioning::LATEST_LEGACY_VERSION)))
    });

    group.bench_function("compare", |b| {
        b.iter(|| {
            versioning::compare(
                black_box(versioning::FIRST_MODERN_VERSION),
                black_box(versioning::V2025_06_18),
            )
            .unwrap()
        })
    });

    group.finish();
}
