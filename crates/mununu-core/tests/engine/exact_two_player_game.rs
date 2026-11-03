//! P2.5-F — the exact-symbolic engine solving TWO-PLAYER μ-calculus games via
//! `exact_two_player_verdict` and `ExactModel::cpre_controllable` / `cpre_environment`. The first step
//! of the unified two-player game verifier (`.claude/plans/unified-two-player-mu-calculus-verifier.md`):
//! the exact ROBDD engine now honours the controllability axis (`<(ctrl = controllable)>`).
//!
//! Validation is by KNOWN ANSWER on two minimal 1-bit games whose winning regions are computed by hand.
//! Both share the state `st` and inputs `c` (controllable) + `e` (environment); they differ only in the
//! transition, which is exactly whether the environment can block the controller:
//!
//! - `CTRL`  : `st' = c`        — the controller sets the next state directly (environment powerless).
//! - `ENVBLK`: `st' = c & ¬e`   — the environment can force `st' = 0` by asserting `e` (blocking).
//!
//! Across the two fixpoint directions (μ = reach, ν = maintain) and both players, the pair pins down
//! `cpre_controllable` and `cpre_environment`. (Cross-validation against the explicit `gr1.rs` /
//! `parity_game.rs` solver on a shared CLTS is a follow-up; here the hand-computed winning regions are
//! the oracle.)

use mununu_core::adapter::btor2::symbolic_bitblast::{
    ExactVerdict, exact_two_player_buchi_realizable, exact_two_player_reach_realizable,
    exact_two_player_strategy, exact_two_player_verdict,
};
use mununu_core::mu_calculus::parser;

// st' = c, with a NAMED COMBINATIONAL OUTPUT `busy_o = ¬st` (not a state register). The realizability
// VERDICT resolves the output atom; the state-INDEXED STRATEGY cannot (no register in `good`). This is
// the FIFO-class datapath shape: the target is a combinational output (`full_o`, `empty_o`), not a cell.
const CTRL_OUT: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 next 1 4 2
8 not 1 4 busy_o
9 output 8
";
// st' = c & ¬e with the same `busy_o = ¬st` output: the environment can block st=1, so busy_o stays 1.
const ENVBLK_OUT: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 not 1 3
8 and 1 2 7
9 next 1 4 8
10 not 1 4 busy_o
11 output 10
";

// st' = c : the controller sets the next state.
const CTRL_INIT0: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 zero 1
6 init 1 4 5
7 next 1 4 2
";
const CTRL_INIT1: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 one 1
6 init 1 4 5
7 next 1 4 2
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
const ENVBLK_INIT1: &str = "\
1 sort bitvec 1
2 input 1 c
3 input 1 e
4 state 1 st
5 one 1
6 init 1 4 5
7 not 1 3
8 and 1 2 7
9 next 1 4 8
";

fn verdict(btor2: &str, formula: &str) -> ExactVerdict {
    let f = parser::parse(formula).expect("formula parses");
    exact_two_player_verdict(btor2, &f, &["c"]).expect("exact two-player verdict")
}

/// Controllable reachability (`μ`): can the CONTROLLER force reaching `good = (st == 1)`?
/// `CTRL` (st'=c): from st=0, `∀e ∃c: c==1` — yes ⇒ HOLDS. `ENVBLK` (st'=c&¬e): for e=1 no c reaches
/// good ⇒ the `∀e` fails ⇒ VIOLATED. Same formula, differing only by whether the environment can block.
#[test]
fn controllable_reachability_holds_iff_environment_cannot_block() {
    let reach_good = "mu X. ((st == 1) || <(ctrl = controllable)> X)";
    assert_eq!(
        verdict(CTRL_INIT0, reach_good),
        ExactVerdict::Holds,
        "st'=c: the controller forces st=1 in one step"
    );
    assert_eq!(
        verdict(ENVBLK_INIT0, reach_good),
        ExactVerdict::Violated,
        "st'=c&¬e: the environment blocks with e=1, so the controller cannot force st=1"
    );
}

