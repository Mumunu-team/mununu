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
use crate::adapter::btor2::symbolic_bitblast::{ExactSymbolicOptions, ExactVerdict, exact_ladder};
use crate::adapter::liveness_rescue::{
    Atom, LivenessVerdict, response_liveness_rescue_under_fairness_cancellable,
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

/// P3 — how the rungs are scheduled under [`JusticeRung::Both`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JusticeOptions {
    pub rung: JusticeRung,
    /// The whole run's wall-clock budget per justice set (`None` = the engines' own budgets).
    pub budget_ms: Option<u64>,
    /// Run the exact ladder and the l2s rung CONCURRENTLY, first definite verdict wins and the
    /// other is cancelled; `false` = `exact ≤ EXACT_SLICE → l2s → exact with the remainder`.
    pub parallel_rungs: bool,
}

/// The exact ladder's first slice under the sequential policy (30 s): every exact decision on
/// the LIVE band came in ≤ 7 s; the l2s lassos that were lost to a longer slice needed 172–258 s.
const EXACT_SLICE_MS: u64 = 30_000;

/// Decide every justice set of `btor2` (the design text, `justice` lines included), sequential
/// policy, no overall budget.
pub fn verify_justice(btor2: &str, rung: JusticeRung) -> Result<Vec<JusticeVerdict>, String> {
    verify_justice_with(
        btor2,
        &JusticeOptions {
            rung,
            budget_ms: None,
            parallel_rungs: false,
        },
    )
}

