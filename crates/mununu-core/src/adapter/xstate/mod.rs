//! XState / Statecharts adapter.
//!
//! Translates XState v5 JSON machine definitions into CTXDSL via the
//! explicit-automaton encoding path. Supports:
//! - Simple, compound (hierarchical), and parallel states
//! - Events as labeled transitions with optional guards
//! - Context variables as variable automata (Promela pattern)
//! - Mununu annotations for controllability and properties

pub mod ast;
pub mod emit_controller;
pub mod flatten;

use super::ir::*;
use super::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, AdapterWarning, FormatAdapter,
    SourceFormat, SourceInfo, WarningKind,
};
use ast::{ContextValue, MununuAnnotations, XStateMachine};
use flatten::FlatRegion;
use std::collections::{HashMap, HashSet};

/// XState adapter implementing [`FormatAdapter`].
pub struct XStateAdapter;

impl FormatAdapter for XStateAdapter {
    fn detect(content: &str) -> bool {
        // XState JSON: must be a JSON object with "states" key and either "initial" or "id"
        let trimmed = content.trim_start();
        if !trimmed.starts_with('{') {
            return false;
        }
        // Quick heuristic before full parse
        trimmed.contains("\"states\"")
            && (trimmed.contains("\"initial\"") || trimmed.contains("\"id\""))
    }

    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
        let machine: XStateMachine = serde_json::from_str(content).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("XState JSON parse error: {e}"),
            location: None,
        })?;

        let mut warnings = Vec::new();
        let ir = to_ir(&machine, options, &mut warnings)?;

        let result = super::emit::emit(&ir).map_err(|e| AdapterError {
            kind: AdapterErrorKind::EmitError,
            message: format!("CTXDSL emission failed: {e}"),
            location: None,
        })?;

        let property_count = ir.properties.len();
        let signal_count = count_events(&machine);

        Ok(AdapterOutput {
            sidecars: Vec::new(),
            ctxdsl: result.ctxdsl,
            warnings,
            source_info: SourceInfo {
                format: SourceFormat::XState,
                title: machine.id.clone(),
                signal_count,
                state_count: result.state_count,
                property_count,
            },
            state_valuations: Default::default(),
            transition_observations: Default::default(),
            partition_summary: None,
        })
    }
}

