use super::parse;
use crate::context_dsl::{
    ResolvedControllerOptions,
    ast::{
        Automaton, BinaryOp, Composition, CompositionKind, ContextDoc, ExprKind, FormulaExpr,
        FormulaTargets, StateSelector, TransitionLabel,
    },
};
use crate::ltl::LtlFormula;

fn parse_context(input: &str) -> ContextDoc {
    parse(input).expect("context DSL should parse")
}

fn automaton_id(auto: &Automaton) -> &str {
    auto.meta.id.as_deref().unwrap_or(auto.name.name.as_str())
}

fn composition_id(comp: &Composition) -> &str {
    comp.meta.id.as_deref().unwrap_or(comp.name.name.as_str())
}

#[test]
fn parses_full_syntax_document() {
    let doc = parse_context(
        r#"
context syntax_demo {
    alphabet {
        label open = "OPEN";
        label close;
        label tick;
    }

    constants {
        const FLOORS = 3;
    }

    ranges {
        range floor = 0 ..= FLOORS;
    }

    automata {
        automaton Door {
            meta { id = "door"; comment = "primary door automaton"; }

            parameters {
                param target in floor;
            }

            alphabet {
                label open;
                label close;
                label tick[target];
            }

            variables {
                var current_floor : i64 = 0;
            }

            states {
                state Closed initial;
                state Moving;
                state Open[target in floor];
            }

            predicates {
                predicate door_closed = state Closed;
            }

            transitions {
                transition Closed -> Moving on label open;
                transition Moving -> Open[target] on label tick
                    guard current_floor < target
                    effects { current_floor = target; };
                transition Open[target] -> Closed on label close;
                transition Open[target] -> Moving on epsilon;
            }
        }
    }

    mu_formulas {
        formula door_safety {
            meta { id = "door_safety.v1"; comment = "door safety invariant"; }
            over Door;
            body = nu Safe. ([] close -> mu Continue. (<> open && Safe));
        }
    }
}
"#,
    );

    assert_eq!(doc.name.name, "syntax_demo");
    let alphabet: Vec<&str> = doc
        .alphabet
        .iter()
        .map(|entry| entry.name.name.as_str())
        .collect();
    assert_eq!(alphabet, vec!["close", "open", "tick"]);
    assert_eq!(doc.constants.len(), 1);
    assert_eq!(doc.ranges.len(), 1);

    let door = &doc.automata[0];
    assert_eq!(automaton_id(door), "door");
    assert_eq!(door.meta.comment.as_deref(), Some("primary door automaton"));
    assert_eq!(door.parameters.len(), 1);
    assert_eq!(door.alphabet.len(), 3);
    assert_eq!(door.states.len(), 3);
    assert_eq!(door.transitions.len(), 4);
    assert_eq!(door.predicates.len(), 1);
    assert_eq!(door.predicates[0].name.name, "door_closed");

    let move_transition = &door.transitions[1];
    assert!(matches!(
        move_transition.label,
        TransitionLabel::Named { ref name, .. } if name.name == "tick"
    ));
    let guard = move_transition.guard.as_ref().expect("guard expected");
    assert!(matches!(
        guard.kind,
        ExprKind::Binary {
            op: BinaryOp::Lt,
            ..
        }
    ));
    assert_eq!(move_transition.effects.len(), 1);
    assert_eq!(move_transition.effects[0].target.name, "current_floor");

    let epsilon_transition = &door.transitions[3];
    assert!(matches!(
        epsilon_transition.label,
        TransitionLabel::Epsilon(_)
    ));

    assert_eq!(doc.mu_formulas.len(), 1);
    let formula = &doc.mu_formulas[0];
    assert_eq!(formula.meta.id.as_deref(), Some("door_safety.v1"));
    assert_eq!(
        formula.meta.comment.as_deref(),
        Some("door safety invariant")
    );
    assert!(
        matches!(formula.targets, FormulaTargets::Named(ref names) if names.len() == 1 && names[0].name == "Door")
    );
    match &formula.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            assert!(
                mu_expr.raw.starts_with("nu Safe."),
                "formula body should be preserved"
            );
        }
        FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula in test"),
    }
}