/// The exact rung on one justice set: the fair-EG formula through [`exact_ladder`].
fn exact_rung(
    named: &str,
    lits: &[String],
    budget: Option<std::time::Duration>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(PropertyVerdict, &'static str, Vec<String>), String> {
    let assumptions: Vec<Assumption> = lits
        .iter()
        .map(|n| Assumption::State {
            atom: format!("{n} == 1"),
        })
        .collect();
    // ¬E_C G true: holds at EVERY initial state ⇔ no fair path from any of them.
    let formula_str = format!("!({})", fair_eg("true", &assumptions));
    let formula = crate::mu_calculus::parser::parse(&formula_str)
        .map_err(|e| format!("exact: fair-EG formula did not parse: {e:?}"))?;
    let out = exact_ladder(
        named,
        &formula,
        &std::collections::HashSet::new(),
        &ExactSymbolicOptions::default(),
        budget,
        cancel,
    );
    match out.result {
        Ok((ExactVerdict::Holds, _)) => Ok((PropertyVerdict::Holds, out.stage, out.notes)),
        Ok((ExactVerdict::Violated, _)) => Ok((PropertyVerdict::Violated, out.stage, out.notes)),
        Err(reason) => {
            let mut notes = out.notes;
            notes.push(format!("exact: abstained — {reason}"));
            Err(notes.join(" | "))
        }
    }
}

/// The l2s rung on one justice set.
/// Rung threads of the parallel policy still running (mununu#689). Incremented when a rung
/// starts, decremented by [`RungGuard`] when it ends — on every path, including a panic.
static LIVE_RUNGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct RungGuard;

impl RungGuard {
    fn enter() -> Self {
        LIVE_RUNGS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        RungGuard
    }
}

impl Drop for RungGuard {
    fn drop(&mut self) {
        LIVE_RUNGS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn l2s_rung(
    named: &str,
    lits: &[String],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> (PropertyVerdict, Vec<String>, Vec<String>) {
    let fair_atoms: Vec<Atom> = lits.iter().map(|n| atom(n)).collect();
    match response_liveness_rescue_under_fairness_cancellable(
        named,
        &atom("mununu_true"),
        &atom("mununu_false"),
        &fair_atoms,
        false,
        cancel,
    ) {
        Some((LivenessVerdict::Holds, outcome)) => {
            let mut by = vec!["l2s".to_string()];
            by.extend(outcome.unreachable_by.iter().map(|s| s.to_string()));
            (PropertyVerdict::Holds, by, Vec::new())
        }
        Some((LivenessVerdict::Violated, outcome)) => {
            let mut by = vec!["l2s".to_string()];
            by.extend(outcome.reachable_by.iter().map(|s| s.to_string()));
            (PropertyVerdict::Violated, by, Vec::new())
        }
        Some((LivenessVerdict::Inconclusive, _)) => (
            PropertyVerdict::Unknown,
            Vec::new(),
            vec!["l2s: the reachability portfolio left the fair-cycle monitor undecided".into()],
        ),
        None => (
            PropertyVerdict::Unknown,
            Vec::new(),
            vec!["l2s: the fair-cycle monitor could not be built".into()],
        ),
    }
}

/// Decide every justice set of `btor2` under `opts` (see [`JusticeOptions`]).
pub fn verify_justice_with(
    btor2: &str,
    opts: &JusticeOptions,
) -> Result<Vec<JusticeVerdict>, String> {
    use std::time::{Duration, Instant};
    let NamedLiterals {
        btor2: named,
        justice: justice_names,
        fairness: fair_names,
    } = name_justice_literals(btor2)?;
    let mut results = Vec::with_capacity(justice_names.len());
    for (index, names) in justice_names.iter().enumerate() {
        let lits: Vec<String> = names.iter().chain(fair_names.iter()).cloned().collect();
        let start = Instant::now();
        let total = opts.budget_ms.map(Duration::from_millis);
        let remaining = |t: Option<Duration>| t.map(|t| t.saturating_sub(start.elapsed()));
        let mut decided_by: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut verdict = PropertyVerdict::Unknown;
        match opts.rung {
            JusticeRung::Exact => match exact_rung(&named, &lits, total, None) {
                Ok((v, stage, mut n)) => {
                    verdict = v;
                    decided_by.push(stage.to_string());
                    notes.append(&mut n);
                }
                Err(e) => notes.push(e),
            },
            JusticeRung::L2s => {
                let (v, by, mut n) = l2s_rung(&named, &lits, None);
                verdict = v;
                decided_by = by;
                notes.append(&mut n);
            }
            JusticeRung::Both if opts.parallel_rungs => {
                // Both rungs at once; the first definite verdict wins and the other is
                // CANCELLED and JOINED before this returns. mununu#689: the rungs used to run on
                // detached threads, so the loser — an l2s reachability portfolio inside z3 —
                // was still running for ~100 ms after this returned (measured: 2 OS threads
                // before the call, 8 right after). A process that exited in that window tore
                // native state down under a live z3 thread; CI saw a SIGSEGV after the test had
                // passed. Scoped threads make "returned" mean "nothing left running".
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::scope(|scope| {
                    let exact_h = {
                        let tx = tx.clone();
                        let cancel = cancel.clone();
                        let (named, lits) = (&named, &lits);
                        scope.spawn(move || {
                            let _live = RungGuard::enter();
                            let r = exact_rung(named, lits, total, Some(cancel));
                            let _ = tx.send(("exact", r));
                        })
                    };
                    let l2s_h = {
                        let tx = tx.clone();
                        let cancel = &*cancel;
                        let (named, lits) = (&named, &lits);
                        scope.spawn(move || {
                            let _live = RungGuard::enter();
                            let (v, by, n) = l2s_rung(named, lits, Some(cancel));
                            let r = if v == PropertyVerdict::Unknown {
                                Err(n.join(" | "))
                            } else {
                                Ok((v, "l2s", by))
                            };
                            let _ = tx.send(("l2s", r));
                        })
                    };
                    drop(tx);
                    let mut pending = 2;
                    while pending > 0 {
                        let Ok((who, r)) = rx.recv() else { break };
                        pending -= 1;
                        match r {
                            Ok((v, stage, mut by)) => {
                                verdict = v;
                                if who == "exact" {
                                    decided_by.push(stage.to_string());
                                } else {
                                    decided_by.append(&mut by);
                                }
                                break;
                            }
                            Err(e) => notes.push(format!("{who}: {e}")),
                        }
                    }
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Joined explicitly so a panicking rung abstains (as the detached thread
                    // did) instead of re-panicking out of the scope.
                    let _ = exact_h.join();
                    let _ = l2s_h.join();
                });
            }
            JusticeRung::Both => {
                // exact ≤ EXACT_SLICE → l2s → exact with the remainder.
                let slice = Duration::from_millis(EXACT_SLICE_MS);
                let first = remaining(total).map_or(slice, |r| r.min(slice));
                match exact_rung(&named, &lits, Some(first), None) {
                    Ok((v, stage, mut n)) => {
                        verdict = v;
                        decided_by.push(stage.to_string());
                        notes.append(&mut n);
                    }
                    Err(e) => notes.push(e),
                }
                if verdict == PropertyVerdict::Unknown {
                    let (v, by, mut n) = l2s_rung(&named, &lits, None);
                    verdict = v;
                    decided_by = by;
                    notes.append(&mut n);
                }
                if verdict == PropertyVerdict::Unknown
                    && !remaining(total).is_some_and(|r| r.is_zero())
                {
                    match exact_rung(&named, &lits, remaining(total), None) {
                        Ok((v, stage, mut n)) => {
                            verdict = v;
                            decided_by.push(stage.to_string());
                            notes.append(&mut n);
                        }
                        Err(e) => notes.push(format!("exact (remainder): {e}")),
                    }
                }
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
    use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict_with_witness_and_options;

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
        let model = bb.exact_model().unwrap();
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::adapter::btor2::symbolic_bitblast::LADDER_STAGE_UNRESTRICTED;

    /// A free-running 2-bit counter: `lo ∧ hi` recurs forever → VIOLATED.
    const COUNTER: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                           6 init 1 4 2\n7 init 1 5 2\n8 not 1 4\n9 next 1 4 8\n10 xor 1 4 5\n\
                           11 next 1 5 10\n12 and 1 4 5\n13 justice 1 12\n";
    /// `s` is 1 forever after reset; justice on `¬s` → HOLDS.
    const STUCK: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 s\n5 init 1 4 2\n\
                         6 next 1 4 3\n7 not 1 4\n8 justice 1 7\n";

    #[test]
    fn the_sequential_policy_decides_on_the_exact_ladder_and_names_the_stage() {
        let r = verify_justice_with(
            COUNTER,
            &JusticeOptions {
                rung: JusticeRung::Both,
                budget_ms: Some(60_000),
                parallel_rungs: false,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Violated, "{:?}", r[0]);
        assert_eq!(r[0].decided_by, vec![LADDER_STAGE_UNRESTRICTED.to_string()]);
    }

    /// `LIVE_RUNGS` is process-wide; the tests that run the parallel policy hold this lock so
    /// one test's rungs are never counted by another.
    static PARALLEL_POLICY: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_parallel_policy_decides_and_cancels_the_other_rung() {
        let _serial = PARALLEL_POLICY.lock().unwrap_or_else(|e| e.into_inner());
        let r = verify_justice_with(
            STUCK,
            &JusticeOptions {
                rung: JusticeRung::Both,
                budget_ms: Some(60_000),
                parallel_rungs: true,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Holds, "{:?}", r[0]);
        assert!(!r[0].decided_by.is_empty(), "{:?}", r[0]);
    }

    /// REPRODUCER (mununu#689) — the parallel policy must not RETURN while a rung still runs.
    ///
    /// CI saw `the_parallel_policy_decides_and_cancels_the_other_rung` report `ok` and then die
    /// with SIGSEGV. The rungs ran on detached threads, so the losing one — an l2s reachability
    /// portfolio inside z3 — outlived the call: measured in the dev image, 2 OS threads before
    /// the call, 8 right after it returned, back to the warm baseline only ~100 ms later. A
    /// process exiting in that window tears native state down under a live z3 thread.
    ///
    /// `LIVE_RUNGS` counts rung threads still running; it must be 0 the instant the call returns.
    /// With only the counter applied to the parent commit this fails (the losing rung is still
    /// counted); with scoped, cancelled, joined rungs it passes. (A raw OS-thread count is not the
    /// observable: idle OxiDD worker pools come and go with BDD managers and make it noisy.)
    #[test]
    fn the_parallel_policy_leaves_no_rung_running_after_it_returns() {
        let _serial = PARALLEL_POLICY.lock().unwrap_or_else(|e| e.into_inner());
        for design in [STUCK, COUNTER] {
            let r = verify_justice_with(
                design,
                &JusticeOptions {
                    rung: JusticeRung::Both,
                    budget_ms: Some(60_000),
                    parallel_rungs: true,
                },
            )
            .expect("runs");
            assert_ne!(r[0].verdict, PropertyVerdict::Unknown, "{:?}", r[0]);
            assert_eq!(
                LIVE_RUNGS.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "a rung is still running after the call returned"
            );
        }
    }

    #[test]
    fn a_zero_budget_is_an_abstention_not_an_error() {
        let r = verify_justice_with(
            COUNTER,
            &JusticeOptions {
                rung: JusticeRung::Exact,
                budget_ms: Some(0),
                parallel_rungs: false,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Unknown, "{:?}", r[0]);
        assert!(!r[0].notes.is_empty());
    }
}