/// Convert an XState machine to the shared AdapterIR.
fn to_ir(
    machine: &XStateMachine,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<AdapterIR, AdapterError> {
    let machine_id = machine
        .id
        .as_deref()
        .or(options.context_name.as_deref())
        .unwrap_or("xstate_machine");

    // Flatten hierarchy
    let flat = flatten::flatten_machine(&machine.states, machine.initial.as_deref(), machine_id);

    if flat.regions.is_empty() {
        return Err(AdapterError {
            kind: AdapterErrorKind::IrConsistencyError,
            message: "XState machine has no states".to_string(),
            location: None,
        });
    }

    // Determine controllability from annotations and options
    let annotations = machine.mununu.as_ref();
    let controllable_events = build_controllable_set(annotations, options);
    let uncontrollable_events = build_uncontrollable_set(annotations);

    // Build automata from flat regions.
    // Track which labels have been claimed as controllable to avoid duplicates
    // across composed automata (CTXDSL requires each controllable label to be
    // claimed by exactly one automaton).
    let mut claimed_controllable: HashSet<String> = HashSet::new();
    let mut automata = Vec::new();
    for region in &flat.regions {
        let aut = build_automaton_from_region(
            region,
            &controllable_events,
            &uncontrollable_events,
            &mut claimed_controllable,
            warnings,
        );
        automata.push(aut);
    }

    // Build variable automata for bounded context variables
    build_variable_automata(machine, annotations, options, &mut automata, warnings);

    // Build compositions (if multiple regions from parallel states)
    let compositions = if automata.len() > 1 {
        let members: Vec<String> = automata.iter().map(|a| a.name.clone()).collect();
        vec![CompositionSpec::Synchronous {
            name: format!("{machine_id}_system"),
            members,
        }]
    } else {
        vec![]
    };

    // Build properties from annotations
    let properties = build_properties(annotations)?;

    // Build controller spec
    let controller = build_controller(annotations, &automata, &compositions);

    Ok(AdapterIR {
        metadata: Metadata {
            title: machine_id.to_string(),
            source_format: SourceFormat::XState,
            description: Some(format!("Translated from XState machine '{machine_id}'")),
            game_semantics: None,
            known_status: None,
        },
        signals: vec![],
        automata,
        compositions,
        properties,
        controller,
    })
}

/// Build an `AutomatonSpec` from a flattened region.
fn build_automaton_from_region(
    region: &FlatRegion,
    controllable: &HashSet<String>,
    uncontrollable: &HashSet<String>,
    claimed_controllable: &mut HashSet<String>,
    warnings: &mut Vec<AdapterWarning>,
) -> AutomatonSpec {
    let states: Vec<StateSpec> = region
        .states
        .iter()
        .map(|s| StateSpec {
            name: s.name.clone(),
            is_initial: s.is_initial,
            valuations: None,
            three_valued: None,
        })
        .collect();

    let transitions: Vec<TransitionSpec> = region
        .transitions
        .iter()
        .map(|t| {
            let mut labels = vec![t.event.clone()];
            // If there's a guard, add it as a label for synchronization
            if let Some(guard) = &t.guard {
                labels.push(format!("test_{guard}"));
            }
            TransitionSpec {
                source: t.source.clone(),
                target: t.target.clone(),
                labels,
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            }
        })
        .collect();

    // Collect all events used in this region
    let region_events: HashSet<String> =
        region.transitions.iter().map(|t| t.event.clone()).collect();

    // Classify controllability — only claim labels not yet claimed by another automaton
    let controllable_labels: Vec<String> = region_events
        .iter()
        .filter(|e| controllable.contains(e.as_str()) && !claimed_controllable.contains(e.as_str()))
        .cloned()
        .collect();
    for label in &controllable_labels {
        claimed_controllable.insert(label.clone());
    }

    // Warn about events with no controllability classification
    for event in &region_events {
        if !controllable.contains(event) && !uncontrollable.contains(event) {
            warnings.push(AdapterWarning {
                kind: WarningKind::NeutralControllability,
                message: format!(
                    "Event '{event}' has no controllability annotation; defaulting to uncontrollable"
                ),
                location: None,
            });
        }
    }

    AutomatonSpec {
        name: region.name.clone(),
        states,
        transitions,
        controllable_labels,
        internal_labels: vec![],
    }
}

/// Build variable automata for bounded context variables.
fn build_variable_automata(
    machine: &XStateMachine,
    annotations: Option<&MununuAnnotations>,
    options: &AdapterOptions,
    automata: &mut Vec<AutomatonSpec>,
    warnings: &mut Vec<AdapterWarning>,
) {
    for (var_name, value) in &machine.context {
        match value {
            ContextValue::Bool(initial) => {
                automata.push(create_bool_variable_automaton(var_name, *initial));
            }
            ContextValue::Number(initial) => {
                // Look for bounds in annotations or options
                let bounds = annotations
                    .and_then(|a| a.bounds.get(var_name))
                    .copied()
                    .or_else(|| options.variable_bounds.get(var_name).copied());

                match bounds {
                    Some((lo, hi)) => {
                        let init_i = *initial as i64;
                        automata.push(create_int_variable_automaton(var_name, lo, hi, init_i));
                    }
                    None => {
                        warnings.push(AdapterWarning {
                            kind: WarningKind::ApproximateTranslation,
                            message: format!(
                                "Context variable '{var_name}' is numeric with no bounds annotation; \
                                 skipping (add bounds via __mununu.bounds or --variable-bounds)"
                            ),
                            location: None,
                        });
                    }
                }
            }
            ContextValue::String(_) => {
                warnings.push(AdapterWarning {
                    kind: WarningKind::UnsupportedConstruct,
                    message: format!(
                        "Context variable '{var_name}' is a string; string context variables are not yet supported"
                    ),
                    location: None,
                });
            }
        }
    }
}

/// Create a boolean variable automaton (2 states: true/false).
fn create_bool_variable_automaton(name: &str, initial: bool) -> AutomatonSpec {
    let true_state = format!("{name}_true");
    let false_state = format!("{name}_false");
    let set_true = format!("set_{name}_true");
    let set_false = format!("set_{name}_false");
    let test_true = format!("test_{name}_true");
    let test_false = format!("test_{name}_false");

    AutomatonSpec {
        name: format!("Var_{name}"),
        states: vec![
            StateSpec {
                name: true_state.clone(),
                is_initial: initial,
                valuations: None,
                three_valued: None,
            },
            StateSpec {
                name: false_state.clone(),
                is_initial: !initial,
                valuations: None,
                three_valued: None,
            },
        ],
        transitions: vec![
            // set transitions
            TransitionSpec {
                source: false_state.clone(),
                target: true_state.clone(),
                labels: vec![set_true.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
            TransitionSpec {
                source: true_state.clone(),
                target: true_state.clone(),
                labels: vec![set_true.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
            TransitionSpec {
                source: true_state.clone(),
                target: false_state.clone(),
                labels: vec![set_false.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
            TransitionSpec {
                source: false_state.clone(),
                target: false_state.clone(),
                labels: vec![set_false.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
            // test transitions (self-loops)
            TransitionSpec {
                source: true_state.clone(),
                target: true_state.clone(),
                labels: vec![test_true.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
            TransitionSpec {
                source: false_state.clone(),
                target: false_state.clone(),
                labels: vec![test_false.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            },
        ],
        controllable_labels: vec![set_true, set_false, test_true, test_false],
        internal_labels: vec![],
    }
}

/// Create a bounded integer variable automaton.
fn create_int_variable_automaton(name: &str, lo: i64, hi: i64, initial: i64) -> AutomatonSpec {
    let mut states = Vec::new();
    let mut transitions = Vec::new();
    let mut ctrl_labels = Vec::new();

    // Create states for each value
    for v in lo..=hi {
        states.push(StateSpec {
            name: format!("{name}_{v}"),
            is_initial: v == initial,
            valuations: None,
            three_valued: None,
        });
    }

    // set transitions: from any state to target value
    for v in lo..=hi {
        let label = format!("set_{name}_{v}");
        ctrl_labels.push(label.clone());
        for src in lo..=hi {
            transitions.push(TransitionSpec {
                source: format!("{name}_{src}"),
                target: format!("{name}_{v}"),
                labels: vec![label.clone()],
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                additional_targets: Vec::new(),
            });
        }
    }

    // test transitions: self-loops for guard checking
    for v in lo..=hi {
        let label = format!("test_{name}_{v}");
        ctrl_labels.push(label.clone());
        transitions.push(TransitionSpec {
            source: format!("{name}_{v}"),
            target: format!("{name}_{v}"),
            labels: vec![label],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });
    }

    AutomatonSpec {
        name: format!("Var_{name}"),
        states,
        transitions,
        controllable_labels: ctrl_labels,
        internal_labels: vec![],
    }
}

/// Build the set of controllable events from annotations and options.
fn build_controllable_set(
    annotations: Option<&MununuAnnotations>,
    options: &AdapterOptions,
) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(ann) = annotations {
        set.extend(ann.controllable.iter().cloned());
    }
    // AdapterOptions.controllable_inputs is used inversely: in XState,
    // these are controllable events (not inputs). The name is from AIGER.
    set.extend(options.controllable_inputs.iter().cloned());
    set
}

/// Build the set of uncontrollable events from annotations.
fn build_uncontrollable_set(annotations: Option<&MununuAnnotations>) -> HashSet<String> {
    annotations
        .map(|a| a.uncontrollable.iter().cloned().collect())
        .unwrap_or_default()
}

/// Build properties from Mununu annotations.
///
/// Returns `Err(AdapterError)` if any property is malformed (missing both
/// `formula` and `template_ref`, or referencing an unknown template). This is a
/// fail-loud behavior: previously such properties were silently dropped, leading
/// to false-positive "satisfied" verdicts where violations were never checked.
fn build_properties(
    annotations: Option<&MununuAnnotations>,
) -> Result<Vec<PropertySpec>, AdapterError> {
    let ann = match annotations {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let registry =
        crate::adapter::templates::TemplateRegistry::configured(&[]).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("the template catalog: {e}"),
            location: None,
        })?;
    let mut out = Vec::with_capacity(ann.properties.len());
    for p in &ann.properties {
        let role = match p.role.as_str() {
            "assumption" => PropertyRole::Assumption,
            "guarantee" => PropertyRole::Guarantee,
            "invariant" => PropertyRole::Invariant,
            _ => PropertyRole::Standalone,
        };

        let formula_str = if let Some(f) = &p.formula {
            f.clone()
        } else if let Some(tref) = &p.template_ref {
            match registry.instantiate(tref) {
                Ok(inst) => inst.formula,
                Err(e) => {
                    return Err(AdapterError {
                        kind: AdapterErrorKind::ParseError,
                        message: format!(
                            "property '{}' references unknown template '{}': {}. \
                             Add the template to the registry or replace `template_ref` with a raw `formula`.",
                            p.name, tref.template, e
                        ),
                        location: None,
                    });
                }
            }
        } else {
            return Err(AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!(
                    "property '{}' declares neither `formula` nor `template_ref` — \
                     cannot translate. Add one of the two fields.",
                    p.name
                ),
                location: None,
            });
        };

        out.push(PropertySpec {
            name: p.name.clone(),
            kind: PropertyKind::Safety,
            formula: PropertyFormula::MuCalculus(formula_str),
            role,
            over: None,
            description: None,
        });
    }
    Ok(out)
}

/// Build controller spec if properties exist.
fn build_controller(
    annotations: Option<&MununuAnnotations>,
    automata: &[AutomatonSpec],
    compositions: &[CompositionSpec],
) -> Option<ControllerSpec> {
    let ann = annotations?;
    let first_prop = ann.properties.first()?;

    // Source is the composition if it exists, otherwise the first automaton
    let source = if let Some(comp) = compositions.first() {
        match comp {
            CompositionSpec::Synchronous { name, .. }
            | CompositionSpec::Asynchronous { name, .. } => name.clone(),
        }
    } else {
        automata.first()?.name.clone()
    };

    Some(ControllerSpec {
        name: "synth".to_string(),
        source_automaton: source,
        formula_name: first_prop.name.clone(),
    })
}

/// Count distinct events in the machine.
fn count_events(machine: &XStateMachine) -> usize {
    let mut events = HashSet::new();
    collect_events_recursive(&machine.states, &mut events);
    events.len()
}

fn collect_events_recursive(
    states: &HashMap<String, ast::XStateNode>,
    events: &mut HashSet<String>,
) {
    for node in states.values() {
        for event in node.on.keys() {
            events.insert(event.clone());
        }
        if let Some(children) = &node.states {
            collect_events_recursive(children, events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate_json(json: &str) -> AdapterOutput {
        let options = AdapterOptions::default();
        XStateAdapter::translate(json, &options).expect("translation should succeed")
    }

    #[test]
    fn detect_xstate_json() {
        assert!(XStateAdapter::detect(
            r#"{"id": "test", "initial": "s0", "states": {}}"#
        ));
        assert!(!XStateAdapter::detect("INFO { TITLE: \"test\" }"));
        assert!(!XStateAdapter::detect("aag 0 0 0 0 0"));
        assert!(!XStateAdapter::detect("active proctype P() { skip }"));
    }

    #[test]
    fn translate_simple_machine() {
        let json = r#"{
            "id": "light",
            "initial": "green",
            "states": {
                "green": { "on": { "TIMER": "yellow" } },
                "yellow": { "on": { "TIMER": "red" } },
                "red": { "on": { "TIMER": "green" } }
            },
            "__mununu": {
                "controllable": ["TIMER"],
                "properties": [
                    { "name": "safety", "formula": "nu X. ([] X)", "role": "guarantee" }
                ]
            }
        }"#;

        let output = translate_json(json);
        assert_eq!(output.source_info.format, SourceFormat::XState);
        assert_eq!(output.source_info.signal_count, 1); // 1 event: TIMER
        assert_eq!(output.source_info.property_count, 1);
        assert!(!output.ctxdsl.is_empty());

        // Verify CTXDSL can be parsed and realized
        let doc = crate::context_dsl::parse(&output.ctxdsl).unwrap();
        let realized = crate::context_dsl::realize_context(&doc, &[]).unwrap();
        let clts = realized.context.clts("light").expect("light automaton");
        assert_eq!(clts.state_count(), 3);
    }

    #[test]
    fn translate_with_controllability_warning() {
        let json = r#"{
            "id": "test",
            "initial": "a",
            "states": {
                "a": { "on": { "GO": "b" } },
                "b": {}
            }
        }"#;
        let output = translate_json(json);
        // GO has no controllability annotation → should warn
        assert!(
            output
                .warnings
                .iter()
                .any(|w| w.kind == WarningKind::NeutralControllability)
        );
    }

    #[test]
    fn translate_parallel_machine() {
        let json = r#"{
            "id": "app",
            "initial": "main",
            "states": {
                "main": {
                    "type": "parallel",
                    "states": {
                        "regionA": {
                            "initial": "off",
                            "states": {
                                "off": { "on": { "TOGGLE": "on" } },
                                "on": { "on": { "TOGGLE": "off" } }
                            }
                        },
                        "regionB": {
                            "initial": "idle",
                            "states": {
                                "idle": { "on": { "START": "active" } },
                                "active": { "on": { "STOP": "idle" } }
                            }
                        }
                    }
                }
            },
            "__mununu": {
                "controllable": ["TOGGLE", "START", "STOP"]
            }
        }"#;

        let output = translate_json(json);
        // Should produce CTXDSL with two automata and a synchronous composition
        let doc = crate::context_dsl::parse(&output.ctxdsl).unwrap();
        let realized = crate::context_dsl::realize_context(&doc, &[]).unwrap();

        // Both regions should exist as separate automata
        assert!(realized.context.clts("main_regionA").is_some());
        assert!(realized.context.clts("main_regionB").is_some());

        // Composition should exist
        assert!(realized.context.clts("app_system").is_some());
    }
}
