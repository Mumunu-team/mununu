//! The k-fault adversary (PR C of the 2026-10-22 plan): "can at most `k` injections at these
//! registers reach this state?" — one player, on the reach portfolio.
//!
//! **Semantics.** The design is rewritten by [`crate::adapter::btor2::fault::inject_faults`] and the
//! question is asked of the injected model. The attacker drives the fault inputs AND every primary
//! input of the design — the strongest attacker:
//!
//! - `unreachable` ⇒ for every input behaviour and every schedule of at most `k` injections after the
//!   warm-up, the target is never reached (sound: every portfolio member honours the model and the
//!   budget is structural, a counter in the model);
//! - `reachable` ⇒ an attack exists for the strongest attacker; the witness lists the injections
//!   (cycle, site, value). Whether an honest environment could block it is the two-player question
//!   (`btor2 game --fault`), not this one.
//!
//! engine: the reach portfolio (native BMC for the witness, native k-induction / rIC3 / btormc /
//! pono for `unreachable`, SPACER corroborating) on the injected BTOR2 — word-level, no bit cap. With
//! a `--target` the design's own `bad` lines are dropped and the target is decided as the invariant
//! `AG !target` through the reach rescue (`reach_portfolio_rescue_diagnosed`), its witness through
//! `safety_witness`.

#![deny(clippy::unwrap_used)] // docs/policies/failure-paths.md Rule 2: abstain, never panic
use crate::adapter::btor2::ast::Node;
use crate::adapter::btor2::fault::{FaultEvent, FaultSpec, faults_in_trace, inject_faults};
use crate::adapter::btor2::{emit, parser};

/// The outcome of one adversary question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultReachOutcome {
    /// `"violated"` (an attack reaches the target / a `bad`), `"holds"` (no attack within the
    /// budget does), `"unknown"`.
    pub verdict: &'static str,
    /// The engines that decided.
    pub decided_by: Vec<String>,
    /// The posture, as one label.
    pub fault: String,
    /// The target asked (`None`: the design's own `bad` lines).
    pub target: Option<String>,
    /// The injections of the witness (empty unless `violated` and a witness was found).
    pub attack: Vec<FaultEvent>,
    /// The witness depth in cycles (`None` without a witness).
    pub depth: Option<usize>,
}

