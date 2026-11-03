//! M.3.5 — 3-valued parity-game evaluator + failure-subgame
//! milestone (§10.3 of `.claude/plans/you-are-a-formal-vast-lake.md`,
//! the closing gate of R.5.0).
//!
//! Spec (§10.3 M.3.5): a hand-built 4-state KMTS with **one
//! `MayOnly` transition** causing one `(state, subformula)` position
//! to be `KleeneBot`, evaluated against the 3-valued μ-calculus
//! formula `νZ. (p ∧ ⟨⟩Z)`. The oracle is the hand-verified
//! failure-subgame shape.
//!
//! Formula note. §10.3 writes `νZ. (p ∧ ⟨⟩Z)`. In this fixture the
//! proposition `p` holds at every state (`p ≡ true`), so the formula
//! reduces to `νZ. ⟨⟩Z` ("there is an infinite a-path"). mununu's
//! μ-calculus encodes atomic propositions through modal guards
//! (`req_cur` / `forb_cur`), not as free propositional atoms — a bare
//! `p` evaluates to `false` everywhere (confirmed empirically). The
//! milestone's load-bearing content is the modal `MayOnly` behavior,
//! which `p ≡ true` leaves unchanged, so `νZ. ⟨⟩Z` is the faithful
//! realization.
//!
//! Done-criteria (§10.3 + §Phase 5 R.5.0):
//!   1. **Verdict equivalence** — on a Sharp-only fixture,
//!      `evaluate_3v_game` agrees with `evaluate_tri` cell-by-cell
//!      and emits no failure subgame.
//!   2. **Non-empty subgame on the MayOnly fixture** — the verdict
//!      at the offending state is `KleeneBot` (`Trit::Unknown`), the
//!      game evaluator returns a non-empty failure subgame, and the
//!      offending `MayOnly` transition is surfaced as a classifying
//!      transition (never a Sharp edge).
//!
//! Per the §10.2 milestone-blocker protocol this is a hand-built
//! KMTS *by design* — M.3.5 validates the evaluator, not an
//! extraction path, so the hand-built fixture is the spec, not a
//! rescue.

use mununu_core::clts::{
    Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability, TransitionModality,
};
use mununu_core::mu_calculus::parser::parse as parse_formula;
use mununu_core::mu_calculus::trit::Trit;
use mununu_core::mu_calculus::{
    Environment, EvaluationOptions, evaluate_3v_game_with_options, evaluate_tri_with_options,
};

/// The M.3.5 formula `νZ. (p ∧ ⟨⟩Z)` with `p ≡ true` (see module
/// note) → `νZ. ⟨⟩Z`.
const M3_5_FORMULA: &str = "nu Z. <> Z";

/// §10.3 M.3.5 fixture — 4-state KMTS, one `MayOnly` transition.
///
/// ```text
///   s0 --a--> s1      (Sharp)
///   s1 --a--> s2      (MayOnly)   <- the offending may-but-not-must edge
///   s2 --a--> s2      (Sharp self-loop, the only must-cycle)
///   s3 --a--> s3      (Sharp self-loop, isolated control)
/// ```
///
/// Under `νZ.⟨⟩Z`:
///   * `s2`, `s3`: must self-loop → `KleeneT`.
///   * `s1`: reaches the must-cycle only via the `MayOnly` edge →
///     `⟨⟩Z` has `must = False`, `may = True` → `KleeneBot`. This is
///     the offending position and `s1` owns the classifying
///     `MayOnly` transition.
///   * `s0`: its only must-successor `s1` is indefinite → `KleeneBot`.
fn build_m3_5_kmts() -> Clts<DefaultStateIdx, DefaultLabelIdx> {
    let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    builder
        .state("s0")
        .state("s1")
        .state("s2")
        .state("s3")
        .initial("s0");
    let lbl = builder.labels().intern(["a"]).expect("intern a");
    builder.set_label_controllability(lbl, LabelControllability::Uncontrollable);
    let s0 = builder.state_id_or_insert("s0").expect("s0");
    let s1 = builder.state_id_or_insert("s1").expect("s1");
    let s2 = builder.state_id_or_insert("s2").expect("s2");
    let s3 = builder.state_id_or_insert("s3").expect("s3");
    builder.transition_ids(s0, &[lbl], s1);
    builder.transition_ids_with_modality(s1, &[lbl], s2, TransitionModality::MayOnly);
    builder.transition_ids(s2, &[lbl], s2);
    builder.transition_ids(s3, &[lbl], s3);
    builder.build().expect("build")
}

