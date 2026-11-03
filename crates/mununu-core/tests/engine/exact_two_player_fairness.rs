//! P2.5-F — FAIRNESS environment-assumption discovery for the two-player RECURRENCE (Büchi) game:
//! the GR(1) 1-pair objective `GF a → GF good`, decided EXACTLY by `exact_two_player_gr1_realizable`
//! over the assumption-latched model, and searched by `discover_game_fairness_assumption`.
//!
//! Validation is by KNOWN ANSWER on a minimal 1-bit "buffer" game whose winning region is computed by
//! hand. State `count ∈ {0,1}`, `good = (count == 1)`. Inputs `c` (controllable = "push") and `e`
//! (environment = "pop"). The transition is a saturating ±1 counter:
//!
//!   count' = (c ∧ ¬e) ∨ (count ∧ ¬(e ∧ ¬c))
//!     c=1,e=0 → 1 (push)      ¬c,e   → 0 (pop)      otherwise → count (hold)
//!
//! Key dynamics: a pop-WITH-push HOLDS (c=1,e=1 → count'=count), so once `count=1` the controller
//! keeps it at 1 by playing c=1 — this is what makes the fairness assumption `GF(e==0)` SUFFICIENT
//! (reach 1 on any e=0 step, then hold). Without the assumption the environment pops forever (e=1) and
//! starves `count`.

use mununu_core::adapter::btor2::concrete_oracle::OracleAtom;
use mununu_core::adapter::btor2::predicate_expr::CmpOp;
use mununu_core::adapter::btor2::symbolic_bitblast::{
    TwoPlayerStrategy, exact_two_player_buchi_realizable, exact_two_player_buchi_strategy,
    exact_two_player_gr1_conjunction_realizable, exact_two_player_gr1_realizable,
    exact_two_player_recurrence_stall_lasso,
};
use mununu_core::adapter::recoverability::discover_game_fairness_assumption;

// count' = (c ∧ ¬e) ∨ (count ∧ ¬(e ∧ ¬c)); init count=0.
const BUFFER: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 count
5 zero 1
6 init 1 4 5
7 not 1 3
8 and 1 2 7
9 not 1 2
10 and 1 3 9
11 not 1 10
12 and 1 4 11
13 or 1 8 12
14 next 1 4 13
";

// st' = c, init st=1 — the controller trivially maintains st=1 (Büchi realizable WITHOUT any assumption).
const CTRL_INIT1: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 one 1
6 init 1 4 5
7 next 1 4 2
";

// TWO independent env gates on the path to `good = (st == 2)`, each needing its own fairness pause:
//   st 0 --(c ∧ ¬e1)--> st 1 --(c ∧ ¬e2)--> st 2 --> st 0 (auto, non-idleable)
// Controller drives `c`; the environment owns e1, e2. Reaching st==2 recurrently needs BOTH `e1` and
// `e2` to pause infinitely often — neither single `GF(e_i==0)` suffices (the other gate stays shut).
const TWOGATE: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 c
4 input 1 e1
5 input 1 e2
6 state 2 st
7 zero 2
8 init 2 6 7
9 one 2
10 constd 2 2
11 not 1 4
12 and 1 3 11
13 not 1 5
14 and 1 3 13
15 eq 1 6 7
16 eq 1 6 9
17 ite 2 12 9 7
18 ite 2 14 10 9
19 ite 2 16 18 7
20 ite 2 15 17 19
21 next 2 6 20
";

/// The RECURRENCE game `GF(count==1)` (controller = push) is UNREALIZABLE: the environment pops forever
/// (e=1) and `count` is starved. This is the ⊥ a fairness assumption must rescue.
#[test]
fn buffer_recurrence_is_unrealizable_without_an_assumption() {
    assert_eq!(
        exact_two_player_buchi_realizable(BUFFER, "count == 1", &["c"]),
        Ok(false),
        "the environment pops every cycle ⇒ count never recurrently reaches 1"
    );
}