/// Decide whether an attacker with the posture `spec` reaches `target` (a boolean atom such as
/// `state_q == 680`), or one of the design's `bad` lines when `target` is `None`.
pub fn verify_fault_reach(
    btor2: &str,
    spec: &FaultSpec,
    target: Option<&str>,
    witness_max_k: u32,
) -> Result<FaultReachOutcome, String> {
    let file = parser::parse(btor2).map_err(|e| format!("BTOR2 parse: {}", e.message))?;
    // With a target, the design's own `bad` lines are not the question.
    let file = if target.is_some() {
        let mut f = file;
        f.lines.retain(|l| !matches!(l.node, Node::Bad { .. }));
        f.by_nid = f
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| (l.nid, i))
            .collect();
        f
    } else {
        file
    };
    let (injected_file, injected) = inject_faults(&file, spec)?;
    let label = spec.as_label();
    match target {
        Some(atom) => {
            use crate::adapter::reach_rescue::{
                RescueVerdict, reach_portfolio_rescue_diagnosed, safety_witness,
            };
            let model = emit::emit_btor2(&injected_file);
            let invariant = format!("nu Z. ((!({atom})) && [] Z)");
            let formula = crate::mu_calculus::parser::parse(&invariant)
                .map_err(|e| format!("--target `{atom}`: {e:?}"))?;
            let (v, outcome) = reach_portfolio_rescue_diagnosed(&model, &formula, false)
                .map_err(|d| format!("--target `{atom}`: the reach rescue declined ({d:?})"))?;
            let (verdict, decided_by) = match v {
                RescueVerdict::Holds => ("holds", outcome.unreachable_by.clone()),
                RescueVerdict::Violated => ("violated", outcome.reachable_by.clone()),
                _ => ("unknown", Vec::new()),
            };
            let (attack, depth) = if verdict == "violated" {
                match safety_witness(&model, &formula, false, witness_max_k) {
                    Some(trace) => {
                        let d = trace.states.len().saturating_sub(1);
                        (faults_in_trace(&trace, &injected), Some(d))
                    }
                    None => (Vec::new(), None),
                }
            } else {
                (Vec::new(), None)
            };
            Ok(FaultReachOutcome {
                verdict,
                decided_by: decided_by.iter().map(|s| s.to_string()).collect(),
                fault: label,
                target: Some(atom.to_string()),
                attack,
                depth,
            })
        }
        None => {
            use crate::adapter::btor2::native_bmc::BmcOutcome;
            use crate::adapter::btor2::witness::bad_reachable_witness_routed;
            use crate::adapter::reach_portfolio::{ReachVerdict, decide_reach_portfolio};
            let outcome = decide_reach_portfolio(&injected_file);
            let (verdict, decided_by) = match outcome.verdict {
                ReachVerdict::Reachable => ("violated", outcome.reachable_by.clone()),
                ReachVerdict::Unreachable => ("holds", outcome.unreachable_by.clone()),
                _ => ("unknown", Vec::new()),
            };
            let (attack, depth) = if verdict == "violated" {
                match bad_reachable_witness_routed(&injected_file, witness_max_k) {
                    Ok((BmcOutcome::Violated { depth }, Some(trace))) => {
                        (faults_in_trace(&trace, &injected), Some(depth as usize))
                    }
                    _ => (Vec::new(), None),
                }
            } else {
                (Vec::new(), None)
            };
            Ok(FaultReachOutcome {
                verdict,
                decided_by: decided_by.iter().map(|s| s.to_string()).collect(),
                fault: label,
                target: None,
                attack,
                depth,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A trap FSM (`st`: 0 → 1 → 2 → 3, 3 absorbing; `go` advances) with a latch `seen3` that
    /// records a visit to 3. Without a fault `seen3 ∧ st == 0` is unreachable (3 is a trap); one
    /// injection on `st` after the warm-up escapes it.
    const TRAP_ESCAPE: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n4 zero 2\n\
                               5 one 2\n6 constd 2 3\n7 state 2 st\n8 init 2 7 4\n9 eq 1 7 6\n\
                               10 add 2 7 5\n11 ite 2 3 10 7\n12 ite 2 9 7 11\n13 next 2 7 12\n\
                               14 zero 1\n15 state 1 seen3\n16 init 1 15 14\n17 or 1 15 9\n\
                               18 next 1 15 17\n";
    /// Two redundant copies of the trap; any disagreement escalates both to 3; `seen3` records a
    /// visit. Escaping the trap (`seen3 ∧ both 0`) needs both copies corrupted in the SAME cycle —
    /// one corrupted copy disagrees and is driven back to 3.
    const REDUNDANT_TRAP: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 input 1 go\n4 zero 2\n\
                                  5 one 2\n6 constd 2 3\n7 state 2 st_a\n8 init 2 7 4\n\
                                  9 state 2 st_b\n10 init 2 9 4\n11 neq 1 7 9\n12 add 2 7 5\n\
                                  13 ite 2 3 12 7\n14 eq 1 7 6\n15 ite 2 14 7 13\n\
                                  16 ite 2 11 6 15\n17 next 2 7 16\n18 add 2 9 5\n\
                                  19 ite 2 3 18 9\n20 eq 1 9 6\n21 ite 2 20 9 19\n\
                                  22 ite 2 11 6 21\n23 next 2 9 22\n24 zero 1\n\
                                  25 state 1 seen3\n26 init 1 25 24\n27 or 1 25 14\n\
                                  28 next 1 25 27\n";

    fn spec(ops: &[&str], warmup: u32) -> FaultSpec {
        FaultSpec::parse_many(
            &ops.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            warmup,
        )
        .expect("parses")
    }

    #[test]
    fn a_zero_budget_reproduces_the_fault_free_design() {
        let o = verify_fault_reach(
            TRAP_ESCAPE,
            &spec(&["st:0"], 2),
            Some("seen3 == 1 && st == 0"),
            20,
        )
        .expect("decides");
        assert_eq!(o.verdict, "holds", "{o:?}");
    }

    #[test]
    fn one_fault_escapes_the_trap_and_the_witness_names_it() {
        let o = verify_fault_reach(
            TRAP_ESCAPE,
            &spec(&["st:1"], 2),
            Some("seen3 == 1 && st == 0"),
            20,
        )
        .expect("decides");
        assert_eq!(o.verdict, "violated", "{o:?}");
        assert_eq!(o.attack.len(), 1, "{o:?}");
        assert_eq!(o.attack[0].site, "st");
        assert_eq!(o.attack[0].value.as_u64(), Some(0));
        assert!(o.attack[0].cycle >= 2, "after the warm-up: {o:?}");
    }

    #[test]
    fn the_budget_is_never_overspent() {
        let o = verify_fault_reach(
            TRAP_ESCAPE,
            &spec(&["st:1"], 0),
            Some("mununu_fault_cnt == 2"),
            20,
        )
        .expect("decides");
        assert_eq!(
            o.verdict, "holds",
            "k = 1: the counter never reads 2: {o:?}"
        );
    }

    #[test]
    fn redundancy_defeats_a_single_fault_and_falls_to_two() {
        let target = Some("seen3 == 1 && st_a == 0 && st_b == 0");
        let one = verify_fault_reach(REDUNDANT_TRAP, &spec(&["st_a", "st_b:1"], 0), target, 20)
            .expect("decides");
        assert_eq!(
            one.verdict, "holds",
            "redundancy defeats one fault: {one:?}"
        );
        let two = verify_fault_reach(REDUNDANT_TRAP, &spec(&["st_a", "st_b:2"], 0), target, 20)
            .expect("decides");
        assert_eq!(two.verdict, "violated", "{two:?}");
        assert_eq!(two.attack.len(), 2, "{two:?}");
        assert_eq!(
            two.attack[0].cycle, two.attack[1].cycle,
            "both in one cycle: {two:?}"
        );
    }
}
