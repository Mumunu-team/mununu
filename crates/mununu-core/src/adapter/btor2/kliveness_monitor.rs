//! Track V — the k-liveness `bad` monitor for a justice set (Claessen–Sörensson 2012).
//!
//! A BTOR2 `justice n l₁ … lₙ` (plus the global `fair` literals) is violated iff an infinite
//! path exists on which every literal holds infinitely often. k-liveness proves the property
//! by bounding how often that can happen: if no reachable path completes more than `k` **rounds**
//! — a round ends once every literal has been seen at least once since the previous round — then
//! some literal occurs finitely often on every path, so no fair path exists and the property
//! HOLDS. The bound is proved as plain `bad`-reachability, which is what IC3 is good at: a small
//! inductive invariant over the design plus a few monitor bits, instead of the full fair-cycle
//! fixpoint (the exact rung) or an invariant that relates two copies of the state (l2s).
//!
//! # The construction (the degeneralised round counter)
//!
//! For literals `l₁ … lₘ` (the justice set's own, then every `fair` literal):
//!
//! - `seen_i` (latch, init 0): `occ_i = seen_i ∨ l_i`; `round = ⋀_i occ_i`;
//!   `next(seen_i) = round ? 0 : occ_i`.
//! - `cnt` (latch of `k_bits`, init 0): `next(cnt) = round ? cnt + 1 : cnt`.
//! - `bad = (cnt == 2^k_bits − 1)`.
//!
//! `bad` unreachable ⇒ no path completes 2^k_bits − 1 rounds ⇒ HOLDS (sound on the concrete
//! design — no abstraction is involved). `bad` reachable says only that this many rounds can
//! happen; it is **not** a violation (a finite count says nothing about an infinite one), so a
//! caller moves to a wider counter. Complete on finite state: a reachable cycle that completes a
//! round gives infinitely many, so `k ≤ |S|` always suffices — the caller's schedule of widths is
//! the only incompleteness, and it says which width it reached.
//!
//! Why rounds and not "all literals at the same step": a justice SET's literals need not hold
//! simultaneously (AIGER 1.9: each infinitely often), so counting steps on which all hold at once
//! would under-count and could "prove" a violated property. The round counter is the standard
//! generalised-Büchi degeneralisation.
//!
//! Text-level, like [`crate::adapter::btor2::l2s_monitor`]: the original lines are kept verbatim
//! except the `justice` / `fair` lines, which are dropped (a safety member must see exactly one
//! `bad` and no justice section); the monitor is appended with fresh NIDs above the current max.
//! The literals are taken from the `justice` line itself, so no output naming is required.
//!
//! # The response shape (Track V, V2)
//!
//! [`emit_response_kliveness_monitor`] reduces `(⋀_j GF fair_j) → AG(a → AF b)` to the same round
//! counter. A violation is a **persistence** — one request outstanding forever — not a recurrence:
//! counting steps on which `pending` holds would never bound a correct design that keeps issuing
//! requests (each granted eventually, `pending` still holds infinitely often). The monitor is the
//! Büchi automaton for `¬φ = F(a ∧ G ¬b)` instead, which guesses the request that is never granted:
//!
//! - `commit` (fresh input), `c` (latch, init 0): `c_eff = c ∨ (commit ∧ a)`; `next(c) = c_eff`.
//! - `failed` (latch, init 0): `fail_now = failed ∨ (c_eff ∧ b)`; `next(failed) = fail_now`.
//! - `acc = c_eff ∧ ¬fail_now` — a committed request with no grant since (a grant in the request's
//!   own step counts: `AF` includes now).
//! - the round counter over `{acc, fair₁ … fairₙ}`.
//!
//! **Sound (bad unreachable ⇒ HOLDS):** `acc` dies for good once `b` follows the commit (`failed`
//! latches), so `acc` holds infinitely often only on a path that commits to a request and never
//! grants it — a violation; with each `fair_j` also infinitely often, that path completes unboundedly
//! many rounds. No path completing `2^w − 1` rounds therefore means no fair violating path. **Never
//! VIOLATED:** a reachable bound is a long-pending request, not a lasso — l2s decides that side.
//! The design's own `justice` / `fair` lines are DROPPED, not added to the set: the property asked is
//! the one with the caller's fairness, and adding the design's would prove a weaker one
//! (the l2s response monitor ignores them too).
//!
//! [`kliveness_ladder`] is the width schedule both rungs share (the justice verb's, and the
//! response verbs' fallback after l2s).

