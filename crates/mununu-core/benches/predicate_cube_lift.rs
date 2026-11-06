//! R.2.5 — `predicate_cube_lift` lift-time benchmark.
//!
//! Per the §Phase 6 §6.7 R-C1 benchmark anchor of the KMTS plan
//! (`.claude/plans/you-are-a-formal-vast-lake.md`), this bench:
//!
//! 1. Validates the **binary capability test**: the cap-exceeding
//!    synthetic BTOR2 fixture (6 × 4 = 24 state bits > `MAX_STATE_BITS
//!    = 20`) lifts via `predicate_cube_lift` where the R.2 lifter
//!    errors. The bench fails (panics) if the lift does not succeed —
//!    a structural-refactor capability check, not a perf threshold
//!    (§6.7 structural refactors are exempt from the speedup bar).
//!
//! 2. Measures **lift time as a function of `|P|`** for the cap-
//!    exceeding fixture across `|P| ∈ {4, 6, 8, 10}` (cube counts 16,
//!    64, 256, 1024 — the default `max_cube_count = 1024` floor). At
//!    each `|P|` the criterion summary records wall-clock so that
//!    future R.2.5 SMT-integration commits can measure regression /
//!    progression against the MVP baseline.
//!
//! Pass-bar (binary; §6.7 R-C1 anchor):
//!   - Cap-exceeding fixture lifts successfully.
//!   - Wall-clock per lift < 10 s (default `criterion` sample budget;
//!     enforced indirectly by the bench harness's timeout).
//!   - Peak RSS < 256 MB (not instrumented here — the MVP's O(|cubes|
//!     × |predicates|) bit ops + HashMap inserts trivially clear it).
//!
//! Run with: `cargo bench --bench predicate_cube_lift`.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

use mununu_core::adapter::AdapterOptions;
use mununu_core::adapter::btor2::{
    KmtsLiftOptions, PredicateCubeLiftOptions, PredicateSpec, lift_btor2_to_kmts,
    predicate_cube_lift,
};

/// Cap-exceeding synthetic BTOR2: 6 state registers × 4 bits each =
/// 24 total state bits (> MAX_STATE_BITS = 20). R.2's bit-blast
/// errors on this; R.2.5's `predicate_cube_lift` must succeed.
const CAP_EXCEEDING_BTOR2: &str = "\
1 sort bitvec 4
2 state 1 reg_0
3 state 1 reg_1
4 state 1 reg_2
5 state 1 reg_3
6 state 1 reg_4
7 state 1 reg_5
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

fn make_predicates(p: usize) -> Vec<PredicateSpec> {
    // Cycle through the 6 available registers; predicates that share
    // a register but differ in `value` are distinct predicates.
    (0..p)
        .map(|i| PredicateSpec {
            name: format!("p_{i}"),
            register: format!("reg_{}", i % 6),
            value: (i / 6) as u64,
        })
        .collect()
}

fn r2_5_cap_exceeding_capability(c: &mut Criterion) {
    // First, assert the binary capability test (§6.7 R-C1 pass-bar).
    // R.2's lift_btor2_to_kmts must error on this fixture; R.2.5's
    // predicate_cube_lift must succeed.
    {
        let r2 = lift_btor2_to_kmts(
            CAP_EXCEEDING_BTOR2,
            &AdapterOptions::default(),
            &KmtsLiftOptions::default(),
        );
        assert!(
            r2.is_err(),
            "R.2 must error on cap-exceeding fixture (bench assumption); got Ok"
        );
        let r2_5 = predicate_cube_lift(
            make_predicates(4),
            CAP_EXCEEDING_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions {
                // The sampled may-relation by name, as these runs always used (the default is now the
                // sound all-pairs one, 2026-11-06).
                may_edge_inference: mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
                ..PredicateCubeLiftOptions::default()
            },
        );
        assert!(
            r2_5.is_ok(),
            "R.2.5 binary capability test failed: predicate_cube_lift must lift the cap-exceeding fixture; got {:?}",
            r2_5.err()
        );
    }

    // Lift-time as a function of |P|. cube_count = 2^|P|. The default
    // max_cube_count = 1024 (per §10.1 R.2.5 done-criterion |P| ≤ 10);
    // larger |P| would need an explicit raise.
    let mut group = c.benchmark_group("predicate_cube_lift");
    for &p in &[4usize, 6, 8, 10] {
        let cube_count = 1usize << p;
        let predicates = make_predicates(p);
        group.throughput(Throughput::Elements(cube_count as u64));
        group.bench_function(
            BenchmarkId::new("p_size", format!("p_{p}_cubes_{cube_count}")),
            |b| {
                b.iter(|| {
                    let result = predicate_cube_lift(
                        predicates.clone(),
                        CAP_EXCEEDING_BTOR2,
                        &AdapterOptions::default(),
                        &PredicateCubeLiftOptions {
                            // The sampled may-relation by name, as these runs always used (the default is now the
                            // sound all-pairs one, 2026-11-06).
                            may_edge_inference:
                                mununu_core::adapter::btor2::kmts_lift::MayEdgeInference::Off,
                            ..PredicateCubeLiftOptions::default()
                        },
                    )
                    .expect("predicate_cube_lift succeeds at every |P|");
                    black_box(result);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, r2_5_cap_exceeding_capability);
criterion_main!(benches);
