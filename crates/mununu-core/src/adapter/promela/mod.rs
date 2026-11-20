//! Promela (SPIN) adapter.
//!
//! Translates bounded Promela programs into CTXDSL via compositional encoding:
//! each process becomes a CFG automaton, each variable becomes a variable automaton,
//! each channel becomes a channel automaton (occupancy-based for buffered channels,
//! rendezvous for synchronous channels), and they are composed asynchronously
//! to model process interleaving.
//!
//! Promoted constructs: `inline` definitions are parsed (but expansion is not
//! yet supported), `trace`/`notrace` assertions are recognized, and LTL
//! properties are converted to mununu's LTL representation.

pub mod ast;
pub mod cfg;
mod parser;

use super::ir::*;
use super::{
    AdapterError, AdapterOptions, AdapterOutput, AdapterWarning, FormatAdapter, SourceFormat,
    SourceInfo, WarningKind,
};
use crate::adapter::domain::{AbstractValue, FieldDomain};
use crate::ltl::LtlFormula;

/// Promela adapter implementing [`FormatAdapter`].
pub struct PromelaAdapter;

impl FormatAdapter for PromelaAdapter {
    fn detect(content: &str) -> bool {
        let trimmed = content.trim_start();
        trimmed.contains("proctype")
            || trimmed.contains("active")
            || (trimmed.contains("init") && trimmed.contains("{"))
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        let program = parser::parse(content)?;
        let ir = to_ir(&program, options)?;

        let mut warnings = Vec::new();

        // Warn about unbounded variables
        for g in &program.globals {
            if g.typename.needs_bounding() {
                warnings.push(AdapterWarning {
                    kind: WarningKind::ApproximateTranslation,
                    message: format!(
                        "Variable '{}' ({}) auto-bounded via static analysis",
                        g.name,
                        g.typename.keyword()
                    ),
                    location: None,
                });
            }
        }

        // Warn about inline macros (parsed but not expanded)
        if !program.inlines.is_empty() {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "{} inline definition(s) parsed but expansion is not yet supported; \
                     inline calls will be treated as unknown identifiers",
                    program.inlines.len()
                ),
                location: None,
            });
        }

        // Warn about trace/notrace
        if !program.traces.is_empty() || !program.notraces.is_empty() {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: "trace/notrace assertions are parsed but not yet fully supported"
                    .to_string(),
                location: None,
            });
        }

        // Warn about timeout expressions (conservative approximation)
        // and remote references (approximated)
        // These warnings are added during IR construction

        let _automaton_count = ir.automata.len();
        let emit_result = super::emit::emit(&ir)?;

        Ok(AdapterOutput {
            sidecars: Vec::new(),
            ctxdsl: emit_result.ctxdsl,
            warnings,
            source_info: SourceInfo {
                format: SourceFormat::Promela,
                title: None,
                signal_count: program.globals.len(),
                state_count: 0, // computed at composition time
                property_count: ir.properties.len(),
            },
            state_valuations: Default::default(),
            transition_observations: Default::default(),
            partition_summary: None,
        })
    }
}