use crate::adapter::btor2::ast::{Btor2File, Nid, Node, Sort};
use crate::adapter::btor2::l2s_monitor::{Emit, emit_atom};
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::CmpOp;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// Name of the counter latch in the emitted text (also the `bad` line's provenance).
pub const COUNTER_SYMBOL: &str = "mununu_kliveness_cnt";

/// Append the k-liveness monitor for the `justice` line at `justice_index` (0-based, in file
/// order) to `content`, with a `k_bits`-wide round counter (`1 ≤ k_bits ≤ 32`). Every `fair`
/// literal joins the set. Returns the augmented BTOR2 text with the `justice` / `fair` lines
/// removed and exactly one `bad` appended.
pub fn emit_kliveness_monitor(
    content: &str,
    justice_index: usize,
    k_bits: u32,
) -> Result<String, AdapterError> {
    check_width(k_bits)?;
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/kliveness_monitor: {}", e.message);
        e
    })?;

    // The literals: the chosen justice line's, then every fair line's.
    let mut justice_lines = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::Justice { .. }));
    let justice = justice_lines
        .nth(justice_index)
        .ok_or_else(|| err(format!("the design has no justice line #{justice_index}")))?;
    let mut lits: Vec<i64> = match &justice.node {
        Node::Justice { signals } => signals.iter().map(|o| o.0).collect(),
        _ => Vec::new(),
    };
    for l in &file.lines {
        if let Node::Fair { signal } = &l.node {
            lits.push(signal.0);
        }
    }
    if lits.is_empty() {
        return Err(err(format!(
            "justice line #{justice_index} has no literals"
        )));
    }
    let mut e = Emit {
        next: file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1,
        lines: Vec::new(),
    };
    let b = bool_sort(&file, &mut e);
    append_round_counter(&mut e, b, &lits, k_bits);
    Ok(assemble(content, &file, e.lines))
}

/// Track V, V2 — the k-liveness monitor for `(⋀_j GF fair_j) → AG(ante → AF cons)` with a
/// `k_bits`-wide round counter: the commit-and-fail automaton for `¬φ` (module docs) and the round
/// counter over `{acc, fair₁ … fairₙ}`. Atoms bind like the l2s response monitor's (state cell →
/// primary input → named output, `reset_pinned` as there). The design's `justice` / `fair` lines are
/// dropped; exactly one `bad` is appended.
pub fn emit_response_kliveness_monitor(
    content: &str,
    ante: (&str, CmpOp, u128),
    cons: (&str, CmpOp, u128),
    fairness_atoms: &[(&str, CmpOp, u128)],
    reset_pinned: bool,
    k_bits: u32,
) -> Result<String, AdapterError> {
    check_width(k_bits)?;
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/kliveness_monitor: {}", e.message);
        e
    })?;
    let mut e = Emit {
        next: file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1,
        lines: Vec::new(),
    };
    let b = bool_sort(&file, &mut e);
    let a = emit_atom(&file, &mut e, b, ante, reset_pinned, "antecedent")?;
    let g = emit_atom(&file, &mut e, b, cons, reset_pinned, "consequent")?;
    let mut fair: Vec<i64> = Vec::with_capacity(fairness_atoms.len());
    for (i, f) in fairness_atoms.iter().enumerate() {
        fair.push(emit_atom(
            &file,
            &mut e,
            b,
            *f,
            reset_pinned,
            &format!("fairness_atom_{i}"),
        )?);
    }
    let zero = e.push(format!("zero {b}"));
    let commit = e.push(format!("input {b} mununu_kliveness_commit"));
    let c = e.push(format!("state {b} mununu_kliveness_committed"));
    e.push(format!("init {b} {c} {zero}"));
    let ca = e.push(format!("and {b} {commit} {a}"));
    let c_eff = e.push(format!("or {b} {c} {ca}"));
    e.push(format!("next {b} {c} {c_eff}"));
    let failed = e.push(format!("state {b} mununu_kliveness_failed"));
    e.push(format!("init {b} {failed} {zero}"));
    let cg = e.push(format!("and {b} {c_eff} {g}"));
    let fail_now = e.push(format!("or {b} {failed} {cg}"));
    e.push(format!("next {b} {failed} {fail_now}"));
    let not_fail = e.push(format!("not {b} {fail_now}"));
    let acc = e.push(format!("and {b} {c_eff} {not_fail}"));
    let mut lits = vec![acc];
    lits.extend(fair);
    append_round_counter(&mut e, b, &lits, k_bits);
    Ok(assemble(content, &file, e.lines))
}

