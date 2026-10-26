//! mununu#804 — the exact engine honours the caller's run budget (`run_budget`).
//!
//! The fixpoint polled only its own opt-in deadline (`MUNUNU_BDD_TIME_BUDGET_MS`, off by default),
//! never the budget the caller installed — the planner's per-property budget, the whole-run budget,
//! a rescue slice, the HWMCC harness's per-instance budget. `vis_arrays_bufferAlloc` therefore ran
//! four hours under a 300 s budget (one OxiDD collection of ~53 M nodes every ~50 s) and came back
//! `Unknown` without a word about the budget — the silence class of
//! `docs/policies/failure-paths.md` Rule 1.
//!
//! The reproducer: a 800 × 8000 raster whose recoverability fixpoint takes hundreds of thousands of
//! iterations with squaring off. Under a 50 ms run budget it must abstain within the budget with the
//! `Deadline` kind. On the parent commit the same run ends on the ITERATION budget (1 << 18),
//! seconds later, with a different message — so this test fails there.

use std::collections::HashMap;

use mununu_core::adapter::btor2::parser;
use mununu_core::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
use mununu_core::adapter::btor2::symbolic_bitblast::{
    AbstentionKind, BddBitBlaster, abstention_kind,
};
use mununu_core::adapter::run_budget;
use mununu_core::mu_calculus::parser as mu_parser;

#[test]
fn the_callers_run_budget_stops_the_fixpoint_with_a_deadline_abstention() {
    let (h, v) = (800u64, 8000u64);
    let src = format!(
        "1 sort bitvec 1\n2 sort bitvec 10\n3 sort bitvec 13\n4 state 2 hcount\n5 state 3 vcount\n\
         6 zero 2\n7 zero 3\n8 init 2 4 6\n9 init 3 5 7\n10 constd 2 {}\n11 constd 3 {}\n\
         12 eq 1 4 10\n13 eq 1 5 11\n14 one 2\n15 one 3\n16 add 2 4 14\n17 add 3 5 15\n\
         18 ite 2 12 6 16\n19 next 2 4 18\n20 ite 3 13 7 17\n21 ite 3 12 20 5\n22 next 3 5 21\n",
        h - 1,
        v - 1
    );
    let file = parser::parse(&src).expect("parse raster");
    let bb = BddBitBlaster::build(&file).expect("build raster");
    let p = bb
        .predicate_bdd(&PredicateExpr::Cmp {
            register: "vcount".into(),
            op: CmpOp::Eq,
            value: v - 1,
        })
        .expect("p");
    let mut atoms = HashMap::new();
    atoms.insert("p", p);
    let formula = mu_parser::parse("nu Y. ((mu X. (p || <> X)) && [] Y)").expect("parse");
    // Squaring off, so the iteration itself runs; the iteration budget is far beyond what 50 ms
    // allows, so the run budget is the only thing that can stop the fixpoint early.
    let plain = bb
        .exact_model()
        .expect("exact model")
        .with_squaring(false)
        .with_iteration_budget(1 << 18);
    let budget = run_budget::Budget::with_ms(50);
    let t0 = std::time::Instant::now();
    let refused = {
        let _g = run_budget::enter(&budget);
        plain.evaluate(&formula, &atoms).err()
    };
    let elapsed = t0.elapsed();
    assert!(
        refused.as_ref().is_some_and(|e| e.contains("RUN budget")),
        "the fixpoint must abstain on the caller's run budget: {refused:?}"
    );
    assert!(
        matches!(
            abstention_kind(refused.as_deref().unwrap_or("")),
            AbstentionKind::Deadline
        ),
        "a run-budget abstention is a Deadline: {refused:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a 50 ms budget stopped the fixpoint after {elapsed:?}"
    );
}

/// Without a budget the poll is inert: the same fixpoint runs to its iteration budget exactly as
/// before, so an unbudgeted run never sees a run-budget abstention.
#[test]
fn an_unbounded_budget_never_fires_the_poll() {
    let src = "1 sort bitvec 1\n2 sort bitvec 10\n3 state 2 cnt\n4 zero 2\n5 init 2 3 4\n\
               6 one 2\n7 add 2 3 6\n8 next 2 3 7\n9 constd 2 1023\n10 eq 1 3 9\n";
    let file = parser::parse(src).expect("parse counter");
    let bb = BddBitBlaster::build(&file).expect("build counter");
    let p = bb
        .predicate_bdd(&PredicateExpr::Cmp {
            register: "cnt".into(),
            op: CmpOp::Eq,
            value: 1023,
        })
        .expect("p");
    let mut atoms = HashMap::new();
    atoms.insert("p", p);
    let formula = mu_parser::parse("mu X. (p || <> X)").expect("parse");
    let model = bb
        .exact_model()
        .expect("exact model")
        .with_squaring(false)
        .with_iteration_budget(1 << 16);
    let decided = model.evaluate(&formula, &atoms);
    assert!(
        decided.is_ok(),
        "no budget installed ⇒ the counter's reachability decides: {:?}",
        decided.as_ref().err()
    );
}