#[test]
fn parses_synchronous_elevator_example() {
    let doc = parse_context(
        r#"
context elevator_sync {
    alphabet {
        label call;
        label move;
        label open;
        label close;
    }

    constants {
        const TOP = 10;
    }

    automata {
        automaton Cabin {
            meta { id = "elevator.cabin"; }

            variables {
                var floor : i64 = 0;
            }

            states {
                state Idle initial;
                state MovingUp;
                state DoorOpen;
            }

            transitions {
                transition Idle -> MovingUp on label move
                    guard floor < TOP
                    effects { floor = floor + 1; };
                transition MovingUp -> Idle on label move
                    guard floor == TOP;
                transition Idle -> DoorOpen on label open;
                transition DoorOpen -> Idle on label close;
            }
        }

        automaton Controller {
            meta { id = "elevator.controller"; }

            variables {
                var pending_calls : i64 = 0;
            }

            states {
                state Waiting initial;
                state Dispatching;
            }

            transitions {
                transition Waiting -> Dispatching on label call
                    effects { pending_calls = pending_calls + 1; };
                transition Dispatching -> Waiting on label move
                    guard pending_calls > 0
                    effects { pending_calls = pending_calls - 1; };
            }
        }
    }

    mu_formulas {
        formula reachability {
            over Cabin, Controller;
            body = mu Reach. (<> move && Reach);
        }
    }
}
"#,
    );

    assert_eq!(doc.constants[0].value, 10);
    let ids: Vec<&str> = doc.automata.iter().map(automaton_id).collect();
    assert_eq!(
        ids,
        vec!["elevator.cabin", "elevator.controller"],
        "automata should be sorted by meta id"
    );

    let cabin = &doc.automata[0];
    assert_eq!(cabin.states.len(), 3);
    assert_eq!(cabin.transitions.len(), 4);
    let move_guard = cabin
        .transitions
        .iter()
        .filter(|transition| {
            matches!(
                transition.label,
                TransitionLabel::Named { ref name, .. } if name.name == "move"
            )
        })
        .filter_map(|transition| transition.guard.as_ref())
        .find(|expr| {
            matches!(
                expr.kind,
                ExprKind::Binary {
                    op: BinaryOp::Lt,
                    ..
                }
            )
        })
        .expect("expected move guard");
    assert!(matches!(
        move_guard.kind,
        ExprKind::Binary {
            op: BinaryOp::Lt,
            ..
        }
    ));

    let controller = &doc.automata[1];
    assert_eq!(controller.states.len(), 2);
    assert_eq!(controller.transitions.len(), 2);
    let dispatch = controller
        .transitions
        .iter()
        .find(|transition| {
            matches!(
                transition.label,
                TransitionLabel::Named { ref name, .. } if name.name == "move"
            )
        })
        .expect("controller should synchronise on `move`");
    assert!(matches!(
        dispatch.guard,
        Some(ref expr) if matches!(expr.kind, ExprKind::Binary { op: BinaryOp::Gt, .. })
    ));

    let formula = &doc.mu_formulas[0];
    match &formula.targets {
        FormulaTargets::Named(names) => {
            let ordered: Vec<&str> = names.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(ordered, vec!["Cabin", "Controller"]);
        }
        FormulaTargets::All(_) => panic!("expected named targets, got FormulaTargets::All"),
    }
}