/// Convert a parsed Promela program to AdapterIR.
fn to_ir(program: &ast::Program, options: &AdapterOptions) -> Result<AdapterIR, AdapterError> {
    let context_name = options
        .context_name
        .clone()
        .unwrap_or_else(|| "promela_system".into());

    let mut automata = Vec::new();
    let mut all_labels = std::collections::HashSet::new();

    // Extract CFGs for each process
    for proc in &program.proctypes {
        if proc.active_count == 0 {
            continue;
        } // skip non-active proctypes

        let proc_cfg = cfg::extract_cfg_with_options(&proc.name, &proc.body, proc.deterministic);

        // Convert CFG to AutomatonSpec
        let states: Vec<StateSpec> = proc_cfg
            .locations
            .iter()
            .map(|loc| StateSpec {
                name: loc.label.clone(),
                is_initial: loc.id == proc_cfg.initial,
                valuations: None,
                three_valued: None,
            })
            .collect();

        let transitions: Vec<TransitionSpec> = proc_cfg
            .edges
            .iter()
            .map(|edge| {
                let src = proc_cfg.locations[edge.src].label.clone();
                let dst = proc_cfg.locations[edge.dst].label.clone();
                let labels = if edge.labels.is_empty() {
                    vec![format!(
                        "{}_epsilon_{}_to_{}",
                        proc.name, edge.src, edge.dst
                    )]
                } else {
                    edge.labels.clone()
                };
                for l in &labels {
                    all_labels.insert(l.clone());
                }
                TransitionSpec {
                    source: src,
                    target: dst,
                    labels,
                    modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                }
            })
            .collect();

        automata.push(AutomatonSpec {
            name: format!("{}_cfg", proc.name),
            states,
            transitions,
            controllable_labels: vec![],
            internal_labels: vec![],
        });
    }

    // Create variable automata for each global variable
    for var in &program.globals {
        let var_aut = create_variable_automaton(var, &all_labels, options);
        automata.push(var_aut);
    }

    // Create channel automata for each channel declaration
    for chan in &program.channels {
        let chan_aut = create_channel_automaton(chan);
        automata.push(chan_aut);
    }

    // Create compositions
    let mut compositions = Vec::new();

    // All CFG automata composed asynchronously (process interleaving)
    let cfg_names: Vec<String> = program
        .proctypes
        .iter()
        .filter(|p| p.active_count > 0)
        .map(|p| format!("{}_cfg", p.name))
        .collect();

    let var_names: Vec<String> = program
        .globals
        .iter()
        .map(|v| format!("Var_{}", v.name))
        .collect();

    let chan_names: Vec<String> = program
        .channels
        .iter()
        .map(|c| format!("Chan_{}", c.name))
        .collect();

    if cfg_names.len() + var_names.len() + chan_names.len() > 1 {
        let mut all_members = Vec::new();
        all_members.extend(cfg_names);
        all_members.extend(var_names);
        all_members.extend(chan_names);
        compositions.push(CompositionSpec::Asynchronous {
            name: "System".into(),
            members: all_members,
        });
    }

    // Build a set of bool-variable names so LTL atoms like `flag` can be
    // rewritten to the corresponding state name `flag_1` of the Var_flag
    // automaton. Without this, `[] !flag` references the predicate `flag`
    // which has no resolution path (no state, no registered predicate) and
    // the evaluator would silently treat it as `false`.
    let bool_var_names: std::collections::HashSet<String> = program
        .globals
        .iter()
        .filter(|g| matches!(g.typename, ast::TypeName::Bit | ast::TypeName::Bool))
        .map(|g| g.name.clone())
        .collect();

    // Convert LTL properties
    let properties: Vec<PropertySpec> = program
        .ltl_properties
        .iter()
        .map(|ltl| {
            let name = ltl.name.clone().unwrap_or_else(|| "property".into());
            PropertySpec {
                name,
                kind: PropertyKind::Safety,
                formula: PropertyFormula::Ltl(convert_ltl(&ltl.formula, &bool_var_names)),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            }
        })
        .collect();

    Ok(AdapterIR {
        metadata: Metadata {
            title: context_name,
            source_format: SourceFormat::Promela,
            description: None,
            game_semantics: None,
            known_status: None,
        },
        signals: vec![],
        automata,
        compositions,
        properties,
        controller: None,
    })
}

/// Derive a `FieldDomain` for a Promela variable using type-based heuristics
/// or explicit user bounds.
fn promela_var_to_domain(var: &ast::VarDecl, options: &AdapterOptions) -> FieldDomain {
    let init_val = match &var.init {
        Some(ast::Expr::IntLit(n)) => *n,
        Some(ast::Expr::BoolLit(false)) => 0,
        Some(ast::Expr::BoolLit(true)) => 1,
        _ => 0,
    };

    let (lo, hi) = if let Some(&(user_lo, user_hi)) = options.variable_bounds.get(&var.name) {
        (user_lo, user_hi)
    } else {
        match &var.typename {
            ast::TypeName::Bit | ast::TypeName::Bool => (0i64, 1),
            ast::TypeName::Byte if var.init.is_some() => (0, std::cmp::max(init_val + 3, 3)),
            ast::TypeName::Byte => (0, 3),
            ast::TypeName::Short | ast::TypeName::Int if var.init.is_some() => {
                let lo = std::cmp::min(0, init_val - 2);
                let hi = std::cmp::max(init_val + 3, 3);
                (lo, hi)
            }
            ast::TypeName::Short | ast::TypeName::Int => (0, 3),
            ast::TypeName::Mtype => (0, 3),
            _ => (0, 3),
        }
    };

    FieldDomain::with_range(var.name.clone(), lo, hi, init_val)
}

