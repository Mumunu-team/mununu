use mununu_core::context::Context;
use mununu_core::context_dsl::{self, ResolvedControllerOptions, ast::TransitionLabel};
use mununu_core::mu_calculus::{Environment, parser as mu_parser};

use mununu_core::clts::Clts;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn controller_options_parsed_from_dsl_drive_synthesis() -> TestResult {
    const SOURCE: &str = r#"
context integration_demo {
    alphabet {
        label step;
        label wait;
    }

    automata {
        automaton Plant {
            states {
                state init initial;
                state idle_a;
                state idle_b;
            }

            transitions {
                transition init -> idle_a on label step;
                transition init -> idle_b on label step, label wait;
                transition idle_a -> idle_a on label wait;
                transition idle_b -> idle_b on label wait;
            }
        }
    }

    mu_formulas {
        formula keep_all {
            over Plant;
            body = true;
        }

        formula impossible {
            over Plant;
            body = false;
        }
    }

    controllers {
        controller MinimizeOk {
            source Plant;
            satisfying keep_all;
            minimize = true;
            diagnostics {
                proof_obligations = false;
            }
        }

        controller FailDiag {
            source Plant;
            satisfying impossible;
            diagnostics {
                counterexample = true;
                proof_obligations = true;
                max_counter_traces = 1;
            }
        }

        controller FailSuppressed {
            source Plant;
            satisfying impossible;
            diagnostics {
                proof_obligations = false;
            }
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;

    let plant_automaton = doc
        .automata
        .iter()
        .find(|auto| auto.name.name == "Plant")
        .expect("Plant automaton present");
    let multi = plant_automaton
        .transitions
        .iter()
        .find(|tr| !tr.additional_labels.is_empty())
        .expect("multi label transition");
    assert_eq!(multi.additional_labels.len(), 1);
    match &multi.additional_labels[0] {
        TransitionLabel::Named { name, .. } => assert_eq!(name.name, "wait"),
        other => panic!("expected named label, got {other:?}"),
    }

    // Build the plant automaton manually (the integration focuses on controller options).
    let mut builder = Clts::builder();
    let step = builder.labels().intern(["step"])?;
    let wait = builder.labels().intern(["wait"])?;
    builder.state("init");
    builder.initial("init");
    builder.state("idle_a");
    builder.state("idle_b");
    builder.transition("init", &[step], "idle_a");
    builder.transition("init", &[step], "idle_b");
    builder.transition("idle_a", &[wait], "idle_a");
    builder.transition("idle_b", &[wait], "idle_b");
    let plant = builder.build()?;

    let context = Context::builder()
        .register_clts("Plant", plant)
        .finish_with_checks()?;

    let mut formula_map = std::collections::HashMap::new();
    for formula in &doc.mu_formulas {
        let raw = match &formula.body {
            context_dsl::ast::FormulaExpr::MuCalculus(mu_expr) => &mu_expr.raw,
            context_dsl::ast::FormulaExpr::Ltl(_) | context_dsl::ast::FormulaExpr::Template(_) => {
                return Err("LTL formulas and template clauses not supported in this test".into());
            }
        };
        let parsed = mu_parser::parse(raw)?;
        formula_map.insert(formula.name.name.clone(), parsed);
    }

    // Controller requesting minimisation with proof obligations disabled.
    let ctrl_min = doc
        .controllers
        .iter()
        .find(|c| c.name.name == "MinimizeOk")
        .expect("MinimizeOk controller");
    let resolved_min = ResolvedControllerOptions::from_ast(&ctrl_min.options);
    assert!(resolved_min.minimize(), "minimize flag should be true");
    assert!(
        !resolved_min
            .diagnostics()
            .expect("resolved diagnostics")
            .proof_obligations
    );
    let env_min = Environment::new(context.clts("Plant").unwrap().state_count());
    let synth_min = context.synthesise_controller_with_options(
        "Plant",
        formula_map
            .get(&ctrl_min.formula.name)
            .expect("keep_all formula"),
        &env_min,
        resolved_min.as_synthesis_options(),
    )?;
    assert!(synth_min.realizable);
    assert!(synth_min.diagnostics.minimization.is_some());
    assert!(synth_min.diagnostics.proof_obligations.is_empty());

    // Controller requesting counterexample + proof obligations for an unrealizable spec.
    let ctrl_fail = doc
        .controllers
        .iter()
        .find(|c| c.name.name == "FailDiag")
        .expect("FailDiag controller");
    let resolved_fail = ResolvedControllerOptions::from_ast(&ctrl_fail.options);
    assert!(
        resolved_fail
            .diagnostics()
            .expect("resolved diagnostics")
            .proof_obligations
    );
    let env_fail = Environment::new(context.clts("Plant").unwrap().state_count());
    let synth_fail = context.synthesise_controller_with_options(
        "Plant",
        formula_map
            .get(&ctrl_fail.formula.name)
            .expect("impossible formula"),
        &env_fail,
        resolved_fail.as_synthesis_options(),
    )?;
    assert!(!synth_fail.realizable);
    assert!(!synth_fail.diagnostics.proof_obligations.is_empty());
    assert!(synth_fail.diagnostics.counterstrategy.is_some());

    // Controller suppressing proof obligations should not emit them even when unrealizable.
    let ctrl_suppress = doc
        .controllers
        .iter()
        .find(|c| c.name.name == "FailSuppressed")
        .expect("FailSuppressed controller");
    let resolved_suppress = ResolvedControllerOptions::from_ast(&ctrl_suppress.options);
    assert!(
        !resolved_suppress
            .diagnostics()
            .expect("resolved diagnostics")
            .proof_obligations
    );
    let env_suppress = Environment::new(context.clts("Plant").unwrap().state_count());
    let synth_suppress = context.synthesise_controller_with_options(
        "Plant",
        formula_map
            .get(&ctrl_suppress.formula.name)
            .expect("impossible formula"),
        &env_suppress,
        resolved_suppress.as_synthesis_options(),
    )?;
    assert!(!synth_suppress.realizable);
    assert!(synth_suppress.diagnostics.proof_obligations.is_empty());
    assert!(synth_suppress.diagnostics.counterstrategy.is_none());

    Ok(())
}

#[test]
fn user_defined_state_predicate_evaluates() -> TestResult {
    const SOURCE: &str = r#"
context light_demo {
    alphabet { label next; }

    automata {
        automaton Light {
            states { state Green initial; state Red; }
            transitions {
                transition Green -> Red on label next;
                transition Red -> Green on label next;
            }
            predicates {
                predicate is_green = state Green;
            }
        }
    }

    mu_formulas {
        formula eventually_green {
            over Light;
            body = mu Reach. (is_green || <next> Reach);
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;
    let realized = context_dsl::realize_context(&doc, &[])?;
    let formula = realized
        .formulas
        .get("eventually_green")
        .expect("formula present");
    let env = realized.environment_for("Light");

    let predicate_bits = env
        .predicate("is_green")
        .expect("is_green predicate should be available");
    assert_eq!(predicate_bits.count_ones(), 1, "only Green state matches");

    let result = realized
        .context
        .evaluate_mu("Light", &formula.formula, &env, None)?;
    assert!(
        result.count_ones() > 0,
        "eventually_green should hold in at least one state"
    );
    let clts = realized.context.clts("Light").unwrap();
    let initial = *clts
        .initial_states()
        .iter()
        .next()
        .expect("Light has an initial state");
    assert!(
        result.get(initial.index()).is_some_and(|bit| *bit),
        "initial state should satisfy eventually_green"
    );
    Ok(())
}

#[test]
fn predicates_survive_composition() -> TestResult {
    const SOURCE: &str = r#"
context composed_demo {
    alphabet {
        label master_tick;
        label worker_tick;
    }

    automata {
        automaton Master {
            states { state Idle initial; state Busy; }
            transitions {
                transition Idle -> Busy on label master_tick;
                transition Busy -> Idle on label master_tick;
            }
            predicates {
                predicate master_idle = state Idle;
            }
        }

        automaton Worker {
            states { state Wait initial; state Run; }
            transitions {
                transition Wait -> Run on label worker_tick;
                transition Run -> Wait on label worker_tick;
            }
            predicates {
                predicate worker_wait = state Wait;
            }
        }
    }

    composition {
        synchronous Duo { members [Master, Worker]; }
    }

    mu_formulas {
        formula master_idle_check {
            over Master;
            body = master_idle;
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;
    let realized = context_dsl::realize_context(&doc, &[])?;

    let master_env = realized.environment_for("Master");
    let master_bits = master_env
        .predicate("master_idle")
        .expect("master predicate present");
    assert_eq!(master_bits.count_ones(), 1);

    let worker_env = realized.environment_for("Worker");
    let worker_bits = worker_env
        .predicate("worker_wait")
        .expect("worker predicate present");
    assert_eq!(worker_bits.count_ones(), 1);

    let formula = realized
        .formulas
        .get("master_idle_check")
        .expect("formula present");
    let result = realized
        .context
        .evaluate_mu("Master", &formula.formula, &master_env, None)?;
    assert!(
        result.count_ones() >= 1,
        "master_idle predicate should evaluate to true in Idle"
    );
    Ok(())
}

#[test]
fn guarded_modal_works_in_ctxdsl() -> TestResult {
    const SOURCE: &str = r#"
context guard_demo {
    alphabet {
        label tick;
        label idle;
    }

    automata {
        automaton System {
            states {
                state S0 initial;
                state S1;
                state S2;
            }
            transitions {
                transition S0 -> S1 on label tick;
                transition S0 -> S2 on label idle;
                transition S1 -> S1 on label idle;
                transition S2 -> S2 on label idle;
            }
        }
    }

    mu_formulas {
        // States that have at least one outgoing `tick` transition.
        formula tick_enabled {
            over System;
            body = < ( labels = {tick} ) > true;
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;
    let realized = context_dsl::realize_context(&doc, &[])?;

    let clts = realized.context.clts("System").unwrap();
    let formula = realized
        .formulas
        .get("tick_enabled")
        .expect("formula present");
    let env = realized.environment_for("System");

    let result = realized
        .context
        .evaluate_mu("System", &formula.formula, &env, None)?;

    let s0 = clts.state_id("S0")?;
    let s1 = clts.state_id("S1")?;
    let s2 = clts.state_id("S2")?;

    assert!(
        result.get(s0.index()).is_some_and(|bit| *bit),
        "S0 should satisfy <labels=tick> true (has a tick-successor)"
    );
    assert!(
        !result.get(s1.index()).is_some_and(|bit| *bit),
        "S1 should not satisfy <labels=tick> true (no tick-successor)"
    );
    assert!(
        !result.get(s2.index()).is_some_and(|bit| *bit),
        "S2 should not satisfy <labels=tick> true (no tick-successor)"
    );

    Ok(())
}

#[test]
fn synchronous_guard_invariant_holds() -> TestResult {
    const SOURCE: &str = r#"
context sync_guard_demo {
    alphabet {
        label step;
        label fault;
    }

    automata {
        automaton System {
            controllable { label step; }

            states {
                state Safe initial;
                state Error;
            }

            transitions {
                transition Safe -> Safe on label step;
                transition Safe -> Error on label fault;
                transition Error -> Error on label step;
                transition Error -> Error on label fault;
            }

            predicates {
                predicate is_safe = state Safe;
            }
        }
    }

    mu_formulas {
        // Real-world style invariant: under controllable `step` actions, the system
        // can stay in safe states forever (controller never chooses a bad `step`).
        formula safe_under_step {
            over System;
            body = nu X. (is_safe and [ ( labels = {step}, ctrl = controllable ) ] X);
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;
    let realized = context_dsl::realize_context(&doc, &[])?;

    let clts = realized.context.clts("System").unwrap();
    let formula = realized
        .formulas
        .get("safe_under_step")
        .expect("formula present");
    let env = realized.environment_for("System");

    let result = realized
        .context
        .evaluate_mu("System", &formula.formula, &env, None)?;

    let safe = clts.state_id("Safe")?;
    let error = clts.state_id("Error")?;

    assert!(
        result.get(safe.index()).is_some_and(|bit| *bit),
        "Safe should satisfy the safety invariant under controllable step"
    );
    assert!(
        !result.get(error.index()).is_some_and(|bit| *bit),
        "Error should not satisfy the safety invariant"
    );

    Ok(())
}

#[test]
fn asynchronous_guard_drainable_buffer() -> TestResult {
    const SOURCE: &str = r#"
context async_buffer_demo {
    alphabet {
        label produce;
        label consume;
    }

    automata {
        automaton Buffer {
            states {
                state Empty initial;
                state Partial;
                state Full;
            }
            transitions {
                transition Empty -> Partial on label produce;
                transition Partial -> Full on label produce;
                transition Full -> Partial on label consume;
                transition Partial -> Empty on label consume;
            }
            predicates {
                predicate buffer_full = state Full;
            }
        }
    }

    mu_formulas {
        // Real-world asynchronous property: from any state, there is a (possibly empty)
        // sequence of `consume` steps that can reach a non-full buffer.
        formula can_drain_buffer {
            over Buffer;
            body = mu Drain. ((not buffer_full) or < ( labels = {consume} ) > Drain);
        }
    }
}
"#;

    let doc = context_dsl::parse(SOURCE)?;
    let realized = context_dsl::realize_context(&doc, &[])?;

    let clts = realized.context.clts("Buffer").unwrap();
    let formula = realized
        .formulas
        .get("can_drain_buffer")
        .expect("formula present");
    let env = realized.environment_for("Buffer");

    let result = realized
        .context
        .evaluate_mu("Buffer", &formula.formula, &env, None)?;

    // In this topology, every state can reach a non-full state by zero or more `consume` steps,
    // so the property should hold in all states.
    for state in clts.states() {
        assert!(
            result.get(state.index()).is_some_and(|bit| *bit),
            "all buffer states should satisfy can_drain_buffer"
        );
    }

    Ok(())
}
