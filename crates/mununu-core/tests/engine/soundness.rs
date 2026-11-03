//! Soundness regression tests.
//!
//! These tests verify the abstraction contracts described in
//! `mununu-private/docs/deep_dive/05_soundness_boundaries.md`:
//!
//! - Over-approximation contract: safe(abstract) → safe(precise)
//! - Under-approximation contract: live(precise) → live(abstract)
//!
//! Each test uses a pair of CTXDSL models — one precise, one abstract —
//! and checks that the soundness invariant holds.

use mununu_core::context_dsl;

/// Helper: parse + realize + evaluate a formula, returning the fraction of
/// initial states satisfying the property (0.0 = none, 1.0 = all).
fn eval_fraction(ctxdsl: &str, automaton: &str, formula_name: &str) -> f64 {
    let doc = context_dsl::parse(ctxdsl).unwrap_or_else(|e| {
        panic!("Parse failed: {e}\n\n{ctxdsl}");
    });
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let clts = realized
        .context
        .clts(automaton)
        .unwrap_or_else(|| panic!("Automaton '{automaton}' not found"));
    let formula = realized
        .formulas
        .get(formula_name)
        .unwrap_or_else(|| panic!("Formula '{formula_name}' not found"));
    let env = realized.environment_for(automaton);
    let result = realized
        .context
        .evaluate_mu(automaton, &formula.formula, &env, None)
        .unwrap_or_else(|e| panic!("Eval failed: {e}"));

    let initial_count = clts.initial_states().len();
    if initial_count == 0 {
        return 0.0;
    }
    let satisfied_initials = clts
        .initial_states()
        .iter()
        .filter(|s| result[s.index()])
        .count();
    satisfied_initials as f64 / initial_count as f64
}

/// Helper: synthesize and return whether the property is realizable.
fn is_realizable(ctxdsl: &str, automaton: &str, formula_name: &str) -> bool {
    let doc = context_dsl::parse(ctxdsl).unwrap_or_else(|e| {
        panic!("Parse failed: {e}\n\n{ctxdsl}");
    });
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized
        .formulas
        .get(formula_name)
        .unwrap_or_else(|| panic!("Formula '{formula_name}' not found"));
    let env = realized.environment_for(automaton);
    let synth = realized
        .context
        .synthesise_controller(automaton, &formula.formula, &env, None)
        .unwrap_or_else(|e| panic!("Synthesis failed: {e}"));
    synth.realizable
}

// ---------------------------------------------------------------------------
// Test 1: Over-approximation preserves safety
// ---------------------------------------------------------------------------

/// Precise model has a guard; abstract model removes it (over-approx).
/// Safety holds on both — over-approx never hides real violations.
#[test]
fn overapprox_preserves_safety() {
    // Precise: Ready --[work]--> Done, Error is unreachable
    let precise = r#"
context test {
    automata {
        automaton M {
            controllable { label work; }
            states {
                state Ready initial;
                state Done;
                state Error;
            }
            transitions {
                transition Ready -> Done on label work;
                transition Done -> Ready on label reset;
            }
        }
    }
    mu_formulas {
        formula safety { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safety; }
    }
}
"#;

    // Abstract: work also fires from Done (over-approx — extra behavior)
    let abstract_model = r#"
context test {
    automata {
        automaton M {
            controllable { label work; }
            states {
                state Ready initial;
                state Done;
                state Error;
            }
            transitions {
                transition Ready -> Done on label work;
                transition Done -> Ready on label reset;
                transition Done -> Done on label work;
            }
        }
    }
    mu_formulas {
        formula safety { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safety; }
    }
}
"#;

    let precise_safe = is_realizable(precise, "M", "safety");
    let abstract_safe = is_realizable(abstract_model, "M", "safety");

    // Contract: if abstract is safe → precise must be safe
    assert!(
        abstract_safe,
        "Abstract model should be safe (Error unreachable)"
    );
    assert!(precise_safe, "Precise model should also be safe");
}

// ---------------------------------------------------------------------------
// Test 2: Noop self-loops mask deadlocks (liveness unsoundness)
// ---------------------------------------------------------------------------