/// The fairness assumption `GF(e==0)` (the environment pauses popping infinitely often) RESCUES it:
/// `GF(e==0) → GF(count==1)` is REALIZABLE — whenever e=0 the controller pushes to 1, and a pop-with-push
/// holds it there. This is the fairness lever's positive case.
#[test]
fn buffer_recurrence_is_realizable_under_pop_pause_fairness() {
    let assume = OracleAtom::new("e", CmpOp::Eq, 0);
    assert_eq!(
        exact_two_player_gr1_realizable(BUFFER, "count == 1", &assume, &["c"]),
        Ok(true),
        "GF(e==0) → GF(count==1): reach 1 on an e=0 step, hold with c=1"
    );
}

/// SOUNDNESS CONTROL — a USELESS assumption does NOT rescue. `GF(e==1)` (the environment pops infinitely
/// often) is satisfied by popping EVERY cycle, which starves `count` ⇒ `GF(e==1) → GF(count==1)` stays
/// UNREALIZABLE. A fairness lever that "rescued" this would be unsound (fabricating a win).
#[test]
fn buffer_recurrence_is_not_rescued_by_a_useless_assumption() {
    let assume = OracleAtom::new("e", CmpOp::Eq, 1);
    assert_eq!(
        exact_two_player_gr1_realizable(BUFFER, "count == 1", &assume, &["c"]),
        Ok(false),
        "GF(e==1) is met by popping every cycle ⇒ no rescue"
    );
}

/// REDUCTION / no-over-firing control — when the plain Büchi game is ALREADY realizable (st'=c, init
/// st=1), the GR(1) game with any assumption is ALSO realizable (an assumption can only help). The
/// fairness path must not spuriously turn a win into a loss.
#[test]
fn gr1_reduces_to_realizable_when_buchi_already_wins() {
    assert_eq!(
        exact_two_player_buchi_realizable(CTRL_INIT1, "st == 1", &["c"]),
        Ok(true),
    );
    let assume = OracleAtom::new("e", CmpOp::Eq, 0);
    assert_eq!(
        exact_two_player_gr1_realizable(CTRL_INIT1, "st == 1", &assume, &["c"]),
        Ok(true),
        "an assumption never breaks an already-realizable recurrence game"
    );
}

/// DISCOVERY end-to-end: on the unrealizable buffer game, `discover_game_fairness_assumption` finds the
/// enabling fairness assumption `GF(e == 0)` (non-vacuous) — and ONLY that one (not the useless
/// `GF(e == 1)`).
#[test]
fn discovery_finds_the_pop_pause_fairness_assumption() {
    let found = discover_game_fairness_assumption(BUFFER, "count == 1", &["c"]);
    assert!(
        found.iter().any(|a| a.phi == "GF(e == 0)" && a.non_vacuous),
        "expected to discover GF(e == 0): {found:?}"
    );
    assert!(
        !found.iter().any(|a| a.phi == "GF(e == 1)"),
        "must NOT discover the useless GF(e == 1): {found:?}"
    );
}

/// DISCOVERY false-positive control — on an already-realizable recurrence game, discovery returns ∅
/// (no assumption is needed; fabricating one would be a false positive).
#[test]
fn discovery_is_empty_when_recurrence_already_holds() {
    assert!(
        discover_game_fairness_assumption(CTRL_INIT1, "st == 1", &["c"]).is_empty(),
        "a realizable recurrence game needs no fairness assumption"
    );
}

// TWOGATE with a STATE-FUNCTION OUTPUT `done_o = (st == 2)` (depends on STATE only, not on the current
// inputs — unlike an input-gated pulse). The output's combinational cone is just `st`, so the cone-
// restricted axis search sees NEITHER `e1` nor `e2` (they only drive `st`'s NEXT) → the conjunction
// discovery must FALL BACK to all narrow env inputs. Exercises `discover_game_fairness_conjunction`'s
// output-target axis fallback end-to-end.
const TWOGATE_OUT: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 c
4 input 1 e1
5 input 1 e2
6 state 2 st
7 zero 2
8 init 2 6 7
9 one 2
10 constd 2 2
11 not 1 4
12 and 1 3 11
13 not 1 5
14 and 1 3 13
15 eq 1 6 7
16 eq 1 6 9
17 ite 2 12 9 7
18 ite 2 14 10 9
19 ite 2 16 18 7
20 ite 2 15 17 19
21 next 2 6 20
22 eq 1 6 10 done_o
23 output 22 done_o
";

