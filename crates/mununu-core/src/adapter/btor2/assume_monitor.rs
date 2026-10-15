//! `assume` statements as preconditions — the `mununu_assume_ok` monitor (OpenTitan coverage,
//! Phase 4).
//!
//! > Source of truth: [`emit_assume_ok_monitor`] + [`assume_shape`] — surface: (CLI+API), through
//! > `sv verify-auto --assumes precondition` (the default).
//!
//! An SVA `assume property (P)` restricts the traces the guarantees are checked on: a guarantee
//! must hold on every trace along which every assumption has held so far. This module turns the
//! assumptions into ONE 1-bit signal, [`ASSUME_OK_SYMBOL`], true at cycle `t` iff every applied
//! assumption held at every cycle `≤ t`:
//!
//! ```text
//! ok_t = prev_ok_t ∧ ⋀ᵢ hold_i,t        prev_ok_0 = 1,  prev_ok_{t+1} = ok_t
//! ```
//!
//! `ok` is MONOTONE along every path (once an assumption breaks it stays false), which is what
//! lets the guarantee rewrite (`adapter::slang::assume_guarantee::relativize`) guard each boolean
//! block of a guarantee with `!ok ||` instead of rewriting its modal structure.
//!
//! **Shapes applied** ([`assume_shape`]): `AG B` (`assume property (B)`) and `AG (A → AX C)`
//! (`A |=> C`), each with an optional kept `disable iff (D)`. Every other shape is not applied and
//! is reported — the guarantees are then checked without it, which is the stronger obligation.
//!
//! **Every lossy step weakens an assumption, never strengthens it** — an assumption that is too
//! strong prunes real traces and turns a VIOLATED into a false HOLDS:
//!
//! - **Warm-up, per assumption and per part.** A `$past` read before its shadow fills reads the
//!   shadow's chosen pre-fill (zero), which SV leaves undefined; enforcing an assumption over it
//!   would prune traces on an invented value. So an invariant holds trivially before its body's
//!   deepest `$past` (`hold_after`), and an `|=>` obligation is armed at `t` only once every part
//!   it reads is filled: `t ≥ max(antecedent depth, disable depth, consequent depth − 1)`
//!   (`arm_after`; the consequent runs at `t + 1`). A uniform warm-up would also be sound but
//!   weaker: an antecedent at cycle 0 that reads no history would never arm, and a guarantee that
//!   needs the assumption at cycle 0 would come out spuriously VIOLATED (measured on OpenTitan's
//!   arbiter, 2026-10-15).
//! - **A kept `disable iff (D)`** disables the attempt at its start (`A ∧ ¬D` arms the latch) and
//!   ALSO when `D` holds at the consequent's cycle — a reset rising mid-attempt disables it in SV.
//! - **A leaf the compiler cannot resolve** drops that assumption (reported as unapplied).

use crate::adapter::btor2::ast::Nid;
use crate::adapter::btor2::bad_monitor::compile_boolean_subtree;
use crate::adapter::btor2::parser;
use crate::adapter::{AdapterError, AdapterErrorKind};
use crate::mu_calculus::{Formula, Guard, ModalKind, Node, NodeId};

/// The 1-bit signal "every applied assumption has held so far".
pub const ASSUME_OK_SYMBOL: &str = "mununu_assume_ok";

/// An assumption to apply: its shape and the cycles before which each part reads unfilled history
/// (see the module doc). `0` means "from cycle 0".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssumeSpec {
    pub shape: AssumeShape,
    /// An invariant holds trivially before this cycle.
    pub hold_after: u32,
    /// An `|=>` obligation is armed only at cycles `≥` this.
    pub arm_after: u32,
}

/// The shape of an applicable assumption, as node ids into its own [`Formula`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssumeShape {
    /// `AG B`, `B` boolean (a kept `disable iff` is already `D ∨ B`).
    Invariant { body: NodeId },
    /// `AG (A → AX C)` — `neg_ante` is the `¬A` disjunct as written; `disable` a kept
    /// `disable iff (D)`.
    ImpliesNext {
        disable: Option<NodeId>,
        neg_ante: NodeId,
        cons: NodeId,
    },
}