/// A model where one state has no outgoing transitions (deadlock).
/// With a noop self-loop added, liveness appears to hold.
#[test]
fn noop_masks_deadlock() {
    // Without noop: Stuck has no successors → deadlock
    let without_noop = r#"
context test {
    automata {
        automaton M {
            states {
                state Active initial;
                state Stuck;
            }
            transitions {
                transition Active -> Stuck on label fail;
            }
        }
    }
    mu_formulas {
        formula no_deadlock { over M; body = nu X. (<> true && [] X); }
    }
}
"#;

    // With noop: Stuck has a self-loop — appears live
    let with_noop = r#"
context test {
    automata {
        automaton M {
            states {
                state Active initial;
                state Stuck;
            }
            transitions {
                transition Active -> Stuck on label fail;
                transition Stuck -> Stuck on label noop;
            }
        }
    }
    mu_formulas {
        formula no_deadlock { over M; body = nu X. (<> true && [] X); }
    }
}
"#;

    let without = eval_fraction(without_noop, "M", "no_deadlock");
    let with = eval_fraction(with_noop, "M", "no_deadlock");

    // Without noop: Stuck has no successors → deadlock → liveness fails
    assert!(
        without < 1.0,
        "Without noop: Stuck is a deadlock, not all states satisfy liveness (got {without})"
    );

    // With noop: Stuck has a self-loop → appears live
    assert!(
        with > without,
        "With noop: self-loop masks deadlock (got with={with}, without={without})"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Counter bound preserves safety violation
// ---------------------------------------------------------------------------

/// Fine bound captures a violation; coarse bound misses it (less precise).
/// Both are sound but the coarse model loses information.
#[test]
fn counter_bound_preserves_safety_violation() {
    // Bound = 3: count_2 is reachable → safety violation found
    let bound_3 = r#"
context test {
    automata {
        automaton M {
            states {
                state count_0 initial;
                state count_1;
                state count_2;
                state count_3;
            }
            transitions {
                transition count_0 -> count_1 on label inc;
                transition count_1 -> count_2 on label inc;
                transition count_2 -> count_3 on label inc;
                transition count_3 -> count_3 on label inc;
                transition count_0 -> count_0 on label noop;
                transition count_1 -> count_1 on label noop;
                transition count_2 -> count_2 on label noop;
                transition count_3 -> count_3 on label noop;
            }
        }
    }
    mu_formulas {
        formula safe { over M; body = nu X. (!count_2 && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safe; }
    }
}
"#;

    // Bound = 1: count_2 doesn't exist → property trivially holds
    let bound_1 = r#"
context test {
    automata {
        automaton M {
            states {
                state count_0 initial;
                state count_1;
            }
            transitions {
                transition count_0 -> count_1 on label inc;
                transition count_1 -> count_1 on label inc;
                transition count_0 -> count_0 on label noop;
                transition count_1 -> count_1 on label noop;
            }
        }
    }
    mu_formulas {
        formula safe { over M; body = nu X. (!count_2 && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safe; }
    }
}
"#;

    let fine_safe = is_realizable(bound_3, "M", "safe");
    let coarse_safe = is_realizable(bound_1, "M", "safe");

    // Fine: count_2 reachable → safety fails
    assert!(!fine_safe, "Bound=3: count_2 reachable, safety should fail");
    // Coarse: count_2 absent → trivially holds (less precise, not unsound)
    assert!(
        coarse_safe,
        "Bound=1: count_2 absent, safety trivially holds"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Havoc is conservative for safety
// ---------------------------------------------------------------------------

/// Havoc model (nondeterministic outcomes) is safe → precise is also safe.
#[test]
fn havoc_preserves_safety() {
    // Precise: action always goes Active → Done
    let precise = r#"
context test {
    automata {
        automaton M {
            controllable { label action; }
            states {
                state Idle initial;
                state Active;
                state Done;
                state Error;
            }
            transitions {
                transition Idle -> Active on label start;
                transition Active -> Done on label action;
                transition Done -> Idle on label reset;
            }
        }
    }
    mu_formulas {
        formula safe { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safe; }
    }
}
"#;

    // Havoc: action from Active can go to Done OR Active (nondeterministic)
    let havoc = r#"
context test {
    automata {
        automaton M {
            controllable { label action; }
            states {
                state Idle initial;
                state Active;
                state Done;
                state Error;
            }
            transitions {
                transition Idle -> Active on label start;
                transition Active -> Done on label action;
                transition Active -> Active on label action;
                transition Done -> Idle on label reset;
            }
        }
    }
    mu_formulas {
        formula safe { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying safe; }
    }
}
"#;

    let precise_safe = is_realizable(precise, "M", "safe");
    let havoc_safe = is_realizable(havoc, "M", "safe");

    // Contract: havoc safe → precise safe
    assert!(havoc_safe, "Havoc model: Error unreachable (conservative)");
    assert!(precise_safe, "Precise model: Error unreachable (subset)");
}

// ---------------------------------------------------------------------------
// Test 5: Over-approximation can produce false liveness
// ---------------------------------------------------------------------------

/// Abstract model has extra transition that provides progress not available
/// in the precise model.
#[test]
fn overapprox_can_provide_spurious_liveness() {
    // Precise: Idle has only a self-loop (no path to Done)
    let precise = r#"
context test {
    automata {
        automaton M {
            states {
                state Idle initial;
                state Done;
            }
            transitions {
                transition Idle -> Idle on label tick;
            }
        }
    }
    mu_formulas {
        formula reach_done { over M; body = mu X. (Done || <> X); }
    }
}
"#;

    // Abstract: Idle can also go to Done (over-approx adds progress)
    let abstract_model = r#"
context test {
    automata {
        automaton M {
            states {
                state Idle initial;
                state Done;
            }
            transitions {
                transition Idle -> Idle on label tick;
                transition Idle -> Done on label progress;
            }
        }
    }
    mu_formulas {
        formula reach_done { over M; body = mu X. (Done || <> X); }
    }
}
"#;

    let precise_live = eval_fraction(precise, "M", "reach_done");
    let abstract_live = eval_fraction(abstract_model, "M", "reach_done");

    // Precise: Done unreachable from Idle (only self-loop) → liveness fails
    assert!(
        precise_live < 1.0,
        "Precise: Done unreachable from Idle (got {precise_live})"
    );
    // Abstract: Done reachable via progress → liveness holds
    assert!(
        abstract_live >= 1.0,
        "Abstract: Done reachable via extra transition (got {abstract_live})"
    );
    // This demonstrates the liveness unsoundness of over-approximation:
    // the abstract model says "live" but the precise model is NOT live.
}

// ---------------------------------------------------------------------------
// Test 6: Synchronous composition preserves safety (exact product)
// ---------------------------------------------------------------------------

/// In synchronous composition, shared labels must fire simultaneously.
/// This verifies the exact product semantics: only shared-label transitions
/// fire together, independent transitions cannot interleave.
#[test]
fn sync_composition_exact_product() {
    // Two automata with disjoint controllability. The shared label "go"
    // is uncontrollable in both (environment-driven). In sync composition,
    // both must advance simultaneously on "go".
    let model = r#"
context test {
    automata {
        automaton A {
            controllable { label fix_a; }
            states {
                state A0 initial;
                state A1;
            }
            transitions {
                transition A0 -> A1 on label go;
                transition A1 -> A0 on label fix_a;
            }
        }
        automaton B {
            controllable { label fix_b; }
            states {
                state B0 initial;
                state B1;
            }
            transitions {
                transition B0 -> B1 on label go;
                transition B1 -> B0 on label fix_b;
            }
        }
    }
    composition {
        synchronous AB {
            members [A, B];
        }
    }
    mu_formulas {
        formula both_advance { over AB; body = mu X. ((A1 && B1) || <> X); }
    }
}
"#;

    // In sync, "go" fires in both A and B simultaneously.
    // From (A0, B0), one "go" step reaches (A1, B1).
    let reachable = eval_fraction(model, "AB", "both_advance");
    assert!(
        reachable >= 1.0,
        "Sync composition: (A1, B1) reachable via shared 'go' (got {reachable})"
    );
}

// ---------------------------------------------------------------------------
// Test 6b: Shared label controllable in one automaton, used by both
// ---------------------------------------------------------------------------

/// When one automaton declares a shared label as controllable and another
/// uses it without declaring controllability, the label should be controllable
/// only in the declaring automaton — not duplicated via legacy inference.
#[test]
fn shared_label_single_controllable_owner() {
    let model = r#"
context test {
    automata {
        automaton Controller {
            controllable { label sync_action; }
            states {
                state C_idle initial;
                state C_active;
            }
            transitions {
                transition C_idle -> C_active on label sync_action;
                transition C_active -> C_idle on label done;
            }
        }
        automaton Responder {
            states {
                state R_idle initial;
                state R_active;
            }
            transitions {
                transition R_idle -> R_active on label sync_action;
                transition R_active -> R_idle on label done;
            }
        }
    }
    composition {
        synchronous CR {
            members [Controller, Responder];
        }
    }
    mu_formulas {
        formula reach_active { over CR; body = mu X. ((C_active && R_active) || <> X); }
    }
}
"#;

    // This should not error — sync_action is controllable only in Controller.
    // Responder uses it as uncontrollable (legacy inference defers to explicit owner).
    let reachable = eval_fraction(model, "CR", "reach_active");
    assert!(
        reachable >= 1.0,
        "Shared controllable label: both active reachable via sync_action (got {reachable})"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Async composition introduces interleavings (more behaviors)
// ---------------------------------------------------------------------------

/// Async composition allows independent actions to interleave, which can
/// expose safety violations that don't exist under synchronous semantics.
#[test]
fn async_composition_more_behaviors_than_sync() {
    // Two automata with independent actions. In async composition,
    // the interleaving creates states unreachable under sync.
    let model = r#"
context test {
    automata {
        automaton C {
            controllable { label c_go; }
            states {
                state C_off initial;
                state C_on;
            }
            transitions {
                transition C_off -> C_on on label c_go;
            }
        }
        automaton D {
            controllable { label d_go; }
            states {
                state D_off initial;
                state D_on;
            }
            transitions {
                transition D_off -> D_on on label d_go;
            }
        }
    }
    composition {
        asynchronous CD {
            members [C, D];
        }
    }
    mu_formulas {
        formula both_on { over CD; body = mu X. ((C_on && D_on) || <> X); }
    }
}
"#;

    // Async allows C_on && D_on via interleaving: c_go then d_go
    let reachable = eval_fraction(model, "CD", "both_on");
    assert!(
        reachable >= 1.0,
        "Async: both_on reachable via interleaving (got {reachable})"
    );
}

// ---------------------------------------------------------------------------
// Test 8: Async composition can mask fairness-dependent liveness
// ---------------------------------------------------------------------------

/// Under async composition, a self-loop in one component can starve the other.
/// Liveness properties that assume progress in both components are unreliable
/// without explicit fairness constraints.
#[test]
fn async_composition_masks_fairness_liveness() {
    let model = r#"
context test {
    automata {
        automaton Worker {
            states {
                state Working initial;
                state Done;
            }
            transitions {
                transition Working -> Done on label finish;
                transition Working -> Working on label spin;
            }
        }
        automaton Idler {
            states {
                state Idle initial;
            }
            transitions {
                transition Idle -> Idle on label noop;
            }
        }
    }
    composition {
        asynchronous WI {
            members [Worker, Idler];
        }
    }
    mu_formulas {
        formula worker_finishes { over WI; body = mu X. (Done || <> X); }
    }
}
"#;

    // Under async, the Idler's noop can fire infinitely, starving Worker.
    // But mu-calculus reachability (mu X. (Done || <> X)) only checks
    // existence of a path — it WILL find the finish path regardless of fairness.
    let reachable = eval_fraction(model, "WI", "worker_finishes");
    assert!(
        reachable >= 1.0,
        "Reachability holds (path exists), but liveness without fairness is unreliable (got {reachable})"
    );
}

// ---------------------------------------------------------------------------
// Test 9: Controllability misclassification produces incorrect controller
// ---------------------------------------------------------------------------

/// If a label is incorrectly classified as controllable when it should be
/// uncontrollable, synthesis may produce a controller that cannot actually
/// prevent the transition in reality.
#[test]
fn controllability_misclassification() {
    // Model where the environment action "env_break" leads to Error.
    // If we incorrectly mark env_break as controllable, synthesis thinks
    // it can avoid Error. If correctly uncontrollable, it cannot.
    //
    // The formula uses [(ctrl=Controllable)] to express the game semantics:
    // the controller must find a controllable move where ALL uncontrollable
    // successors also satisfy the property.
    let correct_uncontrollable = r#"
context test {
    automata {
        automaton M {
            controllable { label fix; }
            states {
                state Ok initial;
                state Error;
            }
            transitions {
                transition Ok -> Error on label env_break;
                transition Ok -> Ok on label fix;
                transition Error -> Ok on label fix;
            }
        }
    }
    mu_formulas {
        formula no_error { over M; body = nu X. (!Error && [(ctrl=Controllable)] X); }
    }
    controllers {
        controller synth { source M; satisfying no_error; }
    }
}
"#;

    let misclassified_controllable = r#"
context test {
    automata {
        automaton M {
            controllable { label fix; label env_break; }
            states {
                state Ok initial;
                state Error;
            }
            transitions {
                transition Ok -> Error on label env_break;
                transition Ok -> Ok on label fix;
                transition Error -> Ok on label fix;
            }
        }
    }
    mu_formulas {
        formula no_error { over M; body = nu X. (!Error && [(ctrl=Controllable)] X); }
    }
    controllers {
        controller synth { source M; satisfying no_error; }
    }
}
"#;

    let correct = is_realizable(correct_uncontrollable, "M", "no_error");
    let misclassified = is_realizable(misclassified_controllable, "M", "no_error");

    // With env_break uncontrollable: environment can always break → unrealizable
    assert!(
        !correct,
        "Correct: env_break is uncontrollable, controller cannot prevent Error"
    );
    // With env_break controllable: controller simply never fires it → realizable
    assert!(
        misclassified,
        "Misclassified: controller thinks it can avoid env_break → falsely realizable"
    );
}

// ---------------------------------------------------------------------------
// Test 10: Extraction-style over-approximation (guard removal)
// ---------------------------------------------------------------------------

/// Simulates the extraction adapter's over-approximation of guards.
/// A guarded model only transitions when a condition holds; the unguarded
/// (over-approx) model allows the transition unconditionally.
/// Safety on unguarded → safety on guarded (sound).
#[test]
fn extraction_guard_removal_preserves_safety() {
    // Guarded: can only enter Critical when lock is held (via acquire)
    let guarded = r#"
context test {
    automata {
        automaton M {
            controllable { label acquire; label release; }
            states {
                state Unlocked initial;
                state Locked;
                state Critical;
                state Error;
            }
            transitions {
                transition Unlocked -> Locked on label acquire;
                transition Locked -> Critical on label enter;
                transition Critical -> Locked on label exit;
                transition Locked -> Unlocked on label release;
            }
        }
    }
    mu_formulas {
        formula no_error { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying no_error; }
    }
}
"#;

    // Unguarded (over-approx): enter can also fire from Unlocked
    let unguarded = r#"
context test {
    automata {
        automaton M {
            controllable { label acquire; label release; }
            states {
                state Unlocked initial;
                state Locked;
                state Critical;
                state Error;
            }
            transitions {
                transition Unlocked -> Locked on label acquire;
                transition Locked -> Critical on label enter;
                transition Unlocked -> Critical on label enter;
                transition Critical -> Locked on label exit;
                transition Locked -> Unlocked on label release;
            }
        }
    }
    mu_formulas {
        formula no_error { over M; body = nu X. (!Error && [] X); }
    }
    controllers {
        controller synth { source M; satisfying no_error; }
    }
}
"#;

    let guarded_safe = is_realizable(guarded, "M", "no_error");
    let unguarded_safe = is_realizable(unguarded, "M", "no_error");

    // Over-approx contract: if unguarded is safe → guarded is safe
    // Both should be safe here (Error is unreachable in both)
    assert!(unguarded_safe, "Unguarded: Error still unreachable");
    assert!(
        guarded_safe,
        "Guarded: Error unreachable (subset of behaviors)"
    );
}

// ---------------------------------------------------------------------------
// Test 11: Extraction-style havoc for BoundedCounter
// ---------------------------------------------------------------------------

/// Simulates a BoundedCounter field with havoc (nondeterministic increment).
/// Havoc admits more behaviors → if safe under havoc, safe under precise increment.
#[test]
fn extraction_havoc_counter_preserves_safety() {
    // Precise: counter increments by 1 each step
    let precise = r#"
context test {
    automata {
        automaton M {
            controllable { label inc; }
            states {
                state c0 initial;
                state c1;
                state c2;
                state c3;
            }
            transitions {
                transition c0 -> c1 on label inc;
                transition c1 -> c2 on label inc;
                transition c2 -> c3 on label inc;
                transition c3 -> c3 on label inc;
            }
        }
    }
    mu_formulas {
        formula bounded { over M; body = nu X. ((!c3 || [] !c3) && [] X); }
    }
}
"#;

    // Havoc: counter can jump to any higher value
    let havoc = r#"
context test {
    automata {
        automaton M {
            controllable { label inc; }
            states {
                state c0 initial;
                state c1;
                state c2;
                state c3;
            }
            transitions {
                transition c0 -> c1 on label inc;
                transition c0 -> c2 on label inc;
                transition c0 -> c3 on label inc;
                transition c1 -> c2 on label inc;
                transition c1 -> c3 on label inc;
                transition c2 -> c3 on label inc;
                transition c3 -> c3 on label inc;
            }
        }
    }
    mu_formulas {
        formula bounded { over M; body = nu X. ((!c3 || [] !c3) && [] X); }
    }
}
"#;

    let precise_result = eval_fraction(precise, "M", "bounded");
    let havoc_result = eval_fraction(havoc, "M", "bounded");

    // Havoc has MORE paths to c3 → harder to satisfy the property
    // Both fail here (c3 is reachable), but havoc should be at most as good
    assert!(
        havoc_result <= precise_result,
        "Havoc should not make safety easier (havoc={havoc_result}, precise={precise_result})"
    );
}

// ---------------------------------------------------------------------------
// Test 12: Mealy vs Moore divergence (SYNTCOMP-style)
// ---------------------------------------------------------------------------

/// Demonstrates a specification that is realizable under Mealy semantics
/// (controller observes input THEN produces output in the same round) but
/// would be unrealizable under Moore (output must be chosen BEFORE seeing
/// the current input).
///
/// Mununu uses Mealy encoding (turn-based: env acts, then ctrl responds).
/// This test documents the semantic choice.
#[test]
fn mealy_vs_moore_divergence() {
    // A 1-input 1-output spec: the controller must echo the environment's
    // choice in the same round. Under Mealy, the controller sees the input
    // first and can copy it. Under Moore, the controller commits blind.
    //
    // We encode this directly in CTXDSL with the turn-based structure that
    // the TLSF adapter produces.
    let mealy_model = r#"
context test {
    automata {
        automaton M {
            controllable { label ctrl_0; label ctrl_1; }
            states {
                state S0 initial;
                state S1;
                state S2;
                state Match;
                state Mismatch;
            }
            transitions {
                transition S0 -> S1 on label env_0;
                transition S0 -> S2 on label env_1;
                transition S1 -> Match on label ctrl_0;
                transition S1 -> Mismatch on label ctrl_1;
                transition S2 -> Match on label ctrl_1;
                transition S2 -> Mismatch on label ctrl_0;
                transition Match -> S0 on label reset;
                transition Mismatch -> S0 on label reset;
            }
        }
    }
    mu_formulas {
        formula always_match {
            over M;
            body = nu X. (!Mismatch && [(ctrl=Controllable)] X);
        }
    }
    controllers {
        controller synth { source M; satisfying always_match; }
    }
}
"#;

    // Under Mealy: the controller sees env_0/env_1 BEFORE choosing ctrl_0/ctrl_1.
    // It can always pick the matching response. → REALIZABLE
    let realizable = is_realizable(mealy_model, "M", "always_match");
    assert!(
        realizable,
        "Mealy semantics: controller can observe input before responding → realizable"
    );

    // Under Moore semantics, this would be UNREALIZABLE because the controller
    // must commit to ctrl_0 or ctrl_1 without seeing the environment's choice.
    // We document this by constructing the Moore variant where controller acts first:
    let moore_model = r#"
context test {
    automata {
        automaton M {
            controllable { label ctrl_0; label ctrl_1; }
            states {
                state S0 initial;
                state C0;
                state C1;
                state Match;
                state Mismatch;
            }
            transitions {
                transition S0 -> C0 on label ctrl_0;
                transition S0 -> C1 on label ctrl_1;
                transition C0 -> Match on label env_0;
                transition C0 -> Mismatch on label env_1;
                transition C1 -> Match on label env_1;
                transition C1 -> Mismatch on label env_0;
                transition Match -> S0 on label reset;
                transition Mismatch -> S0 on label reset;
            }
        }
    }
    mu_formulas {
        formula always_match {
            over M;
            body = nu X. (!Mismatch && [(ctrl=Controllable)] X);
        }
    }
    controllers {
        controller synth { source M; satisfying always_match; }
    }
}
"#;

    // Under Moore: controller commits before seeing env → cannot guarantee match
    let moore_realizable = is_realizable(moore_model, "M", "always_match");
    assert!(
        !moore_realizable,
        "Moore semantics: controller commits blind → unrealizable"
    );
}

// ---------------------------------------------------------------------------
// Test 13: Signature-based functional strategy for GR(1)
// ---------------------------------------------------------------------------

/// Signature-based strategy extraction produces a deterministic functional
/// controller for GR(1) formulas. The controller picks one transition per
/// state using signature ordering (lexicographically best target).
#[test]
fn signature_functional_strategy_gr1() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions, DiagnosticsOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            controllable { label go_a; label go_b; }
            states {
                state Start initial;
                state A;
                state B;
            }
            transitions {
                transition Start -> A on label go_a;
                transition Start -> B on label go_b;
                transition A -> Start on label back;
                transition B -> Start on label back;
            }
        }
    }
    mu_formulas {
        formula gr1 {
            over M;
            body = nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [(ctrl=Controllable)] X);
        }
    }
    controllers {
        controller synth { source M; satisfying gr1; }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("gr1").unwrap();
    let env = realized.environment_for("M");

    // Functional mode: signature-based, one controllable per state
    let synth = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: Some(&DiagnosticsOptions {
                    counterexample: false,
                    deadlock_traces: false,
                    max_counter_traces: None,
                    proof_obligations: false,
                }),
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::Functional,
            },
        )
        .unwrap();

    assert!(synth.realizable, "GR(1) formula should be realizable");
    assert!(
        synth
            .diagnostics
            .messages
            .iter()
            .any(|m| m.contains("Functional")),
        "Diagnostics should report Functional mode"
    );

    // Functional mode should produce fewer transitions than Projection
    // (at most one controllable per state)
    let projection = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::Projection,
            },
        )
        .unwrap();

    // The functional controller should have the same states but potentially
    // fewer transitions (since it picks only the best controllable per state)
    assert_eq!(
        synth.controller.state_count(),
        projection.controller.state_count(),
        "Same winning region, same state count"
    );

    // SignatureMemory mode: same selection rule as Functional, but state names
    // are annotated with `__sig_<rank_tuple>`. Demonstrates the signature
    // memory is now observable at the controller-state level.
    let sigmem = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::SignatureMemory,
            },
        )
        .unwrap();
    assert!(
        sigmem.realizable,
        "SignatureMemory mode should agree with Functional on realizability"
    );
    // State count matches Functional (same selection rule)
    assert_eq!(
        sigmem.controller.state_count(),
        synth.controller.state_count(),
        "SignatureMemory state count should equal Functional"
    );
    // At least one state name carries the signature annotation
    let has_sig_annotation = sigmem.controller.states().any(|s| {
        sigmem
            .controller
            .state_name(s)
            .is_some_and(|n| n.contains("__sig_"))
    });
    assert!(
        has_sig_annotation,
        "Expected at least one state name annotated with `__sig_<...>` in SignatureMemory mode"
    );
}