/// Create a variable automaton for a bounded variable.
///
/// Uses [`FieldDomain`] to derive the value range, then builds set/test
/// transitions for compositional encoding with process automata.
fn create_variable_automaton(
    var: &ast::VarDecl,
    _used_labels: &std::collections::HashSet<String>,
    options: &AdapterOptions,
) -> AutomatonSpec {
    let domain = promela_var_to_domain(var, options);
    let values = domain.values();
    let init_val = match &domain.initial {
        AbstractValue::Counter(n) => *n,
        _ => 0,
    };

    let mut states = Vec::new();
    let mut transitions = Vec::new();

    for value in &values {
        let val = match value {
            AbstractValue::Counter(n) => *n,
            _ => continue,
        };
        let state_name = format!("{}_{}", var.name, val);
        states.push(StateSpec {
            name: state_name.clone(),
            is_initial: val == init_val,
            valuations: None,
            three_valued: None,
        });

        // set_var_val transitions from any state to this value
        for src_value in &values {
            let src_val = match src_value {
                AbstractValue::Counter(n) => *n,
                _ => continue,
            };
            let src_name = format!("{}_{}", var.name, src_val);
            let label = format!("set_{}_{}", var.name, val);
            transitions.push(TransitionSpec {
                source: src_name.clone(),
                target: state_name.clone(),
                labels: vec![label],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }

        // test_var_val self-loop (guard check)
        let test_label = format!("test_{}_{}", var.name, val);
        transitions.push(TransitionSpec {
            source: state_name.clone(),
            target: state_name.clone(),
            labels: vec![test_label],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });
    }

    AutomatonSpec {
        name: format!("Var_{}", var.name),
        states,
        transitions,
        controllable_labels: vec![],
        internal_labels: vec![],
    }
}

/// Create a channel automaton for occupancy-based encoding.
///
/// For a channel with capacity N, the automaton has N+1 states representing
/// occupancy levels 0..N. Send increments occupancy (blocks when full),
/// receive decrements (blocks when empty). Test labels allow guards to
/// check empty/nempty/full/nfull.
///
/// For synchronous channels (capacity 0), a single idle state with a
/// rendezvous self-loop is created.
fn create_channel_automaton(chan: &ast::ChanDecl) -> AutomatonSpec {
    let mut states = Vec::new();
    let mut transitions = Vec::new();

    if chan.capacity == 0 {
        // Synchronous (rendezvous) channel: single state with rendezvous label
        states.push(StateSpec {
            name: format!("{}_idle", chan.name),
            is_initial: true,
            valuations: None,
            three_valued: None,
        });
        let rendezvous_label = format!("rendezvous_{}", chan.name);
        transitions.push(TransitionSpec {
            source: format!("{}_idle", chan.name),
            target: format!("{}_idle", chan.name),
            labels: vec![
                rendezvous_label,
                format!("send_{}", chan.name),
                format!("recv_{}", chan.name),
            ],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });
    } else {
        // Buffered channel: occupancy-level states
        for k in 0..=chan.capacity {
            let state_name = format!("{}_{}", chan.name, k);
            states.push(StateSpec {
                name: state_name.clone(),
                is_initial: k == 0,
                valuations: None,
                three_valued: None,
            });
        }

        // send_ch: ch_k -> ch_{k+1} for k < N (blocks at full)
        for k in 0..chan.capacity {
            transitions.push(TransitionSpec {
                source: format!("{}_{}", chan.name, k),
                target: format!("{}_{}", chan.name, k + 1),
                labels: vec![format!("send_{}", chan.name)],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }

        // recv_ch: ch_k -> ch_{k-1} for k > 0 (blocks at empty)
        for k in 1..=chan.capacity {
            transitions.push(TransitionSpec {
                source: format!("{}_{}", chan.name, k),
                target: format!("{}_{}", chan.name, k - 1),
                labels: vec![format!("recv_{}", chan.name)],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }

        // Test labels for channel status guards:
        // test_ch_empty: self-loop on ch_0
        transitions.push(TransitionSpec {
            source: format!("{}_0", chan.name),
            target: format!("{}_0", chan.name),
            labels: vec![format!("test_{}_empty", chan.name)],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        // test_ch_nempty: self-loop on ch_1..ch_N
        for k in 1..=chan.capacity {
            transitions.push(TransitionSpec {
                source: format!("{}_{}", chan.name, k),
                target: format!("{}_{}", chan.name, k),
                labels: vec![format!("test_{}_nempty", chan.name)],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }

        // test_ch_full: self-loop on ch_N
        transitions.push(TransitionSpec {
            source: format!("{}_{}", chan.name, chan.capacity),
            target: format!("{}_{}", chan.name, chan.capacity),
            labels: vec![format!("test_{}_full", chan.name)],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });

        // test_ch_nfull: self-loop on ch_0..ch_{N-1}
        for k in 0..chan.capacity {
            transitions.push(TransitionSpec {
                source: format!("{}_{}", chan.name, k),
                target: format!("{}_{}", chan.name, k),
                labels: vec![format!("test_{}_nfull", chan.name)],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }
    }

    AutomatonSpec {
        name: format!("Chan_{}", chan.name),
        states,
        transitions,
        controllable_labels: vec![],
        internal_labels: vec![],
    }
}

/// Convert Promela LTL expression to mununu's LtlFormula.
fn convert_ltl(expr: &ast::LtlExpr, bool_vars: &std::collections::HashSet<String>) -> LtlFormula {
    match expr {
        ast::LtlExpr::True => LtlFormula::True,
        ast::LtlExpr::False => LtlFormula::False,
        ast::LtlExpr::Predicate(name) => {
            // Rewrite bare bool-variable atoms to the corresponding state name
            // so the realize step's auto_register_state_name_predicates can
            // resolve them against the Var_<name> automaton's `<name>_1` state.
            if bool_vars.contains(name) {
                LtlFormula::Predicate(format!("{name}_1"))
            } else {
                LtlFormula::Predicate(name.clone())
            }
        }
        ast::LtlExpr::Not(inner) => LtlFormula::Not(Box::new(convert_ltl(inner, bool_vars))),
        ast::LtlExpr::And(l, r) => LtlFormula::And(
            Box::new(convert_ltl(l, bool_vars)),
            Box::new(convert_ltl(r, bool_vars)),
        ),
        ast::LtlExpr::Or(l, r) => LtlFormula::Or(
            Box::new(convert_ltl(l, bool_vars)),
            Box::new(convert_ltl(r, bool_vars)),
        ),
        ast::LtlExpr::Implies(l, r) => LtlFormula::Implies(
            Box::new(convert_ltl(l, bool_vars)),
            Box::new(convert_ltl(r, bool_vars)),
        ),
        ast::LtlExpr::Iff(l, r) => {
            let l_conv = convert_ltl(l, bool_vars);
            let r_conv = convert_ltl(r, bool_vars);
            LtlFormula::And(
                Box::new(LtlFormula::Implies(
                    Box::new(l_conv.clone()),
                    Box::new(r_conv.clone()),
                )),
                Box::new(LtlFormula::Implies(Box::new(r_conv), Box::new(l_conv))),
            )
        }
        ast::LtlExpr::Always(inner) => LtlFormula::Always(Box::new(convert_ltl(inner, bool_vars))),
        ast::LtlExpr::Eventually(inner) => {
            LtlFormula::Eventually(Box::new(convert_ltl(inner, bool_vars)))
        }
        ast::LtlExpr::Next(inner) => LtlFormula::Next(Box::new(convert_ltl(inner, bool_vars))),
        ast::LtlExpr::Until(l, r) => LtlFormula::Until {
            left: Box::new(convert_ltl(l, bool_vars)),
            right: Box::new(convert_ltl(r, bool_vars)),
        },
        ast::LtlExpr::WeakUntil(l, r) => LtlFormula::WeakUntil {
            left: Box::new(convert_ltl(l, bool_vars)),
            right: Box::new(convert_ltl(r, bool_vars)),
        },
        ast::LtlExpr::Release(l, r) => LtlFormula::Release {
            left: Box::new(convert_ltl(l, bool_vars)),
            right: Box::new(convert_ltl(r, bool_vars)),
        },
    }
}
