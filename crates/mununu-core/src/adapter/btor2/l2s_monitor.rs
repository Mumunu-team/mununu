//! P2 — liveness-to-safety `bad` monitor for the response property
//! `AG(a → AF b)` (Biere–Artho–Schuppan).
//!
//! The classifier [`crate::adapter::liveness_rescue::reduce_response_af`] recognises
//! the property; this module emits the `bad` line that makes its violation — a
//! **reachable `b`-free cycle carrying an outstanding request** — a reachability
//! query the portfolio decides. Like [`crate::adapter::btor2::bad_monitor`] and
//! [`crate::adapter::btor2::shadow`], it works at the BTOR2 **text** level: it
//! appends lines with fresh NIDs above the current max, so it composes with the
//! bit-blaster / lift / model checker that re-parse the text.
//!
//! # The construction
//!
//! Let `a`, `b` be the request / grant atoms. The augmentation adds:
//!
//! - `pending` (latch, init 0): `next = (pending ∨ a) ∧ ¬b` — an outstanding
//!   request, set by `a`, cleared by `b`.
//! - `save` (fresh 1-bit input) and `saved` (latch, init 0): a nondeterministic,
//!   at-most-once snapshot trigger — `save_en = save ∧ ¬saved ∧ pending` (only
//!   snapshot when a request is outstanding); `next(saved) = saved ∨ save_en`.
//! - a **shadow** copy `sh_i` of every original state cell `s_i`:
//!   `next(sh_i) = save_en ? s_i : sh_i` (no init — free, only read after a save).
//! - `looped = saved ∧ ⋀_i (s_i == sh_i)` — the snapshotted state has recurred, so
//!   a cycle closed.
//! - `b_seen` (latch, init 0): whether `b` held anywhere on the loop —
//!   `next = save_en ? b : (saved ? (b_seen ∨ b) : 0)`.
//! - `bad = looped ∧ ¬b_seen` (the `and` carries [`bad_monitor::BAD_COND_SYMBOL`]).
//!
//! A reachable `bad` ⇒ a reachable cycle on which an outstanding request is never
//! granted ⇒ `AG(a → AF b)` **VIOLATED**; an unreachable `bad` (a portfolio safety
//! proof) ⇒ **HOLDS**. The construction is **sound** (a `bad` witness is a genuine
//! `b`-free lasso through a pending request) and **complete** (a real violation has
//! a suffix cycle on which `pending` stays 1 and `b` never holds, which the
//! nondeterministic `save` can snapshot).
//!
//! # Fair-cycle extension (mununu#477 Option B)
//!
//! [`emit_response_l2s_monitor_under_fairness`] is the sibling that decides
//! `(⋀_i GF fair_i) → AG(a → AF b)` — response-liveness UNDER a conjunctive
//! justice assumption on primary-input (or combinational) atoms. It adds one
//! `fair_i_seen` latch per fairness constraint (each mirroring `b_seen`'s
//! definition exactly, with `fair_i` in `b`'s slot: was `fair_i` observed
//! anywhere on the snapshotted cycle?) and conjuncts them into `bad`:
//!
//! ```text
//! bad = looped ∧ ¬b_seen ∧ ⋀_i fair_i_seen
//! ```
//!
//! **Soundness (bad reachable ⇒ genuine violation):** a reachable `bad` state
//! has `looped` (snapshotted state repeats — a real cycle exists), `¬b_seen`
//! (no `b` on the cycle segment), each `fair_i_seen` (each `fair_i` fires at
//! least once on the cycle). The infinite unrolling `stem · cycle^ω` then
//! satisfies each `GF fair_i` (fires once per cycle iteration ⇒ infinitely
//! often) and violates `AG(a → AF b)` (the pending request stays outstanding
//! forever since `b` never holds after the snapshot).
//!
//! **Completeness (violation ⇒ bad reachable):** if `π` violates
//! `(⋀ GF fair_i) → AG(a → AF b)`, then `π` satisfies each `GF fair_i` and has
//! a suffix cycle carrying pending-a with no b. The nondeterministic `save` can
//! snapshot at the cycle-entry state; each `fair_i` fires somewhere on the
//! subsequent cycle by `GF fair_i`; `b_seen` never latches by the pending-a
//! assumption. Standard finite-state lasso closure gives `looped`. Hence `bad`
//! is reachable. This is Emerson–Lei fair-cycle detection composed with the
//! l2s save/snapshot argument (already trusted for the fairness-free case).
//!
//! Zero fairness atoms recovers the plain [`emit_response_l2s_monitor`]
//! semantics exactly (the empty conjunction is `one`).