fn check_width(k_bits: u32) -> Result<(), AdapterError> {
    if (1..=32).contains(&k_bits) {
        Ok(())
    } else {
        Err(err(format!(
            "counter width {k_bits} is outside 1..=32 (k = 2^width − 1 rounds)"
        )))
    }
}

/// The 1-bit sort, reused if the design declares one.
fn bool_sort(file: &Btor2File, e: &mut Emit) -> Nid {
    file.lines
        .iter()
        .find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width: 1 },
            } => Some(l.nid),
            _ => None,
        })
        .unwrap_or_else(|| e.push("sort bitvec 1"))
}

/// The degeneralised round counter over `lits` (1-bit operands; a negative one is `not` of its
/// NID) and its single `bad` (module docs).
fn append_round_counter(e: &mut Emit, b: Nid, lits: &[i64], k_bits: u32) {
    let zero1 = e.push(format!("zero {b}"));
    let csort = e.push(format!("sort bitvec {k_bits}"));
    let czero = e.push(format!("zero {csort}"));
    let cones = e.push(format!("ones {csort}"));
    // Literals as positive 1-bit nodes (a negative operand is `not` of its nid).
    let pos: Vec<i64> = lits
        .iter()
        .map(|&l| {
            if l < 0 {
                e.push(format!("not {b} {}", -l))
            } else {
                l
            }
        })
        .collect();
    let seen: Vec<i64> = (0..pos.len())
        .map(|i| e.push(format!("state {b} mununu_kliveness_seen{i}")))
        .collect();
    for &s in &seen {
        e.push(format!("init {b} {s} {zero1}"));
    }
    let occ: Vec<i64> = seen
        .iter()
        .zip(&pos)
        .map(|(&s, &l)| e.push(format!("or {b} {s} {l}")))
        .collect();
    let mut round = occ[0];
    for &o in &occ[1..] {
        round = e.push(format!("and {b} {round} {o}"));
    }
    for (&s, &o) in seen.iter().zip(&occ) {
        let nx = e.push(format!("ite {b} {round} {zero1} {o}"));
        e.push(format!("next {b} {s} {nx}"));
    }
    let cnt = e.push(format!("state {csort} {COUNTER_SYMBOL}"));
    e.push(format!("init {csort} {cnt} {czero}"));
    let inc = e.push(format!("inc {csort} {cnt}"));
    let nx = e.push(format!("ite {csort} {round} {inc} {cnt}"));
    e.push(format!("next {csort} {cnt} {nx}"));
    let bad = e.push(format!("eq {b} {cnt} {cones}"));
    e.push(format!("bad {bad}"));
}