/// Controllable safety (`ν`): can the CONTROLLER MAINTAIN `good = (st == 1)` forever (init st=1)?
/// `CTRL`: from st=1, `∀e ∃c: c==1` keeps st=1 ⇒ HOLDS. `ENVBLK`: e=1 knocks st to 0 against any c ⇒
/// the greatest fixpoint collapses to ∅ ⇒ VIOLATED.
#[test]
fn controllable_safety_holds_iff_environment_cannot_knock_out() {
    let maintain_good = "nu X. ((st == 1) && <(ctrl = controllable)> X)";
    assert_eq!(
        verdict(CTRL_INIT1, maintain_good),
        ExactVerdict::Holds,
        "st'=c: the controller maintains st=1"
    );
    assert_eq!(
        verdict(ENVBLK_INIT1, maintain_good),
        ExactVerdict::Violated,
        "st'=c&¬e: the environment can knock st out of 1"
    );
}

/// Controllable RECURRENCE / Büchi (`νμ`): can the CONTROLLER visit `good = (st == 1)` INFINITELY OFTEN?
/// The recurrence formula is `νZ. μY. ((good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y)`. This is the DECISIVE soundness gate
/// for the nested-fixpoint two-player evaluation (the reach/safety tests exercise only a SINGLE fixpoint):
/// `CTRL` (st'=c) — the controller sets c=1 every step ⇒ st=1 always ⇒ good i.o. ⇒ HOLDS. `ENVBLK`
/// (st'=c&¬e) — the environment plays e=1 forever ⇒ st=0 after step 1 ⇒ good only finitely often ⇒ VIOLATED.
#[test]
fn controllable_recurrence_holds_iff_environment_cannot_starve() {
    let gf_good =
        "nu Z. (mu Y. (((st == 1) && <(ctrl = controllable)> Z) || <(ctrl = controllable)> Y))";
    assert_eq!(
        verdict(CTRL_INIT1, gf_good),
        ExactVerdict::Holds,
        "st'=c: the controller keeps st=1 ⇒ visits good infinitely often"
    );
    assert_eq!(
        verdict(ENVBLK_INIT1, gf_good),
        ExactVerdict::Violated,
        "st'=c&¬e: the environment starves good with e=1 forever"
    );
    // DUAL sanity: the controller CAN force GF(st==0) on ENVBLK (set c=0 ⇒ st'=0) — confirms the νμ
    // nesting is not accidentally collapsing to the reach/safety single-fixpoint answer.
    let gf_zero =
        "nu Z. (mu Y. (((st == 0) && <(ctrl = controllable)> Z) || <(ctrl = controllable)> Y))";
    assert_eq!(
        verdict(ENVBLK_INIT1, gf_zero),
        ExactVerdict::Holds,
        "st'=c&¬e: the controller forces st=0 (c=0) ⇒ visits st=0 infinitely often"
    );
}

/// The DUAL — environment predecessor (`cpre_environment`). Can the ENVIRONMENT force reaching
/// `st == 0` (init st=1)? `ENVBLK`: `∃e ∀c: (c&¬e)==0` holds for e=1 ⇒ HOLDS. `CTRL`: the environment
/// is powerless (`∀c: c==0` fails, c=1 keeps st=1) ⇒ VIOLATED. The mirror image of the controllable
/// results — confirming `cpre_environment` is the genuine dual, not a copy of `cpre_controllable`.
#[test]
fn environment_predecessor_is_the_genuine_dual() {
    let env_forces_zero = "mu X. ((st == 0) || <(ctrl = environment)> X)";
    assert_eq!(
        verdict(ENVBLK_INIT1, env_forces_zero),
        ExactVerdict::Holds,
        "st'=c&¬e: the environment forces st=0 with e=1"
    );
    assert_eq!(
        verdict(CTRL_INIT1, env_forces_zero),
        ExactVerdict::Violated,
        "st'=c: the environment cannot force st=0 (the controller holds st=1)"
    );
}

/// Regression: with NO controllable inputs declared, `<(ctrl = controllable)>` degenerates to the box
/// pre-image (the environment owns everything) — the controller can force `good` only if EVERY move
/// already leads there. On `ENVBLK` reaching `st==1` is then unforceable ⇒ VIOLATED, confirming the
/// empty-partition degeneration is the sound "controller cannot influence" reading.
#[test]
fn empty_partition_degenerates_to_box() {
    let f = parser::parse("mu X. ((st == 1) || <(ctrl = controllable)> X)").unwrap();
    // No controllable inputs: c is (implicitly) environment too.
    assert_eq!(
        exact_two_player_verdict(ENVBLK_INIT0, &f, &[]).unwrap(),
        ExactVerdict::Violated,
    );
}