#[test]
fn parses_asynchronous_producer_consumer_example() {
    let doc = parse_context(
        r#"
context producer_consumer {
    alphabet {
        label enqueue;
        label dequeue;
        label tick;
    }

    constants {
        const CAPACITY = 2;
    }

    automata {
        automaton Producer {
            meta { id = "async.producer"; }

            variables {
                var generated : i64 = 0;
            }

            states {
                state Ready initial;
                state Waiting;
            }

            transitions {
                transition Ready -> Waiting on label enqueue
                    effects { generated = generated + 1; };
                transition Waiting -> Ready on label tick;
            }
        }

        automaton Consumer {
            meta { id = "async.consumer"; }

            variables {
                var consumed : i64 = 0;
            }

            states {
                state Idle initial;
                state Busy;
            }

            transitions {
                transition Idle -> Busy on label dequeue
                    guard consumed < CAPACITY;
                transition Busy -> Idle on label tick
                    effects { consumed = consumed + 1; };
            }
        }

        automaton Buffer {
            meta { id = "async.buffer"; }

            variables {
                var size : i64 = 0;
            }

            states {
                state Empty initial;
                state Partial;
                state Full;
            }

            transitions {
                transition Empty -> Partial on label enqueue
                    effects { size = size + 1; };
                transition Partial -> Full on label enqueue
                    guard size + 1 == CAPACITY
                    effects { size = CAPACITY; };
                transition Full -> Partial on label dequeue
                    effects { size = CAPACITY - 1; };
                transition Partial -> Empty on label dequeue
                    guard size == 1
                    effects { size = 0; };
            }
        }
    }

    mu_formulas {
        formula buffer_integrity {
            over Buffer;
            body = nu Safe. ([] dequeue -> mu Continue. ([] tick && Safe));
        }
    }
}
"#,
    );

    let ids: Vec<&str> = doc.automata.iter().map(automaton_id).collect();
    assert_eq!(
        ids,
        vec!["async.buffer", "async.consumer", "async.producer"],
        "canonical ordering honours meta ids"
    );
    assert_eq!(doc.automata.len(), 3);

    let buffer = doc
        .automata
        .iter()
        .find(|auto| automaton_id(auto) == "async.buffer")
        .expect("buffer automaton present");
    assert_eq!(buffer.transitions.len(), 4);
    let guard_transitions: usize = buffer
        .transitions
        .iter()
        .filter(|t| t.guard.is_some())
        .count();
    assert_eq!(guard_transitions, 2);

    let formula = &doc.mu_formulas[0];
    assert!(matches!(formula.targets, FormulaTargets::Named(ref names) if names.len() == 1));
    match &formula.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            assert!(
                mu_expr.raw.contains("nu Safe."),
                "formula body retained as raw μ-calculus text"
            );
        }
        FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula in test"),
    }
}

