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
//!   portfolio (native BMC/k-induction ⊕ interpolation ⊕ btormc ⊕ Pono ⊕ rIC3) decides at scale.
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
    /// k-liveness (Track V): a bound on the fair rounds, proved as safety by the proof-directed
    /// members ([`crate::adapter::reach_portfolio::ReachMembers::PROOF_DIRECTED`]); HOLDS only.
    Kliveness,
    /// Exact first; l2s, then k-liveness, when the exact engine abstains (the default).
    Both,
}

impl JusticeRung {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "exact" => Ok(Self::Exact),
            "l2s" => Ok(Self::L2s),
            "kliveness" | "k-liveness" => Ok(Self::Kliveness),
            "both" | "auto" => Ok(Self::Both),
            other => Err(format!(
                "unknown justice rung `{other}` — expected `exact`, `l2s`, `kliveness` or `both`"
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
    /// Run the exact ladder, the l2s rung and the k-liveness rung CONCURRENTLY, first definite
    /// verdict wins and the others are cancelled; `false` = `exact ≤ EXACT_SLICE → l2s →
    /// kliveness → exact with the remainder`.
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

/// The l2s rung on one justice set.
///
/// `budget` bounds the reachability portfolio on the fair-cycle monitor: it is installed as a
/// child [`run_budget`](crate::adapter::run_budget) budget for the call, and the portfolio derives
/// every member's bound from the time left (`portfolio_member_timeouts`). `None` keeps the
/// members' own defaults (60 s per subprocess member). Before this the rung took no budget, so
/// `--timeout-ms` never reached it: on HWMCC'17 LIVE at a 3,600 s lane budget the l2s rung still
/// gave each member 60 s.
fn l2s_rung(
    named: &str,
    lits: &[String],
    budget: Option<std::time::Duration>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    members: crate::adapter::reach_portfolio::ReachMembers,
) -> (PropertyVerdict, Vec<String>, Vec<String>) {
    use crate::adapter::run_budget;
    let fair_atoms: Vec<Atom> = lits.iter().map(|n| atom(n)).collect();
    let scoped = budget.map(|b| {
        let ms = u64::try_from(b.as_millis()).unwrap_or(u64::MAX);
        run_budget::Budget::child(&run_budget::current(), Some(ms))
    });
    let _guard = scoped.as_ref().map(run_budget::enter);
    match response_liveness_rescue_under_fairness_cancellable(
        named,
        &atom("mununu_true"),
        &atom("mununu_false"),
        &fair_atoms,
        false,
        cancel,
        members,
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
        Some((LivenessVerdict::Inconclusive, outcome)) => (
            PropertyVerdict::Unknown,
            Vec::new(),
            vec![l2s_undecided_note(budget, &outcome)],
        ),
        None => (
            PropertyVerdict::Unknown,
            Vec::new(),
            vec!["l2s: the fair-cycle monitor could not be built".into()],
        ),
    }
}

/// Track V — the k-liveness rung on one justice set (Claessen–Sörensson): for each width in
/// [`KLIVENESS_WIDTHS`](crate::adapter::btor2::kliveness_monitor::KLIVENESS_WIDTHS), the monitor of
/// [`emit_kliveness_monitor`](crate::adapter::btor2::kliveness_monitor::emit_kliveness_monitor)
/// is decided as `bad`-reachability by the proof-directed members
/// ([`ReachMembers::PROOF_DIRECTED`](crate::adapter::reach_portfolio::ReachMembers)).
/// `Unreachable` at any width ⇒ HOLDS (sound on the concrete design: no path completes that many
/// fair rounds, so no fair path exists). `Reachable` is NOT a violation — it says the bound is
/// too small — so the rung moves to the next width; a member's `Unknown` under an expired budget
/// ends the ladder. **This rung never reports VIOLATED**; the lasso side stays with l2s.
///
/// engine: proof-directed reach portfolio (rIC3 in its own portfolio mode, Pono `ic3bits`, native
/// k-induction) on the round-counter monitor — role: decider on proofs; the exact rung and l2s
/// cross-check it where they also answer.
///
/// `budget` is the rung's whole wall budget, shared across the widths (a child
/// [`run_budget`](crate::adapter::run_budget) budget, so every member's bound follows the time
/// left); `None` keeps the members' own defaults per width.
fn kliveness_rung(
    design: &str,
    justice_index: usize,
    budget: Option<std::time::Duration>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    members: crate::adapter::reach_portfolio::ReachMembers,
) -> (PropertyVerdict, Vec<String>, Vec<String>) {
    use crate::adapter::btor2::kliveness_monitor::{
        KlivenessLadder, emit_kliveness_monitor, kliveness_ladder,
    };
    use crate::adapter::run_budget;
    let scoped = budget.map(|b| {
        let ms = u64::try_from(b.as_millis()).unwrap_or(u64::MAX);
        run_budget::Budget::child(&run_budget::current(), Some(ms))
    });
    let _guard = scoped.as_ref().map(run_budget::enter);
    let (tried, member_notes) = match kliveness_ladder(
        &|w| emit_kliveness_monitor(design, justice_index, w),
        cancel,
        members,
    ) {
        KlivenessLadder::Holds { bound, members } => {
            let mut by = vec!["kliveness".to_string(), bound.to_string()];
            by.extend(members.iter().map(|s| s.to_string()));
            return (PropertyVerdict::Holds, by, Vec::new());
        }
        KlivenessLadder::Failed(note) => return (PropertyVerdict::Unknown, Vec::new(), vec![note]),
        KlivenessLadder::Undecided {
            tried,
            member_notes,
        } => (tried, member_notes),
    };
    let mut note = match budget {
        Some(b) => format!(
            "kliveness: no fair-round bound proved within a {} ms budget ({})",
            b.as_millis(),
            if tried.is_empty() {
                "no width attempted".to_string()
            } else {
                tried.join(", ")
            }
        ),
        None => format!(
            "kliveness: no fair-round bound proved at the members' default budgets ({}); a reachable bound is not a violation — l2s decides the lasso side",
            if tried.is_empty() {
                "no width attempted".to_string()
            } else {
                tried.join(", ")
            }
        ),
    };
    for m in member_notes {
        note.push_str("; ");
        note.push_str(&m);
    }
    (PropertyVerdict::Unknown, Vec::new(), vec![note])
}

/// The l2s rung's abstention note: the budget the portfolio ran under and every member's refusal
/// or budget abstention, so a reader can tell "more time may help" from "no member carries this"
/// (Failure Paths: the note used to say only "undecided", hiding a fixed 60 s member cap).
fn l2s_undecided_note(
    budget: Option<std::time::Duration>,
    outcome: &crate::adapter::reach_portfolio::ReachOutcome,
) -> String {
    let mut note = match budget {
        Some(b) => format!(
            "l2s: the reachability portfolio left the fair-cycle monitor undecided within a {} ms budget",
            b.as_millis()
        ),
        None => "l2s: the reachability portfolio left the fair-cycle monitor undecided at the \
                 members' default budgets (60 s per subprocess member; `--timeout-ms` raises them)"
            .to_string(),
    };
    for (member, why) in &outcome.refused {
        note.push_str(&format!("; {member} refused: {why}"));
    }
    for (member, why) in &outcome.abstained {
        note.push_str(&format!("; {member} abstained: {why}"));
    }
    note
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
                let (v, by, mut n) = l2s_rung(
                    &named,
                    &lits,
                    total,
                    None,
                    crate::adapter::reach_portfolio::ReachMembers::ALL,
                );
                verdict = v;
                decided_by = by;
                notes.append(&mut n);
            }
            JusticeRung::Kliveness => {
                let (v, by, mut n) = kliveness_rung(
                    &named,
                    index,
                    total,
                    None,
                    crate::adapter::reach_portfolio::ReachMembers::PROOF_DIRECTED,
                );
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
                            // mununu#986: without the exact member, which cannot be
                            // cancelled. The exact rung races beside this one with the whole
                            // budget, so that member's question is still asked; with it in, a
                            // cancelled l2s rung held the join until its own budget ran out.
                            let (v, by, n) = l2s_rung(
                                named,
                                lits,
                                total,
                                Some(cancel),
                                crate::adapter::reach_portfolio::ReachMembers::CANCELLABLE,
                            );
                            let r = if v == PropertyVerdict::Unknown {
                                Err(n.join(" | "))
                            } else {
                                Ok((v, "l2s", by))
                            };
                            let _ = tx.send(("l2s", r));
                        })
                    };
                    let kl_h = {
                        let tx = tx.clone();
                        let cancel = &*cancel;
                        let named = &named;
                        scope.spawn(move || {
                            let _live = RungGuard::enter();
                            // mununu#986: rIC3 as one `-e ic3` process, not its ~11-solver
                            // portfolio, which starved the l2s rung racing beside it.
                            let (v, by, n) = kliveness_rung(
                                named,
                                index,
                                total,
                                Some(cancel),
                                crate::adapter::reach_portfolio::ReachMembers::PROOF_DIRECTED_RACED,
                            );
                            let r = if v == PropertyVerdict::Unknown {
                                Err(n.join(" | "))
                            } else {
                                Ok((v, "kliveness", by))
                            };
                            let _ = tx.send(("kliveness", r));
                        })
                    };
                    drop(tx);
                    let mut pending = 3;
                    while pending > 0 {
                        let Ok((who, r)) = rx.recv() else { break };
                        pending -= 1;
                        match r {
                            Ok((v, stage, mut by)) => {
                                verdict = v;
                                if who == "exact" {
                                    // `by` is the exact rung's notes (its cone size, …).
                                    decided_by.push(stage.to_string());
                                    notes.append(&mut by);
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
                    let _ = kl_h.join();
                });
            }
            JusticeRung::Both => {
                // exact ≤ EXACT_SLICE → l2s → kliveness → exact with the remainder.
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
                    // The l2s rung gets HALF of what the exact slice left; the exact remainder
                    // gets the rest plus whatever l2s did not use. Measured on HWMCC'17 LIVE
                    // (2026-10): exact decisions came in ≤ 7 s, l2s lassos needed 68–258 s, and
                    // the remainder rung is the only path for an exact fixpoint slower than the
                    // slice — so neither side may take the whole budget.
                    let l2s_budget = remaining(total).map(|r| r / 2);
                    let (v, by, mut n) = l2s_rung(
                        &named,
                        &lits,
                        l2s_budget,
                        None,
                        crate::adapter::reach_portfolio::ReachMembers::ALL,
                    );
                    verdict = v;
                    decided_by = by;
                    notes.append(&mut n);
                }
                if verdict == PropertyVerdict::Unknown
                    && !remaining(total).is_some_and(|r| r.is_zero())
                {
                    // Track V — the k-liveness rung takes half of what l2s left (the proof side:
                    // l2s finds the lassos, this finds the bounds); the exact remainder gets the
                    // rest. Unbudgeted runs give it the members' defaults per width.
                    let kl_budget = remaining(total).map(|r| r / 2);
                    let (v, by, mut n) = kliveness_rung(
                        &named,
                        index,
                        kl_budget,
                        None,
                        crate::adapter::reach_portfolio::ReachMembers::PROOF_DIRECTED,
                    );
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

    /// The #985 tier's shrunk design: `r0` runs 0, 0, 1, 1, … on every path, so the constraint
    /// `r0 == 0` fails at step 2 and no infinite path satisfies the constraints: the justice
    /// set {¬i0, i0} HOLDS. The constraint reads only `r0`, outside the literals' cone; the
    /// exact rung's cone slice used to drop it and report Violated.
    const DISJOINT_DYING_CONSTRAINT: &str = "1 sort bitvec 1\n2 state 1 r0\n3 state 1 r1\n\
        4 input 1 i0\n5 constd 1 0\n6 init 1 2 5\n7 constd 1 0\n8 init 1 3 7\n9 next 1 2 3\n\
        10 constd 1 1\n11 next 1 3 10\n12 not 1 4\n13 not 1 4\n14 not 1 13\n15 constd 1 0\n\
        16 eq 1 2 15\n17 constraint 16\n18 justice 2 12 14\n";

    #[test]
    fn a_constraint_disjoint_from_the_literals_still_kills_every_path() {
        let v = verify_justice(DISJOINT_DYING_CONSTRAINT, JusticeRung::Exact).unwrap();
        assert_eq!(v[0].verdict, PropertyVerdict::Holds, "{:?}", v[0]);
        // Control: without the constraint the toggling input is a fair path.
        let free: String = DISJOINT_DYING_CONSTRAINT
            .lines()
            .filter(|l| !l.contains("constraint"))
            .map(|l| format!("{l}\n"))
            .collect();
        let v = verify_justice(&free, JusticeRung::Exact).unwrap();
        assert_eq!(v[0].verdict, PropertyVerdict::Violated, "{:?}", v[0]);
    }

    /// The `fair` sibling: `r0` goes 0 → 1 and stays, so `fair (r0 == 0)` holds only
    /// finitely often and no path is fair, although {¬i0, i0} toggles freely and `r0` is
    /// outside the literals' cone.
    #[test]
    fn a_fair_line_disjoint_from_the_literals_still_removes_every_fair_path() {
        let d = "1 sort bitvec 1\n2 state 1 r0\n3 zero 1\n4 init 1 2 3\n5 one 1\n6 next 1 2 5\n\
                 7 input 1 i0\n8 not 1 7\n9 not 1 2\n10 fair 9\n11 justice 2 7 8\n";
        let v = verify_justice(d, JusticeRung::Exact).unwrap();
        assert_eq!(v[0].verdict, PropertyVerdict::Holds, "{:?}", v[0]);
    }
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

    /// A justice set the exact rung decides from a 1-bit cone (`a` is a free input's register, so
    /// `GF ¬a` fails on the path that keeps `a` high), beside a DISJOINT 32-bit register squared
    /// every step. The exact rung slices `x` away. The l2s monitor carries every register, so an
    /// exact ROBDD member in l2s's portfolio would build `x · x` and run to its budget.
    const EXACT_FAST_L2S_EXACT_SLOW: &str = "1 sort bitvec 1
2 sort bitvec 32
3 state 1 a
4 constd 1 0
5 init 1 3 4
6 input 1 i
7 next 1 3 6
8 state 2 x
9 input 2 j
10 mul 2 8 8
11 add 2 10 9
12 next 2 8 11
13 not 1 3
14 justice 1 13
";

    /// REPRODUCER (mununu#986) — the parallel policy returns when the first rung wins, not when
    /// the slowest loser's budget runs out. On the parent, l2s's portfolio ran the exact ROBDD
    /// member, which has no cancellation point (mununu#816), so the join waited for it: measured
    /// in `mununu-sva` with 4 CPUs, exact alone answers this design in 0.1 s, and the parallel
    /// policy returned in 24.8 s. On HWMCC'17 LIVE, 23 of the 129 m3d rows were lost and 54 got ≥2× slower.
    /// The same test pins that the winning exact rung keeps its notes (the parent dropped them).
    #[test]
    fn the_parallel_policy_returns_when_the_exact_rung_wins_without_waiting_for_l2s() {
        let _serial = PARALLEL_POLICY.lock().unwrap_or_else(|e| e.into_inner());
        let start = std::time::Instant::now();
        let r = verify_justice_with(
            EXACT_FAST_L2S_EXACT_SLOW,
            &JusticeOptions {
                rung: JusticeRung::Both,
                budget_ms: Some(60_000),
                parallel_rungs: true,
            },
        )
        .expect("runs");
        let elapsed = start.elapsed();
        assert_eq!(r[0].verdict, PropertyVerdict::Violated, "{:?}", r[0]);
        assert!(
            elapsed < std::time::Duration::from_secs(8),
            "the parallel policy took {elapsed:?} for a verdict the exact rung has in ~0.1 s: a \
             cancelled rung held the join ({:?})",
            r[0]
        );
        assert!(
            r[0].notes.iter().any(|n| n.contains("exact cone")),
            "the winning exact rung's notes were dropped: {:?}",
            r[0].notes
        );
    }

    /// REPRODUCER — the justice budget must reach the l2s rung. Before the fix `l2s_rung` took no
    /// budget, so the reachability portfolio ran its members at their 60 s defaults whatever
    /// `--timeout-ms` said: on HWMCC'17 LIVE (2026-11-10, a 3,600 s lane) 42 of 54 `unknown` rows
    /// stopped within 3 min with the lane budget unused. Observable without a subprocess: under a
    /// spent budget the rung must abstain and say so; on the parent it ignores the budget and
    /// decides the counter.
    #[test]
    fn the_l2s_rung_runs_under_the_justice_budget() {
        let r = verify_justice_with(
            COUNTER,
            &JusticeOptions {
                rung: JusticeRung::L2s,
                budget_ms: Some(0),
                parallel_rungs: false,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Unknown, "{:?}", r[0]);
        assert!(
            r[0].notes
                .iter()
                .any(|n| n.contains("within a 0 ms budget")),
            "the note must name the budget the portfolio ran under: {:?}",
            r[0].notes
        );
    }

    #[test]
    fn the_l2s_abstention_note_names_the_default_cap_and_every_member_reason() {
        let outcome = crate::adapter::reach_portfolio::ReachOutcome {
            refused: vec![("pono", "array sort unsupported".to_string())],
            abstained: vec![("native", "resource limit at depth 9".to_string())],
            ..Default::default()
        };
        let note = l2s_undecided_note(None, &outcome);
        assert!(note.contains("60 s per subprocess member"), "{note}");
        assert!(note.contains("--timeout-ms"), "{note}");
        assert!(
            note.contains("pono refused: array sort unsupported"),
            "{note}"
        );
        assert!(
            note.contains("native abstained: resource limit at depth 9"),
            "{note}"
        );
        let bounded = l2s_undecided_note(Some(std::time::Duration::from_secs(5)), &outcome);
        assert!(bounded.contains("within a 5000 ms budget"), "{bounded}");
    }

    /// Track V — the k-liveness rung proves the stuck design: `¬s` holds only at reset, so
    /// exactly one fair round ever completes; `k = 1` is reachable (the rung moves on) and
    /// `k = 3` is not (HOLDS). The test pins the verdict and the label, not the width.
    #[test]
    fn the_kliveness_rung_proves_the_stuck_design_and_names_the_bound() {
        let r = verify_justice_with(
            STUCK,
            &JusticeOptions {
                rung: JusticeRung::Kliveness,
                budget_ms: Some(60_000),
                parallel_rungs: false,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Holds, "{:?}", r[0]);
        assert_eq!(
            r[0].decided_by.first().map(String::as_str),
            Some("kliveness"),
            "{:?}",
            r[0]
        );
        assert!(
            r[0].decided_by.iter().any(|s| s.starts_with("k=")),
            "the bound is named: {:?}",
            r[0].decided_by
        );
    }

    /// The counter has a fair cycle: every bound is reachable, and the rung must say so as an
    /// ABSTENTION that names the widths it tried — never VIOLATED (a finite count is not a
    /// violation). Exact / l2s decide that side.
    #[test]
    fn the_kliveness_rung_never_reports_violated_on_a_fair_cycle() {
        // 10 s: the small bounds are found reachable in well under a second; a deep bound's
        // trace (k = 255 is ~1,000 steps on this counter) is where IC3 stalls and the budget is
        // what ends the rung — the test pins the abstention, not the depth reached.
        let r = verify_justice_with(
            COUNTER,
            &JusticeOptions {
                rung: JusticeRung::Kliveness,
                budget_ms: Some(10_000),
                parallel_rungs: false,
            },
        )
        .expect("runs");
        assert_eq!(r[0].verdict, PropertyVerdict::Unknown, "{:?}", r[0]);
        assert!(
            r[0].notes
                .iter()
                .any(|n| n.contains("k=1 reachable") && n.contains("k=3 reachable")),
            "the note names the bounds tried: {:?}",
            r[0].notes
        );
    }

    /// The sequential default on the counter is unchanged by the new rung: the exact slice
    /// decides VIOLATED first and k-liveness never runs.
    #[test]
    fn the_sequential_policy_still_decides_the_counter_on_the_exact_slice() {
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

    #[test]
    fn the_rung_name_parses_on_both_spellings() {
        assert_eq!(JusticeRung::parse("kliveness"), Ok(JusticeRung::Kliveness));
        assert_eq!(JusticeRung::parse("K-Liveness"), Ok(JusticeRung::Kliveness));
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