/// Stage-2 partition validation: a declared controllable input that is NOT a primary input is an error,
/// not a silent fall-back to the environment. `"nope"` matches no input ⇒ `Err` (the message lists the
/// real primary inputs). This closes the silent all-environment gap: a typo or an internally-driven
/// signal is rejected rather than quietly reinterpreted as environment.
#[test]
fn unknown_controllable_input_is_rejected() {
    let f = parser::parse("mu X. ((st == 1) || <(ctrl = controllable)> X)").unwrap();
    let err = exact_two_player_verdict(CTRL_INIT0, &f, &["nope"]).unwrap_err();
    assert!(
        err.contains("not primary inputs") && err.contains("nope"),
        "expected a partition-validation error naming the unknown input, got: {err}"
    );
    // The real input names still work.
    assert!(exact_two_player_verdict(CTRL_INIT0, &f, &["c"]).is_ok());
}

/// Relational / combinational-OUTPUT target: `exact_two_player_reach_realizable` decides a game whose
/// `good` is a named combinational output (`busy_o = ¬st`), not a state register — the FIFO-class datapath
/// shape (`full_o`, `empty_o`). The controller forcing `busy_o == 0` ⇔ forcing `st == 1`, so the answer
/// matches the state-atom game: realizable on `CTRL_OUT`, unrealizable on `ENVBLK_OUT`.
#[test]
fn reach_realizable_decides_a_combinational_output_target() {
    assert_eq!(
        exact_two_player_reach_realizable(CTRL_OUT, "busy_o == 0", &["c"]),
        Ok(true),
        "st'=c: the controller forces busy_o=0 (st=1) in one step"
    );
    assert_eq!(
        exact_two_player_reach_realizable(ENVBLK_OUT, "busy_o == 0", &["c"]),
        Ok(false),
        "st'=c&¬e: the environment blocks with e=1, so busy_o stays 1"
    );
    // Sanity: the output atom tracks its state equivalent (`st == 1`) exactly.
    assert_eq!(
        exact_two_player_reach_realizable(CTRL_OUT, "st == 1", &["c"]),
        exact_two_player_reach_realizable(CTRL_OUT, "busy_o == 0", &["c"]),
    );
}

/// The `exact_two_player_buchi_realizable` HELPER (the recurrence primitive behind `--objective
/// recurrence`) agrees with the hand-written recurrence formula: realizable on `CTRL` (the controller
/// keeps `st==1` forever) and unrealizable on `ENVBLK` (the environment starves it), and it accepts a
/// combinational-output target (`busy_o == 0`) just like the reach helper.
#[test]
fn buchi_helper_matches_the_recurrence_known_answer() {
    assert_eq!(
        exact_two_player_buchi_realizable(CTRL_INIT1, "st == 1", &["c"]),
        Ok(true),
        "st'=c: the controller forces st=1 infinitely often"
    );
    assert_eq!(
        exact_two_player_buchi_realizable(ENVBLK_INIT1, "st == 1", &["c"]),
        Ok(false),
        "st'=c&¬e: the environment starves st=1"
    );
    // Combinational-output recurrence: force `busy_o == 0` (⇔ st==1) infinitely often on CTRL_OUT (st'=c).
    assert_eq!(
        exact_two_player_buchi_realizable(CTRL_OUT, "busy_o == 0", &["c"]),
        Ok(true),
        "st'=c: the controller forces busy_o=0 (st=1) infinitely often — a combinational-output target"
    );
}

/// The state-INDEXED strategy cannot be extracted for a combinational-output `good` (it references no
/// state register) — the verb makes it best-effort and falls back to `realizable` + `holds_under`. This
/// pins WHY the strategy is `Option`: the verdict decides the same target the strategy cannot index.
#[test]
fn strategy_extraction_declines_a_combinational_output_target() {
    let err = exact_two_player_strategy(CTRL_OUT, "busy_o == 0", &["c"]).unwrap_err();
    assert!(
        err.contains("state register") || err.contains("resolvable"),
        "expected a no-state-register error for a combinational output, got: {err}"
    );
    // ...but the VERDICT on the very same target decides.
    assert_eq!(
        exact_two_player_reach_realizable(CTRL_OUT, "busy_o == 0", &["c"]),
        Ok(true),
    );
}