#[test]
fn parses_composition_definitions() {
    let doc = parse_context(
        r#"
context composition_demo {
    alphabet {
        label tick;
        label call;
        label ack;
    }

    automata {
        automaton Producer {
            states { state Prod initial; }
            transitions { transition Prod -> Prod on epsilon; }
        }

        automaton Consumer {
            states { state Cons initial; }
            transitions { transition Cons -> Cons on epsilon; }
        }

        automaton Queue {
            states {
                state Empty initial;
                state Busy;
            }
            transitions {
                transition Empty -> Busy on epsilon;
                transition Busy -> Empty on epsilon;
            }
        }

        automaton Controller {
            states { state Ctrl initial; }
            transitions { transition Ctrl -> Ctrl on epsilon; }
        }

        automaton Plant {
            states { state Plant initial; }
            transitions { transition Plant -> Plant on epsilon; }
        }

        automaton Base {
            states { state Base initial; }
            transitions { transition Base -> Base on epsilon; }
        }

        automaton Extension {
            states { state Ext initial; }
            transitions { transition Ext -> Ext on epsilon; }
        }
    }

    composition {
        synchronous SyncCore {
            meta { id = "A.sync"; }
            members [Queue, Controller, Plant];
        }
        asynchronous AsyncCompose {
            members [Queue, Producer];
        }
        superset SupersetDemo {
            meta { id = "Z.superset"; }
            members [Extension, Base];
        }
    }
}
"#,
    );

    let composition_summaries: Vec<(&str, CompositionKind)> = doc
        .compositions
        .iter()
        .map(|comp| (composition_id(comp), comp.kind))
        .collect();
    assert_eq!(
        composition_summaries,
        vec![
            ("A.sync", CompositionKind::Synchronous),
            ("AsyncCompose", CompositionKind::Asynchronous),
            ("Z.superset", CompositionKind::Superset),
        ]
    );

    let mut sync_members = None;
    let mut async_members = None;
    let mut superset_members = None;
    for comp in &doc.compositions {
        match comp.kind {
            CompositionKind::Synchronous => sync_members = Some(comp.members.clone()),
            CompositionKind::Asynchronous => async_members = Some(comp.members.clone()),
            CompositionKind::Superset => superset_members = Some(comp.members.clone()),
        }
    }

    let sync_members = sync_members.expect("synchronous composition present");
    let sync_member_names: Vec<&str> = sync_members.iter().map(|m| m.name.name.as_str()).collect();
    assert_eq!(sync_member_names, vec!["Controller", "Plant", "Queue"]);

    let async_members = async_members.expect("asynchronous composition present");
    let async_names: Vec<&str> = async_members.iter().map(|m| m.name.name.as_str()).collect();
    assert_eq!(async_names, vec!["Producer", "Queue"]);

    let superset_members = superset_members.expect("superset composition present");
    let superset_names: Vec<&str> = superset_members
        .iter()
        .map(|m| m.name.name.as_str())
        .collect();
    assert_eq!(superset_names, vec!["Base", "Extension"]);
}

#[test]
fn parses_controller_entries() {
    let doc = parse_context(
        r#"
context ctrl_spec {
    automata {
        automaton Plant {
            states { state Idle initial; }
            transitions { transition Idle -> Idle on epsilon; }
        }
        automaton Monitor {
            states { state Watch initial; }
            transitions { transition Watch -> Watch on epsilon; }
        }
    }

    mu_formulas {
        formula safe {
            over Plant;
            body = true;
        }
        formula live {
            over Monitor;
            body = false;
        }
    }

    controllers {
        controller PlantCtrl {
            meta { id = "ctrl.main"; }
            source Plant;
            satisfying safe;
            export "controllers/plant.ctxdsl";
        }
        controller MonitorCtrl {
            source Monitor;
            satisfying live;
        }
    }
}
"#,
    );

    assert_eq!(doc.controllers.len(), 2);
    let ordering: Vec<&str> = doc
        .controllers
        .iter()
        .map(|ctrl| ctrl.name.name.as_str())
        .collect();
    assert_eq!(ordering, vec!["MonitorCtrl", "PlantCtrl"]);

    let monitor = doc
        .controllers
        .iter()
        .find(|ctrl| ctrl.name.name == "MonitorCtrl")
        .unwrap();
    assert!(monitor.meta.id.is_none());
    assert_eq!(monitor.source.name, "Monitor");
    assert_eq!(monitor.formula.name, "live");
    assert!(monitor.export.is_none());
    assert!(monitor.options.minimize.is_none());
    assert!(monitor.options.diagnostics.is_none());

    let plant = doc
        .controllers
        .iter()
        .find(|ctrl| ctrl.name.name == "PlantCtrl")
        .unwrap();
    assert_eq!(plant.meta.id.as_deref(), Some("ctrl.main"));
    assert_eq!(plant.source.name, "Plant");
    assert_eq!(plant.formula.name, "safe");
    assert_eq!(plant.export.as_deref(), Some("controllers/plant.ctxdsl"));
    assert!(plant.options.minimize.is_none());
    assert!(plant.options.diagnostics.is_none());
}