fn is_boolean(f: &Formula, id: NodeId) -> bool {
    match f.node(id) {
        Node::True | Node::False | Node::Predicate(_) => true,
        Node::Not(a) => is_boolean(f, *a),
        Node::And(a, b) | Node::Or(a, b) => is_boolean(f, *a) && is_boolean(f, *b),
        _ => false,
    }
}

/// The boolean target of an unguarded box, if `id` is one.
fn box_of_boolean(f: &Formula, id: NodeId) -> Option<NodeId> {
    match f.node(id) {
        Node::Modal {
            kind: ModalKind::Box,
            guard,
            target,
        } if *guard == Guard::default() && is_boolean(f, *target) => Some(*target),
        _ => None,
    }
}

/// `¬A ∨ [] C` in either order: `(neg_ante, cons)`.
fn implication(f: &Formula, id: NodeId) -> Option<(NodeId, NodeId)> {
    let Node::Or(x, y) = f.node(id) else {
        return None;
    };
    match (box_of_boolean(f, *x), box_of_boolean(f, *y)) {
        (None, Some(c)) if is_boolean(f, *x) => Some((*x, c)),
        (Some(c), None) if is_boolean(f, *y) => Some((*y, c)),
        _ => None,
    }
}

/// Classify an assumption's translated formula (`nu X. (BODY && [] X)`). `None` for a shape the
/// monitor does not apply (bounded sequences, liveness, anything with a nested modality).
pub fn assume_shape(f: &Formula) -> Option<AssumeShape> {
    let Node::Nu { var, body } = f.node(f.root()) else {
        return None;
    };
    let Node::And(l, r) = f.node(*body) else {
        return None;
    };
    let is_recur = |id: NodeId| {
        matches!(f.node(id), Node::Modal { kind: ModalKind::Box, guard, target }
            if *guard == Guard::default()
                && matches!(f.node(*target), Node::Variable(v) if v == var))
    };
    let block = match (is_recur(*l), is_recur(*r)) {
        (false, true) => *l,
        (true, false) => *r,
        _ => return None,
    };
    if is_boolean(f, block) {
        return Some(AssumeShape::Invariant { body: block });
    }
    if let Some((neg_ante, cons)) = implication(f, block) {
        return Some(AssumeShape::ImpliesNext {
            disable: None,
            neg_ante,
            cons,
        });
    }
    // A kept `disable iff (D)`: the translator emits `(D || (¬A || [] C))`.
    if let Node::Or(d, inner) = f.node(block)
        && is_boolean(f, *d)
        && let Some((neg_ante, cons)) = implication(f, *inner)
    {
        return Some(AssumeShape::ImpliesNext {
            disable: Some(*d),
            neg_ante,
            cons,
        });
    }
    None
}

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message,
        location: None,
    }
}

