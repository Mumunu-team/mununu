//! Industrial BPM Example: Sterile Pharmaceutical Batch Release
//!
//! This test instantiates a regulated fill-and-finish workflow — mirroring Pfizer/Roche style
//! pipelines under FDA 21 CFR Part 11 & EU GMP Annex 1 — in four CLTS components: production,
//! quality control, qualified-person release, and cold-chain logistics. It documents the
//! compliance requirement that nothing ships before QC pass + QP sign-off, and every completed
//! batch eventually gets released or quarantined.
//!
//! The synchronous composition over these four components admits only the joint initial state
//! as reachable (private labels do not fire alone under strict synchronous semantics), so the
//! textbook safety invariant `nu Safe. ((¬<ship>true ∨ <qp_sign>true) ∧ [] Safe)` and the
//! corresponding leads-to liveness property hold vacuously and controller synthesis succeeds
//! trivially. The test asserts that observable behaviour; a richer composition (asynchronous,
//! or one that explicitly relabels private actions to a global tick) would exercise the
//! formulas non-vacuously.
use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx};
use mununu_core::context_dsl::parser;
use mununu_core::examples::sterile_batch_release::{SterileScenario, sterile_scenario};
use std::collections::VecDeque;
use std::error::Error;

fn reachable_states(clts: &Clts<DefaultStateIdx, DefaultLabelIdx>) -> Vec<bool> {
    let mut visited = vec![false; clts.state_count()];
    let mut queue = VecDeque::new();

    for initial in clts.initial_states() {
        visited[initial.index()] = true;
        queue.push_back(*initial);
    }

    while let Some(state) = queue.pop_front() {
        for transition in clts.outgoing(state) {
            let target = transition.target();
            if !visited[target.index()] {
                visited[target.index()] = true;
                queue.push_back(target);
            }
        }
    }

    visited
}

#[test]
fn sterile_batch_release_properties_hold() -> Result<(), Box<dyn Error>> {
    let SterileScenario {
        context,
        pipeline,
        ship_requires_release,
        completion_leads_to_disposition,
        environment,
    } = sterile_scenario().map_err(|e| -> Box<dyn Error> { Box::new(e) })?;

    let pipeline_clts = context
        .clts(pipeline)
        .expect("pipeline registered in scenario context");
    let reachable = reachable_states(pipeline_clts);

    // The synchronous composition over the four pipeline components has a
    // single reachable joint state (private labels can't fire alone), so the
    // textbook safety invariant holds vacuously at that single state. A
    // non-vacuous version of this test would need an asynchronous composition
    // or a relabeling of private actions.
    let safety = context.evaluate_mu(pipeline, &ship_requires_release, &environment, None)?;
    assert!(
        reachable
            .iter()
            .enumerate()
            .all(|(idx, &rb)| { !rb || safety.get(idx).is_some_and(|bit| *bit) }),
        "ship_requires_release should hold at every reachable state under textbook semantics"
    );

    let liveness = context.evaluate_mu(
        pipeline,
        &completion_leads_to_disposition,
        &environment,
        None,
    )?;
    assert!(
        reachable
            .iter()
            .enumerate()
            .all(|(idx, &rb)| { !rb || liveness.get(idx).is_some_and(|bit| *bit) }),
        "completion_leads_to_disposition should hold at every reachable state under textbook semantics"
    );

    let controller =
        context.synthesise_controller(pipeline, &ship_requires_release, &environment, None)?;
    assert!(controller.realizable);
    assert!(controller.diagnostics.violating_initials.is_empty());
    assert!(controller.diagnostics.counterexample_trace.is_none());
    assert!(controller.diagnostics.deadlock_traces.is_empty());
    assert!(controller.diagnostics.counterstrategy_traces.is_empty());

    Ok(())
}

#[test]
fn sterile_batch_release_dsl_example_parses() -> Result<(), Box<dyn Error>> {
    let doc = parser::parse(include_str!(
        "../../../../examples/sterile_batch_release.ctxdsl"
    ))
    .map_err(|e| -> Box<dyn Error> { Box::new(e) })?;
    assert_eq!(doc.alphabet.len(), 8);
    assert_eq!(doc.automata.len(), 4);
    assert_eq!(doc.compositions.len(), 1);
    assert_eq!(doc.controllers.len(), 1);
    assert_eq!(doc.mu_formulas.len(), 2);
    Ok(())
}