#[test]
fn parses_state_groups_and_wildcards() {
    let doc = parse_context(
        r#"
 context wildcard_groups {
     automata {
         automaton Machine {
             state_groups {
                 group active = { Running, wildcard "Busy_*" };
             }
 
             states {
                 state Idle initial;
                 state Running;
                 state Busy_1;
                 state Busy_2;
             }
 
             transitions {
                 transition group active -> wildcard "Busy_*" on label tick;
                 transition wildcard "Busy_*" -> group active on label resume;
             }
         }
     }
 }
 "#,
    );

    assert_eq!(doc.automata.len(), 1);
    let machine = &doc.automata[0];
    assert_eq!(machine.state_groups.len(), 1);
    let group = &machine.state_groups[0];
    assert_eq!(group.name.name, "active");
    assert_eq!(group.members.len(), 2);
    match &group.members[0] {
        StateSelector::Named(state) => match state {
            crate::context_dsl::ast::StateRef::Simple(ident) => {
                assert_eq!(ident.name, "Running");
            }
            crate::context_dsl::ast::StateRef::Indexed { .. } => {
                panic!("expected simple state, got indexed reference");
            }
        },
        StateSelector::Wildcard(_) => panic!("expected named state, got wildcard"),
        StateSelector::Group(_) => panic!("expected named state, got group"),
    }
    match &group.members[1] {
        StateSelector::Wildcard(pattern) => assert_eq!(pattern.pattern, "Busy_*"),
        StateSelector::Named(_) => panic!("expected wildcard member, got named"),
        StateSelector::Group(_) => panic!("expected wildcard member, got group"),
    }

    assert_eq!(machine.transitions.len(), 2);
    let first = &machine.transitions[0];
    match &first.source {
        StateSelector::Group(ident) => assert_eq!(ident.name, "active"),
        StateSelector::Named(_) => panic!("expected group source, got named"),
        StateSelector::Wildcard(_) => panic!("expected group source, got wildcard"),
    }
    match &first.target {
        StateSelector::Wildcard(pattern) => assert_eq!(pattern.pattern, "Busy_*"),
        StateSelector::Named(_) => panic!("expected wildcard target, got named"),
        StateSelector::Group(_) => panic!("expected wildcard target, got group"),
    }
}

#[test]
fn parses_controller_options_block() {
    let doc = parse_context(
        r#"
context ctrl_options {
    automata {
        automaton Plant {
            states {
                state Init initial;
                state Idle;
            }
            transitions {
                transition Init -> Idle on epsilon;
                transition Idle -> Idle on epsilon;
            }
        }
    }

    mu_formulas {
        formula safe {
            over Plant;
            body = true;
        }
    }

    controllers {
        controller PlantCtrl {
            source Plant;
            satisfying safe;
            minimize = true;
            diagnostics {
                counterexample = true;
                deadlock_traces = false;
                max_counter_traces = 3;
                proof_obligations = false;
            }
        }
    }
}
"#,
    );

    assert_eq!(doc.controllers.len(), 1);
    let controller = &doc.controllers[0];
    assert_eq!(controller.name.name, "PlantCtrl");
    assert_eq!(controller.options.minimize, Some(true));
    let diagnostics = controller
        .options
        .diagnostics
        .as_ref()
        .expect("diagnostics parsed");
    assert_eq!(diagnostics.counterexample, Some(true));
    assert_eq!(diagnostics.deadlock_traces, Some(false));
    assert_eq!(diagnostics.max_counter_traces, Some(3));
    assert_eq!(diagnostics.proof_obligations, Some(false));

    let resolved = ResolvedControllerOptions::from_ast(&controller.options);
    assert!(resolved.minimize());
    let resolved_diag = resolved.diagnostics().expect("resolved diagnostics");
    assert!(resolved_diag.counterexample);
    assert!(!resolved_diag.deadlock_traces);
    assert_eq!(resolved_diag.max_counter_traces, Some(3));
    assert!(!resolved_diag.proof_obligations);
}