// ---------------------------------------------------------------------------
// Test 14: Product-game (memory-aware Mealy) controller — D1a-substantive
// ---------------------------------------------------------------------------

/// On a GR(1) formula with two mu obligations, ProductGame mode emits a
/// controller whose state space is `(plant_state, oblig_idx)` for
/// `oblig_idx ∈ {0, 1}`. The state count is at most `winning_states × 2`,
/// every state name carries `__pg_<i>`, and the controller is realizable.
#[test]
fn product_game_gr1_realizable() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions, DiagnosticsOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            controllable { label go_a; label go_b; }
            states {
                state IDLE initial;
                state A;
                state B;
            }
            transitions {
                transition IDLE -> A on label go_a;
                transition IDLE -> B on label go_b;
                transition A -> IDLE on label reset;
                transition B -> IDLE on label reset;
            }
            predicates {
                predicate A = state A;
                predicate B = state B;
            }
        }
    }
    mu_formulas {
        formula gr1 {
            over M;
            body = nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [(ctrl=Controllable)] X);
        }
    }
    controllers {
        controller synth { source M; satisfying gr1; }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("gr1").unwrap();
    let env = realized.environment_for("M");

    // Functional baseline (one product state per plant state)
    let functional = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::Functional,
            },
        )
        .unwrap();
    assert!(functional.realizable);

    let pg = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: Some(&DiagnosticsOptions {
                    counterexample: false,
                    deadlock_traces: false,
                    max_counter_traces: None,
                    proof_obligations: false,
                }),
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ProductGame,
            },
        )
        .unwrap();
    assert!(
        pg.realizable,
        "ProductGame controller for GR(1) should be realizable"
    );

    // Two mu-obligations → product space ≤ 2× plant states
    let n_obligs = 2;
    assert!(
        pg.controller.state_count() <= functional.controller.state_count() * n_obligs,
        "ProductGame state count ({}) should be ≤ Functional ({}) × {}",
        pg.controller.state_count(),
        functional.controller.state_count(),
        n_obligs
    );

    // At least some state names carry the __pg_ annotation
    let pg_count = pg
        .controller
        .states()
        .filter(|s| {
            pg.controller
                .state_name(*s)
                .is_some_and(|n| n.contains("__pg_"))
        })
        .count();
    assert!(
        pg_count > 0,
        "Expected at least one state name annotated with `__pg_<i>` in ProductGame mode"
    );

    // Diagnostics should mention ProductGame and the obligation count
    assert!(
        pg.diagnostics
            .messages
            .iter()
            .any(|m| m.contains("ProductGame")),
        "Diagnostics should report ProductGame mode"
    );
    assert!(
        pg.diagnostics
            .messages
            .iter()
            .any(|m| m.contains("mu-obligation")),
        "Diagnostics should report obligation count"
    );
}

