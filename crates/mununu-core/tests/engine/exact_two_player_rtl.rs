//! P2.5-F — the exact two-player game engine (`exact_two_player_verdict`) validated on REAL RTL: the
//! OpenTitan `edn_main_sm` boot handshake and `otbn_start_stop_control`, the same verbatim-extracted
//! fixtures the 1-player recoverability work uses. Here they are viewed as genuine TWO-PLAYER games by
//! partitioning their primary inputs into controller-owned vs environment-owned.
//!
//! The industrial reading: the software-programmed MODE/REQUEST signals (`boot_req_mode_i`,
//! `edn_enable_i`, `start_i`) are the CONTROLLER's; the acknowledge / error / escalation signals coming
//! from OTHER modules (`csrng_cmd_ack_i`, `local_escalate_i`, `urnd_reseed_ack_i`, …) are the
//! ENVIRONMENT's. Two hand-verifiable results, on real designs:
//!
//! 1. **Reduction check (all inputs controllable).** With every input the controller's, the controllable
//!    predecessor collapses to the single-agent diamond (`∀∅ ∃all = ∃all`), so `<(ctrl=controllable)>`
//!    ≡ `<>` and the two-player verdict must equal the 1-player reachability verdict. This cross-checks
//!    `cpre_controllable` against the already-validated `diamond_pre` on real RTL.
//!
//! 2. **The genuine two-player finding (mode controllable, acks environment).** Driving the mode signals
//!    is NECESSARY but NOT SUFFICIENT: the environment can withhold the acknowledge (`csrng_cmd_ack_i` /
//!    `urnd_reseed_ack_i`) forever, or escalate — so the controller CANNOT force the handshake to
//!    complete. The verdict flips to VIOLATED. This is exactly the motivation for an assume-guarantee
//!    ENVIRONMENT ASSUMPTION ("the acknowledging module eventually acks"): realizability of the handshake
//!    holds only under such an assumption. The flip HOLDS→VIOLATED between (1) and (2), driven purely by
//!    moving the acks from the controller to the environment, is the industrial validation of the game.

use mununu_core::adapter::btor2::symbolic_bitblast::{
    ExactVerdict, exact_symbolic_verdict, exact_two_player_verdict,
};
use mununu_core::mu_calculus::parser;

const EDN: &str = include_str!("../fixtures/wall_classes/edn_boot_sm.btor");
const OTBN: &str = include_str!("../fixtures/wall_classes/otbn_start_stop_fsm.btor");

// Every primary input of each fixture — the "all controllable" partition (⇒ single-agent reduction).
const EDN_ALL: &[&str] = &[
    "auto_req_mode_i",
    "boot_req_mode_i",
    "clk_i",
    "cmd_sent_i",
    "csrng_ack_err_i",
    "csrng_cmd_ack_i",
    "edn_enable_i",
    "local_escalate_i",
    "max_reqs_cnt_zero_i",
    "rst_ni",
    "sw_cmd_req_load_i",
];
const OTBN_ALL: &[&str] = &[
    "rma_req_i",
    "escalate_i",
    "urnd_reseed_ack_i",
    "secure_wipe_req_i",
    "start_i",
    "rst_ni",
    "clk_i",
];

/// Reduction check on edn: all-controllable two-player reachability equals the 1-player diamond
/// reachability (both HOLD — BootUniAckWait is reachable from reset). Confirms `cpre_controllable`
/// reduces to `diamond_pre` when the environment owns nothing, on real RTL.
#[test]
fn edn_all_controllable_reduces_to_one_player() {
    let two_player = parser::parse("mu X. ((state_q == 44) || <(ctrl = controllable)> X)").unwrap();
    let one_player = parser::parse("mu X. ((state_q == 44) || <> X)").unwrap();
    let tp = exact_two_player_verdict(EDN, &two_player, EDN_ALL).unwrap();
    let op = exact_symbolic_verdict(EDN, &one_player).unwrap();
    assert_eq!(
        tp,
        ExactVerdict::Holds,
        "all-controllable: the handshake is reachable"
    );
    assert_eq!(
        tp, op,
        "all-controllable two-player == one-player diamond reachability"
    );
}

/// The genuine two-player finding on edn: with the mode signals controllable but the acks/escalation
/// coming from the environment, the controller CANNOT force the boot handshake to complete — the
/// environment withholds `csrng_cmd_ack_i` (stalling the ack-wait states) or asserts `local_escalate_i`
/// (diverting to Error). VIOLATED. Realizing the handshake therefore requires an environment assumption
/// (the acknowledging module eventually acks, no spurious escalation). The flip from the all-controllable
/// HOLDS above is driven purely by who owns the acks.
#[test]
fn edn_boot_completion_needs_an_environment_assumption() {
    let reach = parser::parse("mu X. ((state_q == 44) || <(ctrl = controllable)> X)").unwrap();
    let mode_only: &[&str] = &["boot_req_mode_i", "edn_enable_i"];
    assert_eq!(
        exact_two_player_verdict(EDN, &reach, mode_only).unwrap(),
        ExactVerdict::Violated,
        "mode-controllable, acks environment: the controller cannot force completion"
    );
}

/// Reduction check on otbn: all-controllable two-player reachability of Running equals the 1-player
/// diamond reachability (both HOLD).
#[test]
fn otbn_all_controllable_reduces_to_one_player() {
    let two_player =
        parser::parse("mu X. ((state_q == 119) || <(ctrl = controllable)> X)").unwrap();
    let one_player = parser::parse("mu X. ((state_q == 119) || <> X)").unwrap();
    let tp = exact_two_player_verdict(OTBN, &two_player, OTBN_ALL).unwrap();
    let op = exact_symbolic_verdict(OTBN, &one_player).unwrap();
    assert_eq!(
        tp,
        ExactVerdict::Holds,
        "all-controllable: Running is reachable"
    );
    assert_eq!(
        tp, op,
        "all-controllable two-player == one-player diamond reachability"
    );
}

/// The genuine two-player finding on otbn: with only `start_i` controllable, the environment withholds
/// `urnd_reseed_ack_i` (stalling UrndRefresh) or asserts escalation — so the controller cannot force
/// reaching Running. VIOLATED; realizability needs the environment ack assumption. Same pattern as edn,
/// a second real design.
#[test]
fn otbn_run_needs_an_environment_assumption() {
    let reach = parser::parse("mu X. ((state_q == 119) || <(ctrl = controllable)> X)").unwrap();
    let mode_only: &[&str] = &["start_i"];
    assert_eq!(
        exact_two_player_verdict(OTBN, &reach, mode_only).unwrap(),
        ExactVerdict::Violated,
        "start controllable, acks environment: the controller cannot force reaching Running"
    );
}
