//! AIGER (And-Inverter Graph) adapter.
//!
//! Translates AIGER ASCII (.aag) circuits into CTXDSL via the shared IR.
//! The circuit's latches are enumerated into explicit states and AND gates
//! are evaluated to compute transitions.
//!
//! Supported property types:
//! - **Safety** (bad outputs): states where a bad literal is true are forbidden.
//! - **Justice** (liveness): each justice set generates a `G F` fairness property
//!   requiring the set's literals to be satisfied infinitely often.
//! - **Fairness** (single-literal liveness): each fairness constraint generates
//!   a `G F` property for the corresponding literal.
//!
//! Controllability is determined by the `--controllable-inputs` option; inputs
//! not listed are treated as uncontrollable (environment).

pub mod ast;
pub mod parser;
pub mod to_btor2;

use super::ir::*;
use super::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, AdapterWarning, FormatAdapter,
    SourceFormat, SourceInfo, WarningKind,
};

/// AIGER adapter implementing [`FormatAdapter`].
pub struct AigerAdapter;

impl FormatAdapter for AigerAdapter {
    fn detect(content: &str) -> bool {
        content.trim_start().starts_with("aag ")
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        let circuit = parser::parse(content)?;
        let ir = to_ir(&circuit, options)?;

        let num_latches = circuit.num_latches();
        let state_count = 1usize << num_latches;

        let mut warnings = Vec::new();

        if num_latches > 18 {
            return Err(AdapterError {
                kind: AdapterErrorKind::StateSpaceOverflow,
                message: format!(
                    "AIGER circuit has {} latches → {} states (max supported: 2^18 = 262144)",
                    num_latches, state_count
                ),
                location: None,
            });
        }

        if num_latches > 12 {
            warnings.push(AdapterWarning {
                kind: WarningKind::LargeStateSpace,
                message: format!(
                    "AIGER circuit produces {} states from {} latches",
                    state_count, num_latches
                ),
                location: None,
            });
        }

        // Warn about neutral controllability
        let has_controllable = !options.controllable_inputs.is_empty();
        if !has_controllable && !circuit.inputs.is_empty() {
            warnings.push(AdapterWarning {
                kind: WarningKind::NeutralControllability,
                message:
                    "All inputs treated as uncontrollable (no --controllable-inputs specified)"
                        .into(),
                location: None,
            });
        }

        let emit_result = super::emit::emit(&ir)?;

        Ok(AdapterOutput {
            sidecars: Vec::new(),
            ctxdsl: emit_result.ctxdsl,
            warnings,
            source_info: SourceInfo {
                format: SourceFormat::Aiger,
                title: None,
                signal_count: circuit.num_inputs() + circuit.num_latches(),
                state_count,
                property_count: ir.properties.len(),
            },
            state_valuations: Default::default(),
            transition_observations: Default::default(),
            partition_summary: None,
        })
    }
}

