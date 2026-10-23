//! P2.5-F — the exact-symbolic engine EXTRACTING the two-player game strategy via
//! `exact_two_player_strategy`. Where `exact_two_player_verdict` answers realizable? (yes/no), this
//! recovers the WINNER's strategy: the CONTROLLER's Mealy strategy when the reachability game is
//! realizable, or the ENVIRONMENT's positional counterstrategy (the witness for why no controller wins)
//! when it is not.
//!
//! The Mealy/positional asymmetry is intrinsic to the game (`CPre_ctrl = ∀env ∃ctrl` — the environment
//! moves, the controller responds): the environment is the first-mover, so its winning strategy is
//! positional; the controller responds, so its strategy may depend on the environment input. Validation
//! is by KNOWN ANSWER on three minimal 1-bit games plus a reduction + an unrealizable finding on the
//! real OpenTitan `edn_main_sm`:
//!
//! - `CTRL`  (`st' = c`)       : realizable, env-INDEPENDENT — controller forces `c = 1` (Moore move).
//! - `XORG`  (`st' = c ⊕ e`)   : realizable, but REACTIVE — no single `c` works for all `e`, so the
//!   controller must respond `c = ¬e`. This is the case a Moore (state-only) extractor gets WRONG.
//! - `ENVBLK`(`st' = c & ¬e`)  : unrealizable — the environment counterstrategy forces `e = 1`.
//! - `edn` all-controllable    : Moore everywhere, so `MealyStrategy::as_positional` EQUALS the 1-player
//!   positional strategy.
//! - `edn` mode-controllable   : unrealizable — the counterstrategy forces only ENVIRONMENT inputs.

use mununu_core::adapter::btor2::symbolic_bitblast::{
    TwoPlayerStrategy, exact_env_positional_strategy, exact_two_player_strategy,
    game_sound_posture_model,
};

// A reset-artifact game: `st' = rst ? 0 : c` (active-high async reset). controllable = {c}. With `rst`
// ADVERSARIAL the environment holds `rst=1` forever → `st` stuck at 0 → `st==1` unreachable ⇒ spuriously
// UNREALIZABLE (a modeling artifact — reset is not a real adversarial input). `game_sound_posture_model`
// pins the reset inactive (+ reset-init), so the sound game `st'=c` is realizable.
const RESET_GAME: &str = "1 sort bitvec 1\n2 input 1 c\n3 input 1 rst\n4 state 1 st\n5 zero 1\n6 init 1 4 5\n7 ite 1 3 5 2\n8 next 1 4 7\n";

// st' = c : the controller sets the next state directly (environment powerless).
const CTRL_INIT0: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 next 1 4 2
";

// st' = c ⊕ e : reaching st=1 needs c = ¬e — a REACTIVE controller (no env-independent winning move).
const XORG_INIT0: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 xor 1 2 3
8 next 1 4 7
";

// st' = c & ¬e : the environment can force st' = 0 by asserting e.
const ENVBLK_INIT0: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 not 1 3
8 and 1 2 7
9 next 1 4 8
";

const EDN: &str = include_str!("fixtures/wall_classes/edn_boot_sm.btor");
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

/// CTRL (st'=c), `good = st==1`, `c` controllable: realizable and env-INDEPENDENT. The controller
/// strategy is a single Moore move forcing `c = 1` at the rank-1 state `st=0`, nothing at the already-good
/// `st=1`, and it never forces the environment input `e`.
#[test]
fn controller_strategy_moore_move() {
    let strat = exact_two_player_strategy(CTRL_INIT0, "st == 1", &["c"]).unwrap();
    let TwoPlayerStrategy::ControllerStrategy(m) = strat else {
        panic!("st'=c is realizable ⇒ a controller strategy");
    };
    assert_eq!(m.state_register, "st");
    let at0 = m.entries.iter().find(|e| e.state_value == 0).unwrap();
    assert_eq!(at0.rank, 1);
    // A single env-independent move forcing c=1, robust to every e.
    assert_eq!(at0.moves.len(), 1);
    assert!(
        at0.moves[0].env_inputs.is_empty(),
        "the move is env-independent"
    );
    assert_eq!(at0.moves[0].forced_ctrl.get("c"), Some(&1));
    assert!(
        !at0.moves[0].forced_ctrl.contains_key("e"),
        "e is not the controller's"
    );
    // The already-good state needs no move.
    let at1 = m.entries.iter().find(|e| e.state_value == 1).unwrap();
    assert_eq!(at1.rank, 0);
    assert!(at1.moves.is_empty());
    // Moore everywhere ⇒ it projects to a positional strategy.
    assert!(m.as_positional().is_some());
}

