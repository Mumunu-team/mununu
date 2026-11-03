use std::collections::HashSet;

use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
use mununu_core::context_dsl;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type TestClts = Clts<DefaultStateIdx, DefaultLabelIdx>;

/// Collects all (source_name, target_name, label) triples from a CLTS.
fn collect_edges(clts: &TestClts) -> HashSet<(String, String, String)> {
    let mut edges = HashSet::new();
    for sid in clts.states() {
        let src = clts.state_name(sid).unwrap().to_string();
        for transition in clts.outgoing(sid) {
            let tgt = clts.state_name(transition.target()).unwrap().to_string();
            for lid in transition.labels() {
                if let Some(payload) = clts.label_payload(*lid) {
                    let label = payload.join(",");
                    edges.insert((src.clone(), tgt.clone(), label));
                }
            }
        }
    }
    edges
}

/// Collects all state names from a CLTS.
fn collect_state_names(clts: &TestClts) -> HashSet<String> {
    clts.states()
        .filter_map(|sid| clts.state_name(sid).map(|s| s.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// Feature 1: State Groups & Wildcards
// ---------------------------------------------------------------------------

#[test]
fn state_group_expands_transitions() -> TestResult {
    const SOURCE: &str = r#"
context groups_demo {
    alphabet {
        label act;
        label reset;
    }

    automata {
        automaton Plant {
            states {
                state A initial;
                state B;
                state C;
                state Reset;
            }

            state_groups {
                group active = { B, C };
            }

            transitions {
                transition A -> B on label act;
                transition A -> C on label act;
                transition group active -> Reset on label reset;
                transition Reset -> A on label act;
            }
        }
    }

    mu_formulas {
        formula can_reach_reset {
            over Plant;
            body = mu X. (Reset || <> X);
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    let clts = realized.context.clts("Plant").unwrap();
    assert_eq!(clts.state_count(), 4);

    let formula = realized.formulas.get("can_reach_reset").unwrap();
    let env = realized.environment_for("Plant");
    let result = realized
        .context
        .evaluate_mu("Plant", &formula.formula, &env, None)?;
    // All 4 states should be able to reach Reset
    assert_eq!(result.count_ones(), 4);

    Ok(())
}

#[test]
fn wildcard_expands_to_all_states() -> TestResult {
    const SOURCE: &str = r#"
context wildcard_demo {
    alphabet {
        label go;
        label emergency;
    }

    automata {
        automaton System {
            states {
                state S0 initial;
                state S1;
                state S2;
                state Halt;
            }

            transitions {
                transition S0 -> S1 on label go;
                transition S1 -> S2 on label go;
                transition S2 -> S0 on label go;
                transition wildcard "*" -> Halt on label emergency;
            }
        }
    }

    mu_formulas {
        formula halt_reachable {
            over System;
            body = mu X. (Halt || <> X);
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    let clts = realized.context.clts("System").unwrap();
    assert_eq!(clts.state_count(), 4);

    let formula = realized.formulas.get("halt_reachable").unwrap();
    let env = realized.environment_for("System");
    let result = realized
        .context
        .evaluate_mu("System", &formula.formula, &env, None)?;
    // Every state should be able to reach Halt via emergency
    assert_eq!(result.count_ones(), 4);

    Ok(())
}

// ---------------------------------------------------------------------------
// Feature 2: Process Templates (Parameterized Automata)
// ---------------------------------------------------------------------------

#[test]
fn parameterized_automaton_expands() -> TestResult {
    const SOURCE: &str = r#"
context template_demo {
    constants {
        const N = 1;
    }

    ranges {
        range Clients = 0 ..= N;
    }

    alphabet {
        label req_0;
        label req_1;
        label grant_0;
        label grant_1;
    }

    automata {
        automaton Client {
            parameters {
                param i in Clients;
            }

            states {
                state Idle initial;
                state Requesting;
            }

            controllable {
                label req[i];
            }

            transitions {
                transition Idle -> Requesting on label req[i];
                transition Requesting -> Idle on label grant[i];
            }
        }
    }

    mu_formulas {
        formula client0_can_request {
            over Client_0;
            body = mu X. (Requesting || <> X);
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    // Should have expanded Client into Client_0 and Client_1
    assert!(
        realized.context.clts("Client_0").is_some(),
        "Client_0 should exist"
    );
    assert!(
        realized.context.clts("Client_1").is_some(),
        "Client_1 should exist"
    );

    let clts0 = realized.context.clts("Client_0").unwrap();
    assert_eq!(clts0.state_count(), 2);

    let formula = realized.formulas.get("client0_can_request").unwrap();
    let env = realized.environment_for("Client_0");
    let result = realized
        .context
        .evaluate_mu("Client_0", &formula.formula, &env, None)?;
    assert_eq!(result.count_ones(), 2);

    Ok(())
}

#[test]
fn composition_with_indexed_members() -> TestResult {
    const SOURCE: &str = r#"
context indexed_members {
    constants {
        const N = 1;
    }

    ranges {
        range R = 0 ..= N;
    }

    alphabet {
        label req_0;
        label req_1;
        label ack;
    }

    automata {
        automaton Worker {
            parameters {
                param i in R;
            }

            states {
                state Idle initial;
                state Busy;
            }

            controllable {
                label req[i];
            }

            transitions {
                transition Idle -> Busy on label req[i];
                transition Busy -> Idle on label ack;
            }
        }

        automaton Server {
            states {
                state Ready initial;
                state Done;
            }

            transitions {
                transition Ready -> Done on label ack;
                transition Done -> Ready on label ack;
            }
        }
    }

    composition {
        asynchronous System {
            members [Worker[0], Worker[1], Server];
        }
    }

    mu_formulas {
        formula system_safety {
            over System;
            body = nu X. ([] X);
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    assert!(
        realized.context.clts("System").is_some(),
        "System composition should exist"
    );

    let formula = realized.formulas.get("system_safety").unwrap();
    let env = realized.environment_for("System");
    let result = realized
        .context
        .evaluate_mu("System", &formula.formula, &env, None)?;
    assert!(result.count_ones() > 0);

    Ok(())
}

// ---------------------------------------------------------------------------
// Feature 3: Enum Types
// ---------------------------------------------------------------------------

#[test]
fn enum_type_parses_and_resolves() -> TestResult {
    const SOURCE: &str = r#"
context enum_demo {
    alphabet {
        label activate;
        label deactivate;
    }

    enums {
        enum Status { idle, active, error };
    }

    automata {
        automaton Controller {
            variables {
                var mode: Status = idle;
            }

            states {
                state S0 initial;
                state S1;
            }

            transitions {
                transition S0 -> S1 on label activate
                    guard mode == idle
                    effects { mode = active; };
                transition S1 -> S0 on label deactivate
                    guard mode == active
                    effects { mode = idle; };
            }
        }
    }

    mu_formulas {
        formula can_activate {
            over Controller;
            body = mu X. (S1 || <> X);
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;

    // Check that enums were parsed
    assert_eq!(doc.enums.len(), 1);
    assert_eq!(doc.enums[0].name.name, "Status");
    assert_eq!(doc.enums[0].variants.len(), 3);

    // Check variable type is parsed as enum reference
    let var = &doc.automata[0].variables[0];
    assert_eq!(
        var.ty,
        mununu_core::context_dsl::ast::TypeName::Enum("Status".to_string())
    );

    // Realize and verify
    let realized = context_dsl::realize::realize(&doc, &[])?;
    let clts = realized.context.clts("Controller").unwrap();
    // Unrolling produces S0_mode_0 (idle) and S1_mode_1 (active)
    assert_eq!(clts.state_count(), 2);

    let formula = realized.formulas.get("can_activate").unwrap();
    let env = realized.environment_for("Controller");
    let result = realized
        .context
        .evaluate_mu("Controller", &formula.formula, &env, None)?;
    assert!(result.count_ones() > 0);

    Ok(())
}

#[test]
fn enum_section_parses_correctly() -> TestResult {
    const SOURCE: &str = r#"
context enum_parse_test {
    enums {
        enum Color { red, green, blue };
        enum Direction { north, south, east, west };
    }

    automata {
        automaton Stub {
            states { state S initial; }
            transitions { transition S -> S on epsilon; }
        }
    }
}
"#;

    let doc = context_dsl::parser::parse(SOURCE)?;
    assert_eq!(doc.enums.len(), 2);

    // Enums should be sorted by canonicalization
    assert_eq!(doc.enums[0].name.name, "Color");
    assert_eq!(doc.enums[1].name.name, "Direction");

    assert_eq!(doc.enums[0].variants.len(), 3);
    assert_eq!(doc.enums[1].variants.len(), 4);

    Ok(())
}

// ---------------------------------------------------------------------------
// Graph Validation: exact states and transitions for each feature
// ---------------------------------------------------------------------------

#[test]
fn state_group_graph_has_all_transitions() -> TestResult {
    let source = std::fs::read_to_string("tests/fixtures/14_state_groups.ctxdsl")?;
    let doc = context_dsl::parser::parse(&source)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    let clts = realized.context.clts("Process").unwrap();

    // 6 states
    let names = collect_state_names(clts);
    assert_eq!(names.len(), 6, "expected 6 states, got: {:?}", names);
    for expected in &[
        "Init",
        "Running",
        "Complete",
        "ErrorMinor",
        "ErrorMajor",
        "Shutdown",
    ] {
        assert!(names.contains(*expected), "missing state: {}", expected);
    }

    let edges = collect_edges(clts);

    // Normal transitions
    assert!(edges.contains(&edge("Init", "Running", "start")));
    assert!(edges.contains(&edge("Running", "Complete", "step")));
    assert!(edges.contains(&edge("Complete", "Init", "start")));
    assert!(edges.contains(&edge("Running", "ErrorMinor", "fault")));
    assert!(edges.contains(&edge("ErrorMinor", "ErrorMajor", "fault")));

    // State group expansion: error_states -> Init on reset
    assert!(
        edges.contains(&edge("ErrorMinor", "Init", "reset")),
        "state group should expand ErrorMinor -> Init"
    );
    assert!(
        edges.contains(&edge("ErrorMajor", "Init", "reset")),
        "state group should expand ErrorMajor -> Init"
    );

    // Wildcard expansion: every state -> Shutdown on emergency
    for state in &[
        "Init",
        "Running",
        "Complete",
        "ErrorMinor",
        "ErrorMajor",
        "Shutdown",
    ] {
        assert!(
            edges.contains(&edge(state, "Shutdown", "emergency")),
            "wildcard should expand {} -> Shutdown",
            state
        );
    }

    // Total: 5 normal + 2 group + 6 wildcard = 13
    assert_eq!(
        edges.len(),
        13,
        "expected 13 transitions, got: {}",
        edges.len()
    );

    Ok(())
}

#[test]
fn template_graph_has_correct_automata_and_labels() -> TestResult {
    let source = std::fs::read_to_string("tests/fixtures/15_process_templates.ctxdsl")?;
    let doc = context_dsl::parser::parse(&source)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    // Client_0 should have 3 states with indexed labels
    let clts0 = realized
        .context
        .clts("Client_0")
        .expect("Client_0 should exist");
    assert_eq!(clts0.state_count(), 3);
    let edges0 = collect_edges(clts0);
    assert!(
        edges0.contains(&edge("Idle", "Waiting", "req_0")),
        "Client_0 should have Idle -> Waiting on req_0"
    );
    assert!(
        edges0.contains(&edge("Waiting", "Holding", "grant_0")),
        "Client_0 should have Waiting -> Holding on grant_0"
    );
    assert!(
        edges0.contains(&edge("Holding", "Idle", "release_0")),
        "Client_0 should have Holding -> Idle on release_0"
    );

    // Client_1 should have 3 states with different indexed labels
    let clts1 = realized
        .context
        .clts("Client_1")
        .expect("Client_1 should exist");
    assert_eq!(clts1.state_count(), 3);
    let edges1 = collect_edges(clts1);
    assert!(
        edges1.contains(&edge("Idle", "Waiting", "req_1")),
        "Client_1 should have Idle -> Waiting on req_1"
    );
    assert!(
        edges1.contains(&edge("Waiting", "Holding", "grant_1")),
        "Client_1 should have Waiting -> Holding on grant_1"
    );
    assert!(
        edges1.contains(&edge("Holding", "Idle", "release_1")),
        "Client_1 should have Holding -> Idle on release_1"
    );

    // Verify controllability: req_0 should be controllable in Client_0
    let req0_controllable = clts0.controllable_alphabet().iter().any(|lid| {
        clts0
            .label_payload(*lid)
            .is_some_and(|p| p.contains(&"req_0".to_string()))
    });
    assert!(
        req0_controllable,
        "req_0 should be controllable in Client_0"
    );

    // grant_0 should NOT be controllable (it's uncontrollable)
    let grant0_controllable = clts0.controllable_alphabet().iter().any(|lid| {
        clts0
            .label_payload(*lid)
            .is_some_and(|p| p.contains(&"grant_0".to_string()))
    });
    assert!(
        !grant0_controllable,
        "grant_0 should not be controllable in Client_0"
    );

    // Arbiter should have 3 states
    let arbiter = realized
        .context
        .clts("Arbiter")
        .expect("Arbiter should exist");
    assert_eq!(arbiter.state_count(), 3);

    // System composition should exist with > 1 state
    let system = realized
        .context
        .clts("System")
        .expect("System composition should exist");
    assert!(
        system.state_count() > 1,
        "System should have multiple states, got {}",
        system.state_count()
    );

    Ok(())
}

#[test]
fn enum_graph_has_unrolled_transitions() -> TestResult {
    let source = std::fs::read_to_string("tests/fixtures/16_enums.ctxdsl")?;
    let doc = context_dsl::parser::parse(&source)?;
    let realized = context_dsl::realize::realize(&doc, &[])?;

    let clts = realized.context.clts("Light").unwrap();
    let names = collect_state_names(clts);
    let edges = collect_edges(clts);

    // All state names should follow the pattern Location_mode_N
    for name in &names {
        assert!(
            name.contains("_mode_"),
            "state '{}' should contain '_mode_' (unrolled enum variable)",
            name
        );
    }

    // Red_mode_0 (normal) should exist as the initial state
    assert!(
        names.contains("Red_mode_0"),
        "Red_mode_0 (normal) should exist"
    );

    // Normal cycle: Red_mode_0 -> Green_mode_0 on tick (mode == normal)
    assert!(
        edges.contains(&edge("Red_mode_0", "Green_mode_0", "tick")),
        "Red(normal) -> Green(normal) on tick should exist"
    );

    // Green_mode_0 -> Yellow_mode_0 on tick
    assert!(
        edges.contains(&edge("Green_mode_0", "Yellow_mode_0", "tick")),
        "Green(normal) -> Yellow(normal) on tick should exist"
    );

    // Yellow_mode_0 -> Red_mode_0 on tick
    assert!(
        edges.contains(&edge("Yellow_mode_0", "Red_mode_0", "tick")),
        "Yellow(normal) -> Red(normal) on tick should exist"
    );

    // Toggle: Red_mode_0 -> Red_mode_1 on toggle (normal -> flashing)
    assert!(
        edges.contains(&edge("Red_mode_0", "Red_mode_1", "toggle")),
        "Red(normal) -> Red(flashing) on toggle should exist"
    );

    // Toggle back: Red_mode_1 -> Red_mode_0 on toggle (flashing -> normal)
    assert!(
        edges.contains(&edge("Red_mode_1", "Red_mode_0", "toggle")),
        "Red(flashing) -> Red(normal) on toggle should exist"
    );

    // Power off: Red_mode_0 -> Red_mode_2 on power_off (normal -> off)
    assert!(
        edges.contains(&edge("Red_mode_0", "Red_mode_2", "power_off")),
        "Red(normal) -> Red(off) on power_off should exist"
    );

    // Power on: Red_mode_2 -> Red_mode_0 on power_on (off -> normal)
    assert!(
        edges.contains(&edge("Red_mode_2", "Red_mode_0", "power_on")),
        "Red(off) -> Red(normal) on power_on should exist"
    );

    // Flashing: Red_mode_1 -> Red_mode_1 on tick (stays red in flashing mode)
    assert!(
        edges.contains(&edge("Red_mode_1", "Red_mode_1", "tick")),
        "Red(flashing) -> Red(flashing) on tick should exist"
    );

    Ok(())
}

fn edge(src: &str, tgt: &str, label: &str) -> (String, String, String) {
    (src.to_string(), tgt.to_string(), label.to_string())
}