/// Append the `mununu_assume_ok` monitor for `assumes` to the BTOR2 `content`.
///
/// Returns the augmented model and, per assumption, `None` if it was applied or `Some(reason)` if
/// its leaves did not compile (it is then left out of the conjunction — weaker, sound). With no
/// assumption applied, `ok` is still emitted (constantly true after the latch), so a relativized
/// guarantee always binds.
pub fn emit_assume_ok_monitor(
    content: &str,
    assumes: &[(&Formula, AssumeSpec)],
    reset_pinned: bool,
) -> Result<(String, Vec<Option<String>>), AdapterError> {
    let warmup = assumes
        .iter()
        .map(|(_, s)| s.hold_after.max(s.arm_after))
        .max()
        .unwrap_or(0);
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/assume_monitor: {}", e.message);
        e
    })?;
    if file.lines.iter().any(|l| {
        matches!(&l.node, crate::adapter::btor2::ast::Node::Op { symbol: Some(s), .. }
            if s == ASSUME_OK_SYMBOL)
    }) {
        return Err(err(format!(
            "adapter/btor2/assume_monitor: the model already names `{ASSUME_OK_SYMBOL}`"
        )));
    }
    let mut next: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut out: Vec<String> = Vec::new();
    let mut fresh = |out: &mut Vec<String>, line: String| -> Nid {
        let n = next;
        next += 1;
        out.push(format!("{n} {line}"));
        n
    };
    let b = fresh(&mut out, "sort bitvec 1".to_string());
    let one = fresh(&mut out, format!("one {b}"));
    let zero = fresh(&mut out, format!("zero {b}"));

    // A cycle counter saturating at `warmup`; `after[k]` is true from cycle `k` on.
    let mut after: std::collections::HashMap<u32, Nid> = std::collections::HashMap::new();
    after.insert(0, one);
    if warmup > 0 {
        let w = 32 - warmup.leading_zeros();
        let s = fresh(&mut out, format!("sort bitvec {w}"));
        let c0 = fresh(&mut out, format!("zero {s}"));
        let c1 = fresh(&mut out, format!("one {s}"));
        let cmax = fresh(&mut out, format!("constd {s} {warmup}"));
        let cnt = fresh(&mut out, format!("state {s} mununu_assume_warmup"));
        fresh(&mut out, format!("init {s} {cnt} {c0}"));
        let full = fresh(&mut out, format!("eq {b} {cnt} {cmax}"));
        let inc = fresh(&mut out, format!("add {s} {cnt} {c1}"));
        let nxt = fresh(&mut out, format!("ite {s} {full} {cnt} {inc}"));
        fresh(&mut out, format!("next {s} {cnt} {nxt}"));
        let mut ks: Vec<u32> = assumes
            .iter()
            .flat_map(|(_, sp)| [sp.hold_after, sp.arm_after])
            .filter(|k| *k > 0)
            .collect();
        ks.sort_unstable();
        ks.dedup();
        for k in ks {
            let ck = fresh(&mut out, format!("constd {s} {k}"));
            let ge = fresh(&mut out, format!("ugte {b} {cnt} {ck}"));
            after.insert(k, ge);
        }
    }

    let mut holds: Vec<Nid> = Vec::new();
    let mut outcome: Vec<Option<String>> = Vec::new();
    for (i, (f, spec)) in assumes.iter().enumerate() {
        let shape = &spec.shape;
        let arm_warm = after[&spec.arm_after];
        let hold_warm = after[&spec.hold_after];
        // Compile into a scratch buffer; roll back on a leaf that does not resolve.
        let (save_next, save_len) = (next, out.len());
        let compile = |id: NodeId, next: &mut Nid, out: &mut Vec<String>| {
            compile_boolean_subtree(f, id, &file, reset_pinned, b, next, out)
        };
        let built: Result<Nid, AdapterError> = (|| {
            let hold = match *shape {
                AssumeShape::Invariant { body } => compile(body, &mut next, &mut out)?,
                AssumeShape::ImpliesNext {
                    disable,
                    neg_ante,
                    cons,
                } => {
                    let na = compile(neg_ante, &mut next, &mut out)?;
                    let mut arm = {
                        let n = next;
                        next += 1;
                        out.push(format!("{n} not {b} {na}"));
                        n
                    };
                    let disabled = match disable {
                        Some(d) => Some(compile(d, &mut next, &mut out)?),
                        None => None,
                    };
                    if let Some(d) = disabled {
                        let nd = next;
                        next += 1;
                        out.push(format!("{nd} not {b} {d}"));
                        let a = next;
                        next += 1;
                        out.push(format!("{a} and {b} {arm} {nd}"));
                        arm = a;
                    }
                    let armed = next;
                    next += 1;
                    out.push(format!("{armed} and {b} {arm} {arm_warm}"));
                    let prev = next;
                    next += 1;
                    out.push(format!("{prev} state {b} mununu_assume_ante_prev_{i}"));
                    let init = next;
                    next += 1;
                    out.push(format!("{init} init {b} {prev} {zero}"));
                    let nx = next;
                    next += 1;
                    out.push(format!("{nx} next {b} {prev} {armed}"));
                    let c = compile(cons, &mut next, &mut out)?;
                    let not_prev = next;
                    next += 1;
                    out.push(format!("{not_prev} not {b} {prev}"));
                    let mut h = next;
                    next += 1;
                    out.push(format!("{h} or {b} {not_prev} {c}"));
                    if let Some(d) = disabled {
                        // Disabled mid-attempt: `D` at the consequent's cycle voids it too.
                        let h2 = next;
                        next += 1;
                        out.push(format!("{h2} or {b} {h} {d}"));
                        h = h2;
                    }
                    h
                }
            };
            // An invariant holds trivially before its history fills (an `|=>` hold is already
            // gated by its armed latch, so `hold_after` is 0 for it).
            let cold = next;
            next += 1;
            out.push(format!("{cold} not {b} {hold_warm}"));
            let warmed = next;
            next += 1;
            out.push(format!("{warmed} or {b} {hold} {cold}"));
            Ok(warmed)
        })();
        match built {
            Ok(h) => {
                holds.push(h);
                outcome.push(None);
            }
            Err(e) => {
                next = save_next;
                out.truncate(save_len);
                outcome.push(Some(e.message));
            }
        }
    }
    let mut all = one;
    for h in holds {
        let n = next;
        next += 1;
        out.push(format!("{n} and {b} {all} {h}"));
        all = n;
    }
    let prev_ok = next;
    next += 1;
    out.push(format!("{prev_ok} state {b} mununu_assume_prev_ok"));
    out.push(format!("{} init {b} {prev_ok} {one}", next));
    next += 1;
    let ok = next;
    next += 1;
    out.push(format!("{ok} and {b} {prev_ok} {all} {ASSUME_OK_SYMBOL}"));
    out.push(format!("{} next {b} {prev_ok} {ok}", next));
    Ok((
        format!("{}\n{}\n", content.trim_end(), out.join("\n")),
        outcome,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast;
    use std::collections::HashMap;

    /// Step the model from `regs` with `inputs`; the next registers and the observed `ok`.
    fn step(
        model: &str,
        regs: &HashMap<String, u128>,
        inputs: &HashMap<String, u128>,
    ) -> (HashMap<String, u128>, u128) {
        let f = parser::parse(model).expect("parses");
        let o =
            bit_blast::simulate_one_step_observe(&f, regs, inputs, &[ASSUME_OK_SYMBOL.to_string()])
                .expect("steps");
        (o.next_state, o.observed[ASSUME_OK_SYMBOL])
    }

    /// One free input `req` and one free input `gnt`; no state of its own.
    const IO: &str = "1 sort bitvec 1\n2 input 1 req\n3 input 1 gnt\n";

    fn monitor(assume: &str, warmup: u32) -> (String, Vec<Option<String>>) {
        let f = crate::mu_calculus::parser::parse(assume).expect("formula");
        let shape = assume_shape(&f).expect("applicable shape");
        let invariant = matches!(shape, AssumeShape::Invariant { .. });
        let spec = AssumeSpec {
            shape,
            hold_after: if invariant { warmup } else { 0 },
            arm_after: warmup,
        };
        emit_assume_ok_monitor(IO, &[(&f, spec)], false).expect("emits")
    }

    #[test]
    fn shapes_are_classified_and_others_refused() {
        let shape = |s: &str| assume_shape(&crate::mu_calculus::parser::parse(s).unwrap());
        assert!(matches!(
            shape("nu X. ((req || gnt) && [] X)"),
            Some(AssumeShape::Invariant { .. })
        ));
        assert!(matches!(
            shape("nu X. (((!(req)) || [] req) && [] X)"),
            Some(AssumeShape::ImpliesNext { disable: None, .. })
        ));
        assert!(matches!(
            shape("nu X. ((gnt || ((!(req)) || [] req)) && [] X)"),
            Some(AssumeShape::ImpliesNext {
                disable: Some(_),
                ..
            })
        ));
        // A nested box (`req |=> ##1 gnt`) and a liveness body are not applied.
        assert_eq!(shape("nu X. (((!(req)) || [] [] gnt) && [] X)"), None);
        assert_eq!(shape("nu X. ((mu Y. (gnt || [] Y)) && [] X)"), None);
    }

    #[test]
    fn ok_is_the_running_conjunction_and_stays_false_once_broken() {
        // `assume property (req)`.
        let (m, outcome) = monitor("nu X. (req && [] X)", 0);
        assert_eq!(outcome, vec![None]);
        let mut regs: HashMap<String, u128> =
            HashMap::from([("mununu_assume_prev_ok".to_string(), 1)]);
        let inp = |req: u128| HashMap::from([("req".to_string(), req), ("gnt".to_string(), 0)]);
        let (r1, ok0) = step(&m, &regs, &inp(1));
        assert_eq!(ok0, 1);
        regs = r1;
        let (r2, ok1) = step(&m, &regs, &inp(0));
        assert_eq!(ok1, 0, "req dropped: the assumption broke");
        regs = r2;
        let (_, ok2) = step(&m, &regs, &inp(1));
        assert_eq!(ok2, 0, "monotone: never recovers");
    }

    #[test]
    fn an_implies_next_assumption_arms_after_warmup_and_obeys_disable() {
        // `assume property (disable iff (gnt) req |=> req)` — requests stay high.
        let (m, _) = monitor("nu X. ((gnt || ((!(req)) || [] req)) && [] X)", 1);
        let inp = |req: u128, gnt: u128| {
            HashMap::from([("req".to_string(), req), ("gnt".to_string(), gnt)])
        };
        let init: HashMap<String, u128> = HashMap::from([
            ("mununu_assume_prev_ok".to_string(), 1),
            ("mununu_assume_warmup".to_string(), 0),
            ("mununu_assume_ante_prev_0".to_string(), 0),
        ]);
        // Cycle 0 is warm-up: req at cycle 0 arms nothing, so dropping it at cycle 1 is allowed.
        let (r, ok) = step(&m, &init, &inp(1, 0));
        assert_eq!(ok, 1);
        let (r, ok) = step(&m, &r, &inp(0, 0));
        assert_eq!(ok, 1, "warm-up: the cycle-0 request was not an obligation");
        // Warm now: req at cycle 2 obliges req at cycle 3.
        let (r, ok) = step(&m, &r, &inp(1, 0));
        assert_eq!(ok, 1);
        let (r_drop, ok) = step(&m, &r, &inp(0, 0));
        assert_eq!(ok, 0, "the request dropped");
        let _ = r_drop;
        // A disable at the consequent's cycle voids the obligation instead.
        let (_, ok) = step(&m, &r, &inp(0, 1));
        assert_eq!(ok, 1, "disabled mid-attempt");
    }

    #[test]
    fn an_assumption_whose_leaf_does_not_resolve_is_left_out_and_reported() {
        let f = crate::mu_calculus::parser::parse("nu X. (ghost && [] X)").unwrap();
        let shape = assume_shape(&f).unwrap();
        let spec = AssumeSpec {
            shape,
            hold_after: 0,
            arm_after: 0,
        };
        let (m, outcome) = emit_assume_ok_monitor(IO, &[(&f, spec)], false).unwrap();
        assert!(
            outcome[0].as_deref().is_some_and(|r| r.contains("ghost")),
            "{outcome:?}"
        );
        // `ok` still exists, and with no assumption applied it is constantly true.
        let regs = HashMap::from([("mununu_assume_prev_ok".to_string(), 1)]);
        let (_, ok) = step(
            &m,
            &regs,
            &HashMap::from([("req".to_string(), 0), ("gnt".to_string(), 0)]),
        );
        assert_eq!(ok, 1);
    }
}
