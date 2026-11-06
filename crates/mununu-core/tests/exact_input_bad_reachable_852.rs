//! mununu#852 — deterministic reproducer for a false `unreachable` in the exact full-state ROBDD
//! engine when a `bad` node reads a primary input whose witnessing value differs between the
//! transition cycle and the `bad` cycle. The N0 random-BTOR2 differential tier
//! (`tests/engine/btor2_differential.rs`) detected it; that tier has no committed proptest seed, so
//! this pins the proptest-shrunk minimal case as the regression.
//!
//! It is its own top-level integration test (target `exact_input_bad_reachable_852`), deliberately
//! NOT inside `symbolic_bitblast.rs` where the fix lives, so `make verify-repro` builds the parent
//! engine WITHOUT the fix and still compiles and runs this test — the test fails there and passes
//! on the fix, which is what the Reproducers-Before-Fixes rule requires.
//!
//! It is a direct seam call to the pure `exact_bad_reachable`, not an engine-portfolio e2e.

use mununu_core::adapter::btor2::symbolic_bitblast::exact_bad_reachable;

/// `bad = i0[0] & r0[0]`. `r0` inits to 2 (bit0 = 0); `next r0 = not(i0)`, so `r0` at depth 1 is
/// any 2-bit value. Choosing the depth-0 input to make `r0[0]=1` and the depth-1 input with
/// `i0[0]=1` fires `bad` at depth 1 — native BMC, SPACER, interpolation and btormc all agree. The
/// exact engine used to tie the input `bad` reads at its cycle to the input that drove the step
/// into that state (the `EF(bad)` pre-image quantifies one shared `i`) and reported it unreachable.
#[test]
fn exact_bad_reachable_input_read_by_bad_is_independent_of_the_transition_input() {
    const INPUT_TIED: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 r0
4 input 1 i0
5 constd 1 2
6 init 1 3 5
7 slice 2 3 0 0
8 constd 1 1
9 ite 1 7 8 4
10 not 1 9
11 next 1 3 10
12 and 1 4 3
13 slice 2 12 0 0
14 constd 1 1
15 not 1 14
16 ite 1 13 4 15
17 slice 2 16 0 0
18 bad 17
";
    assert_eq!(
        exact_bad_reachable(INPUT_TIED),
        Ok(true),
        "bad is reachable at depth 1 (i0[0]=1 and r0[0]=1); the exact engine must not tie the \
         input that `bad` reads to the input that drove the transition and report it unreachable"
    );
}
