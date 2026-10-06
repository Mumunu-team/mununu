//! Benchmarks for the EXACT-SYMBOLIC engine — full-state ROBDD (OxiDD), functional next-state
//! substitution, bit-blast μ-fixpoint — on the shapes the engine-performance roadmap optimises.
//!
//! Each shape is one of the calibrated families in `scripts/profile_cases.py`, at a size that runs
//! in about a second so criterion can sample it: the iteration-bound raster, the deep twocount
//! latch, the representation-bound relational pair under the cell-major order, and a small
//! multiplier bit-blast. Wall clock is what criterion records; the host-independent numbers (the
//! engine's `work_count` and iteration count) are what a change is judged on first, and the
//! `profile_cases.py run` driver prints them for the same shapes at the calibrated sizes.
//!
//! Run: `cargo bench -p mununu-core --bench exact_symbolic -- --quick`

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use mununu_core::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
use mununu_core::mu_calculus::parser as mu_parser;

fn bits(m: u64) -> u32 {
    (m - 1).max(1).ilog2() + 1
}

/// `hcount` wraps at `h`, `vcount` advances on that wrap and wraps at `v`: ~1.3·h·v iterations.
fn raster(h: u64, v: u64) -> String {
    let (hb, vb) = (bits(h), bits(v));
    format!(
        "1 sort bitvec 1\n2 sort bitvec {hb}\n3 sort bitvec {vb}\n4 state 2 hcount\n5 state 3 vcount\n\
         6 zero 2\n7 zero 3\n8 init 2 4 6\n9 init 3 5 7\n10 constd 2 {}\n11 constd 3 {}\n\
         12 eq 1 4 10\n13 eq 1 5 11\n14 one 2\n15 one 3\n16 add 2 4 14\n17 add 3 5 15\n\
         18 ite 2 12 6 16\n19 next 2 4 18\n20 ite 3 13 7 17\n21 ite 3 12 20 5\n22 next 3 5 21\n",
        h - 1,
        v - 1
    )
}

/// Two counters wrapping at `m`, advanced alternately by a free input; `done` latches when both
/// read `m-1`.
fn twocount(m: u64) -> String {
    let w = bits(m);
    format!(
        "1 sort bitvec 1\n2 sort bitvec {w}\n3 input 1 turn\n4 zero 2\n5 state 2 a\n6 state 2 b\n\
         7 init 2 5 4\n8 init 2 6 4\n9 one 2\n10 add 2 5 9\n11 add 2 6 9\n12 constd 2 {}\n\
         13 eq 1 5 12\n14 eq 1 6 12\n15 ite 2 13 4 10\n16 ite 2 14 4 11\n17 ite 2 3 15 5\n\
         18 ite 2 -3 16 6\n19 next 2 5 17\n20 next 2 6 18\n21 state 1 done\n22 zero 1\n\
         23 init 1 21 22\n24 and 1 13 14\n25 or 1 21 24\n26 next 1 21 25\n",
        m - 1
    )
}

/// Two held `n`-bit registers with explicit `a ≠ b` inits and `done` latched on `a == b`.
fn relational(n: u32) -> String {
    format!(
        "1 sort bitvec 1\n2 sort bitvec {n}\n3 state 2 a\n4 state 2 b\n5 next 2 3 3\n6 next 2 4 4\n\
         7 eq 1 3 4\n8 state 1 done\n9 zero 1\n10 init 1 8 9\n11 or 1 8 7\n12 next 1 8 11\n\
         13 zero 2\n14 one 2\n15 init 2 3 13\n16 init 2 4 14\n"
    )
}

/// `done` latches on `a * b == k` over two free `n`-bit registers (the multiplier bit-blast).
fn mult(n: u32) -> String {
    let k = (1u64 << (2 * n - 3)) + 1;
    format!(
        "1 sort bitvec 1\n2 sort bitvec {n}\n17 sort bitvec {}\n3 state 2 a\n4 state 2 b\n\
         5 next 2 3 3\n6 next 2 4 4\n7 uext 17 3 {n}\n8 uext 17 4 {n}\n9 mul 17 7 8\n\
         10 constd 17 {k}\n11 eq 1 9 10\n12 state 1 done\n13 zero 1\n14 init 1 12 13\n\
         15 or 1 12 11\n16 next 1 12 15\n",
        2 * n
    )
}

fn ag_ef(target: &str) -> mununu_core::mu_calculus::Formula {
    mu_parser::parse(&format!("nu Y. ((mu X. (({target}) || <> X)) && [] Y)"))
        .expect("AG EF parses")
}

fn exact_symbolic(c: &mut Criterion) {
    let mut group = c.benchmark_group("exact_symbolic");
    group.sample_size(10);
    let cases: Vec<(&str, u64, String, &str)> = vec![
        ("raster", 100, raster(800, 100), "vcount == 99"),
        ("twocount", 4096, twocount(4096), "done == 1"),
        ("relational_cell_major", 8, relational(8), "done == 1"),
        ("mult", 12, mult(12), "done == 1"),
    ];
    for (name, size, src, target) in &cases {
        let formula = ag_ef(target);
        group.bench_with_input(BenchmarkId::new(*name, size), &src, |b, src| {
            b.iter(|| {
                if *name == "relational_cell_major" {
                    // SAFETY: single-threaded bench; the engine reads the order at build time.
                    unsafe { std::env::set_var("MUNUNU_BDD_VAR_ORDER", "cell-major") };
                }
                let v = exact_symbolic_verdict(src, &formula);
                if *name == "relational_cell_major" {
                    unsafe { std::env::remove_var("MUNUNU_BDD_VAR_ORDER") };
                }
                std::hint::black_box(v)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, exact_symbolic);
criterion_main!(benches);