use crate::adapter::btor2::ast::{Nid, Node};
use crate::adapter::btor2::bad_monitor::{
    BAD_COND_SYMBOL, btor2_cmp_keyword, find_or_make_bool_sort, input_nid_and_sort,
    output_nid_and_sort, state_nid_and_sort,
};
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::CmpOp;
use crate::adapter::{AdapterError, AdapterErrorKind};

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message,
        location: None,
    }
}

/// A BTOR2-text line appender that hands out fresh NIDs above the design's max.
struct Emit {
    next: Nid,
    lines: Vec<String>,
}

impl Emit {
    /// Append `"<fresh-nid> <body>"` and return the fresh NID.
    fn push(&mut self, body: impl AsRef<str>) -> Nid {
        let nid = self.next;
        self.next += 1;
        self.lines.push(format!("{nid} {}", body.as_ref()));
        nid
    }
}

/// Emit the request / grant atom `signal ⋈ value` as a 1-bit node, returning its
/// NID. `signal` may resolve to a state cell (value-alias / reset-mux aware) or a
/// primary input.
fn emit_atom(
    file: &crate::adapter::btor2::ast::Btor2File,
    e: &mut Emit,
    bool_sort: Nid,
    atom: (&str, CmpOp, u128),
    reset_pinned: bool,
    role: &str,
) -> Result<Nid, AdapterError> {
    let (signal, op, value) = atom;
    // Resolve against a state cell, a primary input, or a named output port. The
    // output case (registered grants like `level1` / `q`) is essential: the standard
    // lift keeps port names but drops state-cell names, so a grant signal surfaces
    // only as an `output` — comparing its driver net per-cycle is a sound observation.
    let (nid, sort) = state_nid_and_sort(file, signal, reset_pinned)
        .or_else(|| input_nid_and_sort(file, signal))
        .or_else(|| output_nid_and_sort(file, signal))
        .ok_or_else(|| {
            err(format!(
                "adapter/btor2/l2s_monitor: {role} `{signal}` is not a state cell, a primary \
                 input, or a named output, so the response monitor cannot bind it"
            ))
        })?;
    let cst = e.push(format!("constd {sort} {value}"));
    Ok(e.push(format!("{} {bool_sort} {nid} {cst}", btor2_cmp_keyword(op))))
}

/// Append the liveness-to-safety `bad` monitor for `AG(ante → AF cons)` to `content`.
///
/// See the module docs for the construction. Returns an error when an atom does not
/// resolve to a signal. Byte-for-byte equivalent to
/// [`emit_response_l2s_monitor_under_fairness`] with an empty `fairness_atoms` slice.
pub fn emit_response_l2s_monitor(
    content: &str,
    ante: (&str, CmpOp, u128),
    cons: (&str, CmpOp, u128),
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    emit_response_l2s_monitor_under_fairness(content, ante, cons, &[], reset_pinned)
}

/// mununu#477 Option B — append the fair-cycle l2s `bad` monitor for
/// `(⋀_i GF fair_i) → AG(ante → AF cons)` to `content`.
///
/// See the module-level "Fair-cycle extension" section for the construction and the
/// soundness / completeness argument. Adds one `fair_i_seen` latch per fairness atom
/// (each mirroring `b_seen`'s definition exactly, with `fair_i` in `b`'s slot) and
/// sets `bad = looped ∧ ¬b_seen ∧ ⋀_i fair_i_seen`. Empty `fairness_atoms` recovers
/// [`emit_response_l2s_monitor`] byte-for-byte.
///
/// Returns an error when any atom (ante, cons, or a fairness atom) does not resolve
/// to a signal.
pub fn emit_response_l2s_monitor_under_fairness(
    content: &str,
    ante: (&str, CmpOp, u128),
    cons: (&str, CmpOp, u128),
    fairness_atoms: &[(&str, CmpOp, u128)],
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    emit_l2s_monitor(content, Some(ante), 0, cons, fairness_atoms, reset_pinned)
}

