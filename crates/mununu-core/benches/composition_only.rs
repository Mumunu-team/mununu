//! Isolated benches for composition (`mununu_core::composition::compose`).
//!
//! Loads pre-built fixtures from the cache so the composition itself is
//! the only measurement. Anchors EXP-0001 baseline and the planned
//! EXP-0010 (FxHashMap drop-in) and EXP-0016 (parallel BFS frontier).

use mununu_core::bench_support as common;

use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use mununu_core::composition::{CompositionOptions, CompositionSemantics, compose};
use std::hint::black_box;

fn bench_chain_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("composition_only/chain_sync");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);
    let chain_a = common::fixtures::chain_1k();
    let chain_b = common::fixtures::ring_1k();
    group.bench_function(BenchmarkId::from_parameter("chain1k_x_ring1k"), |b| {
        b.iter(|| {
            let result = compose(
                &chain_a,
                &chain_b,
                &CompositionOptions::new(CompositionSemantics::Synchronous),
            );
            let _ = black_box(result);
        });
    });
    group.finish();
}

fn bench_grid_async(c: &mut Criterion) {
    let mut group = c.benchmark_group("composition_only/grid_async");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(30);
    let grid_a = common::fixtures::grid_32x32();
    let grid_b = common::fixtures::grid_32x32();
    group.bench_function(BenchmarkId::from_parameter("grid32_x_grid32"), |b| {
        b.iter(|| {
            let result = compose(
                &grid_a,
                &grid_b,
                &CompositionOptions::new(CompositionSemantics::Asynchronous),
            );
            let _ = black_box(result);
        });
    });
    group.finish();
}

fn bench_modes_compare(c: &mut Criterion) {
    let mut group = c.benchmark_group("composition_only/mode_compare");
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(30);
    let small = common::fixtures::chain_1k();
    let other = common::fixtures::ring_1k();
    for (label, sem) in [
        ("sync", CompositionSemantics::Synchronous),
        ("async", CompositionSemantics::Asynchronous),
        ("superset", CompositionSemantics::Superset),
    ] {
        group.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| {
                let r = compose(&small, &other, &CompositionOptions::new(sem));
                let _ = black_box(r);
            });
        });
    }
    group.finish();
}

fn entry(c: &mut Criterion) {
    common::announce("composition_only");
    bench_chain_sync(c);
    bench_grid_async(c);
    bench_modes_compare(c);
}

criterion_group!(benches, entry);
criterion_main!(benches);