/// Convert a parsed AIGER circuit to AdapterIR using state enumeration.
fn to_ir(circuit: &ast::Circuit, options: &AdapterOptions) -> Result<AdapterIR, AdapterError> {
    let num_latches = circuit.num_latches();
    let num_inputs = circuit.num_inputs();
    let state_count = 1usize << num_latches;
    let input_combos = 1usize << num_inputs;

    let context_name = options
        .context_name
        .clone()
        .unwrap_or_else(|| "aiger_circuit".into());

    // Signals: latches are state dimensions, inputs are label dimensions
    let mut signals = Vec::new();
    for i in 0..num_inputs {
        let name = circuit.input_name(i);
        let controllable = options.controllable_inputs.contains(&name);
        signals.push(Signal {
            name,
            kind: if controllable {
                SignalKind::Output
            } else {
                SignalKind::Input
            },
            domain: SignalDomain::Boolean,
            role: SignalRole::Label,
        });
    }
    for i in 0..num_latches {
        signals.push(Signal {
            name: circuit.latch_name(i),
            kind: SignalKind::Neutral,
            domain: SignalDomain::Boolean,
            role: SignalRole::State,
        });
    }

    // Enumerate all states and input combinations to build the transition table
    // State names: bitvector of latch values
    let state_names: Vec<String> = (0..state_count)
        .map(|s| {
            let bits: String = (0..num_latches)
                .rev()
                .map(|b| if s & (1 << b) != 0 { '1' } else { '0' })
                .collect();
            format!("s{bits}")
        })
        .collect();

    // Input combo labels
    let input_labels: Vec<String> = (0..input_combos)
        .map(|ic| {
            let bits: String = (0..num_inputs)
                .rev()
                .map(|b| if ic & (1 << b) != 0 { '1' } else { '0' })
                .collect();
            format!("in_{bits}")
        })
        .collect();

    // Build explicit automaton with computed transitions
    let mut transitions = Vec::new();

    for (state_idx, state_name) in state_names.iter().enumerate() {
        for (input_idx, input_label) in input_labels.iter().enumerate() {
            // Build variable assignment: [unused, input0, input1, ..., latch0, latch1, ...]
            let mut values = vec![false; circuit.max_var + 1];

            // Set inputs
            for (i, &input_lit) in circuit.inputs.iter().enumerate() {
                let var_idx = input_lit / 2;
                values[var_idx] = (input_idx & (1 << (num_inputs - 1 - i))) != 0;
            }

            // Set latches
            for (i, latch) in circuit.latches.iter().enumerate() {
                let var_idx = latch.current / 2;
                values[var_idx] = (state_idx & (1 << (num_latches - 1 - i))) != 0;
            }

            // Evaluate gates
            circuit.eval_gates(&mut values);

            // Compute next state
            let next_latch_values = circuit.next_state(&values);
            let mut next_state_idx = 0usize;
            for (i, &val) in next_latch_values.iter().enumerate() {
                if val {
                    next_state_idx |= 1 << (num_latches - 1 - i);
                }
            }

            transitions.push(TransitionSpec {
                source: state_name.clone(),
                target: state_names[next_state_idx].clone(),
                labels: vec![input_label.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }
    }

    // Determine initial state from latch init values
    let mut init_state_idx = 0usize;
    for (i, latch) in circuit.latches.iter().enumerate() {
        if latch.init == 1 {
            init_state_idx |= 1 << (num_latches - 1 - i);
        }
    }

    let states: Vec<StateSpec> = state_names
        .iter()
        .enumerate()
        .map(|(i, name)| StateSpec {
            name: name.clone(),
            is_initial: i == init_state_idx,
            valuations: None,
            three_valued: None,
        })
        .collect();

    // Controllable labels
    let controllable_labels: Vec<String> = if !options.controllable_inputs.is_empty() {
        // If any input is controllable, ALL input combo labels that set those inputs are controllable
        // Simplified: mark all labels as controllable if any controllable input exists
        // (This is an approximation; full controllability requires game-level encoding)
        input_labels.clone()
    } else {
        vec![]
    };

    let automaton = AutomatonSpec {
        name: "Circuit".into(),
        states,
        transitions,
        controllable_labels,
        internal_labels: vec![],
    };

    // Bad-state properties
    let mut properties = Vec::new();
    for (i, &bad_lit) in circuit.bad_outputs.iter().enumerate() {
        // Find all states where the bad literal evaluates to true
        let mut bad_states = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            // Check if the bad literal is true for any input combination
            // For safety: bad if exists any input that makes bad true
            // Conservative: check all input combos
            for input_idx in 0..input_combos {
                let mut values = vec![false; circuit.max_var + 1];
                for (j, &input_lit) in circuit.inputs.iter().enumerate() {
                    let var_idx = input_lit / 2;
                    values[var_idx] = (input_idx & (1 << (num_inputs - 1 - j))) != 0;
                }
                for (j, latch) in circuit.latches.iter().enumerate() {
                    let var_idx = latch.current / 2;
                    values[var_idx] = (state_idx & (1 << (num_latches - 1 - j))) != 0;
                }
                circuit.eval_gates(&mut values);

                if circuit.eval_literal(bad_lit, &values) {
                    bad_states.push(state_name.clone());
                    break; // at least one input makes it bad
                }
            }
        }

        let bad_name = circuit
            .symbols
            .bad_names
            .get(i)
            .and_then(|n| n.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("bad_{i}"));

        // Safety property: bad states should never be reached
        // nu X. (!(bad_state_0 || bad_state_1 || ...) && [] X)
        if !bad_states.is_empty() {
            let bad_pred = bad_states.join(" || ");
            properties.push(PropertySpec {
                name: format!("safety_{}", super::emit::sanitize(&bad_name)),
                kind: PropertyKind::Safety,
                formula: PropertyFormula::MuCalculus(format!("nu X. ((!({bad_pred})) && ([] X))")),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    // Justice properties: for each justice set, find states where ALL literals are true
    for (i, justice_set) in circuit.justice_sets.iter().enumerate() {
        let mut justice_states = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            // A state satisfies a justice set if there exists an input combo
            // under which ALL literals in the set are true
            for input_idx in 0..input_combos {
                let mut values = vec![false; circuit.max_var + 1];
                for (j, &input_lit) in circuit.inputs.iter().enumerate() {
                    let var_idx = input_lit / 2;
                    values[var_idx] = (input_idx & (1 << (num_inputs - 1 - j))) != 0;
                }
                for (j, latch) in circuit.latches.iter().enumerate() {
                    let var_idx = latch.current / 2;
                    values[var_idx] = (state_idx & (1 << (num_latches - 1 - j))) != 0;
                }
                circuit.eval_gates(&mut values);

                let all_satisfied = justice_set
                    .iter()
                    .all(|&lit| circuit.eval_literal(lit, &values));
                if all_satisfied {
                    justice_states.push(state_name.clone());
                    break;
                }
            }
        }

        if !justice_states.is_empty() {
            let justice_pred = justice_states.join(" || ");
            // G F (justice_states) encoded as nu Y. ((mu X. ((pred) || <> X)) && ([] Y))
            properties.push(PropertySpec {
                name: format!("justice_{i}"),
                kind: PropertyKind::Fairness,
                formula: PropertyFormula::MuCalculus(format!(
                    "nu Y. ((mu X. (({justice_pred}) || (<> X))) && ([] Y))"
                )),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    // Fairness constraints: each is a single literal that must be true infinitely often
    for (i, &fair_lit) in circuit.fairness.iter().enumerate() {
        let mut fair_states = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            for input_idx in 0..input_combos {
                let mut values = vec![false; circuit.max_var + 1];
                for (j, &input_lit) in circuit.inputs.iter().enumerate() {
                    let var_idx = input_lit / 2;
                    values[var_idx] = (input_idx & (1 << (num_inputs - 1 - j))) != 0;
                }
                for (j, latch) in circuit.latches.iter().enumerate() {
                    let var_idx = latch.current / 2;
                    values[var_idx] = (state_idx & (1 << (num_latches - 1 - j))) != 0;
                }
                circuit.eval_gates(&mut values);

                if circuit.eval_literal(fair_lit, &values) {
                    fair_states.push(state_name.clone());
                    break;
                }
            }
        }

        if !fair_states.is_empty() {
            let fair_pred = fair_states.join(" || ");
            // G F (fairness_states) encoded as nu Y. ((mu X. ((pred) || <> X)) && ([] Y))
            properties.push(PropertySpec {
                name: format!("fairness_{i}"),
                kind: PropertyKind::Fairness,
                formula: PropertyFormula::MuCalculus(format!(
                    "nu Y. ((mu X. (({fair_pred}) || (<> X))) && ([] Y))"
                )),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    Ok(AdapterIR {
        metadata: Metadata {
            title: context_name,
            source_format: SourceFormat::Aiger,
            description: None,
            game_semantics: None,
            known_status: None,
        },
        signals,
        automata: vec![automaton],
        compositions: vec![],
        properties,
        controller: None,
    })
}