#[test]
fn parses_transition_with_multiple_labels() {
    let doc = parse_context(
        r#"
context multi_label {
    automata {
        automaton Machine {
            states {
                state Idle initial;
                state Busy;
            }

            transitions {
                transition Idle -> Busy on label start, label engage;
            }
        }
    }
}
"#,
    );

    let machine = &doc.automata[0];
    assert_eq!(machine.transitions.len(), 1);
    let transition = &machine.transitions[0];
    match &transition.label {
        TransitionLabel::Named { name, .. } => assert_eq!(name.name, "start"),
        TransitionLabel::Epsilon(_) => panic!("expected named label, got epsilon"),
    }
    assert_eq!(transition.additional_labels.len(), 1);
    match &transition.additional_labels[0] {
        TransitionLabel::Named { name, .. } => assert_eq!(name.name, "engage"),
        TransitionLabel::Epsilon(_) => panic!("expected named label, got epsilon"),
    }
}

#[test]
fn parses_state_with_multiple_variable_overrides() {
    let doc = parse_context(
        r#"
context multi_vars {
    automata {
        automaton Machine {
            variables {
                var ready : bool = false;
                var armed : bool = false;
            }

            states {
                state Idle initial;
                state Busy { vars { ready = true; armed = true; } };
            }
        }
    }
}
"#,
    );

    let machine = &doc.automata[0];
    assert_eq!(machine.states.len(), 2);
    let busy = machine
        .states
        .iter()
        .find(|state| state.name.name == "Busy")
        .expect("Busy state present");
    assert_eq!(busy.overrides.len(), 2);
    assert_eq!(busy.overrides[0].target.name, "armed");
    assert_eq!(busy.overrides[1].target.name, "ready");
}

#[test]
fn test_formula_expr_enum() {
    use crate::context_dsl::ast::{FormulaExpr, LtlExpr, MuExpr};
    use crate::context_dsl::token::Span;
    use crate::ltl::LtlFormula;

    // Test MuCalculus variant
    let mu_expr = MuExpr {
        raw: "nu X. (safe && [] X)".to_string(),
        span: Span::new(0, 20, 1, 1),
    };
    let formula_expr_mu = FormulaExpr::MuCalculus(mu_expr.clone());
    match formula_expr_mu {
        FormulaExpr::MuCalculus(ref expr) => {
            assert_eq!(expr.raw, "nu X. (safe && [] X)");
        }
        FormulaExpr::Ltl(_) => panic!("Expected MuCalculus variant"),
    }

    // Test Ltl variant
    let ltl_formula = LtlFormula::Always(Box::new(LtlFormula::Predicate("safe".to_string())));
    let ltl_expr = LtlExpr {
        formula: ltl_formula.clone(),
        span: Span::new(0, 10, 1, 1),
    };
    let formula_expr_ltl = FormulaExpr::Ltl(ltl_expr.clone());
    match formula_expr_ltl {
        FormulaExpr::Ltl(ref expr) => {
            assert!(matches!(expr.formula, LtlFormula::Always(_)));
        }
        FormulaExpr::MuCalculus(_) => panic!("Expected Ltl variant"),
    }

    // Test cloning
    let cloned_mu = formula_expr_mu.clone();
    let cloned_ltl = formula_expr_ltl.clone();
    assert!(matches!(cloned_mu, FormulaExpr::MuCalculus(_)));
    assert!(matches!(cloned_ltl, FormulaExpr::Ltl(_)));
}

#[test]
fn test_mu_formula_with_ltl() {
    use crate::context_dsl::ast::{FormulaExpr, Ident, LtlExpr, MuFormula};
    use crate::context_dsl::token::Span;
    use crate::ltl::LtlFormula;

    let ltl_formula = LtlFormula::Always(Box::new(LtlFormula::Predicate("safe".to_string())));
    let ltl_expr = LtlExpr {
        formula: ltl_formula,
        span: Span::new(0, 10, 1, 1),
    };

    let mu_formula = MuFormula {
        name: Ident::new("safety".to_string(), Span::new(0, 6, 1, 1)),
        meta: Default::default(),
        targets: crate::context_dsl::ast::FormulaTargets::All(Span::new(0, 3, 1, 1)),
        body: FormulaExpr::Ltl(ltl_expr),
    };

    match mu_formula.body {
        FormulaExpr::Ltl(ref expr) => {
            assert!(matches!(expr.formula, LtlFormula::Always(_)));
        }
        FormulaExpr::MuCalculus(_) => panic!("Expected Ltl variant in MuFormula"),
    }
}