/// Original text minus the `justice` / `fair` lines, then the monitor lines.
fn assemble(content: &str, file: &Btor2File, monitor: Vec<String>) -> String {
    let dropped: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::Justice { .. } | Node::Fair { .. }))
        .map(|l| l.nid)
        .collect();
    let mut text = String::with_capacity(content.len() + monitor.len() * 24);
    for line in content.lines() {
        let nid = line
            .split_whitespace()
            .next()
            .and_then(|t| t.parse::<i64>().ok());
        if nid.is_some_and(|n| dropped.contains(&n)) {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    for l in monitor {
        text.push_str(&l);
        text.push('\n');
    }
    text
}

/// The counter widths both k-liveness rungs try, in order, with their bound labels (k = 2^w − 1
/// rounds). Measured on HWMCC'17 LIVE (V0, 2026-11-10): `cuabq4fro` proved at w = 4 in 1 s,
/// `cutarb8ro` at w = 8 in 14 s; `cujc128ro` was still reachable at w = 8. Small widths first
/// because a proof at a small bound is the common case and a `reachable` at a small width is found
/// in well under a second.
pub const KLIVENESS_WIDTHS: &[(u32, &str)] = &[
    (1, "k=1"),
    (2, "k=3"),
    (4, "k=15"),
    (8, "k=255"),
    (12, "k=4095"),
    (16, "k=65535"),
];

/// What the width ladder concluded.
#[derive(Debug)]
pub enum KlivenessLadder {
    /// Some width's `bad` was proved unreachable: HOLDS at `bound`, by `members`.
    Holds {
        bound: &'static str,
        members: Vec<&'static str>,
    },
    /// No width proved: `tried` says what each attempted width gave (`k=… reachable` /
    /// `k=… undecided`), `member_notes` every refusal / abstention.
    Undecided {
        tried: Vec<String>,
        member_notes: Vec<String>,
    },
    /// The monitor could not be built or parsed, or the members contradicted each other.
    Failed(String),
}

/// The k-liveness width ladder: for each width in [`KLIVENESS_WIDTHS`], the monitor `monitor(w)`
/// is decided as `bad`-reachability by the proof-directed members
/// ([`ReachMembers::PROOF_DIRECTED`](crate::adapter::reach_portfolio::ReachMembers)). `Unreachable`
/// ⇒ [`KlivenessLadder::Holds`]; `Reachable` moves to the next width (a bound too small is not a
/// violation); the first width no member decides ENDS the ladder (a wider bound is strictly harder
/// to prove and its trace strictly deeper), as does an expired
/// [`run_budget`](crate::adapter::run_budget) or `cancel`.
///
/// engine: proof-directed reach portfolio (rIC3 in its own portfolio mode, Pono `ic3bits`, native
/// k-induction) on the round-counter monitor, each member under `btormc::DEFAULT_TIMEOUT` or the
/// time left in the current run budget — role: decider on proofs.
pub fn kliveness_ladder(
    monitor: &dyn Fn(u32) -> Result<String, AdapterError>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> KlivenessLadder {
    use crate::adapter::reach_portfolio::{
        ReachMembers, ReachVerdict, decide_reach_portfolio_with_members,
    };
    use crate::adapter::run_budget;
    let mut tried: Vec<String> = Vec::new();
    let mut member_notes: Vec<String> = Vec::new();
    for &(w, label) in KLIVENESS_WIDTHS {
        if run_budget::expired()
            || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            break;
        }
        let text = match monitor(w) {
            Ok(m) => m,
            Err(e) => {
                return KlivenessLadder::Failed(format!(
                    "kliveness: the round-counter monitor could not be built: {}",
                    e.message
                ));
            }
        };
        let file = match parser::parse(&text) {
            Ok(f) => f,
            Err(e) => {
                return KlivenessLadder::Failed(format!(
                    "kliveness: the monitor did not parse: {}",
                    e.message
                ));
            }
        };
        let outcome = decide_reach_portfolio_with_members(
            &file,
            crate::adapter::btormc::DEFAULT_TIMEOUT,
            cancel,
            ReachMembers::PROOF_DIRECTED,
        );
        for (m, why) in outcome
            .refused
            .iter()
            .chain(outcome.abstained.iter())
            .chain(outcome.undecided.iter())
        {
            member_notes.push(format!("{m} at {label}: {why}"));
        }
        match outcome.verdict {
            ReachVerdict::Unreachable => {
                return KlivenessLadder::Holds {
                    bound: label,
                    members: outcome.unreachable_by,
                };
            }
            ReachVerdict::Reachable => tried.push(format!("{label} reachable")),
            ReachVerdict::Contradiction => {
                return KlivenessLadder::Failed(format!(
                    "kliveness: the members CONTRADICT each other on the {label} bound (soundness alarm)"
                ));
            }
            ReachVerdict::Unknown => {
                tried.push(format!("{label} undecided"));
                break;
            }
        }
    }
    KlivenessLadder::Undecided {
        tried,
        member_notes,
    }
}

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("adapter/btor2/kliveness_monitor: {message}"),
        location: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast::simulate_one_step;

    /// A free-running 2-bit counter with justice on `lo ∧ hi` — a fair cycle exists (VIOLATED).
    const COUNTER: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                           6 init 1 4 2\n7 init 1 5 2\n8 not 1 4\n9 next 1 4 8\n10 xor 1 4 5\n\
                           11 next 1 5 10\n12 and 1 4 5\n13 justice 1 12\n";
    /// Justice as a SET: `lo` and `hi` each infinitely often (they never hold together here —
    /// `hi` is `lo`'s complement after reset — so a same-step conjunction would never fire).
    const SET: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                       6 init 1 4 2\n7 init 1 5 3\n8 not 1 4\n9 next 1 4 8\n10 not 1 5\n\
                       11 next 1 5 10\n12 justice 2 4 5\n";

    fn parse(t: &str) -> crate::adapter::btor2::ast::Btor2File {
        parser::parse(t).expect("monitor parses")
    }

    /// Simulate `steps` steps from the given register values (the monitor's latches start at 0,
    /// as their `init` lines say) and return the round counter.
    fn cnt_after(text: &str, init: &[(&str, u128)], steps: usize) -> u128 {
        let file = parse(text);
        let mut state: std::collections::HashMap<String, u128> =
            init.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for l in &file.lines {
            if let Node::State {
                symbol: Some(s), ..
            } = &l.node
                && s.starts_with("mununu_kliveness_")
            {
                state.insert(s.clone(), 0);
            }
        }
        let inputs = std::collections::HashMap::new();
        for _ in 0..steps {
            state = simulate_one_step(&file, &state, &inputs).expect("simulates");
        }
        state.get(COUNTER_SYMBOL).copied().unwrap_or(0)
    }

    #[test]
    fn the_monitor_drops_the_justice_lines_and_adds_exactly_one_bad() {
        let m = emit_kliveness_monitor(COUNTER, 0, 2).expect("emits");
        assert!(!m.contains(" justice "), "{m}");
        assert_eq!(m.matches(" bad ").count(), 1, "{m}");
        assert!(m.contains(COUNTER_SYMBOL), "{m}");
        parse(&m);
    }

    /// On the counter `lo ∧ hi` holds every 4th step (3, 7, 11, …): after 12 steps 3 rounds.
    #[test]
    fn the_counter_counts_completed_rounds_of_the_fair_event() {
        let m = emit_kliveness_monitor(COUNTER, 0, 4).expect("emits");
        let init = [("lo", 0), ("hi", 0)];
        assert_eq!(cnt_after(&m, &init, 4), 1);
        assert_eq!(cnt_after(&m, &init, 12), 3);
    }

    /// A SET's literals occur on different steps; a round completes once both have been seen.
    /// `lo` = 0,1,0,1,…, `hi` = 1,0,1,0,…: step 0 sees `hi`, step 1 sees `lo` → round 1 ends at
    /// step 1, and every two steps after.
    #[test]
    fn a_round_needs_every_literal_not_all_at_once() {
        let m = emit_kliveness_monitor(SET, 0, 4).expect("emits");
        let init = [("lo", 0), ("hi", 1)];
        assert_eq!(cnt_after(&m, &init, 2), 1);
        assert_eq!(cnt_after(&m, &init, 6), 3);
    }

    // ---- V2: the response monitor --------------------------------------------------------

    /// `st` idle 0 / busy 1; `req` (input) moves idle → busy; busy → idle unconditionally, and
    /// `st == 1` is the grant. Every request is granted the next cycle, and requests may come
    /// forever: `AG(req → AF st == 1)` HOLDS — the design on which counting `pending` steps would
    /// never bound (pending recurs), and the commit monitor must.
    const RESPONDER: &str = "1 sort bitvec 1\n2 state 1 st\n3 zero 1\n4 init 1 2 3\n5 input 1 req\n\
                             6 one 1\n7 ite 1 2 3 5\n8 next 1 2 7\n";
    /// `st` latches the first request forever; `done` is never set: `AG(req → AF done == 1)` is
    /// VIOLATED.
    const STALLER: &str = "1 sort bitvec 1\n2 state 1 st\n3 zero 1\n4 init 1 2 3\n5 input 1 req\n\
                           6 or 1 2 5\n7 next 1 2 6\n8 state 1 done\n9 init 1 8 3\n10 next 1 8 3\n";
    /// A busy phase that ends only on the input `go`: `next(st) = st ? ¬go : req`,
    /// `next(done) = st ∧ go`. `AG(req → AF done == 1)` is VIOLATED (stay busy with `go = 0`) and
    /// HOLDS under `GF go == 1` — the fairness-gated shape. A `justice` line rides along to check it
    /// is dropped, not counted.
    const GATED: &str = "1 sort bitvec 1\n2 state 1 st\n3 zero 1\n4 init 1 2 3\n5 input 1 req\n\
                         6 input 1 go\n7 not 1 6\n8 ite 1 2 7 5\n9 next 1 2 8\n10 state 1 done\n\
                         11 init 1 10 3\n12 and 1 2 6\n13 next 1 10 12\n14 justice 1 5\n";

    const REQ: (&str, CmpOp, u128) = ("req", CmpOp::Eq, 1);
    const DONE: (&str, CmpOp, u128) = ("done", CmpOp::Eq, 1);
    const GO: (&str, CmpOp, u128) = ("go", CmpOp::Eq, 1);

    /// The solver-free oracle: the largest round count ANY path of the monitor reaches —
    /// exhaustive reachability over every named state, every input valuation each step, via the
    /// BTOR2 interpreter (the monitor is finite-state, so the walk terminates). Max `< 2^w − 1` means the ladder's query at that width is UNREACHABLE (a proof);
    /// max `= 2^w − 1` means it is reachable (no proof at that width).
    fn max_rounds(text: &str) -> u128 {
        let file = parse(text);
        let inputs: Vec<String> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Input {
                    symbol: Some(s), ..
                } => Some(s.clone()),
                _ => None,
            })
            .collect();
        let init: std::collections::BTreeMap<String, u128> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::State {
                    symbol: Some(s), ..
                } => Some((s.clone(), 0)),
                _ => None,
            })
            .collect();
        let mut seen = std::collections::BTreeSet::from([init.clone()]);
        let mut frontier = vec![init];
        let mut best = 0;
        while let Some(s) = frontier.pop() {
            best = best.max(s.get(COUNTER_SYMBOL).copied().unwrap_or(0));
            let state: std::collections::HashMap<String, u128> =
                s.iter().map(|(k, v)| (k.clone(), *v)).collect();
            for bits in 0..(1u32 << inputs.len()) {
                let inp: std::collections::HashMap<String, u128> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.clone(), u128::from((bits >> i) & 1)))
                    .collect();
                let nx = simulate_one_step(&file, &state, &inp).expect("simulates");
                let nx: std::collections::BTreeMap<String, u128> = nx.into_iter().collect();
                if seen.insert(nx.clone()) {
                    frontier.push(nx);
                }
            }
        }
        best
    }

    #[test]
    fn the_response_monitor_drops_the_designs_justice_and_adds_exactly_one_bad() {
        let m = emit_response_kliveness_monitor(GATED, REQ, DONE, &[GO], false, 4).expect("emits");
        assert!(!m.contains(" justice "), "{m}");
        assert_eq!(m.matches(" bad ").count(), 1, "{m}");
        assert!(m.contains("mununu_kliveness_commit"), "{m}");
        parse(&m);
    }

    /// The case the first V2 draft got wrong: requests forever, each granted. A committed
    /// request dies on its grant, so no path completes a second round — bounded at every width.
    #[test]
    fn a_design_that_grants_every_request_completes_at_most_one_round() {
        let m =
            emit_response_kliveness_monitor(RESPONDER, REQ, ("st", CmpOp::Eq, 1), &[], false, 4)
                .expect("emits");
        assert_eq!(max_rounds(&m), 1);
    }

    /// A never-granted request: the committed path stays accepting forever, so every width's
    /// bound is reachable — the ladder can never prove the violated property.
    #[test]
    fn a_never_granted_request_reaches_every_bound() {
        let m = emit_response_kliveness_monitor(STALLER, REQ, DONE, &[], false, 4).expect("emits");
        assert_eq!(max_rounds(&m), 15);
    }

    /// Fairness decides: without `GF go` the busy phase may last forever (every bound reachable);
    /// with it the count is bounded at TWO — a `go` seen before the commit completes round 1 at the
    /// commit step, and the busy phase's `go` completes round 2 (the grant `done` lands a step
    /// later, so `acc` still holds then) — and the grant then kills the commitment.
    #[test]
    fn the_fairness_atom_is_what_bounds_the_gated_design() {
        let unfair =
            emit_response_kliveness_monitor(GATED, REQ, DONE, &[], false, 4).expect("emits");
        assert_eq!(max_rounds(&unfair), 15, "violated without fairness");
        let fair =
            emit_response_kliveness_monitor(GATED, REQ, DONE, &[GO], false, 4).expect("emits");
        assert_eq!(max_rounds(&fair), 2, "holds under GF go");
    }

    /// A grant in the request's own step counts (`AF` includes now): `AG(req → AF req)` holds
    /// trivially, and no round ever completes.
    #[test]
    fn a_grant_in_the_request_step_kills_the_commitment() {
        let m = emit_response_kliveness_monitor(RESPONDER, REQ, REQ, &[], false, 4).expect("emits");
        assert_eq!(max_rounds(&m), 0);
    }

    #[test]
    fn an_unbound_atom_or_a_bad_width_is_a_response_monitor_error() {
        assert!(
            emit_response_kliveness_monitor(RESPONDER, ("nope", CmpOp::Eq, 1), REQ, &[], false, 4)
                .is_err()
        );
        assert!(emit_response_kliveness_monitor(RESPONDER, REQ, REQ, &[], false, 0).is_err());
    }

    /// The ladder on a violated property never proves it, whatever members the environment
    /// carries (the soundness half — the proof half is measured on the corpus, where rIC3 runs).
    #[test]
    fn the_ladder_never_proves_a_never_granted_request() {
        use crate::adapter::run_budget;
        let b = run_budget::Budget::child(&run_budget::current(), Some(10_000));
        let _g = run_budget::enter(&b);
        let r = kliveness_ladder(
            &|w| emit_response_kliveness_monitor(STALLER, REQ, DONE, &[], false, w),
            None,
        );
        assert!(!matches!(r, KlivenessLadder::Holds { .. }), "{r:?}");
    }

    #[test]
    fn a_missing_justice_line_or_a_bad_width_is_an_error() {
        assert!(emit_kliveness_monitor(COUNTER, 1, 4).is_err());
        assert!(emit_kliveness_monitor(COUNTER, 0, 0).is_err());
        assert!(emit_kliveness_monitor(COUNTER, 0, 33).is_err());
    }
}
