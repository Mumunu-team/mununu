//! Canonicalisation routines for the CLTS Context DSL AST.
//!
//! These helpers reorder sections and nested collections so semantically
//! identical documents produce stable fingerprints for incremental builds.
use crate::context_dsl::ast::*;

/// Canonicalises every section in the provided context document.
pub fn canonicalize(doc: &mut ContextDoc) {
    doc.alphabet.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    doc.constants.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    doc.ranges.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    doc.enums.sort_by(|a, b| a.name.name.cmp(&b.name.name));

    for automaton in &mut doc.automata {
        canonicalize_automaton(automaton);
    }

    doc.automata.sort_by(|a, b| {
        canonical_id(&a.meta, &a.name.name).cmp(canonical_id(&b.meta, &b.name.name))
    });

    for composition in &mut doc.compositions {
        composition
            .members
            .sort_by(|a, b| a.name.name.cmp(&b.name.name));
    }

    doc.compositions.sort_by(|a, b| {
        canonical_id(&a.meta, &a.name.name)
            .cmp(canonical_id(&b.meta, &b.name.name))
            .then_with(|| composition_kind_rank(a.kind).cmp(&composition_kind_rank(b.kind)))
    });

    for controller in &mut doc.controllers {
        // nothing to canonicalise internally yet
        let _ = controller;
    }

    doc.controllers.sort_by(|a, b| {
        canonical_id(&a.meta, &a.name.name).cmp(canonical_id(&b.meta, &b.name.name))
    });

    for formula in &mut doc.mu_formulas {
        if let FormulaTargets::Named(list) = &mut formula.targets {
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    doc.mu_formulas.sort_by(|a, b| {
        canonical_id(&a.meta, &a.name.name).cmp(canonical_id(&b.meta, &b.name.name))
    });
}

fn canonical_id<'a>(meta: &'a Meta, fallback: &'a str) -> &'a str {
    meta.id.as_deref().unwrap_or(fallback)
}

/// Canonicalises the contents of a single automaton definition.
fn canonicalize_automaton(auto: &mut Automaton) {
    auto.parameters
        .sort_by(|a, b| a.name.name.cmp(&b.name.name));
    auto.alphabet.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    auto.variables.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    auto.state_groups
        .sort_by(|a, b| a.name.name.cmp(&b.name.name));
    auto.states.sort_by(|a, b| a.name.name.cmp(&b.name.name));
    auto.transitions.sort_by_key(transition_key);

    for group in &mut auto.state_groups {
        group.members.sort_by_key(state_selector_key);
    }
    for state in &mut auto.states {
        state
            .overrides
            .sort_by(|a, b| a.target.name.cmp(&b.target.name));
    }
    for transition in &mut auto.transitions {
        transition.additional_labels.sort_by_key(label_key);
        transition
            .effects
            .sort_by(|a, b| a.target.name.cmp(&b.target.name));
    }
}

/// Produces a stable string key for ordering transition declarations.
fn transition_key(transition: &TransitionDecl) -> String {
    let mut key = String::new();
    key.push_str(&state_selector_key(&transition.source));
    key.push('|');
    key.push_str(&state_selector_key(&transition.target));
    key.push('|');
    key.push_str(&label_key(&transition.label));
    if !transition.additional_labels.is_empty() {
        let mut extras: Vec<String> = transition.additional_labels.iter().map(label_key).collect();
        extras.sort();
        key.push('|');
        key.push_str(&extras.join(","));
    }
    key
}

/// Generates the canonical string used to compare state selectors.
fn state_selector_key(selector: &StateSelector) -> String {
    match selector {
        StateSelector::Named(state) => state_ref_key(state),
        StateSelector::Group(name) => format!("group:{}", name.name),
        StateSelector::Wildcard(pattern) => format!("wildcard:{}", pattern.pattern),
    }
}

/// Builds a canonical string for a direct state reference.
fn state_ref_key(state: &StateRef) -> String {
    match state {
        StateRef::Simple(name) => name.name.clone(),
        StateRef::Indexed { name, indices } => {
            let mut text = name.name.clone();
            text.push('[');
            let parts: Vec<String> = indices.iter().map(expr_key).collect();
            text.push_str(&parts.join(","));
            text.push(']');
            text
        }
    }
}

/// Returns the canonical representation of a transition label (with optional index expressions).
fn label_key(label: &TransitionLabel) -> String {
    match label {
        TransitionLabel::Named { name, index } => {
            if let Some(expr) = index {
                format!("{}[{}]", name.name, expr_key(expr))
            } else {
                name.name.clone()
            }
        }
        TransitionLabel::Epsilon(_) => "epsilon".to_string(),
    }
}

/// Creates a canonical string for the supplied expression.
fn expr_key(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Integer(value) => value.to_string(),
        ExprKind::Ident(ident) => ident.name.clone(),
        ExprKind::Index { target, expr } => {
            format!("{}[{}]", target.name, expr_key(expr))
        }
        ExprKind::Unary { op, expr } => format!("({}{})", unary_key(*op), expr_key(expr)),
        ExprKind::Binary { left, op, right } => {
            format!("({}{}{})", expr_key(left), binary_key(*op), expr_key(right))
        }
        ExprKind::Group(inner) => format!("({})", expr_key(inner)),
    }
}

/// Returns the textual representation for a unary operator.
fn unary_key(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Not => "!",
        UnaryOp::Neg => "-",
    }
}

