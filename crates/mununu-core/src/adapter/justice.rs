//! Justice (liveness) properties on BTOR2 — the HWMCC LIVE study (2026-10-11).
//!
//! A BTOR2 `justice n l₁ … lₙ` (AIGER 1.9 justice set) is **violated** iff the design has an
//! infinite path, respecting every `constraint` at every step, on which every `lₖ` and every
//! `fair` literal holds infinitely often; it **holds** iff no such path exists. Two rungs decide
//! it, both sound at every alternation depth:
//!
//! - **exact** — `exact-symbolic` (full-state ROBDD): the Emerson–Lei fair-`EG` fixpoint
//!   `E_C G true = νZ. ⋀ₖ ◇ μY. ((Z ∧ lₖ) ∨ ◇Y)` ([`crate::verify::fairness::fair_eg`]) is evaluated
//!   on the design; the property holds iff **no** initial state satisfies it, so the engine is
//!   asked `¬E_C G true` and its universal-over-init verdict is the justice verdict. Constraints
//!   restrict the modalities to legal frames (mununu#656). Bounded by the bit cap.
//! - **l2s** — liveness-to-safety: the existing response monitor
//!   ([`crate::adapter::btor2::l2s_monitor::emit_response_l2s_monitor_under_fairness`]) with
//!   antecedent `true`, consequent `false` and the justice + fairness literals as the fairness
//!   atoms is exactly "a reachable cycle on which every literal is seen", a `bad` the reachability
//!   portfolio (native BMC/k-induction ⊕ spacer ⊕ btormc ⊕ Pono) decides at scale.
//!
//! The literals of a `justice` / `fair` line are rarely named, so [`name_justice_literals`] first
//! appends `output` lines (`mununu_j<i>_<k>`, `mununu_f<m>`, plus `mununu_true` / `mununu_false`
//! on the 1-bit constants) — an atom over an `output` name binds in both rungs, and the design is
//! otherwise untouched (an `output` is observation only).
//!
//! engine: `exact-symbolic` ROBDD fair-cycle fixpoint (decider within the cap) ⊕ l2s + reach
//! portfolio (decider at scale); the two cross-check each other where both answer.

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Operand, Sort};
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::CmpOp;
use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
use crate::adapter::liveness_rescue::{
    Atom, LivenessVerdict, response_liveness_rescue_under_fairness,
};
use crate::verdict::PropertyVerdict;
use crate::verify::fairness::{Assumption, fair_eg};

/// Which rung(s) to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JusticeRung {
    /// The exact ROBDD fair-cycle fixpoint only.
    Exact,
    /// Liveness-to-safety + the reachability portfolio only.
    L2s,
    /// Exact first; l2s when the exact engine abstains (the default).
    Both,
}

impl JusticeRung {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "exact" => Ok(Self::Exact),
            "l2s" => Ok(Self::L2s),
            "both" | "auto" => Ok(Self::Both),
            other => Err(format!(
                "unknown justice rung `{other}` — expected `exact`, `l2s` or `both`"
            )),
        }
    }
}

/// One justice set's result.
#[derive(Debug, Clone)]
pub struct JusticeVerdict {
    /// Position of the `justice` line among the design's justice lines (0-based).
    pub index: usize,
    /// Number of literals in the set.
    pub size: usize,
    /// `Holds` = no fair infinite path (HWMCC `uns`), `Violated` = one exists (`sat`), `Unknown`
    /// = every rung abstained.
    pub verdict: PropertyVerdict,
    /// Which rung decided (`exact-symbolic` or `l2s` + the portfolio members).
    pub decided_by: Vec<String>,
    /// Why a rung abstained, when it did (the exact engine's refusal / cap message, the
    /// portfolio's inconclusive outcome).
    pub notes: Vec<String>,
}

/// Append `body` as the next line; returns its nid.
fn append(out: &mut String, next: &mut Nid, body: &str) -> Nid {
    let n = *next;
    *next += 1;
    out.push_str(&format!("{n} {body}\n"));
    n
}

/// The 1-bit `one` / `zero` constant of `bool_sort`, if the design already has one.
fn find_bool_const(file: &Btor2File, bool_sort: Nid, want_one: bool) -> Option<Nid> {
    file.lines.iter().find_map(|l| match &l.node {
        Node::Const { sort, value } if *sort == bool_sort => match (value, want_one) {
            (ConstValue::One, true) | (ConstValue::Zero, false) => Some(l.nid),
            _ => None,
        },
        _ => None,
    })
}

