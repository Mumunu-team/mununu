//! M.4 regression — a formula's bare `Node::Predicate` atom must bind
//! to a CLTS's `state_3valued_predicates` labelling.
//!
//! Background. The BTOR2 CEGAR path (`predicate_cube_lift`) records each
//! predicate's per-state truth in `Clts::state_3valued_predicates`, keyed
//! by the predicate's display name. The 3-valued evaluator resolves a
//! formula's `Node::Predicate(name)` through `predicate_bits`, which —
//! before the Option-1 fix — consulted only the `Environment` predicate
//! map and on-demand expression evaluation, then fell through to
//! "unknown predicate ⇒ false". Cube-lifted models populate neither of
//! those, so a bare predicate atom evaluated **false everywhere**, and a
//! safety formula `νX.((¬p…) ∧ [−]X)` collapsed to `νX.[−]X` — a vacuous
//! `PROPERTY HOLDS`. That masked, e.g., the Caliptra boot-FSM CWE-1245
//! analysis: the `boot_fsm_ns ∈ {5,6,7}` predicates never bound, so the
//! unmatched-encoding reachability was never decided.
//!
//! These tests pin the fixed behaviour so a future regression is caught:
//!   1. a bare atom binds to its `KleeneT` / `KleeneF` labels;
//!   2. the safety-shape formula is no longer vacuously all-true;
//!   3. a CLTS with NO 3-valued labelling keeps the legacy
//!      "unknown atom ⇒ false" behaviour (the fix is purely additive).

use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability, Tristate};
use mununu_core::mu_calculus::parser::parse as parse_formula;
use mununu_core::mu_calculus::trit::Trit;
use mununu_core::mu_calculus::{Environment, EvaluationOptions, evaluate_3v_game_with_options};

/// Two-state CLTS labelling predicate `p` `KleeneT` at `s0` and
/// `KleeneF` at `s1` (the shape `predicate_cube_lift` produces, where a
/// cube cell definitely does or does not carry a predicate value). Both
/// states have an `a`-self/forward edge so modal formulas are
/// well-defined.
fn build_labelled_clts() -> Clts<DefaultStateIdx, DefaultLabelIdx> {
    let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    b.state("s0").state("s1").initial("s0");
    let a = b.labels().intern(["a"]).expect("intern a");
    b.set_label_controllability(a, LabelControllability::Uncontrollable);
    let s0 = b.state_id_or_insert("s0").expect("s0");
    let s1 = b.state_id_or_insert("s1").expect("s1");
    b.transition_ids(s0, &[a], s1);
    b.transition_ids(s1, &[a], s1);
    b.with_3valued_predicate(s0, "p", Tristate::KleeneT);
    b.with_3valued_predicate(s1, "p", Tristate::KleeneF);
    b.build().expect("labelled CLTS builds")
}

/// Same shape, but with NO 3-valued predicate labelling — exercises the
/// legacy "unknown atom ⇒ false" fallback (the additive-safety guard).
fn build_unlabelled_clts() -> Clts<DefaultStateIdx, DefaultLabelIdx> {
    let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    b.state("s0").state("s1").initial("s0");
    let a = b.labels().intern(["a"]).expect("intern a");
    b.set_label_controllability(a, LabelControllability::Uncontrollable);
    let s0 = b.state_id_or_insert("s0").expect("s0");
    let s1 = b.state_id_or_insert("s1").expect("s1");
    b.transition_ids(s0, &[a], s1);
    b.transition_ids(s1, &[a], s1);
    b.build().expect("unlabelled CLTS builds")
}

#[test]
fn bare_atom_binds_to_state_3valued_labels() {
    let clts = build_labelled_clts();
    let env = Environment::new(clts.state_count());
    let options = EvaluationOptions::default();
    let formula = parse_formula("p").expect("parse");

    let game =
        evaluate_3v_game_with_options(&formula, &clts, &env, &options).expect("evaluate_3v_game");
    let s0 = clts.state_id("s0").expect("s0").index();
    let s1 = clts.state_id("s1").expect("s1").index();

    // The whole point: `p` is True where labelled KleeneT, False where
    // KleeneF — NOT false everywhere (the pre-fix bug).
    assert_eq!(
        game.verdicts.verdict_at(s0),
        Trit::True,
        "predicate `p` must be True where labelled KleeneT"
    );
    assert_eq!(
        game.verdicts.verdict_at(s1),
        Trit::False,
        "predicate `p` must be False where labelled KleeneF"
    );
}

#[test]
fn safety_shape_formula_is_not_vacuously_true() {
    // The CWE-1245 shape: `νX. ((¬p) ∧ [−]X)`. At s0, `p` is KleeneT so
    // `¬p` is false, so s0 must be EXCLUDED from the greatest fixpoint.
    // Pre-fix (p false everywhere) this collapsed to `νX.[−]X` and
    // returned True at s0 too — a vacuous PROPERTY HOLDS. Pinning
    // `s0 ≠ True` is the anti-vacuous-collapse guard.
    let clts = build_labelled_clts();
    let env = Environment::new(clts.state_count());
    let options = EvaluationOptions::default();
    let formula = parse_formula("nu X. ((!p) && ([] X))").expect("parse");

    let game =
        evaluate_3v_game_with_options(&formula, &clts, &env, &options).expect("evaluate_3v_game");
    let s0 = clts.state_id("s0").expect("s0").index();

    assert_ne!(
        game.verdicts.verdict_at(s0),
        Trit::True,
        "the state where `p` definitely holds must be excluded from νX.((¬p) ∧ [−]X), \
         not vacuously satisfied"
    );
}

#[test]
fn unlabelled_clts_keeps_unknown_atom_false() {
    // Additive-safety: with no 3-valued labelling, a bare atom keeps the
    // legacy "unknown predicate ⇒ false" behaviour (sound under-approx).
    let clts = build_unlabelled_clts();
    let env = Environment::new(clts.state_count());
    let options = EvaluationOptions::default();
    let formula = parse_formula("p").expect("parse");

    let game =
        evaluate_3v_game_with_options(&formula, &clts, &env, &options).expect("evaluate_3v_game");
    for state in 0..clts.state_count() {
        assert_eq!(
            game.verdicts.verdict_at(state),
            Trit::False,
            "unknown atom over an unlabelled CLTS must be False at state {state}"
        );
    }
}
