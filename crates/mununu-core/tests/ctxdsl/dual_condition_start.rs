use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
use mununu_core::context_dsl::parser;
use mununu_core::examples::dual_condition;
use std::collections::HashSet;
use std::error::Error;

fn label_payloads(machine: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> Vec<HashSet<String>> {
    machine
        .states()
        .flat_map(|state| machine.outgoing(state).iter())
        .map(|transition| {
            transition
                .labels()
                .iter()
                .flat_map(|id| {
                    machine
                        .label_payload(*id)
                        .expect("label payload present")
                        .iter()
                        .cloned()
                })
                .collect::<HashSet<_>>()
        })
        .collect()
}

#[test]
fn machine_start_requires_gate_and_operator() -> Result<(), Box<dyn Error>> {
    let machine = dual_condition::assembly_machine()?;
    let idle = machine.state_id("Idle")?;
    let running = machine.state_id("Running")?;

    let start_transition = machine
        .outgoing(idle)
        .iter()
        .find(|transition| transition.target() == running)
        .expect("Idle -> Running transition present");

    let start_labels: HashSet<String> = start_transition
        .labels()
        .iter()
        .flat_map(|label| machine.label_payload(*label).unwrap().iter().cloned())
        .collect();

    assert!(start_labels.contains("gate_closed"));
    assert!(start_labels.contains("operator_ready"));
    assert_eq!(
        start_labels.len(),
        2,
        "expected both conditions to be required"
    );

    // Ensure no other transition reuses the same multi-element set unintentionally.
    for payload in label_payloads(&machine) {
        if payload != start_labels {
            assert!(
                payload.len() <= 1,
                "only the start transition should require both conditions"
            );
        }
    }

    Ok(())
}

#[test]
fn dual_condition_ctxdsl_example_parses() -> Result<(), Box<dyn Error>> {
    parser::parse(include_str!(
        "../../../../examples/dual_condition_start.ctxdsl"
    ))
    .map_err(|e| -> Box<dyn Error> { Box::new(e) })?;
    Ok(())
}
