//! V.6 — R.6.7 controllability-aware KMTS proof-of-fire integration test.
//!
//! Demonstrates the verdict-divergence pattern that the R.6.3/4/5/6
//! controllability-aware evaluator track is designed to produce, on
//! a real-RTL-derived KMTS. The fixture is a hand-authored AMBA-style
//! arbiter (`examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2`,
//! corresponding to `amba_arbiter.sv` in the same directory).
//!
//! Per the R.6.7 fixture-path analysis (2026-06-09), the original
//! R.6 plan §2 primary candidate — public AMBA AHB from the
//! SYNTCOMP/TLSF corpus — required infrastructure mununu doesn't
//! have (TLSF → BTOR2 extraction). This test is **Option B**: a
//! hand-authored Verilog/BTOR2 fixture that exercises the R.6.6
//! controllability-aware lifter + R.6.3 modality-aware evaluator
//! end-to-end on RTL semantics (a real burst counter, real
//! predicate-induced MayOnly edges, real env/ctrl input split).
//!
//! What this test demonstrates (V.6 done-criterion):
//!
//! 1. R.2.5 + R.6.6 pipeline reach: the AMBA-arbiter BTOR2 lifts to
//!    a KMTS via `predicate_cube_lift` with the R.6.6
//!    controllability-aware dual-label emission (env_cN +
//!    ctrl_cN labels with Uncontrollable + Controllable tags).
//! 2. Predicate-abstraction-induced MayOnly edges fire on the
//!    `{burst==0}` predicate set (the {¬burst==0} cube has
//!    non-deterministic successors under the abstraction since
//!    burst ∈ {1, 2, 3} all collapse to one abstract state).
//! 3. The lifted CLTS carries BOTH controllable labels AND MayOnly
//!    edges from the same source — the model R.6.3/4/5/6 evaluators
//!    are designed to consume.
//!
//! What this test does NOT demonstrate (deferred):
//!
//! - End-to-end verdict-divergence between pre-R.6.3 modality-blind
//!   and post-R.6.3 modality-aware evaluation. The R.6.3 wire-in
//!   replaced the production verdict path, so the pre-R.6.3 path
//!   is no longer reachable from the current `evaluate_tri`. The
//!   divergence is demonstrated on synthetic fixtures by the
//!   `r6_3_evaluate_tri_mayonly_diamond_is_unknown_at_source` unit
//!   test in `mu_calculus/evaluator.rs`.
//! - CLI invocation via `mununu btor2 cegar --controllable-input`.
//!   The CLI flag for `controllable_inputs` doesn't yet exist on
//!   the BTOR2 subcommand (R.6.6 lifter reads it from
//!   `AdapterOptions::controllable_inputs` which today is only
//!   populated by sidecar resolvers for SV/AIGER/XState).
//!   A CLI flag is a strict-additive follow-up.

use mununu_core::adapter::AdapterOptions;
use mununu_core::adapter::btor2::kmts_lift::{
    MustEdgeInference, PredicateCubeLiftOptions, PredicateSpec, predicate_cube_lift,
};
use mununu_core::clts::{LabelControllability, TransitionModality};

const AMBA_ARBITER_BTOR2: &str =
    include_str!("../../../examples/verify/v6_controllability_kmts/source/amba_arbiter.btor2");

