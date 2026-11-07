//! R.5 lazy KMTS sub-item 2.5 (2026-06-04) — eager vs lazy
//! cube-lift benchmark.
//!
//! Per the breakdown spec at
//! `.claude/plans/r-track-multi-session-breakdown-2026-05-29.md`,
//! sub-item 2.5: "Add a benchmark fixture comparing Eager vs
//! Lazy under a fixture where only a small subset of cubes is
//! reachable. Document the memory savings."
//!
//! This bench measures two distinct comparisons:
//!
//! 1. **Wall-clock per cube visit.** `LazyLift::expand_cube` on
//!    a single cube vs `predicate_cube_lift` of the full cube
//!    space (the eager path inherently visits every cube).
//!    Expectation: per-cube lazy is much faster than full-eager
//!    when cube_count is large; per-cube lazy ≤ amortized
//!    per-cube eager.
//!
//! 2. **Memory footprint (proxied by `cached_count`).** After
//!    visiting only N cubes via `LazyLift`, `cached_count() ==
//!    N`. The eager `EagerLazyLift` would have all `2^|P|`
//!    cubes materialized. This is asserted as an inline test
//!    in the bench rather than a criterion measurement —
//!    criterion is wall-clock-only.
//!
//! Pass-bar (qualitative):
//!   - Lazy per-cube wall-clock is bounded (< 100 ms per
//!     visit on the test fixture).
//!   - Memory: visiting K cubes leaves exactly K entries in
//!     the lazy cache.
//!
//! The memory savings of the truly-lazy path only fire when
//! the caller visits a strict subset of cubes — which is what
//! sub-item 2.4's `LiftStrategy::Lazy` path WILL exercise
//! once the verdict-evaluator gains lazy-handle support. For
//! now, the CEGAR-loop path materializes all cubes; this
//! bench validates the standalone memory model.
//!
//! Run with: `cargo bench --bench lazy_lift`.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use mununu_core::adapter::AdapterOptions;
use mununu_core::adapter::btor2::{
    EagerLazyLift, KmtsLiftLazy, LazyLift, PredicateCubeLiftOptions, PredicateSpec,
    predicate_cube_lift,
};

/// 6 state registers × 1 bit each — small + cheap to lift
/// repeatedly under criterion's per-iteration budget. `|P|`
/// is parameterised below so the cube count varies from 2 to
/// 64.
const SMALL_BTOR2: &str = "\
1 sort bitvec 1
2 state 1 reg_a
3 state 1 reg_b
4 state 1 reg_c
5 state 1 reg_d
6 state 1 reg_e
7 state 1 reg_f
8 zero 1
9 init 1 2 8
10 init 1 3 8
11 init 1 4 8
12 init 1 5 8
13 init 1 6 8
14 init 1 7 8
15 next 1 2 8
16 next 1 3 8
17 next 1 4 8
18 next 1 5 8
19 next 1 6 8
20 next 1 7 8
";

/// Build a predicate set of size `n_preds`, picking the first
/// N register names from {reg_a, reg_b, reg_c, reg_d, reg_e,
/// reg_f}.
fn build_predicates(n_preds: usize) -> Vec<PredicateSpec> {
    let regs = ["reg_a", "reg_b", "reg_c", "reg_d", "reg_e", "reg_f"];
    assert!(n_preds <= regs.len(), "fixture supports up to 6 predicates");
    regs.iter()
        .take(n_preds)
        .enumerate()
        .map(|(i, r)| PredicateSpec {
            name: format!("p_{i}"),
            register: r.to_string(),
            value: 0,
        })
        .collect()
}

fn bench_eager_full_lift(c: &mut Criterion) {
    let mut group = c.benchmark_group("eager_full_lift");
    for n_preds in [2usize, 4, 6].iter() {
        let preds = build_predicates(*n_preds);
        let opts = PredicateCubeLiftOptions {
            // The sampled may-relation by name, as these runs always used (the default is now the
            // sound all-pairs one, 2026-11-06).
            may_edge_inference: mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            ..PredicateCubeLiftOptions::default()
        };
        group.bench_with_input(BenchmarkId::from_parameter(n_preds), n_preds, |b, _| {
            b.iter(|| {
                let result = predicate_cube_lift(
                    preds.clone(),
                    SMALL_BTOR2,
                    &AdapterOptions::default(),
                    &opts,
                )
                .expect("eager lift succeeds");
                black_box(result.cube_count)
            });
        });
    }
    group.finish();
}