/// AXIS-FALLBACK regression (a fix for the conjunction discovery on OUTPUT / relational targets): the
/// combinational cone of `done_o = (st==2)` is just `st`, so the cone-restricted axis search finds
/// NEITHER gate input (both only drive `st`'s next). The discovery must fall back to all narrow env
/// inputs and still report `GF(e1==0) && GF(e2==0)`. (`done_o` is a STATE-function output — the game
/// handles it; contrast a `req & ack`-style INPUT-dependent output, which is not a state predicate.)
#[test]
fn discovery_finds_conjunction_on_output_target_via_axis_fallback() {
    let found = discover_game_fairness_assumption(TWOGATE_OUT, "done_o == 1", &["c"]);
    assert!(
        found.iter().any(|a| a.phi.contains("GF(e1 == 0)")
            && a.phi.contains("GF(e2 == 0)")
            && a.kind == mununu_core::verdict::AssumptionKind::InputFairnessConjunction),
        "expected the conjunction via the output-target axis fallback: {found:?}"
    );
}

/// SOUNDNESS GATE for the CONJUNCTIVE lever — the multi-pair GR(1) `νZ.μY. ⋁_i νX_i(…)`. On TWOGATE,
/// two independent env gates each need their own pause, so:
///   - the bare recurrence is unrealizable,
///   - NEITHER single `GF(e_i==0)` rescues (the other gate stays shut — the crucial non-over-firing
///     control: a lever that "rescued" with one assumption would be unsound), and
///   - the CONJUNCTION `GF(e1==0) ∧ GF(e2==0)` DOES rescue.
#[test]
fn conjunctive_fairness_rescues_when_no_single_assumption_does() {
    let e1_low = OracleAtom::new("e1", CmpOp::Eq, 0);
    let e2_low = OracleAtom::new("e2", CmpOp::Eq, 0);

    assert_eq!(
        exact_two_player_buchi_realizable(TWOGATE, "st == 2", &["c"]),
        Ok(false),
        "bare recurrence: the environment shuts either gate forever"
    );
    // Neither single justice assumption suffices.
    assert_eq!(
        exact_two_player_gr1_realizable(TWOGATE, "st == 2", &e1_low, &["c"]),
        Ok(false),
        "GF(e1==0) alone: gate 1 (e2) still shut ⇒ st==2 unreachable"
    );
    assert_eq!(
        exact_two_player_gr1_realizable(TWOGATE, "st == 2", &e2_low, &["c"]),
        Ok(false),
        "GF(e2==0) alone: gate 0 (e1) still shut ⇒ st==1 unreachable"
    );
    // The conjunction rescues.
    assert_eq!(
        exact_two_player_gr1_conjunction_realizable(
            TWOGATE,
            "st == 2",
            &[e1_low.clone(), e2_low.clone()],
            &["c"]
        ),
        Ok(true),
        "GF(e1==0) ∧ GF(e2==0): both gates pause i.o. ⇒ st==2 recurs"
    );
}

/// DISCOVERY end-to-end for the CONJUNCTIVE fallback: on TWOGATE (no single fairness rescues),
/// `discover_game_fairness_assumption` falls through to the conjunction and reports
/// `GF(e1 == 0) && GF(e2 == 0)` with kind `InputFairnessConjunction`.
#[test]
fn discovery_finds_the_fairness_conjunction_when_no_single_works() {
    let found = discover_game_fairness_assumption(TWOGATE, "st == 2", &["c"]);
    assert!(
        found.iter().any(|a| a.phi.contains("GF(e1 == 0)")
            && a.phi.contains("GF(e2 == 0)")
            && a.kind == mununu_core::verdict::AssumptionKind::InputFairnessConjunction
            && a.non_vacuous),
        "expected the conjunction GF(e1==0) && GF(e2==0): {found:?}"
    );
}