/// For a pure safety formula (alternation 0, no mu-fixpoints), the obligation
/// dimension collapses to size 1. The product-game controller has the same
/// state count as `Functional`, with `__pg_0` annotations.
#[test]
fn product_game_no_mus_safety() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            states {
                state Good initial;
                state Bad;
            }
            transitions {
                transition Good -> Good on epsilon;
            }
            predicates {
                predicate Bad = state Bad;
            }
        }
    }
    mu_formulas {
        formula safety { over M; body = nu X. ((!Bad) && [] X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("safety").unwrap();
    let env = realized.environment_for("M");

    let functional = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::Functional,
            },
        )
        .unwrap();
    let pg = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ProductGame,
            },
        )
        .unwrap();

    assert_eq!(pg.realizable, functional.realizable);
    assert_eq!(
        pg.controller.state_count(),
        functional.controller.state_count(),
        "Pure safety formula has no mu-obligations → product space size = plant size"
    );
    // Every retained state should be annotated `__pg_0`
    let all_pg0 = pg.controller.states().all(|s| {
        pg.controller
            .state_name(s)
            .is_some_and(|n| n.ends_with("__pg_0"))
    });
    assert!(all_pg0, "All states should carry `__pg_0` suffix");
}

/// For a pure reachability formula (single mu, alternation 1), there is one
/// mu-obligation. Memory dimension still collapses to size 1; the controller
/// shape matches Functional with `__pg_0` annotations.
#[test]
fn product_game_single_mu_reachability() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            controllable { label step; }
            states {
                state Start initial;
                state Goal;
            }
            transitions {
                transition Start -> Goal on label step;
                transition Goal -> Goal on label step;
            }
            predicates {
                predicate Goal = state Goal;
            }
        }
    }
    mu_formulas {
        formula reach { over M; body = mu X. (Goal || <> X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("reach").unwrap();
    let env = realized.environment_for("M");

    let functional = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::Functional,
            },
        )
        .unwrap();
    let pg = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ProductGame,
            },
        )
        .unwrap();

    assert!(pg.realizable);
    assert_eq!(
        pg.controller.state_count(),
        functional.controller.state_count(),
        "Single mu-obligation → product space size = plant size"
    );
}