/// The l2s `bad` monitor with an OPTIONAL request atom and a request DELAY. `ante: None` is the
/// antecedent-free `AG AF cons` (`assert property (s_eventually b)`): every cycle is a request,
/// the `pending` latch's `a` input is the constant `1`.
///
/// `ante_delay = m > 0` decides `AG(a → AXᵐ AF cons)` (`a |=> s_eventually b` is m = 1,
/// `a |-> ##[m:$] b` is m). The request is read through an m-latch delay line
/// (`a_d₁ … a_dₘ`, init 0, `next(a_d₁) = a`, `next(a_dᵢ₊₁) = a_dᵢ`), and the monitor checks
/// `AG(a_dₘ → AF cons)`. The two are equivalent: `a_dₘ` holds at a state exactly when `a` held m
/// steps earlier ON THAT PATH, and `AG(a → AXᵐ ψ)` says every m-step successor of an `a` state
/// satisfies ψ. The delay line is deterministic and does not feed the design, so a lasso over the
/// ORIGINAL state cells (the only cells the snapshot compares, as before) is still a genuine path —
/// the soundness and completeness arguments of [`emit_response_l2s_monitor_under_fairness`] carry
/// over with `a` replaced by `a_dₘ`. Init 0 is right: no request precedes cycle 0.
pub fn emit_l2s_monitor(
    content: &str,
    ante: Option<(&str, CmpOp, u128)>,
    ante_delay: u32,
    cons: (&str, CmpOp, u128),
    fairness_atoms: &[(&str, CmpOp, u128)],
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    emit_l2s_core(
        content,
        ante_delay,
        fairness_atoms,
        reset_pinned,
        |file, e, bool_sort| {
            let a = match ante {
                Some(ante) => emit_atom(file, e, bool_sort, ante, reset_pinned, "antecedent")?,
                None => e.push(format!("one {bool_sort}")),
            };
            let b = emit_atom(file, e, bool_sort, cons, reset_pinned, "consequent")?;
            Ok((a, b))
        },
    )
}

/// [`emit_l2s_monitor`] with the request and the grant given as BOOLEAN SUBTREES of `formula`
/// (`And` / `Or` / `Not` / constants over register comparisons), compiled by the same
/// [`compile_boolean_subtree`] the `|=>` safety monitor uses — so `(req && st == IDLE) |->
/// s_eventually done` reduces, not only single-atom requests. Leaves resolve state cell → output
/// → primary input → named internal net. `ante: None` is `AG AF cons`.
///
/// The soundness argument is unchanged: `a` and `b` are still functions of the CURRENT state and
/// inputs, the snapshot still compares only the original state cells, and a cycle's inputs can be
/// replayed, so a `b`-free loop over the original states is a genuine lasso on which `b` never
/// holds.
pub fn emit_l2s_monitor_from_formula(
    content: &str,
    formula: &crate::mu_calculus::Formula,
    ante: Option<crate::mu_calculus::NodeId>,
    ante_delay: u32,
    cons: crate::mu_calculus::NodeId,
    fairness_atoms: &[(&str, CmpOp, u128)],
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    use crate::adapter::btor2::bad_monitor::compile_boolean_subtree;
    emit_l2s_core(
        content,
        ante_delay,
        fairness_atoms,
        reset_pinned,
        |file, e, bool_sort| {
            let a = match ante {
                Some(id) => compile_boolean_subtree(
                    formula,
                    id,
                    file,
                    reset_pinned,
                    bool_sort,
                    &mut e.next,
                    &mut e.lines,
                )?,
                None => e.push(format!("one {bool_sort}")),
            };
            let b = compile_boolean_subtree(
                formula,
                cons,
                file,
                reset_pinned,
                bool_sort,
                &mut e.next,
                &mut e.lines,
            )?;
            Ok((a, b))
        },
    )
}

