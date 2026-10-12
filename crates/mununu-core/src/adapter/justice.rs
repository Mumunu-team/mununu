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
use crate::adapter::btor2::symbolic_bitblast::{
    ExactSymbolicOptions, ExactVerdict, exact_symbolic_verdict_with_witness_and_options,
};
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
    // The exact rung's time slice when l2s follows it (`MUNUNU_JUSTICE_EXACT_BUDGET_MS`): without
    // one, a wide νμ that neither converges nor hits a budget consumes the whole run and the l2s
    // rung never gets its turn — measured on the LIVE sweep (two l2s lassos lost to timeouts).
    // The levers themselves come from the environment through `ExactSymbolicOptions::default()`.
    let exact_opts = ExactSymbolicOptions {
        time_budget_ms: if rung == JusticeRung::Both {
            std::env::var("MUNUNU_JUSTICE_EXACT_BUDGET_MS")
                .ok()
                .and_then(|v| v.parse().ok())
        } else {
            None
        },
        ..Default::default()
    };
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
                Ok(formula) => match exact_symbolic_verdict_with_witness_and_options(
                    &named,
                    &formula,
                    &exact_opts,
                )
                .map(|(v, _)| v)
                {
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

    /// `a` rises after reset and `b` never does: the states `01` / `11` are UNREACHABLE, and the
    /// only fair cycles for `justice {b}` live there. Unrestricted, the fair-EG ν fixpoint is
    /// non-empty (those states); restricted to `Reach` it is empty. The verdict is HOLDS either way.
    const UNREACHABLE_CYCLE: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 a\n5 state 1 b\n\
                                     6 init 1 4 2\n7 init 1 5 2\n8 next 1 4 3\n9 next 1 5 5\n\
                                     10 justice 1 5\n";

    fn lever_opts(reach_restrict: bool, early_exit: bool) -> ExactSymbolicOptions {
        ExactSymbolicOptions {
            reach_restrict,
            early_exit,
            ..Default::default()
        }
    }

    /// The verdict-identity gate for the HWMCC LIVE levers: the reachable-state restriction and
    /// the root fixpoint's early exit may turn an abstention into a verdict, never flip one.
    #[test]
    fn the_levers_never_change_a_justice_verdict() {
        for (name, design) in [
            ("counter", COUNTER),
            ("stuck", STUCK),
            ("constrained", CONSTRAINED),
            ("unreachable-cycle", UNREACHABLE_CYCLE),
        ] {
            let n = name_justice_literals(design).expect("named");
            let assumptions: Vec<Assumption> = n.justice[0]
                .iter()
                .map(|l| Assumption::State {
                    atom: format!("{l} == 1"),
                })
                .collect();
            let formula =
                crate::mu_calculus::parser::parse(&format!("!({})", fair_eg("true", &assumptions)))
                    .expect("parses");
            let run = |r: bool, e: bool| {
                exact_symbolic_verdict_with_witness_and_options(
                    &n.btor2,
                    &formula,
                    &lever_opts(r, e),
                )
                .map(|(v, _)| v)
                .unwrap_or_else(|why| panic!("{name} reach={r} early={e}: {why}"))
            };
            let base = run(false, false);
            for (r, e) in [(true, false), (false, true), (true, true)] {
                assert_eq!(run(r, e), base, "{name}: reach_restrict={r} early_exit={e}");
            }
            if name == "unreachable-cycle" {
                assert_eq!(base, ExactVerdict::Holds);
            }
        }
    }

    /// Lever A computes the FORWARD reachable set: on `UNREACHABLE_CYCLE` it is `{00, 10}`,
    /// closed after two image steps, and excludes every `b = 1` state.
    #[test]
    fn the_reachable_restriction_is_the_forward_reachable_set() {
        use crate::adapter::btor2::predicate_expr::parse_predicate_expr;
        use crate::adapter::btor2::symbolic_bitblast::BddBitBlaster;
        use oxidd::BooleanFunction;
        let file = parser::parse(UNREACHABLE_CYCLE).expect("parses");
        let bb = BddBitBlaster::build(&file).expect("bit-blasts");
        let model = bb.exact_model();
        let init = bb.initial_state_bdd(&file).expect("init");
        let steps = model
            .install_reachable_restriction(&init)
            .expect("the restriction installs");
        assert_eq!(steps, 2, "00 → {{00, 10}} → closed");
        let reach = model.reachable_restriction().expect("installed");
        let bdd = |s: &str| {
            bb.predicate_bdd(&parse_predicate_expr(s).expect("expr"))
                .expect("bdd")
        };
        // `BDDFunction` has no `Debug`, so plain asserts on canonical equality.
        assert!(
            reach.and(&bdd("b == 1")).unwrap() == *model.ff(),
            "no reachable state has b = 1"
        );
        assert!(
            reach.and(&bdd("a == 1")).unwrap() != *model.ff(),
            "a = 1 is reached"
        );
        assert!(
            reach.and(&bdd("a == 0")).unwrap() != *model.ff(),
            "the initial state is in Reach"
        );
    }
}