/// V.6 sub-test: the controllability-aware lifter produces a CLTS
/// with the env/ctrl dual-label dispatch from R.6.6.
#[test]
fn v6_amba_arbiter_lifts_with_controllability_aware_dual_labels() {
    // R.6.6 controllability split: `ctrl_g0` + `ctrl_g1` are the
    // controller inputs (named with the `ctrl_` convention so the
    // sidecar list is unambiguous).
    let adapter_options = AdapterOptions {
        controllable_inputs: vec!["ctrl_g0".into(), "ctrl_g1".into()],
        ..Default::default()
    };

    // Predicate set: a single predicate `burst==0` on the burst
    // counter. With this predicate, the abstraction collapses
    // burst ∈ {1, 2, 3} into one cube whose successors are
    // non-deterministic under the abstraction (the {¬burst==0}
    // cube's transitions are predicate-image-undetermined ⇒ MayOnly).
    let predicates = vec![PredicateSpec {
        name: "burst_zero".into(),
        register: "burst".into(),
        value: 0,
    }];

    let lift_opts = PredicateCubeLiftOptions {
        max_cube_count: 1024,
        // 4 boolean inputs (req_0, req_1, ctrl_g0, ctrl_g1) ⇒
        // 16 input combos enumerated. R.6.6 partitions these into
        // 2 env inputs × 2 ctrl inputs = 4 env-combos × 4
        // ctrl-combos.
        max_input_bits: 8,
        must_edge_inference: MustEdgeInference::Off,
        may_edge_inference: Default::default(),
        config_values: std::collections::HashMap::new(),
        compound_exprs: std::collections::HashMap::new(),
        derived_predicates: Vec::new(),
        may_postimage: false,
        reachable_only: false,
    };

    let result = predicate_cube_lift(predicates, AMBA_ARBITER_BTOR2, &adapter_options, &lift_opts)
        .expect("V.6 lift: AMBA arbiter BTOR2 must parse + lift cleanly");

    // The R.6.6 dispatch emits env_cN + ctrl_cN labels when
    // controllable_inputs is non-empty. The counter fixture's
    // analogous test verifies the same shape on `ctrl_c0` /
    // `ctrl_c1`; here we have 4 env-combos + 4 ctrl-combos.
    let alphabet = result.clts.alphabet();
    let env_labels: Vec<&String> = alphabet.iter().filter(|l| l.starts_with("env_c")).collect();
    let ctrl_labels: Vec<&String> = alphabet
        .iter()
        .filter(|l| l.starts_with("ctrl_c"))
        .collect();

    assert_eq!(
        env_labels.len(),
        4,
        "V.6: expected 4 env-combo labels (2 uncontrollable inputs ⇒ 2^2 combos); got {env_labels:?}"
    );
    assert_eq!(
        ctrl_labels.len(),
        4,
        "V.6: expected 4 ctrl-combo labels (2 controllable inputs ⇒ 2^2 combos); got {ctrl_labels:?}"
    );

    // Controllability tags: every env_c* label must land in the
    // Uncontrollable alphabet; every ctrl_c* must be Controllable.
    for &label_id in result.clts.uncontrollable_alphabet() {
        if let Some(payload) = result.clts.label_payload(label_id)
            && let Some(name) = payload.first()
        {
            assert!(
                name.starts_with("env_c") || name == "step",
                "V.6: Uncontrollable alphabet must contain only env_c* or legacy step; got {name:?}"
            );
        }
    }
    for &label_id in result.clts.controllable_alphabet() {
        if let Some(payload) = result.clts.label_payload(label_id)
            && let Some(name) = payload.first()
        {
            assert!(
                name.starts_with("ctrl_c") || name == "step",
                "V.6: Controllable alphabet must contain only ctrl_c* or legacy step; got {name:?}"
            );
        }
    }
}

/// V.6 sub-test: the lifted CLTS contains MayOnly transitions
/// emitted by predicate_cube_lift's R.2.5 may-edge enumeration.
/// This is the load-bearing R.6.6 done-criterion: a CLTS that
/// carries BOTH controllable labels AND MayOnly edges from the
/// same source.
#[test]
fn v6_amba_arbiter_lifts_with_mayonly_transitions_present() {
    let adapter_options = AdapterOptions {
        controllable_inputs: vec!["ctrl_g0".into(), "ctrl_g1".into()],
        ..Default::default()
    };

    let predicates = vec![PredicateSpec {
        name: "burst_zero".into(),
        register: "burst".into(),
        value: 0,
    }];
    let lift_opts = PredicateCubeLiftOptions {
        max_cube_count: 1024,
        max_input_bits: 8,
        must_edge_inference: MustEdgeInference::Off,
        may_edge_inference: Default::default(),
        config_values: std::collections::HashMap::new(),
        compound_exprs: std::collections::HashMap::new(),
        derived_predicates: Vec::new(),
        may_postimage: false,
        reachable_only: false,
    };

    let result = predicate_cube_lift(predicates, AMBA_ARBITER_BTOR2, &adapter_options, &lift_opts)
        .expect("V.6 lift: must succeed");

    // Count MayOnly + Sharp transitions across all states.
    let mut mayonly = 0usize;
    let mut sharp = 0usize;
    for state in result.clts.states() {
        for trans in result.clts.outgoing(state) {
            match trans.modality() {
                TransitionModality::MayOnly => mayonly += 1,
                TransitionModality::Sharp => sharp += 1,
                TransitionModality::MustHyperOnly(_) => {} // none expected
            }
        }
    }

    assert!(
        mayonly > 0,
        "V.6 done-criterion: the lifted CLTS must contain MayOnly transitions \
         (from R.2.5 predicate-image sampling). Got mayonly={mayonly}, sharp={sharp}. \
         Without MayOnly edges, R.6.3/4/5 evaluators reduce to the pre-R.6.3 \
         path and the V.6 verdict-divergence pattern cannot fire."
    );

    // Sanity: the lift produced a non-empty CLTS.
    assert!(
        result.clts.state_count() >= 2,
        "V.6: predicate set {{burst==0}} yields 2 cubes; got state_count={}",
        result.clts.state_count()
    );
}