/// The l2s construction shared by both entry points; `request_grant` emits the 1-bit request and
/// grant nodes and returns their NIDs.
fn emit_l2s_core(
    content: &str,
    ante_delay: u32,
    fairness_atoms: &[(&str, CmpOp, u128)],
    reset_pinned: bool,
    request_grant: impl FnOnce(
        &crate::adapter::btor2::ast::Btor2File,
        &mut Emit,
        Nid,
    ) -> Result<(Nid, Nid), AdapterError>,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/l2s_monitor: {}", e.message);
        e
    })?;

    // Original state cells (before augmentation): the shadow copies these.
    let states: Vec<(Nid, Nid)> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::State { sort, .. } => Some((l.nid, *sort)),
            _ => None,
        })
        .collect();

    let next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut e = Emit {
        next: next_nid,
        lines: Vec::new(),
    };
    let mut prelude: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut e.next, &mut prelude);
    e.lines.extend(prelude);

    // Request / grant nodes.
    let (a_now, b) = request_grant(&file, &mut e, bool_sort)?;

    // A fresh `zero` of the boolean sort for the 1-bit latch inits.
    let zero1 = e.push(format!("zero {bool_sort}"));

    // The request delay line: `a_d₁ … a_dₘ`, init 0; the monitor reads `a_dₘ` (or `a` for m = 0).
    let mut a = a_now;
    for i in 1..=ante_delay {
        let d = e.push(format!("state {bool_sort} mununu_l2s_req_d{i}"));
        e.push(format!("init {bool_sort} {d} {zero1}"));
        e.push(format!("next {bool_sort} {d} {a}"));
        a = d;
    }

    // pending: next = (pending || a) && !b.
    let pending = e.push(format!("state {bool_sort} mununu_l2s_pending"));
    e.push(format!("init {bool_sort} {pending} {zero1}"));
    let p_or_a = e.push(format!("or {bool_sort} {pending} {a}"));
    let not_b = e.push(format!("not {bool_sort} {b}"));
    let next_pending = e.push(format!("and {bool_sort} {p_or_a} {not_b}"));
    e.push(format!("next {bool_sort} {pending} {next_pending}"));

    // save input + saved latch + save_en = save && !saved && pending.
    let saved = e.push(format!("state {bool_sort} mununu_l2s_saved"));
    e.push(format!("init {bool_sort} {saved} {zero1}"));
    let save_in = e.push(format!("input {bool_sort} mununu_l2s_save"));
    let not_saved = e.push(format!("not {bool_sort} {saved}"));
    let save_np = e.push(format!("and {bool_sort} {save_in} {not_saved}"));
    let save_en = e.push(format!("and {bool_sort} {save_np} {pending}"));
    let next_saved = e.push(format!("or {bool_sort} {saved} {save_en}"));
    e.push(format!("next {bool_sort} {saved} {next_saved}"));

    // Shadow copy of each original state cell + per-cell equality.
    let mut eq_terms: Vec<Nid> = Vec::new();
    for (s_nid, s_sort) in &states {
        let sh = e.push(format!("state {s_sort} mununu_l2s_sh_{s_nid}"));
        // next(sh) = ite(save_en, s, sh); no init — free, only read after a save.
        let nx = e.push(format!("ite {s_sort} {save_en} {s_nid} {sh}"));
        e.push(format!("next {s_sort} {sh} {nx}"));
        eq_terms.push(e.push(format!("eq {bool_sort} {s_nid} {sh}")));
    }
    // all_eq = ⋀ eq_terms; the empty conjunction (stateless design) is `one`.
    let all_eq = if let Some((&first, rest)) = eq_terms.split_first() {
        rest.iter().fold(first, |acc, &t| {
            e.push(format!("and {bool_sort} {acc} {t}"))
        })
    } else {
        e.push(format!("one {bool_sort}"))
    };
    let looped = e.push(format!("and {bool_sort} {saved} {all_eq}"));

    // b_seen: next = ite(save_en, b, ite(saved, b_seen || b, 0)).
    let b_seen = e.push(format!("state {bool_sort} mununu_l2s_bseen"));
    e.push(format!("init {bool_sort} {b_seen} {zero1}"));
    let bseen_or_b = e.push(format!("or {bool_sort} {b_seen} {b}"));
    let inner = e.push(format!("ite {bool_sort} {saved} {bseen_or_b} {zero1}"));
    let next_bseen = e.push(format!("ite {bool_sort} {save_en} {b} {inner}"));
    e.push(format!("next {bool_sort} {b_seen} {next_bseen}"));

    // Fair-cycle extension: one fair_i_seen per fairness atom (mirrors b_seen).
    // Skipped entirely when fairness_atoms is empty — keeps the plain
    // `emit_response_l2s_monitor` emission byte-for-byte unchanged.
    let mut fair_seen_terms: Vec<Nid> = Vec::with_capacity(fairness_atoms.len());
    for (i, fair) in fairness_atoms.iter().enumerate() {
        let f = emit_atom(
            &file,
            &mut e,
            bool_sort,
            *fair,
            reset_pinned,
            &format!("fairness_atom_{i}"),
        )?;
        let f_seen = e.push(format!("state {bool_sort} mununu_l2s_fair{i}_seen"));
        e.push(format!("init {bool_sort} {f_seen} {zero1}"));
        let f_seen_or_f = e.push(format!("or {bool_sort} {f_seen} {f}"));
        let f_inner = e.push(format!("ite {bool_sort} {saved} {f_seen_or_f} {zero1}"));
        let f_next = e.push(format!("ite {bool_sort} {save_en} {f} {f_inner}"));
        e.push(format!("next {bool_sort} {f_seen} {f_next}"));
        fair_seen_terms.push(f_seen);
    }

    // bad = looped && !b_seen (&& ⋀_i fair_i_seen when fairness_atoms non-empty).
    // A b-free closed loop carrying a pending request AND — under fairness — each
    // justice constraint observed at least once on the cycle. The FINAL `and`
    // carries the BAD_COND_SYMBOL for provenance-stability with the plain monitor.
    let not_bseen = e.push(format!("not {bool_sort} {b_seen}"));
    let bad_cond = if fair_seen_terms.is_empty() {
        // Byte-equivalent to the pre-extension emission: one two-way `and` with
        // the symbol name.
        e.push(format!(
            "and {bool_sort} {looped} {not_bseen} {BAD_COND_SYMBOL}"
        ))
    } else {
        let core_and = e.push(format!("and {bool_sort} {looped} {not_bseen}"));
        let (last, rest) = fair_seen_terms
            .split_last()
            .expect("non-empty checked above");
        let mut acc = core_and;
        for &t in rest {
            acc = e.push(format!("and {bool_sort} {acc} {t}"));
        }
        e.push(format!("and {bool_sort} {acc} {last} {BAD_COND_SYMBOL}"))
    };
    e.push(format!("bad {bad_cond}"));

    Ok(format!("{}\n{}\n", content.trim_end(), e.lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 2-state request/grant FSM: `st` 0=idle, 1=busy. Input `req` drives idle→busy;
    // busy→idle unconditionally, asserting `grant` (grant = st==1). `req` pending in
    // idle is always granted next cycle ⇒ AG(req→AF grant) HOLDS.
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

    #[test]
    fn emitted_monitor_parses_and_has_bad_and_latches() {
        // req atom = (st == 0) proxy via input `req`; grant = (st == 1).
        let out = emit_response_l2s_monitor(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            false,
        )
        .expect("emit");
        let file = parser::parse(&out).expect("emitted BTOR2 re-parses");
        // The monitor added the l2s latches + a bad line.
        assert!(out.contains("mununu_l2s_pending"), "pending latch present");
        assert!(out.contains("mununu_l2s_saved"), "saved latch present");
        assert!(out.contains("mununu_l2s_save"), "save input present");
        assert!(out.contains("mununu_l2s_bseen"), "b_seen latch present");
        assert!(
            out.contains("mununu_l2s_sh_2"),
            "shadow of state cell 2 present"
        );
        assert!(
            file.lines
                .iter()
                .any(|l| matches!(l.node, Node::Bad { .. })),
            "a bad line is present"
        );
    }

    #[test]
    fn unresolvable_atom_errors() {
        let e = emit_response_l2s_monitor(
            RESPONDER,
            ("nonexistent", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            false,
        );
        assert!(e.is_err(), "an atom that binds no signal must error");
    }

    // The real-lift shape: `flatten; opt_clean; dffunmap` keeps port names but drops
    // state-cell names, so a registered grant surfaces only as a named `output` over
    // an *unnamed* state. Binding the grant atom must follow the output to its driver.
    const REGISTERED_OUTPUT: &str = "\
1 sort bitvec 1
2 input 1 req
3 state 1
4 zero 1
5 init 1 3 4
6 next 1 3 2
7 output 3 grant
";

    /// mununu#477 Option B — the empty-fairness path of the fair-cycle emitter is
    /// byte-for-byte identical to the plain emitter. Guards against silent NID
    /// drift on the existing `verify-liveness` verb.
    #[test]
    fn empty_fairness_matches_plain_emitter_byte_for_byte() {
        let plain = emit_response_l2s_monitor(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            false,
        )
        .expect("plain");
        let fair_empty = emit_response_l2s_monitor_under_fairness(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            &[],
            false,
        )
        .expect("empty fair");
        assert_eq!(
            plain, fair_empty,
            "empty-fairness path must be byte-for-byte identical to the plain emitter"
        );
    }

    /// mununu#477 Option B — a non-empty fairness list adds `fair_i_seen` latches
    /// and folds them into `bad`. Structural check on the emitted monitor.
    #[test]
    fn fairness_atoms_add_fair_seen_latches_and_extend_bad() {
        let out = emit_response_l2s_monitor_under_fairness(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            &[("req", CmpOp::Eq, 0)],
            false,
        )
        .expect("emit");
        assert!(
            out.contains("mununu_l2s_fair0_seen"),
            "fair0_seen latch present: {out}"
        );
        // The bad line still names the BAD_COND_SYMBOL on its final and.
        let file = parser::parse(&out).expect("emitted BTOR2 re-parses");
        assert!(
            file.lines
                .iter()
                .any(|l| matches!(l.node, Node::Bad { .. })),
            "a bad line is present"
        );
    }

    /// mununu#477 Option B — multiple fairness atoms produce one latch per atom
    /// and chain into `bad` via a left-associative `and` fold.
    #[test]
    fn multiple_fairness_atoms_produce_one_latch_each() {
        let out = emit_response_l2s_monitor_under_fairness(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            &[("req", CmpOp::Eq, 0), ("req", CmpOp::Eq, 1)],
            false,
        )
        .expect("emit");
        assert!(out.contains("mununu_l2s_fair0_seen"));
        assert!(out.contains("mununu_l2s_fair1_seen"));
    }

    /// mununu#477 Option B — a fairness atom that binds no signal errors just like
    /// an ante / cons atom that binds no signal.
    #[test]
    fn unresolvable_fairness_atom_errors() {
        let e = emit_response_l2s_monitor_under_fairness(
            RESPONDER,
            ("req", CmpOp::Eq, 1),
            ("st", CmpOp::Eq, 1),
            &[("nonexistent_fair_signal", CmpOp::Eq, 1)],
            false,
        );
        assert!(e.is_err(), "unbound fairness atom must error");
    }

    #[test]
    fn binds_registered_output_grant() {
        // `grant` is a named output over the unnamed state 3; `req` is a primary input.
        let out = emit_response_l2s_monitor(
            REGISTERED_OUTPUT,
            ("req", CmpOp::Eq, 1),
            ("grant", CmpOp::Eq, 1),
            false,
        )
        .expect("a registered-output grant must bind via output_nid_and_sort");
        let file = parser::parse(&out).expect("emitted BTOR2 re-parses");
        assert!(
            file.lines
                .iter()
                .any(|l| matches!(l.node, Node::Bad { .. })),
            "the monitor emits a bad line"
        );
        // The grant comparison binds the output's driver (state 3), not the output line.
        assert!(
            out.contains("eq 1 3 "),
            "grant compares against driver nid 3"
        );
    }
}
