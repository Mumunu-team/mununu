//! Integration tests for synthesis diagnostics: lasso traces, deadlock traces,
//! proof obligations, and counterexample content.
//!
//! These tests exercise `context/diagnostics.rs` logic (lasso cycle detection,
//! deadlock trace collection, proof obligation generation) through the public
//! synthesis API.

use mununu_core::context::{Context, ControllerSynthesisOptions, DiagnosticsOptions};
use mununu_core::mu_calculus::{Environment, parser};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a simple 3-state cyclic CLTS: s0 →tick→ s1 →tick→ s2 →tick→ s0.
/// With a liveness formula that requires visiting `goal` (not present), this
/// system produces a lasso.
fn cyclic_clts_no_goal() -> (Context, Environment) {
    use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
    let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    b.state("s0");
    b.state("s1");
    b.state("s2");
    b.initial("s0");
    let tick = b.labels().intern(["tick"]).unwrap();
    b.transition("s0", &[tick], "s1");
    b.transition("s1", &[tick], "s2");
    b.transition("s2", &[tick], "s0");
    let clts = b.build().unwrap();
    let n = clts.state_count();
    let ctx = Context::builder()
        .register_clts("cycle", clts)
        .finish_with_checks()
        .unwrap();
    let env = Environment::new(n); // no predicates — "goal" is always false
    (ctx, env)
}

/// Build a 2-state CLTS where s0 →a→ s1, s1 has no outgoing transitions
/// (deadlock state).
fn deadlock_clts() -> (Context, Environment) {
    use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
    let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    b.state("live");
    b.state("dead");
    b.initial("live");
    let a = b.labels().intern(["a"]).unwrap();
    b.transition("live", &[a], "dead");
    let clts = b.build().unwrap();
    let n = clts.state_count();
    let ctx = Context::builder()
        .register_clts("dl", clts)
        .finish_with_checks()
        .unwrap();
    let env = Environment::new(n);
    (ctx, env)
}

/// Build an unrealizable GR(1)-style CLTS:
///   - `idle` →request(uncontrollable, nondeterministic)→ `req` OR `sink`
///   - `req`  →grant(controllable)→ `idle`
///   - `sink` →loop(uncontrollable)→ `sink`  (environment trap)
///
/// Grant predicate: {idle}.
/// Formula: GF(Grant) = `ν NuX. (μ MuX. (Grant ∨ <> MuX)) ∧ [] NuX`.
///
/// The environment can always choose the `request → sink` outcome, trapping
/// the system in `sink` forever (never visiting Grant=idle). The formula
/// evaluates to ∅ — unrealizable.
fn gr1_unrealizable_clts() -> (Context, Environment) {
    use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability};
    let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    b.state("idle");
    b.state("req");
    b.state("sink");
    b.initial("idle");
    let grant = b.labels().intern(["grant"]).unwrap();
    let request = b.labels().intern(["request"]).unwrap();
    let looplbl = b.labels().intern(["loop"]).unwrap();
    b.set_label_controllability(grant, LabelControllability::Controllable);
    b.set_label_controllability(request, LabelControllability::Uncontrollable);
    b.set_label_controllability(looplbl, LabelControllability::Uncontrollable);
    // Nondeterministic: request from idle can go to req OR sink
    let idle_id = b.state_id_or_insert("idle").unwrap();
    let req_id = b.state_id_or_insert("req").unwrap();
    let sink_id = b.state_id_or_insert("sink").unwrap();
    b.transition_ids(idle_id, &[request], req_id);
    b.transition_ids(idle_id, &[request], sink_id);
    b.transition_ids(req_id, &[grant], idle_id);
    b.transition_ids(sink_id, &[looplbl], sink_id);
    let clts = b.build().unwrap();
    let n = clts.state_count();
    let idle = clts.state_id("idle").unwrap();

    let mut grant_pred = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    grant_pred.set(idle.index(), true); // Grant predicate: satisfied at idle

    let ctx = Context::builder()
        .register_clts("gr1", clts)
        .finish_with_checks()
        .unwrap();
    let env = Environment::new(n).with_predicate("Grant", grant_pred);
    (ctx, env)
}

// ── lasso trace tests ─────────────────────────────────────────────────────────

/// A formula that requires infinitely visiting `goal` (GF goal) is violated by
/// the cyclic CLTS (no goal state). The lasso DFS should produce a prefix + cycle.
#[test]
fn lasso_trace_produced_for_liveness_violation() -> TestResult {
    let (ctx, env) = cyclic_clts_no_goal();
    // GF(goal): ν NuX. (μ MuX. (goal || <> MuX)) && [] NuX
    // With goal = ∅ (empty predicate), no state satisfies goal, so the formula
    // evaluates to ∅. The synthesis with counterexample enabled should produce
    // a lasso trace showing the infinite path that avoids goal.
    let formula = parser::parse("nu NuX. ((mu MuX. (goal || <> MuX)) && ([] NuX))")?;

    let diag_opts = DiagnosticsOptions {
        counterexample: true,
        deadlock_traces: false,
        max_counter_traces: Some(1),
        proof_obligations: false,
    };
    let synthesis = ctx.synthesise_controller_with_options(
        "cycle",
        &formula,
        &env,
        ControllerSynthesisOptions {
            diagnostics: Some(&diag_opts),
            ..Default::default()
        },
    )?;

    assert!(
        !synthesis.realizable,
        "GF(goal) with no goal state must be unrealizable"
    );
    assert!(
        !synthesis.diagnostics.lasso_traces.is_empty(),
        "expected at least one lasso trace for liveness violation"
    );

    let lasso = &synthesis.diagnostics.lasso_traces[0];
    assert!(
        !lasso.cycle.is_empty(),
        "lasso cycle must be non-empty (infinite counterexample)"
    );
    // The cycle must contain only states from the 3-state system
    let valid_states = ["s0", "s1", "s2"];
    for state in &lasso.cycle {
        assert!(
            valid_states.contains(&state.as_str()),
            "unexpected state in lasso cycle: {state}"
        );
    }

    Ok(())
}