/// REDUCTION sanity for the conjunctive helper: a 1-element conjunction equals the single-pair GR(1),
/// and a 0-element conjunction equals the bare Büchi game. Confirms the multi-pair νZ.μY.⋁νX formula
/// does not drift from the shipped single-pair / recurrence primitives.
#[test]
fn conjunction_reduces_to_single_and_buchi() {
    let e_low = OracleAtom::new("e", CmpOp::Eq, 0);
    // m = 1 ≡ single-pair GR(1) (on the BUFFER game where GF(e==0) rescues).
    assert_eq!(
        exact_two_player_gr1_conjunction_realizable(
            BUFFER,
            "count == 1",
            std::slice::from_ref(&e_low),
            &["c"]
        ),
        exact_two_player_gr1_realizable(BUFFER, "count == 1", &e_low, &["c"]),
    );
    // m = 0 ≡ bare Büchi recurrence.
    assert_eq!(
        exact_two_player_gr1_conjunction_realizable(BUFFER, "count == 1", &[], &["c"]),
        exact_two_player_buchi_realizable(BUFFER, "count == 1", &["c"]),
    );
}

// ============================================================================================
// P2.5-F (b): the ENVIRONMENT STARVATION LASSO — the actionable witness for an unrealizable
// RECURRENCE game. `exact_two_player_recurrence_stall_lasso` returns a concrete play (reset →
// `¬good` cycle, with the env's per-step inputs) proving the env can starve `good` forever.
// ============================================================================================

/// On the UNREALIZABLE buffer recurrence `GF(count==1)` the env starves `count` at 0 forever by
/// popping (`e == 1`). The lasso must be present, the cycle must never satisfy `good` (`count != 1`
/// on every cycle state), and the env's forcing input on the cycle is uniquely `e == 1` — from
/// `count == 0`, `∀c count' = c ∧ ¬e`, so `count' == 0 ∀c` requires `e == 1` (the `env_forcing_moves`).
#[test]
fn recurrence_stall_lasso_witnesses_the_pop_starvation() {
    let lasso = exact_two_player_recurrence_stall_lasso(BUFFER, "count == 1", &["c"])
        .expect("engine call")
        .expect("unrealizable recurrence has an env starvation lasso");
    assert!(
        !lasso.cycle.is_empty(),
        "the starvation witness must have a non-empty ¬good cycle"
    );
    // `good = (count == 1)` is FALSE on every cycle state — the env prevents recurrence.
    assert!(
        lasso.cycle.iter().all(|st| st.get("count") != Some(&1)),
        "good (count==1) must never hold on the stall cycle, got {:?}",
        lasso.cycle
    );
    // The env's ∀ctrl-robust move to keep `count == 0` is `e == 1` (pop) — recorded on every step.
    assert!(
        !lasso.inputs.is_empty(),
        "the lasso must record the env's forcing inputs"
    );
    assert!(
        lasso.inputs.iter().all(|inp| inp.get("e") == Some(&1)),
        "the env starves by holding e==1 (pop) each step, got {:?}",
        lasso.inputs
    );
}

/// FALSE-POSITIVE control — a REALIZABLE recurrence game has NO env starvation lasso. On `CTRL_INIT1`
/// (`st' = c`, init `st == 1`) the controller trivially maintains `GF(st==1)` (hold `c == 1`), so the
/// env can force `¬good` forever from nowhere: `stall_env = ∅` ⇒ `None`.
#[test]
fn recurrence_stall_lasso_is_absent_when_realizable() {
    assert!(
        exact_two_player_recurrence_stall_lasso(CTRL_INIT1, "st == 1", &["c"])
            .expect("engine call")
            .is_none(),
        "a realizable recurrence game has no environment starvation lasso"
    );
}