#[test]
fn parses_ltl_formula_with_explicit_marker() {
    let doc = parse_context(
        r#"
        context test {
            automata {
                automaton A {
                    states { state S initial; }
                    transitions { transition S -> S on epsilon; }
                }
            }
            mu_formulas {
                formula safety {
                    over A;
                    body = ltl G safe;
                }
            }
        }
        "#,
    );

    assert_eq!(doc.mu_formulas.len(), 1);
    let formula = &doc.mu_formulas[0];
    match &formula.body {
        FormulaExpr::Ltl(ltl_expr) => {
            assert!(matches!(ltl_expr.formula, LtlFormula::Always(_)));
        }
        FormulaExpr::MuCalculus(_) => panic!("Expected LTL formula"),
    }
}

#[test]
fn parses_mu_formula_with_explicit_marker() {
    let doc = parse_context(
        r#"
        context test {
            automata {
                automaton A {
                    states { state S initial; }
                    transitions { transition S -> S on epsilon; }
                }
            }
            mu_formulas {
                formula safety {
                    over A;
                    body = mu nu X. (safe && [] X);
                }
            }
        }
        "#,
    );

    assert_eq!(doc.mu_formulas.len(), 1);
    let formula = &doc.mu_formulas[0];
    match &formula.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            assert!(mu_expr.raw.contains("nu X"));
        }
        FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula"),
    }
}

#[test]
fn parses_mu_formula_without_marker_backward_compatible() {
    let doc = parse_context(
        r#"
        context test {
            automata {
                automaton A {
                    states { state S initial; }
                    transitions { transition S -> S on epsilon; }
                }
            }
            mu_formulas {
                formula safety {
                    over A;
                    body = nu X. (safe && [] X);
                }
            }
        }
        "#,
    );

    assert_eq!(doc.mu_formulas.len(), 1);
    let formula = &doc.mu_formulas[0];
    match &formula.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            assert!(mu_expr.raw.contains("nu X"));
        }
        FormulaExpr::Ltl(_) => panic!("Expected μ-calculus formula (default)"),
    }
}

#[test]
fn parses_mixed_ltl_and_mu_formulas() {
    let doc = parse_context(
        r#"
        context test {
            automata {
                automaton A {
                    states { state S initial; }
                    transitions { transition S -> S on epsilon; }
                }
            }
            mu_formulas {
                formula safety {
                    over A;
                    body = ltl G safe;
                }
                formula liveness {
                    over A;
                    body = mu mu X. (completed || [] X);
                }
                formula reachability {
                    over A;
                    body = mu X. (goal || <> X);
                }
            }
        }
        "#,
    );

    assert_eq!(doc.mu_formulas.len(), 3);

    // Find formulas by name (order may vary due to canonicalization)
    let safety = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "safety")
        .unwrap();
    let liveness = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "liveness")
        .unwrap();
    let reachability = doc
        .mu_formulas
        .iter()
        .find(|f| f.name.name == "reachability")
        .unwrap();

    // Safety formula: LTL
    match &safety.body {
        FormulaExpr::Ltl(_) => {}
        FormulaExpr::MuCalculus(_) => panic!("Safety formula should be LTL"),
    }

    // Liveness formula: μ-calculus with explicit marker
    // Note: After consuming "mu" marker, the body is "mu X. (completed || [] X)"
    match &liveness.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            // The raw should contain the μ-calculus formula (without the "mu" marker)
            assert!(mu_expr.raw.contains("X") || mu_expr.raw.contains("completed"));
        }
        FormulaExpr::Ltl(_) => panic!("Liveness formula should be μ-calculus"),
    }

    // Reachability formula: μ-calculus without marker (default)
    match &reachability.body {
        FormulaExpr::MuCalculus(mu_expr) => {
            assert!(mu_expr.raw.contains("X") || mu_expr.raw.contains("goal"));
        }
        FormulaExpr::Ltl(_) => panic!("Reachability formula should be μ-calculus"),
    }
}

