//! CrewAI JSON → `AdapterIR` translation.
//!
//! Per-agent automaton (Idle → Executing → Done cycle on
//! `agent_<role>_start` / `agent_<role>_complete`) plus a Crew
//! supervisor automaton sequencing the tasks. Asynchronous
//! composition of the supervisor + each agent matches Doc C §C.5
//! soundness — task ordering is asynchronous in real CrewAI flows
//! (LLM latency is non-deterministic).
//!
//! Only `process = "sequential"` is fully modelled today.
//! `hierarchical` / `consensual` emit a warning and fall back to
//! sequential.

use crate::adapter::crewai::ast::{Crew, MununuAnnotations};
use crate::adapter::ir::{
    AdapterIR, AutomatonSpec, CompositionSpec, Metadata, StateSpec, TransitionSpec,
};
use crate::adapter::{AdapterWarning, SourceFormat, WarningKind};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Translate a parsed [`Crew`] into the shared `AdapterIR`.
pub fn to_ir(crew: &Crew, warnings: &mut Vec<AdapterWarning>) -> AdapterIR {
    if crew.process != "sequential" {
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: crate::adapter::owner::mununu_limitation(
                &format!(
                    "process = \"{}\" is modelled as sequential: the tasks run one after \
                     another in listed order, and the manager's choice of order and delegation \
                     is dropped",
                    crew.process
                ),
                None,
            ),
            location: None,
        });
    }

    let crew_name = crew
        .name
        .as_ref()
        .map(|s| sanitise_ident(s))
        .unwrap_or_else(|| "Crew".to_string());

    let (override_ctrl, override_internal) = derive_controllability(&crew.mununu);

    // Per-agent automata.
    let mut automata: Vec<AutomatonSpec> = Vec::with_capacity(crew.agents.len() + 1);
    for agent in &crew.agents {
        let role = sanitise_ident(&agent.role);
        let start = format!("agent_{role}_start");
        let complete = format!("agent_{role}_complete");

        // Default controllability: `start` is controllable (the
        // crew supervisor / scheduler chooses when to dispatch);
        // `complete` is uncontrollable (the agent's LLM completes
        // when it completes — environment-driven). `__mununu`
        // overrides win.
        let mut controllable_labels = vec![start.clone()];
        let mut internal_labels: Vec<String> = Vec::new();
        if override_ctrl.contains(&complete) {
            controllable_labels.push(complete.clone());
        }
        if override_internal.contains(&start) {
            controllable_labels.retain(|l| l != &start);
            internal_labels.push(start.clone());
        }
        if override_internal.contains(&complete) {
            internal_labels.push(complete.clone());
        }

        automata.push(AutomatonSpec {
            name: format!("Agent_{role}"),
            states: vec![state_initial("Idle"), state("Executing"), state("Done")],
            transitions: vec![
                TransitionSpec {
                    source: "Idle".to_string(),
                    target: "Executing".to_string(),
                    labels: vec![start.clone()],
                    modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
                TransitionSpec {
                    source: "Executing".to_string(),
                    target: "Done".to_string(),
                    labels: vec![complete.clone()],
                    modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
                TransitionSpec {
                    source: "Done".to_string(),
                    target: "Idle".to_string(),
                    labels: vec![start.clone()],
                    modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
            ],
            controllable_labels,
            internal_labels,
        });
    }

    // Supervisor automaton (sequential process).
    let supervisor = build_sequential_supervisor(crew, &crew_name);

    // Composition: asynchronous over supervisor + every agent.
    let mut composition_members: Vec<String> = Vec::with_capacity(crew.agents.len() + 1);
    if let Some(s) = &supervisor {
        composition_members.push(s.name.clone());
    }
    for agent in &crew.agents {
        composition_members.push(format!("Agent_{}", sanitise_ident(&agent.role)));
    }

    if let Some(s) = supervisor {
        automata.push(s);
    }

    let compositions = if composition_members.len() >= 2 {
        vec![CompositionSpec::Asynchronous {
            name: format!("{crew_name}System"),
            members: composition_members,
        }]
    } else {
        Vec::new()
    };

    AdapterIR {
        metadata: Metadata {
            title: crew_name.clone(),
            source_format: SourceFormat::XState, // closest existing variant; new variants added in a follow-up
            description: Some(format!(
                "Translated from CrewAI crew '{}' ({} agents, {} tasks, process = {})",
                crew_name,
                crew.agents.len(),
                crew.tasks.len(),
                crew.process,
            )),
            game_semantics: None,
            known_status: None,
        },
        signals: Vec::new(),
        automata,
        compositions,
        properties: Vec::new(),
        controller: None,
    }
}

// ---------------------------------------------------------------------------
// Supervisor builder
// ---------------------------------------------------------------------------

fn build_sequential_supervisor(crew: &Crew, crew_name: &str) -> Option<AutomatonSpec> {
    if crew.tasks.is_empty() {
        return None;
    }
    let mut states: Vec<StateSpec> = Vec::with_capacity(crew.tasks.len() + 1);
    states.push(state_initial("Init"));
    for i in 0..crew.tasks.len() {
        states.push(state(&format!("AfterTask_{}", i + 1)));
    }

    let mut transitions: Vec<TransitionSpec> = Vec::with_capacity(crew.tasks.len());
    for (i, task) in crew.tasks.iter().enumerate() {
        let role = sanitise_ident(&task.agent);
        let complete = format!("agent_{role}_complete");
        let src = if i == 0 {
            "Init".to_string()
        } else {
            format!("AfterTask_{}", i)
        };
        let tgt = format!("AfterTask_{}", i + 1);
        transitions.push(TransitionSpec {
            source: src,
            target: tgt,
            labels: vec![complete],
            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,

            additional_targets: Vec::new(),
        });
    }

    Some(AutomatonSpec {
        name: format!("{crew_name}Supervisor"),
        states,
        transitions,
        controllable_labels: Vec::new(),
        internal_labels: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn state_initial(name: &str) -> StateSpec {
    StateSpec {
        name: name.to_string(),
        is_initial: true,
        valuations: None,
        three_valued: None,
    }
}

fn state(name: &str) -> StateSpec {
    StateSpec {
        name: name.to_string(),
        is_initial: false,
        valuations: None,
        three_valued: None,
    }
}

/// Sanitise a CrewAI role / name into a CTXDSL identifier.
pub(crate) fn sanitise_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for c in s.chars() {
        let ok = if first {
            c.is_ascii_alphabetic() || c == '_'
        } else {
            c.is_ascii_alphanumeric() || c == '_'
        };
        out.push(if ok { c } else { '_' });
        first = false;
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn derive_controllability(mununu: &Option<MununuAnnotations>) -> (Vec<String>, Vec<String>) {
    match mununu {
        Some(m) => (m.controllable.clone(), m.internal.clone()),
        None => (Vec::new(), Vec::new()),
    }
}
