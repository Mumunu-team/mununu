//! mununu#849 — a VIOLATED liveness / recoverability row whose cone holds a cell wider than 128
//! bits ships no path (the lasso carries a cell as `u128`, mununu#723), and the exact ladder must
//! RECORD why (`LadderOutcome::witness_miss`, the report's `witness-unavailable` note). Both
//! witness builders used to return `Ok(None)` there with only a log line, so the row reached the
//! report with neither a path nor a reason.
//!
//! Its own top-level integration test (target `exact_witness_wide_cell_849`), outside
//! `symbolic_bitblast.rs` where the fix lives, so `make verify-repro` builds the parent engine
//! without the fix; it uses only API the parent already has.

use mununu_core::adapter::btor2::symbolic_bitblast::{
    ExactSymbolicOptions, ExactVerdict, exact_ladder,
};
use mununu_core::mu_calculus::parser::parse;
use std::collections::HashSet;

/// A 2-bit FSM `st` that leaves reset (0) for the absorbing trap 3, its transition gated on a
/// 130-bit register `wide` that always equals its constant — so the wide cell is in the cone of
/// every property over `st`.
fn wide_trap_fsm() -> String {
    let bits130 = "1".to_string() + &"0".repeat(129);
    format!(
        "1 sort bitvec 1\n2 sort bitvec 2\n3 sort bitvec 130\n4 const 3 {bits130}\n\
         5 state 3 wide\n6 next 3 5 4\n7 init 3 5 4\n8 eq 1 5 4\n9 state 2 st\n\
         10 const 2 11\n11 const 2 00\n12 one 2\n13 add 2 9 12\n14 const 2 10\n\
         15 eq 1 9 14\n16 ite 2 15 11 13\n17 eq 1 9 10\n18 ite 2 17 10 16\n\
         19 ite 2 8 10 18\n20 next 2 9 19\n21 init 2 9 11\n"
    )
}

/// The ladder's outcome for `formula` on the wide-cell FSM: the verdict and the recorded miss.
fn ladder(formula: &str) -> (Result<(ExactVerdict, bool), String>, Option<String>) {
    let f = parse(formula).expect("parse");
    let out = exact_ladder(
        &wide_trap_fsm(),
        &f,
        &HashSet::new(),
        &ExactSymbolicOptions::default(),
        None,
        None,
    );
    (
        out.result.map(|(v, lasso)| (v, lasso.is_some())),
        out.witness_miss,
    )
}

mod exact_witness_wide_cell_849 {
    use super::*;

    /// `AG EF (st == 0)` is refuted (the trap 3 is absorbing); the trap-path builder cannot carry
    /// the 130-bit cell, so no path ships — and the miss names the cell and its width.
    #[test]
    fn a_refuted_ag_ef_with_a_wide_cell_in_the_cone_records_why_it_ships_no_path() {
        let (result, miss) = ladder("nu Y. ((mu X. ((st == 0) or <> X)) and [] Y)");
        assert_eq!(
            result,
            Ok((ExactVerdict::Violated, false)),
            "verdict stands, no path"
        );
        let miss = miss.expect("the omission is recorded for the report, not only logged");
        assert!(
            miss.contains("`wide`") && miss.contains("130 bits wide"),
            "the reason names the cell and its width: {miss}"
        );
    }

    /// `AF (st == 1)` is refuted (`st` goes 0 → 3 and stays); the stall-lasso builder has the
    /// same wide-cell refusal and must record it the same way.
    #[test]
    fn a_refuted_af_with_a_wide_cell_in_the_cone_records_why_it_ships_no_path() {
        let (result, miss) = ladder("mu X. ((st == 1) or [] X)");
        assert_eq!(
            result,
            Ok((ExactVerdict::Violated, false)),
            "verdict stands, no path"
        );
        let miss = miss.expect("the omission is recorded for the report, not only logged");
        assert!(
            miss.contains("`wide`") && miss.contains("130 bits wide"),
            "the reason names the cell and its width: {miss}"
        );
    }
}