#[test]
fn parses_complex_ltl_formula() {
    let doc = parse_context(
        r#"
        context test {
            automata {
                automaton A {
                    states { state S initial; }
                    transitions { transition S -> S on epsilon; }
                }
            }
            mu_formulas {
                formula responsiveness {
                    over A;
                    body = ltl G (request -> F grant);
                }
            }
        }
        "#,
    );

    assert_eq!(doc.mu_formulas.len(), 1);
    let formula = &doc.mu_formulas[0];
    match &formula.body {
        FormulaExpr::Ltl(ltl_expr) => {
            // Verify it's a response pattern: G (request -> F grant)
            match &ltl_expr.formula {
                LtlFormula::Always(inner) => {
                    match &**inner {
                        LtlFormula::Implies(_left, right) => {
                            // left should be request, right should be F grant
                            assert!(matches!(&**right, LtlFormula::Eventually(_)));
                        }
                        _ => panic!("Expected Implies inside Always"),
                    }
                }
                _ => panic!("Expected Always at top level"),
            }
        }
        FormulaExpr::MuCalculus(_) => panic!("Expected LTL formula"),
    }
}

/// A guard with `&&`, `||` or `!` constrains its transition instead of disabling it. It used to
/// be lowered to one string and split at its first comparison operator (`(a` vs `1)&&(b==0)`),
/// whose mangled variable no state knew, so the transition silently never fired and `Open` was
/// unreachable.
#[test]
fn a_compound_guard_constrains_its_transition_instead_of_disabling_it() {
    let doc = parse_context(
        r#"
context guards {
    alphabet { label tick; label open; label shut; }
    automata {
        automaton Valve {
            alphabet { label tick; label open; label shut; }
            variables { var a : i64 = 0; var b : i64 = 0; }
            states { state Closed initial; state Open; state Never; }
            transitions {
                transition Closed -> Closed on label tick guard a < 3 effects { a = a + 1; };
                transition Closed -> Open on label open guard a >= 2 && b == 0;
                transition Closed -> Never on label shut guard a > 5 || !(b == 0);
                transition Open -> Closed on label shut;
            }
        }
    }
}
"#,
    );
    let realized = crate::context_dsl::realize::realize(&doc, &[]).expect("realizes");
    let clts = realized.context.clts("Valve").expect("the automaton");
    let names: Vec<String> = clts
        .states()
        .filter_map(|s| clts.state_name(s).map(str::to_string))
        .collect();
    assert!(
        names.iter().any(|n| n.starts_with("Open")),
        "`a >= 2 && b == 0` holds once `a` reaches 2: `Open` must be reachable, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("Never")),
        "`a > 5 || !(b == 0)` never holds (`a` stops at 3, `b` stays 0), got {names:?}"
    );
}

/// A guard over a name that is no declared variable cannot be decided; unrolling refuses it
/// instead of dropping the transition silently.
#[test]
fn a_guard_over_an_undeclared_name_is_refused() {
    let doc = parse_context(
        r#"
context typo {
    alphabet { label go; }
    automata {
        automaton A {
            alphabet { label go; }
            variables { var count : i64 = 0; }
            states { state S initial; state T; }
            transitions {
                transition S -> T on label go guard cuont >= 1;
            }
        }
    }
}
"#,
    );
    let err = crate::context_dsl::realize::realize(&doc, &[])
        .err()
        .expect("an undecidable guard is an error");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot be decided") && msg.contains("cuont"),
        "{msg}"
    );
}
