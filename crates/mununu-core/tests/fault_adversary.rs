//! PR C (2026-10-23) — the k-fault adversary as a two-player game, by known answer: the fault
//! inputs join the CONTROLLABLE side (the attacker), the design's own inputs stay the environment.
//! `realizable` ⇔ an attack wins against every environment behaviour; unrealizable ⇔ a positional
//! environment counterstrategy blocks every attack. The one-player question (the attacker also
//! drives the environment) is `fault_adversary::verify_fault_reach`; the pair separates "an attack
//! exists" from "an attack exists that the environment cannot block".
//!
//! The fixture is a trap FSM — `st`: 0 → 1 → 2 → 3 on the environment's `go`, 3 absorbing — with a
//! `done` latch. BLOCKABLE: `done' = (st == 2) ∧ ¬esc` (the environment's `esc` vetoes);
//! UNBLOCKABLE: `done' = (st == 2)`.

use mununu_core::adapter::btor2::fault::{FaultSpec, inject_faults};
use mununu_core::adapter::btor2::symbolic_bitblast::{
    exact_two_player_reach_realizable, exact_two_player_strategy,
};
use mununu_core::adapter::btor2::{emit, parser};
use mununu_core::adapter::fault_adversary::verify_fault_reach;

const BLOCKABLE: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n4 input 1 esc\n\
                         5 zero 2\n6 one 2\n7 constd 2 3\n8 constd 2 2\n9 state 2 st\n\
                         10 init 2 9 5\n11 eq 1 9 7\n12 add 2 9 6\n13 ite 2 3 12 9\n\
                         14 ite 2 11 9 13\n15 next 2 9 14\n16 zero 1\n17 state 1 done\n\
                         18 init 1 17 16\n19 eq 1 9 8\n20 not 1 4\n21 and 1 19 20\n\
                         22 next 1 17 21\n";
const UNBLOCKABLE: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n4 input 1 esc\n\
                           5 zero 2\n6 one 2\n7 constd 2 3\n8 constd 2 2\n9 state 2 st\n\
                           10 init 2 9 5\n11 eq 1 9 7\n12 add 2 9 6\n13 ite 2 3 12 9\n\
                           14 ite 2 11 9 13\n15 next 2 9 14\n16 zero 1\n17 state 1 done\n\
                           18 init 1 17 16\n19 eq 1 9 8\n20 next 1 17 19\n";

fn faulted(design: &str, ops: &[&str]) -> (String, Vec<String>, FaultSpec) {
    let spec = FaultSpec::parse_many(&ops.iter().map(|s| s.to_string()).collect::<Vec<_>>(), 1)
        .expect("parses");
    let file = parser::parse(design).expect("parses");
    let (out, _) = inject_faults(&file, &spec).expect("injects");
    (emit::emit_btor2(&out), spec.input_names(), spec)
}

fn realizable(model: &str, ctrl: &[String]) -> bool {
    let c: Vec<&str> = ctrl.iter().map(String::as_str).collect();
    exact_two_player_reach_realizable(model, "done == 1", &c).expect("decides")
}

#[test]
fn an_attack_the_environment_can_veto_is_unrealizable_though_it_exists() {
    let (model, ctrl, spec) = faulted(BLOCKABLE, &["st:1"]);
    assert!(
        !realizable(&model, &ctrl),
        "the environment holds `esc = 1`"
    );
    // the counterstrategy is the defence; it agrees with the verdict. (It is indexed by the goal's
    // register `done` alone, so the state-dependent `esc = 1` — forced only where `st == 2` — is
    // not unique per `done` value and is left out of the entry: the strategy-indexing follow-up the
    // plan records, not a wrong strategy.)
    let c: Vec<&str> = ctrl.iter().map(String::as_str).collect();
    let s = exact_two_player_strategy(&model, "done == 1", &c).expect("extracts");
    assert!(!s.realizable(), "{s:?}");
    // the one-player question: with the environment on its side the attacker gets there
    let one = verify_fault_reach(BLOCKABLE, &spec, Some("done == 1"), 20).expect("decides");
    assert_eq!(one.verdict, "violated", "{one:?}");
}

#[test]
fn an_unblockable_attack_is_realizable_with_one_fault_and_not_without() {
    let (model, ctrl, _) = faulted(UNBLOCKABLE, &["st:1"]);
    assert!(realizable(&model, &ctrl), "inject st := 2, done follows");
    let (model0, ctrl0, _) = faulted(UNBLOCKABLE, &["st:0"]);
    assert!(
        !realizable(&model0, &ctrl0),
        "without a fault the environment keeps go = 0 and st never reaches 2"
    );
}

/// PR C — the RTL pin on the OpenTitan edn boot FSM (`tests/fixtures/wall_classes/edn_boot_sm.btor`):
/// the mode inputs controllable, the acks from the environment. Without an injection the boot
/// handshake cannot be forced to complete (`exact_two_player_rtl.rs`); a zero budget must not change
/// that verdict, and one injection on `state_q` lets the attacker jump to the completed state.
#[test]
fn edn_boot_one_fault_forces_completion_a_zero_budget_does_not() {
    let edn = include_str!("fixtures/wall_classes/edn_boot_sm.btor");
    let mode_only = ["boot_req_mode_i".to_string(), "edn_enable_i".to_string()];
    let decide = |ops: &[&str]| {
        let (model, fault_inputs, _) = faulted(edn, ops);
        let ctrl: Vec<String> = mode_only.iter().cloned().chain(fault_inputs).collect();
        let c: Vec<&str> = ctrl.iter().map(String::as_str).collect();
        exact_two_player_reach_realizable(&model, "state_q == 44", &c).expect("decides")
    };
    assert!(
        !decide(&["state_q:0"]),
        "a zero budget reproduces the un-faulted verdict"
    );
    assert!(
        decide(&["state_q:1"]),
        "one injection reaches the completed state"
    );
}