/// The node of a literal operand; a negated one gets a `not` node appended.
fn literal_node(op: &Operand, bool_sort: Nid, out: &mut String, next: &mut Nid) -> Nid {
    if op.is_negated() {
        append(out, next, &format!("not {bool_sort} {}", op.nid()))
    } else {
        op.nid()
    }
}

/// [`name_justice_literals`]'s result: the design with the literals exposed, and their names.
#[derive(Debug, Clone)]
pub struct NamedLiterals {
    /// The design text with the `output` lines appended.
    pub btor2: String,
    /// One name list per `justice` line, in file order.
    pub justice: Vec<Vec<String>>,
    /// One name per `fair` line, in file order.
    pub fairness: Vec<String>,
}

/// The design with every justice / fairness literal exposed as a named `output`, plus the names:
/// one `Vec<String>` per justice set, and the fairness names.
pub fn name_justice_literals(btor2: &str) -> Result<NamedLiterals, String> {
    let file = parser::parse(btor2).map_err(|e| format!("justice: {}", e.message))?;
    let mut next: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut out = String::from(btor2);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // A 1-bit sort and its constants, found or appended.
    let bool_sort = match file.lines.iter().find_map(|l| match &l.node {
        Node::Sort {
            sort: Sort::BitVec { width: 1 },
        } => Some(l.nid),
        _ => None,
    }) {
        Some(n) => n,
        None => append(&mut out, &mut next, "sort bitvec 1"),
    };
    let one = match find_bool_const(&file, bool_sort, true) {
        Some(n) => n,
        None => append(&mut out, &mut next, &format!("one {bool_sort}")),
    };
    let zero = match find_bool_const(&file, bool_sort, false) {
        Some(n) => n,
        None => append(&mut out, &mut next, &format!("zero {bool_sort}")),
    };
    append(&mut out, &mut next, &format!("output {one} mununu_true"));
    append(&mut out, &mut next, &format!("output {zero} mununu_false"));
    let mut justice_names: Vec<Vec<String>> = Vec::new();
    let mut fair_names: Vec<String> = Vec::new();
    let (mut ji, mut fi) = (0usize, 0usize);
    for line in &file.lines {
        match &line.node {
            Node::Justice { signals } => {
                let mut names = Vec::with_capacity(signals.len());
                for (k, sig) in signals.iter().enumerate() {
                    let n = literal_node(sig, bool_sort, &mut out, &mut next);
                    let name = format!("mununu_j{ji}_{k}");
                    append(&mut out, &mut next, &format!("output {n} {name}"));
                    names.push(name);
                }
                justice_names.push(names);
                ji += 1;
            }
            Node::Fair { signal } => {
                let n = literal_node(signal, bool_sort, &mut out, &mut next);
                let name = format!("mununu_f{fi}");
                append(&mut out, &mut next, &format!("output {n} {name}"));
                fair_names.push(name);
                fi += 1;
            }
            _ => {}
        }
    }
    if justice_names.is_empty() {
        return Err("justice: the design has no `justice` line".to_string());
    }
    Ok(NamedLiterals {
        btor2: out,
        justice: justice_names,
        fairness: fair_names,
    })
}

fn atom(name: &str) -> Atom {
    Atom {
        signal: name.to_string(),
        op: CmpOp::Eq,
        value: 1,
    }
}