fn bench_lazy_per_cube_expand(c: &mut Criterion) {
    let mut group = c.benchmark_group("lazy_per_cube_expand");
    for n_preds in [2usize, 4, 6].iter() {
        let preds = build_predicates(*n_preds);
        let opts = PredicateCubeLiftOptions {
            // The sampled may-relation by name, as these runs always used (the default is now the
            // sound all-pairs one, 2026-11-06).
            may_edge_inference: mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            ..PredicateCubeLiftOptions::default()
        };
        group.bench_with_input(BenchmarkId::from_parameter(n_preds), n_preds, |b, _| {
            b.iter(|| {
                // Build a fresh LazyLift per iteration so
                // the per-cube cost includes the parse +
                // context build. Visit ONLY cube 0 —
                // measures the cost of touching ONE cube
                // rather than all 2^|P|.
                let mut lazy = LazyLift::from_btor2(
                    preds.clone(),
                    SMALL_BTOR2,
                    &AdapterOptions::default(),
                    &opts,
                )
                .expect("lazy lift succeeds");
                let edges = lazy.expand_cube(0);
                black_box(edges.len())
            });
        });
    }
    group.finish();
}

fn bench_lazy_cache_growth_proof(c: &mut Criterion) {
    // Not a wall-clock measurement per se — a single
    // criterion-tracked iteration that ASSERTS the cache
    // growth proportional to visited cubes. The bench harness
    // surfaces this as a measurement; failure (panic) is
    // visible in the criterion summary.
    c.bench_function("lazy_cache_growth_proof", |b| {
        let preds = build_predicates(6); // 64 cubes
        let opts = PredicateCubeLiftOptions {
            // The sampled may-relation by name, as these runs always used (the default is now the
            // sound all-pairs one, 2026-11-06).
            may_edge_inference: mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            ..PredicateCubeLiftOptions::default()
        };
        b.iter(|| {
            let mut lazy = LazyLift::from_btor2(
                preds.clone(),
                SMALL_BTOR2,
                &AdapterOptions::default(),
                &opts,
            )
            .expect("lazy lift succeeds");
            assert_eq!(lazy.cube_count(), 64, "6 predicates ⇒ 64 cubes");
            // Visit only 4 cubes.
            for cube in [0usize, 7, 31, 63] {
                let _ = lazy.expand_cube(cube);
            }
            // The load-bearing assertion: cache size == 4
            // (NOT 64). This is the truly-lazy memory savings.
            assert_eq!(
                lazy.cached_count(),
                4,
                "cache MUST contain exactly the 4 visited cubes"
            );
            black_box(lazy.cached_count())
        });
    });
}

fn bench_eager_wrapper_vs_lazy_wrapper(c: &mut Criterion) {
    // Compare EagerLazyLift::from_btor2 (full eager work) vs
    // LazyLift::from_btor2 + single expand_cube(0). When the
    // caller only needs one cube's edges, the lazy path
    // should be cheaper.
    let mut group = c.benchmark_group("wrapper_one_cube");
    let preds = build_predicates(6); // 64 cubes
    let opts = PredicateCubeLiftOptions {
        // The sampled may-relation by name, as these runs always used (the default is now the
        // sound all-pairs one, 2026-11-06).
        may_edge_inference: mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
        ..PredicateCubeLiftOptions::default()
    };
    group.bench_function("eager_full_then_walk", |b| {
        b.iter(|| {
            let mut wrapper = EagerLazyLift::from_btor2(
                preds.clone(),
                SMALL_BTOR2,
                &AdapterOptions::default(),
                &opts,
            )
            .expect("eager wrapper succeeds");
            let edges = wrapper.expand_cube(0);
            black_box(edges.len())
        });
    });
    group.bench_function("lazy_only_cube_0", |b| {
        b.iter(|| {
            let mut lazy = LazyLift::from_btor2(
                preds.clone(),
                SMALL_BTOR2,
                &AdapterOptions::default(),
                &opts,
            )
            .expect("lazy succeeds");
            let edges = lazy.expand_cube(0);
            black_box(edges.len())
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_eager_full_lift,
    bench_lazy_per_cube_expand,
    bench_lazy_cache_growth_proof,
    bench_eager_wrapper_vs_lazy_wrapper,
);
criterion_main!(benches);