/// V.6 sub-test: the cube count matches the predicate-set's
/// expected 2^|P| = 2 (one predicate). Sanity check that the
/// predicate-cube lifter handled the BTOR2 cleanly.
#[test]
fn v6_amba_arbiter_lift_produces_expected_cube_count() {
    let adapter_options = AdapterOptions {
        controllable_inputs: vec!["ctrl_g0".into(), "ctrl_g1".into()],
        ..Default::default()
    };

    let predicates = vec![PredicateSpec {
        name: "burst_zero".into(),
        register: "burst".into(),
        value: 0,
    }];
    let lift_opts = PredicateCubeLiftOptions::default();

    let result = predicate_cube_lift(predicates, AMBA_ARBITER_BTOR2, &adapter_options, &lift_opts)
        .expect("V.6 lift: must succeed");

    assert_eq!(
        result.cube_count, 2,
        "V.6: single predicate ⇒ 2 cubes; got cube_count={}",
        result.cube_count
    );
}

/// V.6 sub-test: controllability-aware mode preserves the R.6.6
/// post-pass gate (Sharp/MustHyperOnly promotions are skipped).
/// Confirms the R.6.6 gate fires correctly on the AMBA fixture
/// even when `MustEdgeInference::SmtPerTarget` is requested.
#[test]
fn v6_amba_arbiter_controllability_aware_skips_smt_post_pass() {
    let adapter_options = AdapterOptions {
        controllable_inputs: vec!["ctrl_g0".into(), "ctrl_g1".into()],
        ..Default::default()
    };

    let predicates = vec![PredicateSpec {
        name: "burst_zero".into(),
        register: "burst".into(),
        value: 0,
    }];
    let lift_opts = PredicateCubeLiftOptions {
        max_cube_count: 1024,
        max_input_bits: 8,
        must_edge_inference: MustEdgeInference::SmtPerTarget,
        may_edge_inference: Default::default(),
        config_values: std::collections::HashMap::new(),
        compound_exprs: std::collections::HashMap::new(),
        derived_predicates: Vec::new(),
        may_postimage: false,
        reachable_only: false,
    };

    let result = predicate_cube_lift(predicates, AMBA_ARBITER_BTOR2, &adapter_options, &lift_opts)
        .expect("V.6 lift: must succeed");

    assert_eq!(
        result.sharp_edges_promoted, 0,
        "V.6: R.6.6 gate skips SmtPerTarget promotion under controllability-aware mode; \
         got {} promotions",
        result.sharp_edges_promoted
    );
    assert_eq!(
        result.hyper_must_edges_emitted, 0,
        "V.6: R.6.6 gate skips MustHyperOnly emission under controllability-aware mode; \
         got {} hyper-must edges",
        result.hyper_must_edges_emitted
    );
}

/// V.6 sub-test: verdict-equivalence baseline. With
/// `controllable_inputs = []` (controllability-aware mode OFF),
/// the AMBA arbiter lifts to the legacy single-`step` label shape
/// — confirming the controllability-aware path is strictly
/// opt-in and the pre-R.6.6 verdict on this fixture is preserved.
#[test]
fn v6_amba_arbiter_without_controllability_preserves_legacy_lift() {
    let adapter_options = AdapterOptions::default(); // empty controllable_inputs

    let predicates = vec![PredicateSpec {
        name: "burst_zero".into(),
        register: "burst".into(),
        value: 0,
    }];
    let lift_opts = PredicateCubeLiftOptions::default();

    let result = predicate_cube_lift(predicates, AMBA_ARBITER_BTOR2, &adapter_options, &lift_opts)
        .expect("V.6 legacy lift: must succeed");

    let alphabet = result.clts.alphabet();
    assert!(
        alphabet.iter().any(|l| l == "step"),
        "V.6 legacy: with empty controllable_inputs, the lifter emits the single \
         `step` label; got: {alphabet:?}"
    );
    assert!(
        !alphabet
            .iter()
            .any(|l| l.starts_with("env_c") || l.starts_with("ctrl_c")),
        "V.6 legacy: no env_/ctrl_ labels in non-controllability-aware mode; got: {alphabet:?}"
    );
}

/// Discard the silence that `LabelControllability` imports trigger
/// when the assertions above don't directly use the type (the
/// alphabet-iteration loops indirectly verify it).
#[allow(dead_code)]
fn _import_use(_: LabelControllability) {}