/// Decide every justice set of `btor2` (the design text, `justice` lines included).
pub fn verify_justice(btor2: &str, rung: JusticeRung) -> Result<Vec<JusticeVerdict>, String> {
    let NamedLiterals {
        btor2: named,
        justice: justice_names,
        fairness: fair_names,
    } = name_justice_literals(btor2)?;
    let mut results = Vec::with_capacity(justice_names.len());
    for (index, names) in justice_names.iter().enumerate() {
        let lits: Vec<&String> = names.iter().chain(fair_names.iter()).collect();
        let mut decided_by: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut verdict = PropertyVerdict::Unknown;
        if matches!(rung, JusticeRung::Exact | JusticeRung::Both) {
            let assumptions: Vec<Assumption> = lits
                .iter()
                .map(|n| Assumption::State {
                    atom: format!("{n} == 1"),
                })
                .collect();
            // ¬E_C G true: holds at EVERY initial state ⇔ no fair path from any of them.
            let formula_str = format!("!({})", fair_eg("true", &assumptions));
            match crate::mu_calculus::parser::parse(&formula_str) {
                Err(e) => notes.push(format!("exact: fair-EG formula did not parse: {e:?}")),
                Ok(formula) => match exact_symbolic_verdict(&named, &formula) {
                    Ok(ExactVerdict::Holds) => {
                        verdict = PropertyVerdict::Holds;
                        decided_by.push("exact-symbolic".into());
                    }
                    Ok(ExactVerdict::Violated) => {
                        verdict = PropertyVerdict::Violated;
                        decided_by.push("exact-symbolic".into());
                    }
                    Err(reason) => notes.push(format!("exact: abstained — {reason}")),
                },
            }
        }
        if verdict == PropertyVerdict::Unknown
            && matches!(rung, JusticeRung::L2s | JusticeRung::Both)
        {
            let fair_atoms: Vec<Atom> = lits.iter().map(|n| atom(n)).collect();
            match response_liveness_rescue_under_fairness(
                &named,
                &atom("mununu_true"),
                &atom("mununu_false"),
                &fair_atoms,
                false,
            ) {
                Some((LivenessVerdict::Holds, outcome)) => {
                    verdict = PropertyVerdict::Holds;
                    decided_by.push("l2s".into());
                    decided_by.extend(outcome.unreachable_by.iter().map(|s| s.to_string()));
                }
                Some((LivenessVerdict::Violated, outcome)) => {
                    verdict = PropertyVerdict::Violated;
                    decided_by.push("l2s".into());
                    decided_by.extend(outcome.reachable_by.iter().map(|s| s.to_string()));
                }
                Some((LivenessVerdict::Inconclusive, _)) => notes.push(
                    "l2s: the reachability portfolio left the fair-cycle monitor undecided".into(),
                ),
                None => notes.push("l2s: the fair-cycle monitor could not be built".into()),
            }
        }
        results.push(JusticeVerdict {
            index,
            size: names.len(),
            verdict,
            decided_by,
            notes,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A free-running 2-bit counter: `lo ∧ hi` recurs forever → a fair cycle exists → VIOLATED (sat).
    const COUNTER: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                           6 init 1 4 2\n7 init 1 5 2\n8 not 1 4\n9 next 1 4 8\n10 xor 1 4 5\n\
                           11 next 1 5 10\n12 and 1 4 5\n13 justice 1 12\n";
    /// `s` is 0 only at reset and 1 forever after; justice on `¬s` has no fair path → HOLDS (uns).
    const STUCK: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 s\n5 init 1 4 2\n\
                         6 next 1 4 3\n7 not 1 4\n8 justice 1 7\n";
    /// The counter under `constraint ¬lo`: no legal infinite path at all → HOLDS.
    const CONSTRAINED: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                               6 init 1 4 2\n7 init 1 5 2\n8 not 1 4\n9 next 1 4 8\n10 xor 1 4 5\n\
                               11 next 1 5 10\n12 and 1 4 5\n13 justice 1 12\n14 constraint 8\n";

    #[test]
    fn literals_get_names_and_the_design_still_parses() {
        let n = name_justice_literals(COUNTER).expect("named");
        assert_eq!(n.justice, vec![vec!["mununu_j0_0".to_string()]]);
        assert!(n.fairness.is_empty());
        assert!(
            n.btor2.contains(" output 3 mununu_true\n")
                && n.btor2.contains(" output 2 mununu_false\n")
        );
        parser::parse(&n.btor2).expect("still parses");
    }

    #[test]
    fn both_rungs_find_the_counters_fair_cycle() {
        for rung in [JusticeRung::Exact, JusticeRung::L2s] {
            let r = verify_justice(COUNTER, rung).expect("runs");
            assert_eq!(r.len(), 1);
            assert_eq!(
                r[0].verdict,
                PropertyVerdict::Violated,
                "{rung:?}: {:?}",
                r[0]
            );
        }
    }

    #[test]
    fn both_rungs_prove_the_stuck_latch_has_no_fair_path() {
        for rung in [JusticeRung::Exact, JusticeRung::L2s] {
            let r = verify_justice(STUCK, rung).expect("runs");
            assert_eq!(r[0].verdict, PropertyVerdict::Holds, "{rung:?}: {:?}", r[0]);
        }
    }

    #[test]
    fn both_rungs_honour_a_constraint_that_kills_every_infinite_path() {
        for rung in [JusticeRung::Exact, JusticeRung::L2s] {
            let r = verify_justice(CONSTRAINED, rung).expect("runs");
            assert_eq!(r[0].verdict, PropertyVerdict::Holds, "{rung:?}: {:?}", r[0]);
        }
    }

    #[test]
    fn a_design_without_justice_is_an_error_not_a_vacuous_holds() {
        assert!(verify_justice("1 sort bitvec 1\n2 state 1 s\n", JusticeRung::Both).is_err());
    }
}
