//! R46-6a — end-to-end COI constraint/fairness-pullback soundness.
//!
//! The per-cluster verification path (R.4.6) slices a BTOR2 design to one
//! cluster's cone of influence before bit-blasting, and reports the
//! sliced model's verdict as a SOUND full-mu-calculus verdict for the
//! cluster's properties (`cone_slice`'s "exact / bisimilar" guarantee).
//!
//! That guarantee holds only if the cone is closed over BOTH data-flow
//! AND `constraint` / `fair` / `justice` co-occurrence. A `constraint`
//! mentioning an in-cone signal restricts the reachable state space (the
//! joint bit-blaster enforces it via `constraints_hold`); dropping it
//! removes an assumption and turns the slice into an unsound
//! over-approximation — a *spurious counterexample* for safety.
//!
//! This test verifies the end of the pipeline the unit tests in
//! `adapter::btor2::{bit_blast, dep_graph}` only check structurally:
//! the **verdict** the verification engine produces on the cone-restricted
//! model agrees with the verdict on the full joint model.
//!
//! Before the constraint-pullback fix, the cone-restricted slice of the
//! witness below dropped `reg_b` and the `reg_a == reg_b` constraint,
//! leaving `reg_a` free. The joint design is SAFE (the constraint pins
//! `reg_a` so the bad state is unreachable); the broken slice was UNSAFE
//! (a manufactured counterexample). This test asserts the two verdicts
//! agree — it fails on the pre-fix over-approximating slice.

use mununu_core::adapter::btor2::Btor2Adapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

/// A 1-bit `reg_a` observed by the property, coupled by
/// `constraint (reg_a == reg_b)` to a 1-bit `reg_b` that no `next`
/// `reg_a` depends on (so `reg_b` is out-of-cone by data-flow alone).
/// `reg_b` is held at 0, so the constraint pins `reg_a == 0`: the bad
/// state (`reg_a == 1`) is unreachable and the design is SAFE.
const WITNESS: &str = r#"
1 sort bitvec 1
2 zero 1
3 state 1 reg_a
4 init 1 3 2
5 input 1 tgl
6 next 1 3 5
7 state 1 reg_b
8 init 1 7 2
9 next 1 7 2
10 eq 1 3 7
11 constraint 10
12 bad 3
"#;

/// Parse + realize a bit-blasted CTXDSL document and return the fraction
/// of initial states satisfying its sole auto-emitted safety property
/// (`1.0` = safe / property holds, `0.0` = a counterexample exists).
fn safety_fraction(ctxdsl: &str) -> f64 {
    const AUTOMATON: &str = "Circuit";
    // The BTOR2 bit-blaster names the property of the first `bad` line
    // `safety_bad_0` (= `nu X. (!bad && [] X)`, i.e. AG !bad).
    const FORMULA: &str = "safety_bad_0";

    let doc =
        context_dsl::parse(ctxdsl).unwrap_or_else(|e| panic!("parse failed: {e}\n\n{ctxdsl}"));
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("realize failed: {e}"));
    let clts = realized
        .context
        .clts(AUTOMATON)
        .unwrap_or_else(|| panic!("automaton '{AUTOMATON}' not found"));
    let formula = realized
        .formulas
        .get(FORMULA)
        .unwrap_or_else(|| panic!("formula '{FORMULA}' not found in:\n{ctxdsl}"));
    let env = realized.environment_for(AUTOMATON);
    let result = realized
        .context
        .evaluate_mu(AUTOMATON, &formula.formula, &env, None)
        .unwrap_or_else(|e| panic!("eval failed: {e}"));

    let initials = clts.initial_states();
    if initials.is_empty() {
        return 0.0;
    }
    let satisfied = initials.iter().filter(|s| result[s.index()]).count();
    satisfied as f64 / initials.len() as f64
}

#[test]
fn cone_restricted_verdict_agrees_with_joint_under_constraint_coupling() {
    let joint = Btor2Adapter::translate(WITNESS, &AdapterOptions::default())
        .expect("joint translate succeeds");

    let sliced_opts = AdapterOptions {
        cone_restrict_atoms: Some(vec!["reg_a".to_string()]),
        ..Default::default()
    };
    let sliced =
        Btor2Adapter::translate(WITNESS, &sliced_opts).expect("cone-restricted translate succeeds");

    let joint_verdict = safety_fraction(&joint.ctxdsl);
    let sliced_verdict = safety_fraction(&sliced.ctxdsl);

    // Ground truth: the joint design is SAFE — the constraint pins
    // `reg_a == reg_b == 0`, so the bad state (`reg_a == 1`) is
    // unreachable and AG !bad holds at the initial state.
    assert_eq!(
        joint_verdict, 1.0,
        "joint design must be SAFE (constraint makes the bad state unreachable)"
    );

    // The soundness property: the cone-restricted slice must report the
    // SAME verdict. Pre-fix, the slice dropped the `reg_a == reg_b`
    // constraint (and `reg_b`), freeing `reg_a` and yielding a spurious
    // counterexample (`sliced_verdict == 0.0`) — an unsound
    // over-approximation reported as an exact full-mu-calculus verdict.
    assert_eq!(
        sliced_verdict, joint_verdict,
        "cone-restricted slice verdict must equal the joint verdict; a mismatch \
         means the slice dropped a coupling constraint and over-approximated"
    );
}
