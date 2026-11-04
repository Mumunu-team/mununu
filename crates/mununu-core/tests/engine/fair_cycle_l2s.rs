//! mununu#477 Option B — soundness tests for the Emerson–Lei fair-cycle l2s
//! `response_liveness_rescue_under_fairness`, over minimal hand-authored BTOR2
//! fixtures with known-answer verdicts.
//!
//! The engine mechanics:
//! - `AG(req → AF ack)` under `(⋀_j GF fair_j)` is decided by the fair-cycle l2s
//!   `bad = looped ∧ ¬b_seen ∧ ⋀_j fair_j_seen` (see `adapter::btor2::l2s_monitor`).
//! - Reachable `bad` ⇒ VIOLATED; unreachable ⇒ HOLDS; portfolio abstain ⇒ Inconclusive.
//!
//! The four load-bearing soundness checks:
//! 1. **Positive rescue** — a design that starves the response without fairness but
//!    HOLDS under a matching `GF fair` assumption.
//! 2. **Regression** — a design that already HOLDS without fairness continues to HOLD
//!    under any fairness assumption (an assumption can only remove violating paths,
//!    never introduce them).
//! 3. **Useless-fairness soundness control** — a `GF fair` assumption that is
//!    trivially satisfied without constraining anything must NOT rescue a genuinely
//!    starving design (analog of `exact_two_player_fairness.rs::buffer_recurrence_is_not_rescued_by_a_useless_assumption`).
//! 4. **Zero-fairness equivalence** — the fair-cycle library entry with an empty
//!    fairness slice returns the identical verdict as the plain
//!    `response_liveness_rescue_atoms` (also confirmed at the byte level in the
//!    l2s_monitor unit tests).
//! 5. **Multi-conjunction TWOGATE analog** — a design where NEITHER `GF fair_1` NOR
//!    `GF fair_2` alone rescues, but BOTH together do (analog of
//!    `exact_two_player_fairness.rs::TWOGATE`).

use mununu_core::adapter::btor2::predicate_expr::CmpOp;
use mununu_core::adapter::liveness_rescue::{
    Atom, LivenessVerdict, response_liveness_rescue_atoms, response_liveness_rescue_under_fairness,
};

fn atom(signal: &str, op: CmpOp, value: u128) -> Atom {
    Atom {
        signal: signal.to_string(),
        op,
        value,
    }
}

// FAIR_GATED — ack fires only when `fair == 1` on a pending request; without a
// fairness assumption an env can hold fair=0 forever and starve the response.
// Also carries a `dead` primary input never read by the design — the useless-
// fairness control keys on `GF(dead == 0)`, which env satisfies trivially by
// holding dead=0 forever and thus adds no real constraint.
//
// next(pending) = (pending || req) && !ack
// next(ack)     = fair && pending
const FAIR_GATED: &str = "\
1 sort bitvec 1
2 input 1 req
3 input 1 fair
4 input 1 dead
5 state 1 pending
6 zero 1
7 init 1 5 6
8 state 1 ack
9 init 1 8 6
10 or 1 5 2
11 not 1 8
12 and 1 10 11
13 next 1 5 12
14 and 1 3 5
15 next 1 8 14
";

/// (1) POSITIVE RESCUE — under `GF(fair == 1)` the response HOLDS. Without any
/// assumption it VIOLATED (a plain-l2s baseline confirms).
#[test]
fn fair_gated_holds_under_gf_fair() {
    // Baseline: no fairness ⇒ starving env exists ⇒ VIOLATED.
    let (baseline, _) = response_liveness_rescue_atoms(
        FAIR_GATED,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        false,
    )
    .expect("plain l2s baseline");
    assert_eq!(
        baseline,
        LivenessVerdict::Violated,
        "without fairness, env holds fair=0 forever and starves ack"
    );

    // Under GF(fair == 1) ⇒ every pending req eventually meets a fair cycle ⇒ HOLDS.
    let fair = [atom("fair", CmpOp::Eq, 1)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        FAIR_GATED,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        &fair,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Holds,
        "GF(fair == 1) rescues the response — fair-cycle l2s must say HOLDS"
    );
}

/// (3) USELESS-FAIRNESS SOUNDNESS CONTROL — a fairness assumption `GF(dead == 0)`
/// that env satisfies for free (by holding `dead = 0` on every cycle) must NOT
/// rescue a genuinely starving design. Guards against the classic Emerson–Lei
/// bug where a badly-placed `save_en` guard makes every fair_seen latch trivially
/// true and the assumption becomes vacuous.
#[test]
fn fair_gated_is_not_rescued_by_useless_fairness() {
    let useless = [atom("dead", CmpOp::Eq, 0)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        FAIR_GATED,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        &useless,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Violated,
        "GF(dead == 0) is satisfied by env holding dead=0 forever — it adds no \
         constraint, so a genuinely starving design must still VIOLATED"
    );
}