/// Sharp-only sibling of the M.3.5 fixture — same shape, but the
/// `s1 → s2` edge is Sharp. Used for the verdict-equivalence clause.
fn build_m3_5_sharp_only() -> Clts<DefaultStateIdx, DefaultLabelIdx> {
    let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    builder
        .state("s0")
        .state("s1")
        .state("s2")
        .state("s3")
        .initial("s0");
    let lbl = builder.labels().intern(["a"]).expect("intern a");
    builder.set_label_controllability(lbl, LabelControllability::Uncontrollable);
    let s0 = builder.state_id_or_insert("s0").expect("s0");
    let s1 = builder.state_id_or_insert("s1").expect("s1");
    let s2 = builder.state_id_or_insert("s2").expect("s2");
    let s3 = builder.state_id_or_insert("s3").expect("s3");
    builder.transition_ids(s0, &[lbl], s1);
    builder.transition_ids(s1, &[lbl], s2);
    builder.transition_ids(s2, &[lbl], s2);
    builder.transition_ids(s3, &[lbl], s3);
    builder.build().expect("build")
}

/// Done-criterion 1 — verdict equivalence + no subgame on Sharp-only.
#[test]
fn m3_5_sharp_only_verdict_equivalence_and_no_subgame() {
    let clts = build_m3_5_sharp_only();
    let env = Environment::new(clts.state_count());
    let options = EvaluationOptions::default();
    let formula = parse_formula(M3_5_FORMULA).expect("parse");

    let trit = evaluate_tri_with_options(&formula, &clts, &env, &options).expect("evaluate_tri");
    let game =
        evaluate_3v_game_with_options(&formula, &clts, &env, &options).expect("evaluate_3v_game");

    for state in 0..clts.state_count() {
        assert_eq!(
            game.verdicts.verdict_at(state),
            trit.verdict_at(state),
            "Sharp-only verdict mismatch at state {state}"
        );
    }
    assert!(
        game.failure_subgame.is_none(),
        "Sharp-only fixture must emit no failure subgame; got {:?}",
        game.failure_subgame
    );
}

/// Done-criterion 2 — the MayOnly edge induces a `KleeneBot` verdict
/// and a non-empty failure subgame whose classifying transition is
/// the offending may-but-not-must edge.
#[test]
fn m3_5_mayonly_edge_induces_kleenebot_and_nonempty_subgame() {
    let clts = build_m3_5_kmts();
    let env = Environment::new(clts.state_count());
    let options = EvaluationOptions::default();
    let formula = parse_formula(M3_5_FORMULA).expect("parse");

    let game =
        evaluate_3v_game_with_options(&formula, &clts, &env, &options).expect("evaluate_3v_game");

    let s1 = clts.state_id("s1").expect("s1").index();
    let s2 = clts.state_id("s2").expect("s2").index();

    // s1 reaches the must-cycle (s2) only through the MayOnly edge.
    assert_eq!(
        game.verdicts.verdict_at(s1),
        Trit::Unknown,
        "s1 verdict must be KleeneBot (offending MayOnly edge)"
    );
    // s2 has a Sharp self-loop → definite True.
    assert_eq!(
        game.verdicts.verdict_at(s2),
        Trit::True,
        "s2 verdict must be definite True (Sharp must-cycle)"
    );

    let subgame = game
        .failure_subgame
        .as_ref()
        .expect("KleeneBot verdict must emit a failure subgame");
    assert!(
        !subgame.positions.is_empty(),
        "failure subgame must have indefinite positions"
    );
    assert!(
        !subgame.classifying_transitions.is_empty(),
        "the MayOnly edge must appear as a classifying transition"
    );
    // The offending state s1 (its only path to the must-cycle is the
    // MayOnly edge) must own a classifying transition. The
    // "classifying transitions are MayOnly" modality invariant is
    // unit-tested at the Game3v level by
    // `parity_game_3v_subgame::r5_0_4_4_classifying_edges_are_mayonly`.
    assert!(
        subgame
            .classifying_transitions
            .iter()
            .any(|(src, _idx, _owner)| *src == s1),
        "the offending state s1 must own a classifying transition"
    );
}