/// Cross-mode agreement check: all four extracting modes (Functional,
/// SignatureMemory, ProductGame, plus Projection) agree on realizability of
/// the GR(1) formula. Ensures no mode silently disagrees with the others.
#[test]
fn product_game_agrees_with_other_modes_on_realizability() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            controllable { label go_a; label go_b; }
            states {
                state IDLE initial;
                state A;
                state B;
            }
            transitions {
                transition IDLE -> A on label go_a;
                transition IDLE -> B on label go_b;
                transition A -> IDLE on label reset;
                transition B -> IDLE on label reset;
            }
            predicates {
                predicate A = state A;
                predicate B = state B;
            }
        }
    }
    mu_formulas {
        formula gr1 {
            over M;
            body = nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [(ctrl=Controllable)] X);
        }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("gr1").unwrap();
    let env = realized.environment_for("M");

    let mk = |mode: ControllerMode| {
        realized
            .context
            .synthesise_controller_with_options(
                "M",
                &formula.formula,
                &env,
                ControllerSynthesisOptions {
                    evaluation: None,
                    diagnostics: None,
                    minimize: false,
                    extract_strategy: false,
                    mode,
                },
            )
            .unwrap()
    };

    let projection = mk(ControllerMode::Projection);
    let functional = mk(ControllerMode::Functional);
    let sigmem = mk(ControllerMode::SignatureMemory);
    let pg = mk(ControllerMode::ProductGame);
    let parity = mk(ControllerMode::ParityGame);

    assert_eq!(projection.realizable, functional.realizable);
    assert_eq!(functional.realizable, sigmem.realizable);
    assert_eq!(sigmem.realizable, pg.realizable);
    assert_eq!(pg.realizable, parity.realizable);
}