// RESPONDER — the existing plain-l2s fixture (busy FSM). `AG(req → AF (st == 1))`
// HOLDS without any fairness: pending req in idle is always granted next cycle.
// Regression guard: adding a fairness assumption cannot make an already-holding
// design fail (an assumption can only remove violating paths).
const RESPONDER: &str = "\
1 sort bitvec 1
2 state 1 st
3 zero 1
4 init 1 2 3
5 input 1 req
6 one 1
7 ite 1 2 3 5
8 next 1 2 7
";

/// (2) REGRESSION — a design that HOLDS without fairness continues to HOLD under
/// any fairness assumption.
#[test]
fn already_holds_stays_holds_under_arbitrary_fairness() {
    let fair = [atom("req", CmpOp::Eq, 1)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        RESPONDER,
        &atom("req", CmpOp::Eq, 1),
        &atom("st", CmpOp::Eq, 1),
        &fair,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Holds,
        "an assumption never breaks an already-holding response — this must stay HOLDS"
    );
}

/// (4) ZERO-FAIRNESS EQUIVALENCE — the fair-cycle library entry with `&[]` matches
/// the plain `response_liveness_rescue_atoms` verdict. Complements the byte-level
/// check in `l2s_monitor.rs::tests::empty_fairness_matches_plain_emitter_byte_for_byte`.
#[test]
fn zero_fairness_matches_plain_rescue_verdict() {
    let ante = atom("req", CmpOp::Eq, 1);
    let cons = atom("st", CmpOp::Eq, 1);
    let (plain, _) = response_liveness_rescue_atoms(RESPONDER, &ante, &cons, false).expect("plain");
    let (fair_empty, _) =
        response_liveness_rescue_under_fairness(RESPONDER, &ante, &cons, &[], false)
            .expect("fair &[]");
    assert_eq!(
        plain, fair_empty,
        "empty-fairness verdict must match the plain-l2s verdict"
    );
}

// TWOGATE — `ack` fires only when `fair_1 && saw_fair2` where `saw_fair2` latches
// once `fair_2` has fired. Under `GF fair_1` alone the env picks fair_2=0 forever
// ⇒ saw_fair2 stays 0 ⇒ ack never fires. Under `GF fair_2` alone the env picks
// fair_1=0 forever ⇒ ack never fires. Under BOTH ⇒ fair_2 fires (latches
// saw_fair2), then a later fair_1 fires ack. Direct analog of
// `exact_two_player_fairness.rs::TWOGATE` for the fair-cycle l2s.
//
// next(pending)   = (pending || req) && !ack
// next(saw_fair2) = saw_fair2 || fair_2
// next(ack)       = fair_1 && saw_fair2 && pending
const TWOGATE: &str = "\
1 sort bitvec 1
2 input 1 req
3 input 1 fair_1
4 input 1 fair_2
5 state 1 pending
6 zero 1
7 init 1 5 6
8 state 1 saw_fair2
9 init 1 8 6
10 state 1 ack
11 init 1 10 6
12 or 1 5 2
13 not 1 10
14 and 1 12 13
15 next 1 5 14
16 or 1 8 4
17 next 1 8 16
18 and 1 3 8
19 and 1 18 5
20 next 1 10 19
";

/// (5) MULTI-CONJUNCTION — `GF fair_1` alone does not rescue TWOGATE.
#[test]
fn twogate_not_rescued_by_gf_fair1_alone() {
    let fair = [atom("fair_1", CmpOp::Eq, 1)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        TWOGATE,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        &fair,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Violated,
        "GF fair_1 alone leaves fair_2 free to stay 0 ⇒ saw_fair2 never latches ⇒ ack never fires"
    );
}

/// (5) MULTI-CONJUNCTION — `GF fair_2` alone does not rescue TWOGATE.
#[test]
fn twogate_not_rescued_by_gf_fair2_alone() {
    let fair = [atom("fair_2", CmpOp::Eq, 1)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        TWOGATE,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        &fair,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Violated,
        "GF fair_2 alone leaves fair_1 free to stay 0 ⇒ ack's fair_1 conjunct never fires"
    );
}

/// (5) MULTI-CONJUNCTION — `GF fair_1 ∧ GF fair_2` rescues TWOGATE.
#[test]
fn twogate_holds_under_gf_fair1_and_gf_fair2() {
    let fair = [atom("fair_1", CmpOp::Eq, 1), atom("fair_2", CmpOp::Eq, 1)];
    let (verdict, _) = response_liveness_rescue_under_fairness(
        TWOGATE,
        &atom("req", CmpOp::Eq, 1),
        &atom("ack", CmpOp::Eq, 1),
        &fair,
        false,
    )
    .expect("fair-cycle l2s");
    assert_eq!(
        verdict,
        LivenessVerdict::Holds,
        "GF fair_1 ∧ GF fair_2 lets fair_2 latch saw_fair2 and a later fair_1 fire ack"
    );
}