/// The lasso prefix may be empty (cycle starts from initial), but must not
/// contain duplicates that would indicate a bug in the DFS.
#[test]
fn lasso_cycle_has_no_duplicate_states() -> TestResult {
    let (ctx, env) = cyclic_clts_no_goal();
    let formula = parser::parse("nu NuX. ((mu MuX. (goal || <> MuX)) && ([] NuX))")?;
    let diag_opts = DiagnosticsOptions {
        counterexample: true,
        deadlock_traces: false,
        max_counter_traces: Some(1),
        proof_obligations: false,
    };
    let synthesis = ctx.synthesise_controller_with_options(
        "cycle",
        &formula,
        &env,
        ControllerSynthesisOptions {
            diagnostics: Some(&diag_opts),
            ..Default::default()
        },
    )?;

    if let Some(lasso) = synthesis.diagnostics.lasso_traces.first() {
        let mut seen = std::collections::HashSet::new();
        for state in &lasso.cycle {
            assert!(
                seen.insert(state.clone()),
                "duplicate state in lasso cycle: {state}"
            );
        }
    }

    Ok(())
}

// ── deadlock trace tests ──────────────────────────────────────────────────────

/// The deadlock CLTS has `dead` as a sink state. With deadlock detection
/// enabled, the trace live → dead must be reported.
#[test]
fn deadlock_trace_reaches_sink_state() -> TestResult {
    let (ctx, env) = deadlock_clts();
    let formula = parser::parse("true")?;

    let diag_opts = DiagnosticsOptions {
        counterexample: false,
        deadlock_traces: true,
        max_counter_traces: None,
        proof_obligations: false,
    };
    let synthesis = ctx.synthesise_controller_with_options(
        "dl",
        &formula,
        &env,
        ControllerSynthesisOptions {
            diagnostics: Some(&diag_opts),
            ..Default::default()
        },
    )?;

    assert!(synthesis.realizable);
    assert_eq!(
        synthesis.diagnostics.deadlock_traces.len(),
        1,
        "expected exactly one deadlock trace"
    );
    let trace = &synthesis.diagnostics.deadlock_traces[0];
    assert!(
        trace.last() == Some(&"dead".to_string()),
        "trace must end at the deadlock state, got: {trace:?}"
    );
    assert!(
        trace.contains(&"live".to_string()),
        "trace must pass through the live state"
    );

    Ok(())
}

/// With deadlock detection disabled, no traces are reported even when sinks exist.
#[test]
fn deadlock_trace_not_produced_when_disabled() -> TestResult {
    let (ctx, env) = deadlock_clts();
    let formula = parser::parse("true")?;

    let diag_opts = DiagnosticsOptions {
        counterexample: false,
        deadlock_traces: false, // disabled
        max_counter_traces: None,
        proof_obligations: false,
    };
    let synthesis = ctx.synthesise_controller_with_options(
        "dl",
        &formula,
        &env,
        ControllerSynthesisOptions {
            diagnostics: Some(&diag_opts),
            ..Default::default()
        },
    )?;

    assert!(synthesis.diagnostics.deadlock_traces.is_empty());

    Ok(())
}

// ── proof obligation tests ────────────────────────────────────────────────────

/// An unrealizable synthesis with proof obligations enabled must produce at
/// least one obligation naming the violating initial state.
#[test]
fn proof_obligation_names_violating_initial() -> TestResult {
    let (ctx, env) = gr1_unrealizable_clts();
    // GF(Grant): infinitely often grant
    let formula = parser::parse("nu NuX. ((mu MuX. (Grant || <> MuX)) && ([] NuX))")?;

    let diag_opts = DiagnosticsOptions {
        counterexample: true,
        deadlock_traces: false,
        max_counter_traces: Some(2),
        proof_obligations: true,
    };
    let synthesis = ctx.synthesise_controller_with_options(
        "gr1",
        &formula,
        &env,
        ControllerSynthesisOptions {
            diagnostics: Some(&diag_opts),
            ..Default::default()
        },
    )?;

    assert!(
        !synthesis.realizable,
        "GF(Grant) must be unrealizable on this system"
    );
    assert!(
        !synthesis.diagnostics.proof_obligations.is_empty(),
        "expected proof obligations for unrealizable synthesis"
    );

    // Every obligation must reference a non-empty obligation state name
    for obligation in &synthesis.diagnostics.proof_obligations {
        assert!(
            !obligation.state.is_empty(),
            "proof obligation state name must not be empty"
        );
    }

    Ok(())
}