// ---------------------------------------------------------------------------
// Test 15: ParityGame mode — full Zielonka synthesis
// ---------------------------------------------------------------------------

/// ParityGame mode synthesizes a controller via explicit parity-game
/// construction + Zielonka. For a simple safety formula, the controller is
/// realizable and emits a CLTS with `__pg_n<node_id>` state names (one per
/// game position in Eve's winning region).
#[test]
fn parity_game_safety_realizable() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            states {
                state Good initial;
                state Bad;
            }
            transitions {
                transition Good -> Good on epsilon;
            }
            predicates {
                predicate Bad = state Bad;
            }
        }
    }
    mu_formulas {
        formula safety { over M; body = nu X. ((!Bad) && [] X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("safety").unwrap();
    let env = realized.environment_for("M");

    let parity = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ParityGame,
            },
        )
        .unwrap();
    assert!(
        parity.realizable,
        "Safety formula should be realizable in ParityGame mode"
    );

    // At least one state should be annotated with `__pg_n<node_id>`
    let has_pg_naming = parity.controller.states().any(|s| {
        parity
            .controller
            .state_name(s)
            .is_some_and(|n| n.contains("__pg_n"))
    });
    assert!(
        has_pg_naming,
        "ParityGame controller should have `__pg_n<id>` state name annotations"
    );
}

