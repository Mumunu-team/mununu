//! Integration tests for LTL temporal logic patterns.
//!
//! These tests verify that common LTL patterns from `docs/ltl_templates/temporal_logic_patterns.md`
//! can be parsed, translated to μ-calculus, and realized correctly in the Context DSL.

use mununu_core::clts::Clts;
use mununu_core::context::Context;
use mununu_core::context_dsl;
use mununu_core::context_dsl::realize_context;
use mununu_core::ltl::{self, LtlFormula};
use mununu_core::mu_calculus::Environment;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Helper to parse a context with LTL formulas
fn parse_context_with_ltl(
    source: &str,
) -> Result<context_dsl::ast::ContextDoc, context_dsl::ParseError> {
    context_dsl::parse(source)
}

/// Helper to realize a context and check for errors
fn realize_test_context(source: &str) -> TestResult<mununu_core::context_dsl::RealizedContext> {
    let doc = parse_context_with_ltl(source)?;
    let realized = realize_context(&doc, &[] as &[mununu_core::context_dsl::ast::ContextDoc])?;
    Ok(realized)
}

// ============================================================================
// Safety Properties
// ============================================================================

#[test]
fn test_safety_mutual_exclusion() -> TestResult {
    // Pattern: G(!(in_critical_section_1 && in_critical_section_2))
    let source = r#"
    context test {
        automata {
            automaton System {
                states {
                    state idle initial;
                    state critical_1;
                    state critical_2;
                }
                transitions {
                    transition idle -> critical_1 on epsilon;
                    transition idle -> critical_2 on epsilon;
                    transition critical_1 -> idle on epsilon;
                    transition critical_2 -> idle on epsilon;
                }
            }
        }
        mu_formulas {
            formula mutual_exclusion {
                over System;
                body = ltl G !(in_critical_section_1 && in_critical_section_2);
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "mutual_exclusion")
        .unwrap();

    let ltl_expr = match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => {
            // Verify it's an Always with Not and And inside
            match &ltl_expr.formula {
                LtlFormula::Always(inner) => match &**inner {
                    LtlFormula::Not(not_inner) => match &**not_inner {
                        LtlFormula::And(_, _) => {}
                        _ => panic!("Expected And inside Not"),
                    },
                    _ => panic!("Expected Not inside Always"),
                },
                _ => panic!("Expected Always at top level"),
            }
            ltl_expr
        }
        _ => panic!("Expected LTL formula"),
    };

    // Verify it can be translated
    let translated = ltl::translator::translate(&ltl_expr.formula)?;
    assert!(matches!(
        translated.node(translated.root()),
        mununu_core::mu_calculus::Node::Nu { .. }
    ));

    // Verify it can be realized
    realize_test_context(source)?;
    Ok(())
}

#[test]
fn test_safety_bounded_buffer() -> TestResult {
    // Pattern: G(buffer_count <= N)
    // Note: This is a simplified test - actual bounded buffer would need predicates
    let source = r#"
    context test {
        automata {
            automaton Buffer {
                states {
                    state empty initial;
                    state partial;
                    state full;
                }
                transitions {
                    transition empty -> partial on epsilon;
                    transition partial -> full on epsilon;
                    transition full -> partial on epsilon;
                    transition partial -> empty on epsilon;
                }
                predicates {
                    predicate overflow = state full;
                }
            }
        }
        mu_formulas {
            formula bounded {
                over Buffer;
                body = ltl G !overflow;
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "bounded")
        .unwrap();

    match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => match &ltl_expr.formula {
            LtlFormula::Always(inner) => match &**inner {
                LtlFormula::Not(_) => {}
                _ => panic!("Expected Not inside Always"),
            },
            _ => panic!("Expected Always at top level"),
        },
        _ => panic!("Expected LTL formula"),
    }

    realize_test_context(source)?;
    Ok(())
}

// ============================================================================
// Liveness Properties
// ============================================================================

#[test]
fn test_liveness_request_response() -> TestResult {
    // Pattern: G(request -> F(response))
    let source = r#"
    context test {
        automata {
            automaton Protocol {
                states {
                    state idle initial;
                    state waiting;
                    state responding;
                }
                transitions {
                    transition idle -> waiting on epsilon;
                    transition waiting -> responding on epsilon;
                    transition responding -> idle on epsilon;
                }
                predicates {
                    predicate request = state waiting;
                    predicate response = state responding;
                }
            }
        }
        mu_formulas {
            formula responsiveness {
                over Protocol;
                body = ltl G (request -> F response);
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "responsiveness")
        .unwrap();

    let ltl_expr = match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => {
            // Verify structure: Always(Implies(request, Eventually(response)))
            match &ltl_expr.formula {
                LtlFormula::Always(inner) => match &**inner {
                    LtlFormula::Implies(_trigger, response) => match &**response {
                        LtlFormula::Eventually(_) => {}
                        _ => panic!("Expected Eventually in response"),
                    },
                    _ => panic!("Expected Implies inside Always"),
                },
                _ => panic!("Expected Always at top level"),
            }
            ltl_expr
        }
        _ => panic!("Expected LTL formula"),
    };

    // Verify translation produces Nu (for G) with Mu inside (for F)
    let translated = ltl::translator::translate(&ltl_expr.formula)?;
    match translated.node(translated.root()) {
        mununu_core::mu_calculus::Node::Nu { .. } => {}
        _ => panic!("Expected Nu (Always) at root"),
    }

    realize_test_context(source)?;
    Ok(())
}

#[test]
fn test_liveness_termination() -> TestResult {
    // Pattern: F(terminated)
    let source = r#"
    context test {
        automata {
            automaton Algorithm {
                states {
                    state running initial;
                    state terminated;
                }
                transitions {
                    transition running -> terminated on epsilon;
                    transition terminated -> terminated on epsilon;
                }
            }
        }
        mu_formulas {
            formula termination {
                over Algorithm;
                body = ltl F terminated;
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "termination")
        .unwrap();

    match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => match &ltl_expr.formula {
            LtlFormula::Eventually(_) => {}
            _ => panic!("Expected Eventually at top level"),
        },
        _ => panic!("Expected LTL formula"),
    }

    realize_test_context(source)?;
    Ok(())
}

#[test]
fn test_liveness_recurrence() -> TestResult {
    // Pattern: GF(heartbeat)
    let source = r#"
    context test {
        automata {
            automaton System {
                states {
                    state active initial;
                }
                transitions {
                    transition active -> active on epsilon;
                }
                predicates {
                    predicate heartbeat = state active;
                }
            }
        }
        mu_formulas {
            formula heartbeat {
                over System;
                body = ltl G F heartbeat;
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "heartbeat")
        .unwrap();

    let ltl_expr = match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => {
            // GF = G(F(...)) = Always(Eventually(...))
            match &ltl_expr.formula {
                LtlFormula::Always(inner) => match &**inner {
                    LtlFormula::Eventually(_) => {}
                    _ => panic!("Expected Eventually inside Always"),
                },
                _ => panic!("Expected Always at top level"),
            }
            ltl_expr
        }
        _ => panic!("Expected LTL formula"),
    };

    // Verify translation: GF should produce Nu with Mu inside
    let translated = ltl::translator::translate(&ltl_expr.formula)?;
    match translated.node(translated.root()) {
        mununu_core::mu_calculus::Node::Nu { .. } => {}
        _ => panic!("Expected Nu (G) at root for GF"),
    }

    realize_test_context(source)?;
    Ok(())
}

// ============================================================================
// Reactiveness Properties
// ============================================================================

#[test]
fn test_reactiveness_conditional_response() -> TestResult {
    // Pattern: G((req1 -> F(grant1)) && (req2 -> F(grant2)))
    let source = r#"
    context test {
        automata {
            automaton Controller {
                states {
                    state idle initial;
                }
                transitions {
                    transition idle -> idle on epsilon;
                }
                predicates {
                    predicate req1 = state idle;
                    predicate req2 = state idle;
                    predicate grant1 = state idle;
                    predicate grant2 = state idle;
                }
            }
        }
        mu_formulas {
            formula conditional_response {
                over Controller;
                body = ltl G ((req1 -> F grant1) && (req2 -> F grant2));
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "conditional_response")
        .unwrap();

    match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => {
            // Verify structure: Always(And(Implies(...), Implies(...)))
            match &ltl_expr.formula {
                LtlFormula::Always(inner) => match &**inner {
                    LtlFormula::And(_, _) => {}
                    _ => panic!("Expected And inside Always"),
                },
                _ => panic!("Expected Always at top level"),
            }
        }
        _ => panic!("Expected LTL formula"),
    }

    realize_test_context(source)?;
    Ok(())
}

// ============================================================================
// GR(1) Patterns
// ============================================================================

#[test]
fn test_gr1_basic_pattern() -> TestResult {
    // Pattern: (G(env_assume) && GF(env_justice)) -> (G(sys_guarantee) && GF(sys_justice))
    // Simplified: G(env_assume) && GF(env_justice)
    let source = r#"
    context test {
        automata {
            automaton System {
                states {
                    state s0 initial;
                }
                transitions {
                    transition s0 -> s0 on epsilon;
                }
                predicates {
                    predicate env_assume = state s0;
                    predicate env_justice = state s0;
                }
            }
        }
        mu_formulas {
            formula env_safety {
                over System;
                body = ltl G env_assume;
            }
            formula env_justice {
                over System;
                body = ltl G F env_justice;
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;

    // Verify both formulas are parsed correctly
    assert_eq!(doc.mu_formulas.len(), 2);

    let safety = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "env_safety")
        .unwrap();
    match &safety.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => match &ltl_expr.formula {
            LtlFormula::Always(_) => {}
            _ => panic!("Expected Always for safety"),
        },
        _ => panic!("Expected LTL formula"),
    }

    let justice = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "env_justice")
        .unwrap();
    match &justice.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => match &ltl_expr.formula {
            LtlFormula::Always(inner) => match &**inner {
                LtlFormula::Eventually(_) => {}
                _ => panic!("Expected Eventually inside Always for justice"),
            },
            _ => panic!("Expected Always for justice"),
        },
        _ => panic!("Expected LTL formula"),
    }

    realize_test_context(source)?;
    Ok(())
}

// ============================================================================
// Until Patterns
// ============================================================================

#[test]
fn test_until_phase_transition() -> TestResult {
    // Pattern: initialization U operational
    let source = r#"
    context test {
        automata {
            automaton StateMachine {
                states {
                    state init initial;
                    state operational;
                }
                transitions {
                    transition init -> operational on epsilon;
                    transition operational -> operational on epsilon;
                }
                predicates {
                    predicate initialization = state init;
                }
            }
        }
        mu_formulas {
            formula phase_transition {
                over StateMachine;
                body = ltl initialization U operational;
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    let formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "phase_transition")
        .unwrap();

    let ltl_expr = match &formula.body {
        context_dsl::ast::FormulaExpr::Ltl(ltl_expr) => {
            match &ltl_expr.formula {
                LtlFormula::Until { left: _, right: _ } => {}
                _ => panic!("Expected Until at top level"),
            }
            ltl_expr
        }
        _ => panic!("Expected LTL formula"),
    };

    // Verify translation produces Mu (for Until)
    let translated = ltl::translator::translate(&ltl_expr.formula)?;
    match translated.node(translated.root()) {
        mununu_core::mu_calculus::Node::Mu { .. } => {}
        _ => panic!("Expected Mu (Until) at root"),
    }

    realize_test_context(source)?;
    Ok(())
}

// ============================================================================
// End-to-End Realization Tests
// ============================================================================

#[test]
fn test_realize_ltl_safety_formula() -> TestResult {
    // Test that LTL formulas can be fully realized and used
    let source = r#"
    context test {
        automata {
            automaton A {
                states {
                    state s initial;
                }
                transitions {
                    transition s -> s on epsilon;
                }
                predicates {
                    predicate safe = state s;
                }
            }
        }
        mu_formulas {
            formula safety {
                over A;
                body = ltl G safe;
            }
        }
    }
    "#;

    let realized = realize_test_context(source)?;

    // Verify formula exists in realized context
    assert!(realized.formulas.contains_key("safety"));

    Ok(())
}

#[test]
fn test_realize_ltl_liveness_formula() -> TestResult {
    let source = r#"
    context test {
        automata {
            automaton A {
                states {
                    state s initial;
                }
                transitions {
                    transition s -> s on epsilon;
                }
                predicates {
                    predicate completed = state s;
                }
            }
        }
        mu_formulas {
            formula liveness {
                over A;
                body = ltl F completed;
            }
        }
    }
    "#;

    let realized = realize_test_context(source)?;
    assert!(realized.formulas.contains_key("liveness"));
    Ok(())
}

#[test]
fn test_realize_mixed_ltl_and_mu_formulas() -> TestResult {
    // Test that both LTL and μ-calculus formulas can coexist
    let source = r#"
    context test {
        automata {
            automaton A {
                states {
                    state s initial;
                }
                transitions {
                    transition s -> s on epsilon;
                }
                predicates {
                    predicate safe = state s;
                }
            }
        }
        mu_formulas {
            formula ltl_formula {
                over A;
                body = ltl G safe;
            }
            formula mu_formula {
                over A;
                body = nu X. (safe && [] X);
            }
        }
    }
    "#;

    let doc = parse_context_with_ltl(source)?;
    assert_eq!(doc.mu_formulas.len(), 2);

    let ltl_formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "ltl_formula")
        .unwrap();
    match &ltl_formula.body {
        context_dsl::ast::FormulaExpr::Ltl(_) => {}
        _ => panic!("Expected LTL formula"),
    }

    let mu_formula = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "mu_formula")
        .unwrap();
    match &mu_formula.body {
        context_dsl::ast::FormulaExpr::MuCalculus(_) => {}
        _ => panic!("Expected μ-calculus formula"),
    }

    let realized = realize_test_context(source)?;
    assert!(realized.formulas.contains_key("ltl_formula"));
    assert!(realized.formulas.contains_key("mu_formula"));

    Ok(())
}

// ============================================================================
// End-to-End Evaluation Tests (translate → evaluate over a known CLTS)
// ============================================================================

/// Build a small linear CLTS: s0 →a→ s1 →b→ s2 (s2 has a self-loop on b).
/// Predicates: Target = {s2}, Safe = {s0, s1}.
fn three_state_clts() -> (Context, Environment) {
    use mununu_core::clts::DefaultLabelIdx;
    use mununu_core::clts::DefaultStateIdx;
    let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    builder.state("s0");
    builder.state("s1");
    builder.state("s2");
    let s0 = builder.state_id_or_insert("s0").unwrap();
    builder.initial_state_id(s0);
    let s1 = builder.state_id_or_insert("s1").unwrap();
    let s2 = builder.state_id_or_insert("s2").unwrap();
    let a = builder.labels().intern(["a"]).unwrap();
    let b = builder.labels().intern(["b"]).unwrap();
    builder.transition_ids(s0, &[a], s1);
    builder.transition_ids(s1, &[b], s2);
    builder.transition_ids(s2, &[b], s2); // self-loop

    let clts = builder.build().unwrap();
    let n = clts.state_count();

    let ctx = Context::builder()
        .register_clts("m", clts)
        .finish_with_checks()
        .unwrap();

    let mut target = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    target.set(s2.index(), true);
    let mut safe = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    safe.set(s0.index(), true);
    safe.set(s1.index(), true);
    let env = Environment::new(n)
        .with_predicate("Target", target)
        .with_predicate("Safe", safe);
    (ctx, env)
}

/// `G(!Target)` — `ν X. (!Target ∧ [] X)`.
/// No state satisfies: all paths from s0 and s1 eventually force a visit to
/// s2 (Target) via the s2 self-loop, so even s0 and s1 cannot guarantee
/// always staying outside Target.
#[test]
fn ltl_g_evaluated_over_clts() -> TestResult {
    let (ctx, env) = three_state_clts();
    let ltl = ltl::parse("G (!Target)")?;
    let mu = ltl::translator::translate(&ltl)?;
    let result = ctx.evaluate_mu("m", &mu, &env, None)?;
    // s2 does not satisfy (it IS Target); s1 only successor is s2 so fails too;
    // s0 only successor is s1 which fails, so s0 also fails.
    assert_eq!(
        result.count_ones(),
        0,
        "no state satisfies G(!Target) in this clts"
    );
    Ok(())
}

/// `F(Target)` — `μ X. (Target ∨ <> X)`.
/// All states satisfy because Target (s2) is reachable from everywhere.
#[test]
fn ltl_f_evaluated_over_clts() -> TestResult {
    let (ctx, env) = three_state_clts();
    let ltl = ltl::parse("F Target")?;
    let mu = ltl::translator::translate(&ltl)?;
    let result = ctx.evaluate_mu("m", &mu, &env, None)?;
    assert_eq!(result.count_ones(), 3, "all states can reach Target");
    Ok(())
}

/// `Safe U Target` — `μ X. (Target ∨ (Safe ∧ <> X))`.
/// s0 and s1 are Safe and lead to Target, s2 is Target: all satisfy.
#[test]
fn ltl_until_evaluated_over_clts() -> TestResult {
    let (ctx, env) = three_state_clts();
    let ltl = ltl::parse("Safe U Target")?;
    let mu = ltl::translator::translate(&ltl)?;
    let result = ctx.evaluate_mu("m", &mu, &env, None)?;
    assert_eq!(result.count_ones(), 3, "Safe U Target holds for all states");
    Ok(())
}

/// `GF(Target)` — `ν NuX. (μ MuX. (Target ∨ <> MuX)) ∧ [] NuX`.
/// All states satisfy: from every state, the only path eventually reaches s2
/// (Target) via s2's self-loop. Since all successors also satisfy, the nu
/// fixpoint closes over all three states.
#[test]
fn ltl_gf_evaluated_over_clts() -> TestResult {
    let (ctx, env) = three_state_clts();
    let ltl = ltl::parse("G F Target")?;
    let mu = ltl::translator::translate(&ltl)?;
    let result = ctx.evaluate_mu("m", &mu, &env, None)?;
    // All states: the only infinite path through this CLTS visits s2 (Target)
    // infinitely often (s2 self-loops), so GF(Target) holds everywhere.
    assert_eq!(result.count_ones(), 3, "all states satisfy GF(Target)");
    Ok(())
}

/// Response: `G(Safe -> F Target)` applied to a 2-state machine where
/// Safe=initial and Target=final with a direct transition.
/// Both states should satisfy because Target is immediately reachable.
#[test]
fn ltl_response_evaluated_over_two_state_clts() -> TestResult {
    use mununu_core::clts::DefaultLabelIdx;
    use mununu_core::clts::DefaultStateIdx;
    let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    builder.state("req");
    builder.state("done");
    let req = builder.state_id_or_insert("req").unwrap();
    builder.initial_state_id(req);
    let done = builder.state_id_or_insert("done").unwrap();
    let go = builder.labels().intern(["go"]).unwrap();
    builder.transition_ids(req, &[go], done);
    builder.transition_ids(done, &[go], done); // done loops

    let clts = builder.build().unwrap();
    let n = clts.state_count();
    let ctx = Context::builder()
        .register_clts("r", clts)
        .finish_with_checks()
        .unwrap();

    let mut req_set = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    req_set.set(req.index(), true);
    let mut done_set = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    done_set.set(done.index(), true);
    let env = Environment::new(n)
        .with_predicate("Safe", req_set)
        .with_predicate("Target", done_set);

    let ltl = ltl::parse("G (Safe -> F Target)")?;
    let mu = ltl::translator::translate(&ltl)?;
    let result = ctx.evaluate_mu("r", &mu, &env, None)?;
    assert_eq!(
        result.count_ones(),
        2,
        "both states satisfy G(Safe->F Target)"
    );
    Ok(())
}