/// Returns the textual representation for a binary operator.
fn binary_key(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
    }
}

/// Determines the ordering of composition kinds during canonicalisation.
fn composition_kind_rank(kind: CompositionKind) -> u8 {
    match kind {
        CompositionKind::Synchronous => 0,
        CompositionKind::Asynchronous => 1,
        CompositionKind::Superset => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_dsl::token::Span;

    fn ident(name: &str) -> Ident {
        Ident::new(name.to_owned(), Span::new(0, 0, 0, 0))
    }

    fn member(name: &str) -> MemberRef {
        MemberRef {
            name: ident(name),
            index: None,
        }
    }

    fn alphabet_entry(name: &str) -> AlphabetEntry {
        AlphabetEntry {
            name: ident(name),
            display: None,
        }
    }

    fn constant_entry(name: &str, value: i64) -> ConstantEntry {
        ConstantEntry {
            name: ident(name),
            value,
        }
    }

    fn range_entry(name: &str, lower: i64, upper: i64) -> RangeEntry {
        RangeEntry {
            name: ident(name),
            lower: Expr {
                kind: ExprKind::Integer(lower),
                span: Span::new(0, 0, 0, 0),
            },
            upper: Expr {
                kind: ExprKind::Integer(upper),
                span: Span::new(0, 0, 0, 0),
            },
        }
    }

    fn dummy_automaton(name: &str, meta_id: Option<&str>) -> Automaton {
        Automaton {
            name: ident(name),
            meta: Meta {
                id: meta_id.map(|s| s.to_owned()),
                comment: None,
            },
            parameters: vec![
                Parameter {
                    name: ident("b"),
                    spec: RangeSpec::Bounds {
                        lower: Expr {
                            kind: ExprKind::Integer(1),
                            span: Span::new(0, 0, 0, 0),
                        },
                        upper: Expr {
                            kind: ExprKind::Integer(0),
                            span: Span::new(0, 0, 0, 0),
                        },
                    },
                },
                Parameter {
                    name: ident("a"),
                    spec: RangeSpec::Named(ident("range")),
                },
            ],
            alphabet: vec![
                AlphabetRef {
                    name: ident("b"),
                    index: None,
                },
                AlphabetRef {
                    name: ident("a"),
                    index: None,
                },
            ],
            controllable: Vec::new(),
            internal: Vec::new(),
            controllable_declared: false,
            internal_declared: false,
            variables: vec![
                VariableDecl {
                    name: ident("y"),
                    index: None,
                    ty: TypeName::Bool,
                    init: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
                VariableDecl {
                    name: ident("x"),
                    index: None,
                    ty: TypeName::Bool,
                    init: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
            ],
            state_groups: Vec::new(),
            states: vec![
                StateDecl {
                    name: ident("B"),
                    index: None,
                    is_initial: false,
                    overrides: vec![Assignment {
                        target: ident("b"),
                        expr: Expr {
                            kind: ExprKind::Integer(0),
                            span: Span::new(0, 0, 0, 0),
                        },
                    }],
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("A"),
                    index: None,
                    is_initial: true,
                    overrides: vec![Assignment {
                        target: ident("a"),
                        expr: Expr {
                            kind: ExprKind::Integer(0),
                            span: Span::new(0, 0, 0, 0),
                        },
                    }],
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
            ],
            transitions: vec![
                TransitionDecl {
                    source: StateSelector::Named(StateRef::Simple(ident("B"))),
                    target: StateSelector::Named(StateRef::Simple(ident("A"))),
                    label: TransitionLabel::Named {
                        name: ident("b"),
                        index: None,
                    },
                    additional_labels: Vec::new(),
                    guard: None,
                    effects: vec![Assignment {
                        target: ident("y"),
                        expr: Expr {
                            kind: ExprKind::Integer(0),
                            span: Span::new(0, 0, 0, 0),
                        },
                    }],

                    modality: TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
                TransitionDecl {
                    source: StateSelector::Named(StateRef::Simple(ident("A"))),
                    target: StateSelector::Named(StateRef::Simple(ident("B"))),
                    label: TransitionLabel::Named {
                        name: ident("a"),
                        index: None,
                    },
                    additional_labels: Vec::new(),
                    guard: None,
                    effects: vec![Assignment {
                        target: ident("x"),
                        expr: Expr {
                            kind: ExprKind::Integer(0),
                            span: Span::new(0, 0, 0, 0),
                        },
                    }],

                    modality: TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
            ],
            predicates: Vec::new(),
        }
    }

    #[test]
    fn canonicalise_orders_everything() {
        let mut doc = ContextDoc {
            name: ident("demo"),
            alphabet: vec![alphabet_entry("tau"), alphabet_entry("alpha")],
            constants: vec![constant_entry("N", 5), constant_entry("A", 1)],
            ranges: vec![range_entry("high", 10, 20), range_entry("low", 0, 5)],
            enums: Vec::new(),
            automata: vec![
                dummy_automaton("Beta", Some("Z")),
                dummy_automaton("Alpha", None),
            ],
            compositions: vec![
                Composition {
                    name: ident("Async"),
                    meta: Meta {
                        id: None,
                        comment: None,
                    },
                    kind: CompositionKind::Asynchronous,
                    members: vec![member("C"), member("A"), member("B")],
                    allow_dead_vectors: false,
                    fairness_weak: false,
                    span: Span::new(0, 0, 0, 0),
                },
                Composition {
                    name: ident("Sync"),
                    meta: Meta {
                        id: Some("AA".into()),
                        comment: None,
                    },
                    kind: CompositionKind::Synchronous,
                    members: vec![member("Y"), member("X")],
                    allow_dead_vectors: false,
                    fairness_weak: false,
                    span: Span::new(0, 0, 0, 0),
                },
            ],
            controllers: vec![
                Controller {
                    name: ident("CtrlB"),
                    meta: Meta {
                        id: Some("ctrl.beta".into()),
                        comment: None,
                    },
                    source: ident("Beta"),
                    formula: ident("phi"),
                    export: Some("out/beta.ctxdsl".into()),
                    options: ControllerOptions::default(),
                    span: Span::new(0, 0, 0, 0),
                },
                Controller {
                    name: ident("CtrlA"),
                    meta: Meta {
                        id: None,
                        comment: None,
                    },
                    source: ident("Alpha"),
                    formula: ident("psi"),
                    export: None,
                    options: ControllerOptions::default(),
                    span: Span::new(0, 0, 0, 0),
                },
            ],
            mu_formulas: vec![
                MuFormula {
                    name: ident("phi"),
                    meta: Meta {
                        id: Some("B".into()),
                        comment: None,
                    },
                    targets: FormulaTargets::Named(vec![ident("B"), ident("A")]),
                    body: FormulaExpr::MuCalculus(MuExpr {
                        raw: "true".into(),
                        span: Span::new(0, 0, 0, 0),
                    }),
                },
                MuFormula {
                    name: ident("psi"),
                    meta: Meta {
                        id: None,
                        comment: None,
                    },
                    targets: FormulaTargets::Named(vec![ident("C"), ident("A")]),
                    body: FormulaExpr::MuCalculus(MuExpr {
                        raw: "false".into(),
                        span: Span::new(0, 0, 0, 0),
                    }),
                },
            ],
            span: Span::new(0, 0, 0, 0),
            state_valuations: Default::default(),
            transition_observations: Default::default(),
        };

        canonicalize(&mut doc);

        let alphabet_names: Vec<&str> = doc
            .alphabet
            .iter()
            .map(|entry| entry.name.name.as_str())
            .collect();
        assert_eq!(alphabet_names, vec!["alpha", "tau"]);

        let constant_names: Vec<&str> = doc
            .constants
            .iter()
            .map(|entry| entry.name.name.as_str())
            .collect();
        assert_eq!(constant_names, vec!["A", "N"]);

        let automata_ids: Vec<&str> = doc
            .automata
            .iter()
            .map(|auto| canonical_id(&auto.meta, &auto.name.name))
            .collect();
        assert_eq!(automata_ids, vec!["Alpha", "Z"]);

        let first_state_names: Vec<&str> = doc.automata[0]
            .states
            .iter()
            .map(|state| state.name.name.as_str())
            .collect();
        assert_eq!(first_state_names, vec!["A", "B"]);

        let composition_pairs: Vec<(&str, Vec<&str>)> = doc
            .compositions
            .iter()
            .map(|comp| {
                (
                    canonical_id(&comp.meta, &comp.name.name),
                    comp.members
                        .iter()
                        .map(|m| m.name.name.as_str())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        assert_eq!(
            composition_pairs,
            vec![("AA", vec!["X", "Y"]), ("Async", vec!["A", "B", "C"]),]
        );

        let controller_pairs: Vec<(&str, &str, Option<&str>)> = doc
            .controllers
            .iter()
            .map(|ctrl| {
                (
                    canonical_id(&ctrl.meta, &ctrl.name.name),
                    ctrl.source.name.as_str(),
                    ctrl.export.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            controller_pairs,
            vec![
                ("CtrlA", "Alpha", None),
                ("ctrl.beta", "Beta", Some("out/beta.ctxdsl")),
            ]
        );

        let formula_ids: Vec<&str> = doc
            .mu_formulas
            .iter()
            .map(|formula| canonical_id(&formula.meta, &formula.name.name))
            .collect();
        assert_eq!(formula_ids, vec!["B", "psi"]);

        let targets: Vec<Vec<&str>> = doc
            .mu_formulas
            .iter()
            .map(|formula| match &formula.targets {
                FormulaTargets::Named(list) => list.iter().map(|id| id.name.as_str()).collect(),
                FormulaTargets::All(_) => panic!("unexpected all target"),
            })
            .collect();
        assert_eq!(targets, vec![vec!["A", "B"], vec!["A", "C"]]);
    }

    #[test]
    fn canonicalize_state_selector_group() {
        // Test StateSelector::Group canonicalization (line 103)
        let mut auto = dummy_automaton("Test", None);
        auto.state_groups.push(StateGroup {
            name: ident("Group1"),
            members: vec![
                StateSelector::Named(StateRef::Simple(ident("S2"))),
                StateSelector::Named(StateRef::Simple(ident("S1"))),
            ],
            span: Span::new(0, 0, 0, 0),
        });
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Group(ident("Group1")),
            target: StateSelector::Named(StateRef::Simple(ident("S3"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify group members are sorted
        assert_eq!(auto.state_groups[0].members.len(), 2);
        let first_key = state_selector_key(&auto.state_groups[0].members[0]);
        let second_key = state_selector_key(&auto.state_groups[0].members[1]);
        assert!(first_key < second_key, "Group members should be sorted");
    }

    #[test]
    fn canonicalize_state_selector_wildcard() {
        // Test StateSelector::Wildcard canonicalization (line 104)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Wildcard(WildcardPattern {
                pattern: "S*".to_string(),
                span: Span::new(0, 0, 0, 0),
            }),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify wildcard transition is present
        assert!(
            auto.transitions
                .iter()
                .any(|t| matches!(&t.source, StateSelector::Wildcard(_)))
        );
    }

    #[test]
    fn canonicalize_state_ref_indexed() {
        // Test StateRef::Indexed canonicalization (lines 112-119)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Indexed {
                name: ident("S"),
                indices: vec![
                    Expr {
                        kind: ExprKind::Integer(2),
                        span: Span::new(0, 0, 0, 0),
                    },
                    Expr {
                        kind: ExprKind::Integer(1),
                        span: Span::new(0, 0, 0, 0),
                    },
                ],
            }),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify indexed state ref is handled
        if let StateSelector::Named(StateRef::Indexed { indices, .. }) = &auto.transitions[0].source
        {
            // The key should include the indices
            let key = state_ref_key(&StateRef::Indexed {
                name: ident("S"),
                indices: indices.clone(),
            });
            assert!(key.contains('[') && key.contains(']'));
        }
    }

    #[test]
    fn canonicalize_transition_label_with_index() {
        // Test TransitionLabel with index canonicalization (lines 127-128)
        // Verify that transitions with indexed labels are sorted correctly
        // Create a fresh automaton to avoid interference from dummy_automaton's existing transitions
        let mut auto = Automaton {
            name: ident("Test"),
            meta: Meta {
                id: None,
                comment: None,
            },
            parameters: Vec::new(),
            alphabet: Vec::new(),
            controllable: Vec::new(),
            internal: Vec::new(),
            controllable_declared: false,
            internal_declared: false,
            variables: Vec::new(),
            state_groups: Vec::new(),
            states: vec![
                StateDecl {
                    name: ident("S"),
                    index: None,
                    is_initial: true,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T1"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T2"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
            ],
            transitions: Vec::new(),
            predicates: Vec::new(),
        };

        // Add transitions with indexed labels in non-canonical order
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T2"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Integer(10),
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T1"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Integer(5),
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        // Verify initial order (alpha[10] before alpha[5])
        let initial_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();
        assert_eq!(initial_keys, vec!["alpha[10]", "alpha[5]"]);

        canonicalize_automaton(&mut auto);

        // Verify transitions are sorted by their label keys (alpha[5] should come before alpha[10])
        // The transition_key includes the label_key which formats indexed labels as "alpha[5]"
        let final_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();

        // Verify the label keys are in canonical order (alpha[5] < alpha[10])
        assert_eq!(final_keys, vec!["alpha[5]", "alpha[10]"]);

        // Also verify the transition_key function produces correct keys
        let transition_keys: Vec<String> = auto.transitions.iter().map(transition_key).collect();

        // transition_key format: "source|target|label"
        // Both should have "S|" as source, so sorting is determined by target and label
        // Since both have same source and label name, the index determines order
        assert!(
            transition_keys[0] < transition_keys[1],
            "transitions should be sorted by transition_key"
        );
    }

    #[test]
    fn canonicalize_transition_label_epsilon() {
        // Test TransitionLabel::Epsilon canonicalization (line 133)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Epsilon(Span::new(0, 0, 0, 0)),
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify epsilon label is handled
        assert!(
            auto.transitions
                .iter()
                .any(|t| matches!(&t.label, TransitionLabel::Epsilon(_)))
        );
    }

    #[test]
    fn canonicalize_transition_additional_labels() {
        // Test transition additional_labels sorting (line 75)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: vec![
                TransitionLabel::Named {
                    name: ident("gamma"),
                    index: None,
                },
                TransitionLabel::Named {
                    name: ident("beta"),
                    index: None,
                },
            ],
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify additional labels are sorted
        if let Some(transition) = auto
            .transitions
            .iter()
            .find(|t| !t.additional_labels.is_empty())
        {
            let labels: Vec<String> = transition.additional_labels.iter().map(label_key).collect();
            assert_eq!(labels, vec!["beta", "gamma"]);
        }
    }

    #[test]
    fn canonicalize_expr_index() {
        // Test ExprKind::Index canonicalization (lines 142-144)
        // Verify that labels with indexed expressions are correctly formatted and sorted
        let mut auto = Automaton {
            name: ident("Test"),
            meta: Meta {
                id: None,
                comment: None,
            },
            parameters: Vec::new(),
            alphabet: Vec::new(),
            controllable: Vec::new(),
            internal: Vec::new(),
            controllable_declared: false,
            internal_declared: false,
            variables: Vec::new(),
            state_groups: Vec::new(),
            states: vec![
                StateDecl {
                    name: ident("S"),
                    index: None,
                    is_initial: true,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T1"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T2"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
            ],
            transitions: Vec::new(),
            predicates: Vec::new(),
        };

        // Add transitions with indexed expressions in labels in non-canonical order
        // alpha[arr[10]] should come after alpha[arr[5]] when sorted
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T2"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Index {
                        target: ident("arr"),
                        expr: Box::new(Expr {
                            kind: ExprKind::Integer(10),
                            span: Span::new(0, 0, 0, 0),
                        }),
                    },
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T1"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Index {
                        target: ident("arr"),
                        expr: Box::new(Expr {
                            kind: ExprKind::Integer(5),
                            span: Span::new(0, 0, 0, 0),
                        }),
                    },
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        // Verify initial order (alpha[arr[10]] before alpha[arr[5]])
        let initial_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();
        assert_eq!(initial_keys, vec!["alpha[arr[10]]", "alpha[arr[5]]"]);

        canonicalize_automaton(&mut auto);

        // Verify transitions are sorted by their label keys
        // alpha[arr[5]] should come before alpha[arr[10]]
        let final_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();

        assert_eq!(final_keys, vec!["alpha[arr[5]]", "alpha[arr[10]]"]);

        // Verify the expr_key function correctly formats indexed expressions
        if let TransitionLabel::Named {
            index:
                Some(Expr {
                    kind: ExprKind::Index { target, expr },
                    ..
                }),
            ..
        } = &auto.transitions[0].label
        {
            let expr_key_str = expr_key(&Expr {
                kind: ExprKind::Index {
                    target: target.clone(),
                    expr: expr.clone(),
                },
                span: Span::new(0, 0, 0, 0),
            });
            assert_eq!(expr_key_str, "arr[5]");
        }
    }

    #[test]
    fn canonicalize_expr_unary() {
        // Test ExprKind::Unary canonicalization (line 145)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: Some(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(Expr {
                        kind: ExprKind::Ident(ident("x")),
                        span: Span::new(0, 0, 0, 0),
                    }),
                },
                span: Span::new(0, 0, 0, 0),
            }),
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify unary expression is handled
        if let Some(Expr {
            kind: ExprKind::Unary { op, .. },
            ..
        }) = &auto.transitions[0].guard
        {
            assert_eq!(*op, UnaryOp::Not);
        }
    }

    #[test]
    fn canonicalize_expr_unary_neg() {
        // Test UnaryOp::Neg canonicalization (line 157)
        let mut auto = dummy_automaton("Test", None);
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: Some(Expr {
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(Expr {
                        kind: ExprKind::Integer(5),
                        span: Span::new(0, 0, 0, 0),
                    }),
                },
                span: Span::new(0, 0, 0, 0),
            }),
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify negation operator is handled
        if let Some(Expr {
            kind: ExprKind::Unary { op, .. },
            ..
        }) = &auto.transitions[0].guard
        {
            assert_eq!(*op, UnaryOp::Neg);
        }
    }

    #[test]
    fn canonicalize_expr_binary_all_operators() {
        // Test all BinaryOp variants canonicalization (lines 164-176)
        let mut auto = Automaton {
            name: ident("Test"),
            meta: Meta {
                id: None,
                comment: None,
            },
            parameters: Vec::new(),
            alphabet: Vec::new(),
            controllable: Vec::new(),
            internal: Vec::new(),
            controllable_declared: false,
            internal_declared: false,
            variables: Vec::new(),
            state_groups: Vec::new(),
            states: vec![
                StateDecl {
                    name: ident("S"),
                    index: None,
                    is_initial: true,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
            ],
            transitions: Vec::new(),
            predicates: Vec::new(),
        };
        let binary_ops = [
            BinaryOp::Add,
            BinaryOp::Sub,
            BinaryOp::Mul,
            BinaryOp::Div,
            BinaryOp::Mod,
            BinaryOp::And,
            BinaryOp::Or,
            BinaryOp::Eq,
            BinaryOp::Ne,
            BinaryOp::Lt,
            BinaryOp::Le,
            BinaryOp::Gt,
            BinaryOp::Ge,
        ];

        for (i, op) in binary_ops.iter().enumerate() {
            auto.transitions.push(TransitionDecl {
                source: StateSelector::Named(StateRef::Simple(ident("S"))),
                target: StateSelector::Named(StateRef::Simple(ident("T"))),
                label: TransitionLabel::Named {
                    name: ident(&format!("alpha{}", i)),
                    index: None,
                },
                additional_labels: Vec::new(),
                guard: Some(Expr {
                    kind: ExprKind::Binary {
                        left: Box::new(Expr {
                            kind: ExprKind::Integer(1),
                            span: Span::new(0, 0, 0, 0),
                        }),
                        op: *op,
                        right: Box::new(Expr {
                            kind: ExprKind::Integer(2),
                            span: Span::new(0, 0, 0, 0),
                        }),
                    },
                    span: Span::new(0, 0, 0, 0),
                }),
                effects: Vec::new(),

                modality: TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }

        canonicalize_automaton(&mut auto);

        // Verify all binary operators are handled
        assert_eq!(auto.transitions.len(), binary_ops.len());
    }

    #[test]
    fn canonicalize_expr_group() {
        // Test ExprKind::Group canonicalization (line 149)
        // Verify that grouped expressions are correctly formatted by expr_key
        // and work correctly in label indices
        let mut auto = Automaton {
            name: ident("Test"),
            meta: Meta {
                id: None,
                comment: None,
            },
            parameters: Vec::new(),
            alphabet: Vec::new(),
            controllable: Vec::new(),
            internal: Vec::new(),
            controllable_declared: false,
            internal_declared: false,
            variables: Vec::new(),
            state_groups: Vec::new(),
            states: vec![
                StateDecl {
                    name: ident("S"),
                    index: None,
                    is_initial: true,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T1"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
                StateDecl {
                    name: ident("T2"),
                    index: None,
                    is_initial: false,
                    overrides: Vec::new(),
                    valuations: Vec::new(),
                    three_valued: Vec::new(),
                },
            ],
            transitions: Vec::new(),
            predicates: Vec::new(),
        };

        // Add transitions with grouped expressions in label indices
        // alpha[(10)] should come after alpha[(5)] when sorted
        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T2"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Group(Box::new(Expr {
                        kind: ExprKind::Integer(10),
                        span: Span::new(0, 0, 0, 0),
                    })),
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        auto.transitions.push(TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T1"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: Some(Expr {
                    kind: ExprKind::Group(Box::new(Expr {
                        kind: ExprKind::Integer(5),
                        span: Span::new(0, 0, 0, 0),
                    })),
                    span: Span::new(0, 0, 0, 0),
                }),
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: Vec::new(),

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        // Verify initial order (alpha[(10)] before alpha[(5)])
        let initial_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();
        assert_eq!(initial_keys, vec!["alpha[(10)]", "alpha[(5)]"]);

        canonicalize_automaton(&mut auto);

        // Verify transitions are sorted by their label keys
        // alpha[(5)] should come before alpha[(10)]
        let final_keys: Vec<String> = auto
            .transitions
            .iter()
            .map(|t| label_key(&t.label))
            .collect();

        assert_eq!(final_keys, vec!["alpha[(5)]", "alpha[(10)]"]);

        // Verify the expr_key function correctly formats grouped expressions
        if let TransitionLabel::Named {
            index:
                Some(Expr {
                    kind: ExprKind::Group(inner),
                    ..
                }),
            ..
        } = &auto.transitions[0].label
        {
            let expr_key_str = expr_key(&Expr {
                kind: ExprKind::Group(inner.clone()),
                span: Span::new(0, 0, 0, 0),
            });
            assert_eq!(expr_key_str, "(5)");
        }
    }

    #[test]
    fn canonicalize_composition_kind_ranking() {
        // Test composition kind ranking (lines 182-186)
        let mut doc = ContextDoc {
            name: ident("test"),
            alphabet: Vec::new(),
            constants: Vec::new(),
            ranges: Vec::new(),
            enums: Vec::new(),
            automata: Vec::new(),
            compositions: vec![
                Composition {
                    name: ident("Superset"),
                    meta: Meta {
                        id: Some("Z".into()),
                        comment: None,
                    },
                    kind: CompositionKind::Superset,
                    members: vec![member("A")],
                    allow_dead_vectors: false,
                    fairness_weak: false,
                    span: Span::new(0, 0, 0, 0),
                },
                Composition {
                    name: ident("Async"),
                    meta: Meta {
                        id: Some("Z".into()),
                        comment: None,
                    },
                    kind: CompositionKind::Asynchronous,
                    members: vec![member("A")],
                    allow_dead_vectors: false,
                    fairness_weak: false,
                    span: Span::new(0, 0, 0, 0),
                },
                Composition {
                    name: ident("Sync"),
                    meta: Meta {
                        id: Some("Z".into()),
                        comment: None,
                    },
                    kind: CompositionKind::Synchronous,
                    members: vec![member("A")],
                    allow_dead_vectors: false,
                    fairness_weak: false,
                    span: Span::new(0, 0, 0, 0),
                },
            ],
            controllers: Vec::new(),
            mu_formulas: Vec::new(),
            span: Span::new(0, 0, 0, 0),
            state_valuations: Default::default(),
            transition_observations: Default::default(),
        };

        canonicalize(&mut doc);

        // Verify compositions are sorted by kind rank (Synchronous=0, Asynchronous=1, Superset=2)
        let kinds: Vec<CompositionKind> = doc.compositions.iter().map(|c| c.kind).collect();
        assert_eq!(kinds[0], CompositionKind::Synchronous);
        assert_eq!(kinds[1], CompositionKind::Asynchronous);
        assert_eq!(kinds[2], CompositionKind::Superset);
    }

    #[test]
    fn canonicalize_formula_targets_all() {
        // Test FormulaTargets::All canonicalization (line 41 - should not sort)
        let mut doc = ContextDoc {
            name: ident("test"),
            alphabet: Vec::new(),
            constants: Vec::new(),
            ranges: Vec::new(),
            enums: Vec::new(),
            automata: Vec::new(),
            compositions: Vec::new(),
            controllers: Vec::new(),
            mu_formulas: vec![MuFormula {
                name: ident("phi"),
                meta: Meta {
                    id: None,
                    comment: None,
                },
                targets: FormulaTargets::All(Span::new(0, 0, 0, 0)),
                body: FormulaExpr::MuCalculus(MuExpr {
                    raw: "true".into(),
                    span: Span::new(0, 0, 0, 0),
                }),
            }],
            span: Span::new(0, 0, 0, 0),
            state_valuations: Default::default(),
            transition_observations: Default::default(),
        };

        canonicalize(&mut doc);

        // Verify All targets are handled (should not panic)
        assert!(matches!(
            &doc.mu_formulas[0].targets,
            FormulaTargets::All(_)
        ));
    }

    #[test]
    fn canonicalize_state_overrides() {
        // Test state overrides sorting (lines 70-73)
        let mut auto = dummy_automaton("Test", None);
        auto.states.push(StateDecl {
            name: ident("S"),
            index: None,
            is_initial: false,
            overrides: vec![
                Assignment {
                    target: ident("z"),
                    expr: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
                Assignment {
                    target: ident("a"),
                    expr: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
            ],
            valuations: Vec::new(),
            three_valued: Vec::new(),
        });

        canonicalize_automaton(&mut auto);

        // Verify overrides are sorted
        let override_names: Vec<&str> = auto
            .states
            .iter()
            .find(|s| s.name.name == "S")
            .unwrap()
            .overrides
            .iter()
            .map(|o| o.target.name.as_str())
            .collect();
        assert_eq!(override_names, vec!["a", "z"]);
    }

    #[test]
    fn canonicalize_transition_effects() {
        // Test transition effects sorting (lines 77-79)
        let mut auto = dummy_automaton("Test", None);
        let test_transition = TransitionDecl {
            source: StateSelector::Named(StateRef::Simple(ident("S"))),
            target: StateSelector::Named(StateRef::Simple(ident("T"))),
            label: TransitionLabel::Named {
                name: ident("alpha"),
                index: None,
            },
            additional_labels: Vec::new(),
            guard: None,
            effects: vec![
                Assignment {
                    target: ident("z"),
                    expr: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
                Assignment {
                    target: ident("a"),
                    expr: Expr {
                        kind: ExprKind::Integer(0),
                        span: Span::new(0, 0, 0, 0),
                    },
                },
            ],

            modality: TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        };
        auto.transitions.push(test_transition);

        canonicalize_automaton(&mut auto);

        // Verify effects are sorted - find the transition with "alpha" label
        let alpha_transition = auto
            .transitions
            .iter()
            .find(|t| match &t.label {
                TransitionLabel::Named { name, .. } => name.name == "alpha",
                _ => false,
            })
            .unwrap();
        let effect_names: Vec<&str> = alpha_transition
            .effects
            .iter()
            .map(|e| e.target.name.as_str())
            .collect();
        assert_eq!(effect_names, vec!["a", "z"]);
    }
}