/// On an unrealizable safety formula (referencing an unreachable nu region),
/// ParityGame mode reports unrealizable.
#[test]
fn parity_game_unrealizable_when_nu_violated() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            states { state s0 initial; }
            transitions {
                transition s0 -> s0 on epsilon;
            }
        }
    }
    mu_formulas {
        formula impossible { over M; body = nu X. (false && [] X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("impossible").unwrap();
    let env = realized.environment_for("M");

    let parity = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ParityGame,
            },
        )
        .unwrap();
    assert!(
        !parity.realizable,
        "Formula `nu X. (false && [] X)` is never satisfiable; ParityGame should report unrealizable"
    );
}

/// Reachability formula `mu X. (Goal || <> X)` should be realizable when
/// Goal is reachable. ParityGame mode handles single-mu (alternation 1)
/// formulas correctly.
#[test]
fn parity_game_reachability_realizable() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            states {
                state Start initial;
                state Goal;
            }
            transitions {
                transition Start -> Goal on epsilon;
                transition Goal -> Goal on epsilon;
            }
            predicates {
                predicate Goal = state Goal;
            }
        }
    }
    mu_formulas {
        formula reach { over M; body = mu X. (Goal || <> X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("reach").unwrap();
    let env = realized.environment_for("M");

    let parity = realized
        .context
        .synthesise_controller_with_options(
            "M",
            &formula.formula,
            &env,
            ControllerSynthesisOptions {
                evaluation: None,
                diagnostics: None,
                minimize: false,
                extract_strategy: false,
                mode: ControllerMode::ParityGame,
            },
        )
        .unwrap();
    assert!(
        parity.realizable,
        "Reachability to Goal should be realizable"
    );
}

/// Alternation depth 2 (Buchi-style `nu X. ((mu Y. (P || <> Y)) && [] X)`):
/// the controller must guarantee fairness — visit P infinitely often.
/// ParityGame mode should agree with ProductGame on realizability.
#[test]
fn parity_game_alternation_2_buchi_agrees_with_product_game() {
    use mununu_core::context::{ControllerMode, ControllerSynthesisOptions};

    let model = r#"
context test {
    automata {
        automaton M {
            states {
                state Idle initial;
                state Hot;
            }
            transitions {
                transition Idle -> Hot on epsilon;
                transition Hot -> Idle on epsilon;
            }
            predicates {
                predicate Hot = state Hot;
            }
        }
    }
    mu_formulas {
        formula buchi { over M; body = nu X. ((mu Y. (Hot || <> Y)) && [] X); }
    }
}
"#;

    let doc = context_dsl::parse(model).unwrap();
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));
    let formula = realized.formulas.get("buchi").unwrap();
    let env = realized.environment_for("M");

    let mk = |mode: ControllerMode| {
        realized
            .context
            .synthesise_controller_with_options(
                "M",
                &formula.formula,
                &env,
                ControllerSynthesisOptions {
                    evaluation: None,
                    diagnostics: None,
                    minimize: false,
                    extract_strategy: false,
                    mode,
                },
            )
            .unwrap()
    };

    let pg = mk(ControllerMode::ProductGame);
    let parity = mk(ControllerMode::ParityGame);

    // Both modes agree on realizability for alternation 2
    assert_eq!(
        pg.realizable, parity.realizable,
        "ProductGame and ParityGame must agree on alternation-2 realizability"
    );
}