/// XORG (st'=c⊕e), `good = st==1`, `c` controllable: realizable but REACTIVE. There is NO single `c` that
/// reaches good for every `e` (`∃c ∀e. c⊕e==1` is false), yet `∀e ∃c` holds (`c=¬e`). The Mealy strategy
/// must give two env-conditioned responses: `e=0 → c=1`, `e=1 → c=0`. A state-only (Moore) extractor would
/// emit no forced input here — the bug this fix closes — so `as_positional` must be `None`.
#[test]
fn controller_strategy_reactive_mealy_move() {
    let strat = exact_two_player_strategy(XORG_INIT0, "st == 1", &["c"]).unwrap();
    let TwoPlayerStrategy::ControllerStrategy(m) = strat else {
        panic!("st'=c⊕e is realizable (Mealy c=¬e) ⇒ a controller strategy");
    };
    let at0 = m.entries.iter().find(|e| e.state_value == 0).unwrap();
    assert_eq!(at0.rank, 1);
    assert!(
        at0.complete,
        "the reactive fan-out (2 moves) is fully enumerated"
    );
    // Two env-conditioned responses, each c = ¬e.
    assert_eq!(at0.moves.len(), 2, "one response per environment input");
    for mv in &at0.moves {
        let e = *mv
            .env_inputs
            .get("e")
            .expect("the response is conditioned on e");
        let c = *mv.forced_ctrl.get("c").expect("the controller forces c");
        assert_eq!(c, 1 - e, "the controller plays c = ¬e");
    }
    // No env-independent positional move exists here.
    assert!(
        m.as_positional().is_none(),
        "a reactive controller has no state-only positional strategy"
    );
}

/// ENVBLK (st'=c&¬e), `good = st==1`, `c` controllable: UNREALIZABLE — for `e=1` no `c` reaches good.
/// The extractor returns the ENVIRONMENT's positional counterstrategy: at `st=0` it forces `e = 1` (the
/// move keeping the play out of `{st==1}` against every `c`), and never forces the controllable input `c`.
#[test]
fn environment_counterstrategy_positional_move() {
    let strat = exact_two_player_strategy(ENVBLK_INIT0, "st == 1", &["c"]).unwrap();
    let TwoPlayerStrategy::EnvironmentCounterstrategy(p) = strat else {
        panic!("st'=c&¬e is unrealizable ⇒ an environment counterstrategy");
    };
    let at0 = p.entries.iter().find(|e| e.state_value == 0).unwrap();
    assert_eq!(
        at0.forced_inputs.get("e"),
        Some(&1),
        "the environment blocks with e=1"
    );
    assert!(
        !at0.forced_inputs.contains_key("c"),
        "c is the controller's, not forced"
    );
}

/// Reduction on real RTL: with EVERY input controllable, the game is Moore everywhere (no environment to
/// react to), so the controller Mealy strategy projects to a positional one that EQUALS the 1-player
/// `exact_env_positional_strategy`. Cross-checks the game extractor against the 1-player extractor on the
/// OpenTitan boot FSM.
#[test]
fn edn_all_controllable_projects_to_one_player() {
    let good = "state_q == 44";
    let one = exact_env_positional_strategy(EDN, good).expect("1-player strategy");
    let strat = exact_two_player_strategy(EDN, good, EDN_ALL).expect("2-player strategy");
    let TwoPlayerStrategy::ControllerStrategy(m) = strat else {
        panic!("all-controllable: the handshake is reachable ⇒ a controller strategy");
    };
    assert_eq!(
        m.as_positional(),
        Some(one),
        "all-controllable Mealy strategy projects to the 1-player positional strategy"
    );
}

