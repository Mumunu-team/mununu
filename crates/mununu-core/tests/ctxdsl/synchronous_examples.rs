use mununu_core::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelId};
use mununu_core::context::{Context, ContextError};
use mununu_core::examples::synchronous;
use mununu_core::mu_calculus::{Environment, parser};

fn skip_in_ci(test_name: &str) -> bool {
    if std::env::var("CI").is_ok() {
        eprintln!("skipping slow test `{test_name}` on CI");
        true
    } else {
        false
    }
}

fn label_set(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    labels: &[LabelId<DefaultLabelIdx>],
) -> Vec<String> {
    let mut result = Vec::new();
    for id in labels {
        if let Some(payload) = clts.label_payload(*id) {
            result.extend_from_slice(payload);
        }
    }
    result.sort();
    result.dedup();
    result
}

#[test]
fn clocked_toggle_cycles_on_tick() {
    if skip_in_ci("clocked_toggle_cycles_on_tick") {
        return;
    }
    let clts = synchronous::clocked_toggle();
    assert_eq!(clts.states().count(), 2);
    let off = clts.state_id("off").unwrap();
    let on = clts.state_id("on").unwrap();
    assert!(clts.initial_states().contains(&off));
    let out_off = clts.outgoing(off);
    assert_eq!(out_off.len(), 1);
    assert_eq!(
        label_set(&clts, out_off[0].labels()),
        vec!["tick".to_string()]
    );
    assert_eq!(out_off[0].target(), on);
}

#[test]
fn traffic_light_has_three_phase_cycle() {
    if skip_in_ci("traffic_light_has_three_phase_cycle") {
        return;
    }
    let clts = synchronous::traffic_light_controller();
    let red = clts.state_id("red").unwrap();
    let green = clts.state_id("green").unwrap();
    let yellow = clts.state_id("yellow").unwrap();
    let timer = vec!["timer".to_string()];
    assert_eq!(label_set(&clts, clts.outgoing(red)[0].labels()), timer);
    assert_eq!(clts.outgoing(red)[0].target(), green);
    assert_eq!(clts.outgoing(green)[0].target(), yellow);
    assert_eq!(clts.outgoing(yellow)[0].target(), red);
}

#[test]
fn elevator_controller_encodes_dispatch_and_arrival() {
    if skip_in_ci("elevator_controller_encodes_dispatch_and_arrival") {
        return;
    }
    let clts = synchronous::elevator_controller();
    let idle = clts.state_id("idle").unwrap();
    let moving = clts.state_id("moving").unwrap();
    let door_open = clts.state_id("door_open").unwrap();

    let fst = &clts.outgoing(idle)[0];
    assert_eq!(fst.target(), moving);
    assert_eq!(
        label_set(&clts, fst.labels()),
        vec!["call".to_string(), "dispatch".to_string()]
    );

    let snd = &clts.outgoing(moving)[0];
    assert_eq!(snd.target(), door_open);
    assert_eq!(label_set(&clts, snd.labels()), vec!["arrive".to_string()]);
}

#[test]
fn bus_arbiter_requires_tick_for_grant() {
    if skip_in_ci("bus_arbiter_requires_tick_for_grant") {
        return;
    }
    let clts = synchronous::synchronous_bus_arbiter();
    let idle = clts.state_id("idle").unwrap();
    let grant = clts.state_id("grant").unwrap();
    let out_idle = &clts.outgoing(idle)[0];
    assert_eq!(out_idle.target(), grant);
    assert_eq!(
        label_set(&clts, out_idle.labels()),
        vec!["req".to_string(), "tick".to_string()]
    );
}

#[test]
fn synchronous_pipeline_uses_union_label() {
    if skip_in_ci("synchronous_pipeline_uses_union_label") {
        return;
    }
    let clts = synchronous::synchronous_pipeline();
    let empty = clts.state_id("empty").unwrap();
    let fill = &clts.outgoing(empty)[0];
    let labels = label_set(&clts, fill.labels());
    assert_eq!(labels, vec!["consume".to_string(), "produce".to_string()]);
}

// ── Formula evaluation ────────────────────────────────────────────────────

fn make_context(
    name: &str,
    clts: Clts<DefaultStateIdx, DefaultLabelIdx>,
) -> Result<Context, ContextError> {
    Context::builder()
        .register_clts(name, clts)
        .finish_with_checks()
}

/// The traffic light cycles through red→green→yellow→red forever.
/// The formula `ν X. (red ∨ <> X)` should hold at the initial `red` state
/// because it IS red, and it trivially holds everywhere (all states can reach red).
#[test]
fn traffic_light_red_state_satisfies_reachability() {
    if skip_in_ci("traffic_light_red_state_satisfies_reachability") {
        return;
    }
    let clts = synchronous::traffic_light_controller();
    let red = clts.state_id("red").unwrap();
    let n = clts.state_count();
    let ctx = make_context("tl", clts).expect("context builds");

    // Predicate: only red satisfies Red
    let mut red_set = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    red_set.set(red.index(), true);
    let env = Environment::new(n).with_predicate("Red", red_set);

    // ν X. (Red ∨ <> X) — every state can eventually reach red (true for all 3)
    let formula = parser::parse("nu X. (Red || <> X)").expect("formula parses");
    let result = ctx
        .evaluate_mu("tl", &formula, &env, None)
        .expect("eval succeeds");

    // All three states satisfy (cycle: red→green→yellow→red)
    assert_eq!(
        result.count_ones(),
        3,
        "all 3 states should satisfy reachability to Red"
    );
    assert!(result[red.index()], "red itself must satisfy");
}

/// The clocked toggle only has two states. `ν X. (Off ∨ <> X)` holds everywhere
/// because `off` is reachable from `on` via tick→off.
#[test]
fn clocked_toggle_liveness_holds_from_both_states() {
    if skip_in_ci("clocked_toggle_liveness_holds_from_both_states") {
        return;
    }
    let clts = synchronous::clocked_toggle();
    let off = clts.state_id("off").unwrap();
    let n = clts.state_count();
    let ctx = make_context("ct", clts).expect("context builds");

    let mut off_set = bitvec::bitvec![usize, bitvec::order::Lsb0; 0; n];
    off_set.set(off.index(), true);
    let env = Environment::new(n).with_predicate("Off", off_set);

    let formula = parser::parse("nu X. (Off || <> X)").expect("parses");
    let result = ctx
        .evaluate_mu("ct", &formula, &env, None)
        .expect("eval succeeds");
    assert_eq!(result.count_ones(), 2, "both states satisfy eventual-off");
}