/// On the TWO-gate design the bare recurrence `GF(st==2)` is unrealizable (a single gate held shut
/// starves it). The lasso is present and never reaches `good` (`st != 2` on the cycle) — the env keeps
/// the FSM in the pre-`good` region forever.
#[test]
fn recurrence_stall_lasso_on_two_gate_design() {
    let lasso = exact_two_player_recurrence_stall_lasso(TWOGATE, "st == 2", &["c"])
        .expect("engine call")
        .expect("unrealizable two-gate recurrence has an env starvation lasso");
    assert!(!lasso.cycle.is_empty(), "non-empty ¬good cycle expected");
    assert!(
        lasso.cycle.iter().all(|st| st.get("st") != Some(&2)),
        "good (st==2) must never hold on the stall cycle, got {:?}",
        lasso.cycle
    );
}

// ============================================================================================
// P2.5-F (b): the CONTROLLER's Büchi (recurrence) STRATEGY — how the controller forces `good`
// infinitely often when the recurrence game is REALIZABLE. `exact_two_player_buchi_strategy`
// returns the winning controller Mealy strategy, or `None` when unrealizable (env-lasso case).
// ============================================================================================

/// On the REALIZABLE recurrence game `CTRL_INIT1` (`st' = c`, init `st == 1`), the controller forces
/// `GF(st==1)`: it must drive `c == 1` from the non-good state `st == 0` to return to `good`. The result
/// is a CONTROLLER strategy (not the environment counterstrategy), and — all inputs being controllable —
/// a Moore (positional) discipline. NOTE the recur/good state `st == 1` legitimately leaves `c` FREE:
/// the whole winning region re-attracts to `good`, so any move from `st == 1` is winning (`c = 0` drops
/// to `st = 0`, whence `c == 1` is forced back). The load-bearing forced move is at `st == 0`.
#[test]
fn recurrence_strategy_forces_good_when_realizable() {
    // Sanity: the game is realizable, so a strategy must exist.
    assert!(exact_two_player_buchi_realizable(CTRL_INIT1, "st == 1", &["c"]).unwrap());
    let strat = exact_two_player_buchi_strategy(CTRL_INIT1, "st == 1", &["c"])
        .expect("engine call")
        .expect("a realizable recurrence game has a controller strategy");
    let mealy = match strat {
        TwoPlayerStrategy::ControllerStrategy(m) => m,
        TwoPlayerStrategy::EnvironmentCounterstrategy(_) => {
            panic!("realizable recurrence must yield a CONTROLLER strategy")
        }
    };
    assert_eq!(mealy.state_register, "st");
    // All-controllable ⇒ Moore, so the strategy projects to a positional discipline.
    let positional = mealy
        .as_positional()
        .expect("all-controllable Büchi strategy is positional (Moore)");
    // The good state st==1 is the recur point (a strategy row exists for it).
    assert!(
        positional.entries.iter().any(|e| e.state_value == 1),
        "the good state st==1 must be a strategy row (the recur point), got {:?}",
        positional.entries
    );
    // The load-bearing progress move: from the non-good, reachable state st==0 the controller forces
    // c==1 to return to good — this is what makes GF(st==1) recur.
    let from_zero = positional
        .entries
        .iter()
        .find(|e| e.state_value == 0)
        .expect("st==0 is reachable (c==0 from st==1) and carries a strategy row");
    assert_eq!(
        from_zero.forced_inputs.get("c"),
        Some(&1),
        "from the non-good state st==0 the controller forces c==1 to reach good, got {:?}",
        from_zero.forced_inputs
    );
}

/// FALSE-POSITIVE control — an UNREALIZABLE recurrence game has NO controller strategy (`None`); its
/// actionable witness is the environment starvation lasso instead. BUFFER `GF(count==1)` (env pops
/// forever) is the ⊥ case.
#[test]
fn recurrence_strategy_is_absent_when_unrealizable() {
    assert!(!exact_two_player_buchi_realizable(BUFFER, "count == 1", &["c"]).unwrap());
    assert!(
        exact_two_player_buchi_strategy(BUFFER, "count == 1", &["c"])
            .expect("engine call")
            .is_none(),
        "an unrealizable recurrence game has no controller strategy (the env lasso is its witness)"
    );
}