/// The genuine two-player finding on real RTL: with only the mode signals controllable, the boot
/// handshake is UNREALIZABLE (the environment withholds the ack / escalates). The counterstrategy is
/// positional and forces ONLY environment inputs, never the controllable mode signals — the witness
/// behind the assume-guarantee reading (the handshake needs an environment ack assumption).
#[test]
fn edn_mode_controllable_counterstrategy_is_environment_only() {
    let mode: &[&str] = &["boot_req_mode_i", "edn_enable_i"];
    let strat = exact_two_player_strategy(EDN, "state_q == 44", mode).expect("2-player strategy");
    let TwoPlayerStrategy::EnvironmentCounterstrategy(p) = strat else {
        panic!("mode-controllable, acks environment: unrealizable ⇒ a counterstrategy");
    };
    assert!(
        !p.entries.is_empty(),
        "the initial state is in the environment's winning region ⇒ at least one row"
    );
    for e in &p.entries {
        for k in e.forced_inputs.keys() {
            assert!(
                k != "boot_req_mode_i" && k != "edn_enable_i",
                "the counterstrategy forces environment inputs only, got controllable {k}"
            );
        }
    }
}

/// The sound-posture transform removes the reset-hold ARTIFACT: the raw reset-adversarial game is
/// (spuriously) unrealizable — the environment holds `rst=1` forever — but on `game_sound_posture_model`
/// (reset pinned inactive + reset-init) the genuine functional game `st'=c` is realizable.
#[test]
fn sound_posture_removes_the_reset_hold_artifact() {
    // Raw model: reset is adversarial → the environment blocks by holding reset → unrealizable.
    let raw = exact_two_player_strategy(RESET_GAME, "st == 1", &["c"]).unwrap();
    assert!(
        matches!(raw, TwoPlayerStrategy::EnvironmentCounterstrategy(_)),
        "raw (reset-adversarial): the env holds reset ⇒ spuriously unrealizable"
    );
    // Sound posture: reset is a released posture (not adversarial) → the controller wins `st'=c`.
    let sound_model = game_sound_posture_model(RESET_GAME);
    let sound = exact_two_player_strategy(&sound_model, "st == 1", &["c"]).unwrap();
    assert!(
        matches!(sound, TwoPlayerStrategy::ControllerStrategy(_)),
        "sound posture (reset released): the controller forces st=1 ⇒ realizable"
    );
}

/// PR C (2026-10-23) — the strategy path and the verdict must agree on what "the controller wins
/// from the initial state" means: EVERY initial state is in the winning region. `st` has no `init`
/// (free at cycle 0, the mununu#505 / #579 reading) and `st' = st & c`: from `st = 1` the controller
/// keeps it (it is already `good`), from `st = 0` nothing reaches `good`. The verdict is therefore
/// unrealizable; the strategy path used to test `init ∩ winning ≠ ∅` and returned a controller
/// strategy for a game the controller does not win.
#[test]
fn the_strategy_and_the_verdict_agree_when_only_some_initial_states_win() {
    use mununu_core::adapter::btor2::symbolic_bitblast::exact_two_player_reach_realizable;
    const PARTIAL_INIT: &str =
        "1 sort bitvec 1\n2 input 1 c\n3 state 1 st\n4 and 1 3 2\n5 next 1 3 4\n";
    let realizable =
        exact_two_player_reach_realizable(PARTIAL_INIT, "st == 1", &["c"]).expect("decides");
    assert!(!realizable, "from st = 0 the goal is unreachable");
    let s = exact_two_player_strategy(PARTIAL_INIT, "st == 1", &["c"]).expect("extracts");
    assert_eq!(
        s.realizable(),
        realizable,
        "the strategy must agree with the verdict: {s:?}"
    );
}
