//! H.O.1b (2026-06-29) — emit the H.O safety fragment as a BTOR2 `bad` monitor.
//!
//! The external model checker ([`crate::adapter::btormc`], H.O.1a) decides a
//! BTOR2 `bad` property: a reachable `bad` ⇒ VIOLATED, an unreachable one (proven
//! by k-induction) ⇒ SAFE. This module turns the same safety fragment the
//! internal oracle ([`crate::adapter::btor2::concrete_oracle`]) checks — `AG`
//! over a state/input atom, and the `ante |=> cons` implication — into that
//! `bad` line. A `bad` monitors the property's NEGATION (the violation), so:
//!
//! - **`AG (signal ⋈ value)`** → `bad = !(signal ⋈ value)` (the negated
//!   comparison). A reachable state breaking the invariant makes `bad` true.
//! - **`ante |=> cons`** (= `AG (ante → AX cons)`) → a 1-bit latch
//!   `mununu_ante_prev` (init 0, `next = ante`) plus
//!   `bad = mununu_ante_prev && !(cons)`. When the antecedent held last cycle and
//!   the consequent fails now, `bad` fires — the standard `|=>` safety-monitor
//!   construction.
//!
//! The augmentation works at the BTOR2 **text** level (append lines with fresh
//! NIDs above the current max), mirroring
//! [`crate::adapter::btor2::shadow::augment_with_past_shadows`] and
//! [`crate::adapter::btor2::pin`] so it composes with anything that re-parses the
//! text (bit-blast, the lift, the model checker).
//!
//! **btormc input-ordering note.** `btormc` enforces that an `init sid state val`
//! line has `state_nid > val_nid` (the init value's const is declared before the
//! state). Yosys' `write_btor` output satisfies this, so real lifted designs feed
//! straight into [`crate::adapter::btormc::run_btormc`]. This emitter only ever
//! APPENDS lines (its own `init` for the latch puts `zero` before the state by
//! construction), so it never breaks a compatible input — but it does not rewrite
//! a pre-existing btormc-incompatible `init` either (mununu's own parser is
//! lenient about that ordering; some hand-written fixtures rely on it). The H.O.1c
//! real-RTL e2e runs the model checker on yosys-lifted designs, which are
//! btormc-compatible by construction.
//!
//! Signal resolution mirrors the internal oracle
//! ([`crate::adapter::btor2::concrete_oracle`]): a state signal resolves through
//! [`parser::resolve_state_alias`] (following a `uext`/`sext`-0 rename, or an
//! async-reset register mux when `reset_pinned`); the `|=>` antecedent may also be
//! a 1-bit primary input. The comparison node carries the trailing symbol
//! `mununu_bad`, so the H.O.1b differential can `observe` it via
//! [`crate::adapter::btor2::bit_blast::simulate_one_step_observe`] and confirm —
//! WITHOUT any external tool — that the emitted `bad` fires exactly when the
//! internal oracle reports a violation.

use crate::adapter::btor2::ast::{Nid, Node, Sort};
use crate::adapter::btor2::concrete_oracle::OracleAtom;
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::CmpOp;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// The trailing symbol placed on the node `bad` references, so callers can
/// `observe` the monitor's truth value by name (the H.O.1b differential, and
/// diagnostics).
pub const BAD_COND_SYMBOL: &str = "mununu_bad";

/// The 1-bit latch synthesised for the `|=>` monitor (holds the previous cycle's
/// antecedent truth).
pub const ANTE_PREV_SYMBOL: &str = "mununu_ante_prev";

/// The 1-bit latch synthesised for a FAIRNESS environment assumption (holds the
/// previous cycle's truth of the assumption predicate `a`). See
/// [`emit_latched_predicate_state`].
pub const ASSUME_PREV_SYMBOL: &str = "mununu_assume_prev";

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message,
        location: None,
    }
}

/// BTOR2 keyword for an *unsigned* comparison (the internal oracle compares
/// `u128`, so unsigned is the matching semantics).
pub(crate) fn btor2_cmp_keyword(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "eq",
        CmpOp::Ne => "neq",
        CmpOp::Lt => "ult",
        CmpOp::Le => "ulte",
        CmpOp::Gt => "ugt",
        CmpOp::Ge => "ugte",
    }
}

/// The negation of a comparison — `bad` watches the invariant's *violation*.
fn negate_cmp(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Eq => CmpOp::Ne,
        CmpOp::Ne => CmpOp::Eq,
        CmpOp::Lt => CmpOp::Ge,
        CmpOp::Le => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Ge => CmpOp::Lt,
    }
}

/// Find an existing `sort bitvec 1` line, or append one. Returns its NID.
pub(crate) fn find_or_make_bool_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    next_nid: &mut Nid,
    appended: &mut Vec<String>,
) -> Nid {
    for line in &file.lines {
        if let Node::Sort {
            sort: Sort::BitVec { width: 1 },
        } = &line.node
        {
            return line.nid;
        }
    }
    let nid = *next_nid;
    *next_nid += 1;
    appended.push(format!("{nid} sort bitvec 1"));
    nid
}

/// Resolve a STATE signal (value-alias / reset-mux aware) to `(value_nid,
/// sort_nid)`. `None` when it is not a state cell.
pub(crate) fn state_nid_and_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    signal: &str,
    reset_pinned: bool,
) -> Option<(Nid, Nid)> {
    let nid = parser::resolve_state_alias(file, signal, reset_pinned)?;
    match file.lookup(nid).map(|l| &l.node) {
        Some(Node::State { sort, .. }) => Some((nid, *sort)),
        _ => None,
    }
}

/// Resolve a 1-bit (or any) primary-input signal to `(nid, sort_nid)`.
pub(crate) fn input_nid_and_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    signal: &str,
) -> Option<(Nid, Nid)> {
    let symbols = parser::collect_symbols(file);
    file.lines.iter().find_map(|l| match &l.node {
        Node::Input { sort, .. } if symbols.get(&l.nid).map(String::as_str) == Some(signal) => {
            Some((l.nid, *sort))
        }
        _ => None,
    })
}

/// Resolve an OUTPUT port symbol to `(driver_nid, sort_nid)` — the net driving the
/// named output.
///
/// A *registered* output (e.g. a grant flop) surfaces here even when the underlying
/// state cell is unnamed after the standard `flatten; opt_clean; dffunmap` lift drops
/// state symbols but keeps port names. Comparing the driver's value is a **sound**
/// per-cycle observation for the response monitor: it is exactly the signal's value
/// each step, and the l2s `AF` semantics stay sound whether the driver is a pure
/// function of state or of state+inputs (a `b`-free lasso is still a genuine
/// never-granted path). A *negated* output operand is skipped — binding its
/// un-negated net would flip the comparison sense — so it falls through to the
/// caller's bind error rather than binding the wrong polarity.
/// mununu#503 — resolve a named INTERNAL COMBINATIONAL net.
///
/// The monitor compiler could resolve a state cell, an output port, or a primary input — but not
/// an internal wire, and that is what a great many SVA leaves actually are. `sysrst`'s
/// `assign trigger_active = (trigger_i == 1'b0)` is the canonical case: every `|->` property over
/// it was declined by the rescue lane with *"leaf atom `trigger_active` does not resolve to a
/// state cell, output port, or primary input"*, so the ⊥ stood even though the property was fine
/// and the monitor was one node reference away from being emittable.
///
/// Yosys names an internal net with a no-op alias line — `<nid> uext <sort> <src> 0 <name>` — so
/// the symbol sits on an ordinary `Op` node. Referencing that nid from the `bad` expression is
/// legal BTOR2: it is a combinational node in the same file, evaluated in the same cycle.
///
/// Tried LAST in the resolution chain, so a name that is also a state cell, output port, or input
/// still binds to that first — this only widens what resolves, it never re-points an existing one.
pub(crate) fn comb_nid_and_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    signal: &str,
) -> Option<(Nid, Nid)> {
    file.lines.iter().find_map(|l| match &l.node {
        Node::Op {
            sort,
            symbol: Some(sym),
            ..
        } if sym == signal => Some((l.nid, *sort)),
        _ => None,
    })
}

pub(crate) fn output_nid_and_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    signal: &str,
) -> Option<(Nid, Nid)> {
    let drv = file.lines.iter().find_map(|l| match &l.node {
        Node::Output {
            signal: op,
            symbol: Some(sym),
        } if sym == signal && !op.is_negated() => Some(op.nid()),
        _ => None,
    })?;
    let sort = match &file.lookup(drv)?.node {
        Node::Input { sort, .. }
        | Node::State { sort, .. }
        | Node::Const { sort, .. }
        | Node::Op { sort, .. } => *sort,
        _ => return None,
    };
    Some((drv, sort))
}

/// H.O.1b — append a `bad` monitor for `AG (signal ⋈ value)`.
///
/// `signal` must resolve to a state register (value-alias / reset-mux aware, like
/// the internal oracle). The emitted `bad` is the negated comparison
/// `!(signal ⋈ value)`; the comparison node carries [`BAD_COND_SYMBOL`].
pub fn emit_ag_state_atom_monitor(
    content: &str,
    signal: &str,
    op: CmpOp,
    value: u128,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;

    // Resolve the atom's signal to its per-cycle value net: a STATE register (value-alias / reset-mux
    // aware) first, else a combinational OUTPUT port (its driver net). Many real recoverability targets
    // (`busy` / `done` / `valid` / `ready`) are outputs driven from the FSM (e.g. `state != IDLE`), not
    // state cells — comparing the driver's value is the same sound per-cycle observation. Without the
    // output fallback the `EF(atom)` reachability monitor (used by `probe_vacuous` and the assumption
    // non-vacuity gate) fails on every output target.
    let (sig_nid, sig_sort) = state_nid_and_sort(&file, signal, reset_pinned)
        .or_else(|| output_nid_and_sort(&file, signal))
        .ok_or_else(|| {
            err(format!(
                "adapter/btor2/bad_monitor: `{signal}` does not resolve to a state cell or output port \
                 (the AG(state-atom) monitor requires a state register, a value-alias of one, or an \
                 output net)"
            ))
        })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    let value_const = next_nid;
    next_nid += 1;
    appended.push(format!("{value_const} constd {sig_sort} {value}"));

    // bad = !(signal ⋈ value) = (signal <negated-⋈> value); named for `observe`.
    let bad_kw = btor2_cmp_keyword(negate_cmp(op));
    let bad_cond = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{bad_cond} {bad_kw} {bool_sort} {sig_nid} {value_const} {BAD_COND_SYMBOL}"
    ));

    let bad_line = next_nid;
    appended.push(format!("{bad_line} bad {bad_cond}"));

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// P1 — append a `bad` monitor for `AG (signal ∈ legal)`, the FSM-encoding-legality
/// invariant. `signal` must resolve to a state register (value-alias / reset-mux
/// aware). The emitted `bad` fires when `signal` holds a value **outside** `legal` (an
/// illegal encoding): `bad = ⋀_{c ∈ legal} (signal != c)`. `legal` must be non-empty
/// and is deduplicated by the caller; the final `and` node carries [`BAD_COND_SYMBOL`].
pub fn emit_ag_state_in_set_monitor(
    content: &str,
    signal: &str,
    legal: &[u64],
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;
    let (sig_nid, sig_sort) = state_nid_and_sort(&file, signal, reset_pinned).ok_or_else(|| {
        err(format!(
            "adapter/btor2/bad_monitor: `{signal}` does not resolve to a state cell \
             (the AG(state ∈ set) monitor requires a state register or a value-alias of one)"
        ))
    })?;
    emit_ag_state_in_set_monitor_by_nid(content, sig_nid, sig_sort, legal)
}

/// As [`emit_ag_state_in_set_monitor`], but with the state cell already resolved to its
/// `(value_nid, sort_nid)` — for callers (the FSM-encoding scan) that discover the cell
/// directly and would otherwise have to round-trip through a name that a combinational
/// alias may shadow.
pub fn emit_ag_state_in_set_monitor_by_nid(
    content: &str,
    sig_nid: Nid,
    sig_sort: Nid,
    legal: &[u64],
) -> Result<String, AdapterError> {
    if legal.is_empty() {
        return Err(err(
            "adapter/btor2/bad_monitor: the state-in-set monitor needs a non-empty legal set"
                .to_string(),
        ));
    }
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    // neq_c = (signal != c) for each legal encoding c.
    let mut terms: Vec<Nid> = Vec::with_capacity(legal.len());
    for &c in legal {
        let const_nid = next_nid;
        next_nid += 1;
        appended.push(format!("{const_nid} constd {sig_sort} {c}"));
        let neq_nid = next_nid;
        next_nid += 1;
        appended.push(format!("{neq_nid} neq {bool_sort} {sig_nid} {const_nid}"));
        terms.push(neq_nid);
    }

    // illegal = ⋀ terms; fold with `and`, naming the final node so it can be observed.
    let mut acc = terms[0];
    let last = terms.len() - 1;
    for (i, &t) in terms.iter().enumerate().skip(1) {
        let and_nid = next_nid;
        next_nid += 1;
        let sym = if i == last {
            format!(" {BAD_COND_SYMBOL}")
        } else {
            String::new()
        };
        appended.push(format!("{and_nid} and {bool_sort} {acc} {t}{sym}"));
        acc = and_nid;
    }

    let bad_line = next_nid;
    appended.push(format!("{bad_line} bad {acc}"));
    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// H.O.1b — append a `bad` monitor for `ante |=> cons` (= `AG (ante → AX cons)`).
///
/// `cons` must resolve to a state register; `ante` may be a state register OR a
/// 1-bit primary input. Synthesises the latch [`ANTE_PREV_SYMBOL`] (init 0,
/// `next = (ante ⋈ value)`) and emits `bad = mununu_ante_prev && !(cons ⋈ value)`,
/// with the `and` node carrying [`BAD_COND_SYMBOL`].
pub fn emit_ag_implies_next_monitor(
    content: &str,
    ante: &OracleAtom,
    cons: &OracleAtom,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;

    // Consequent: a state cell (checked at the next cycle, via the latch).
    let (cons_nid, cons_sort) =
        state_nid_and_sort(&file, &cons.signal, reset_pinned).ok_or_else(|| {
            err(format!(
                "adapter/btor2/bad_monitor: consequent `{}` does not resolve to a state cell",
                cons.signal
            ))
        })?;
    // Antecedent: a state cell OR a primary input.
    let (ante_nid, ante_sort) = state_nid_and_sort(&file, &ante.signal, reset_pinned)
        .or_else(|| input_nid_and_sort(&file, &ante.signal))
        .ok_or_else(|| {
            err(format!(
                "adapter/btor2/bad_monitor: antecedent `{}` is neither a state cell nor a \
                 primary input (out of the |=> fragment)",
                ante.signal
            ))
        })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    // ante holds (positive comparison) — latched into mununu_ante_prev.
    let ante_const = next_nid;
    next_nid += 1;
    appended.push(format!("{ante_const} constd {ante_sort} {}", ante.value));
    let ante_cmp = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{ante_cmp} {} {bool_sort} {ante_nid} {ante_const}",
        btor2_cmp_keyword(ante.op)
    ));

    // cons fails (negated comparison), evaluated at the current cycle.
    let cons_const = next_nid;
    next_nid += 1;
    appended.push(format!("{cons_const} constd {cons_sort} {}", cons.value));
    let cons_fail = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{cons_fail} {} {bool_sort} {cons_nid} {cons_const}",
        btor2_cmp_keyword(negate_cmp(cons.op))
    ));

    // mununu_ante_prev latch: init 0, next = ante_cmp. (`init` requires the state
    // NID > the value NID, so `zero` is emitted before the state.)
    let zero_bool = next_nid;
    next_nid += 1;
    appended.push(format!("{zero_bool} zero {bool_sort}"));
    let ante_prev = next_nid;
    next_nid += 1;
    appended.push(format!("{ante_prev} state {bool_sort} {ANTE_PREV_SYMBOL}"));
    let ante_prev_init = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{ante_prev_init} init {bool_sort} {ante_prev} {zero_bool}"
    ));
    let ante_prev_next = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{ante_prev_next} next {bool_sort} {ante_prev} {ante_cmp}"
    ));

    // bad = ante_prev && cons_fail; named for `observe`.
    let bad_cond = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{bad_cond} and {bool_sort} {ante_prev} {cons_fail} {BAD_COND_SYMBOL}"
    ));
    let bad_line = next_nid;
    appended.push(format!("{bad_line} bad {bad_cond}"));

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// Append a 1-bit latch state (init 0, `next = (atom.signal atom.op atom.value)`) named
/// [`ASSUME_PREV_SYMBOL`], recording whether the assumption predicate `atom` held on each transition.
/// The atom's signal may be a STATE cell, a PRIMARY INPUT, or a combinational OUTPUT port.
///
/// This is the SOUND encoding of an INPUT/transition-level fairness assumption `GF a` as a
/// STATE-predicate fairness `GF(mununu_assume_prev == 1)`: an input is not part of the state, so it
/// cannot appear directly in a state-fairness fixpoint (a naive νμν with an input-predicate conjunct
/// would be unsound). Latching `a` into a fresh 1-bit state samples the assumption on each transition;
/// `GF(mununu_assume_prev == 1) ⟺ GF a` on the play (a one-cycle offset is immaterial to an
/// infinitely-often objective). The latch is a PURE MONITOR — no new choice, no feedback into the
/// original transition — so every strategy/play is preserved and a game verdict transfers unchanged.
pub fn emit_latched_predicate_state(
    content: &str,
    atom: &OracleAtom,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    emit_latched_predicate_state_named(content, atom, ASSUME_PREV_SYMBOL, reset_pinned)
}

/// As [`emit_latched_predicate_state`] but with an explicit latch `symbol` — for a CONJUNCTION of
/// fairness assumptions `GF a_1 ∧ … ∧ GF a_m`, where each `a_i` needs its OWN latch state
/// (`mununu_assume_prev_0`, `_1`, …) so the multi-pair GR(1) fixpoint can reference them independently.
pub fn emit_latched_predicate_state_named(
    content: &str,
    atom: &OracleAtom,
    symbol: &str,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;
    // The assumption signal: a state cell, a primary input, or a combinational output port.
    let (sig_nid, sig_sort) = state_nid_and_sort(&file, &atom.signal, reset_pinned)
        .or_else(|| input_nid_and_sort(&file, &atom.signal))
        .or_else(|| output_nid_and_sort(&file, &atom.signal))
        .ok_or_else(|| {
            err(format!(
                "adapter/btor2/bad_monitor: fairness assumption signal `{}` does not resolve to a \
                 state cell, primary input, or output port",
                atom.signal
            ))
        })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    // a = (signal op value)
    let a_const = next_nid;
    next_nid += 1;
    appended.push(format!("{a_const} constd {sig_sort} {}", atom.value));
    let a_cmp = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{a_cmp} {} {bool_sort} {sig_nid} {a_const}",
        btor2_cmp_keyword(atom.op)
    ));

    // `symbol` latch: init 0, next = a_cmp. (`init` needs the state NID > the value NID, so
    // `zero` is emitted before the state — the same ordering emit_ag_implies_next_monitor relies on.)
    let zero_bool = next_nid;
    next_nid += 1;
    appended.push(format!("{zero_bool} zero {bool_sort}"));
    let latch = next_nid;
    next_nid += 1;
    appended.push(format!("{latch} state {bool_sort} {symbol}"));
    let latch_init = next_nid;
    next_nid += 1;
    appended.push(format!("{latch_init} init {bool_sort} {latch} {zero_bool}"));
    let latch_next = next_nid;
    appended.push(format!("{latch_next} next {bool_sort} {latch} {a_cmp}"));

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// mununu#503 — compile a pure-boolean mu-calculus subtree into BTOR2 nodes, returning the NID of
/// the resulting 1-bit expression.
///
/// Extracted verbatim from [`emit_ag_boolean_invariant_monitor`] so the `|=>` monitor
/// ([`emit_ag_implies_next_compound_monitor`]) compiles its antecedent and consequent through the
/// SAME code path. Two monitors that disagree about what a leaf means is exactly the class of bug
/// mununu#503 already hit once (the gate and the compiler disagreeing about bare identifiers), so
/// there is deliberately one compiler, not two.
///
/// Accepts `And`/`Or`/`Not`/`True`/`False`/`Predicate` only — a modal, fixpoint, or variable node
/// is an error, not a silent skip. Leaves resolve state cell → output net → primary input →
/// named combinational net.
pub(crate) fn compile_boolean_subtree(
    formula: &crate::mu_calculus::Formula,
    id: crate::mu_calculus::NodeId,
    file: &crate::adapter::btor2::ast::Btor2File,
    reset_pinned: bool,
    bool_sort: Nid,
    next_nid: &mut Nid,
    appended: &mut Vec<String>,
) -> Result<Nid, AdapterError> {
    use crate::adapter::btor2::predicate_expr::{PredicateExpr, parse_predicate_atom_bool};
    use crate::mu_calculus::Node as MuNode;

    match formula.node(id) {
        MuNode::True => {
            let n = *next_nid;
            *next_nid += 1;
            appended.push(format!("{n} constd {bool_sort} 1"));
            Ok(n)
        }
        MuNode::False => {
            let n = *next_nid;
            *next_nid += 1;
            appended.push(format!("{n} constd {bool_sort} 0"));
            Ok(n)
        }
        MuNode::Not(inner) => {
            let a = compile_boolean_subtree(
                formula,
                *inner,
                file,
                reset_pinned,
                bool_sort,
                next_nid,
                appended,
            )?;
            let n = *next_nid;
            *next_nid += 1;
            appended.push(format!("{n} not {bool_sort} {a}"));
            Ok(n)
        }
        MuNode::And(l, r) => {
            let a = compile_boolean_subtree(
                formula,
                *l,
                file,
                reset_pinned,
                bool_sort,
                next_nid,
                appended,
            )?;
            let b = compile_boolean_subtree(
                formula,
                *r,
                file,
                reset_pinned,
                bool_sort,
                next_nid,
                appended,
            )?;
            let n = *next_nid;
            *next_nid += 1;
            appended.push(format!("{n} and {bool_sort} {a} {b}"));
            Ok(n)
        }
        MuNode::Or(l, r) => {
            let a = compile_boolean_subtree(
                formula,
                *l,
                file,
                reset_pinned,
                bool_sort,
                next_nid,
                appended,
            )?;
            let b = compile_boolean_subtree(
                formula,
                *r,
                file,
                reset_pinned,
                bool_sort,
                next_nid,
                appended,
            )?;
            let n = *next_nid;
            *next_nid += 1;
            appended.push(format!("{n} or {bool_sort} {a} {b}"));
            Ok(n)
        }
        // mununu#503 — `parse_predicate_atom_bool`, matching the gate in
        // `reach_rescue::is_compilable_boolean_body`. A bare identifier is "signal is
        // true" ⇒ `!= 0` (sound for any width; `== 1` would exclude truthy 2, 3, …).
        // The gate and this compiler MUST use the same entry point: if the gate accepts a
        // leaf this rejects, a property passes reducibility and then fails to compile.
        MuNode::Predicate(atom) => match parse_predicate_atom_bool(atom) {
            Ok(PredicateExpr::Cmp {
                register,
                op,
                value,
            }) => {
                let (sig_nid, sig_sort) = state_nid_and_sort(file, &register, reset_pinned)
                    .or_else(|| output_nid_and_sort(file, &register))
                    .or_else(|| input_nid_and_sort(file, &register))
                    // mununu#503 — last: a named internal combinational net.
                    .or_else(|| comb_nid_and_sort(file, &register))
                    .ok_or_else(|| {
                        err(format!(
                            "adapter/btor2/bad_monitor: leaf atom `{register}` does not \
                             resolve to a state cell, output port, primary input, or named \
                             internal net"
                        ))
                    })?;
                let const_nid = *next_nid;
                *next_nid += 1;
                appended.push(format!("{const_nid} constd {sig_sort} {value}"));
                let cmp_kw = btor2_cmp_keyword(op);
                let n = *next_nid;
                *next_nid += 1;
                appended.push(format!("{n} {cmp_kw} {bool_sort} {sig_nid} {const_nid}"));
                Ok(n)
            }
            Ok(_) => Err(err(format!(
                "adapter/btor2/bad_monitor: leaf atom `{atom}` uses a relational / addend / \
                 select shape not supported by the compound monitor"
            ))),
            Err(e) => Err(err(format!(
                "adapter/btor2/bad_monitor: leaf atom `{atom}` did not parse as a predicate \
                 expression: {e:?}"
            ))),
        },
        other => Err(err(format!(
            "adapter/btor2/bad_monitor: compound safety monitor accepts \
             And/Or/Not/True/False/Predicate only; found {other:?}"
        ))),
    }
}

/// mununu#503 — append a `bad` monitor for `AG (A → AX C)`, the shape SVA's `A |=> C` lifts to
/// (`nu X. ((¬A ∨ [] C) ∧ [] X)`).
///
/// Widens [`emit_ag_implies_next_monitor`] on the two axes that kept it from reaching real
/// properties: the antecedent may be a **compound** boolean expression (it took a single
/// [`OracleAtom`]), and the consequent may be **combinational** (it required a state cell).
/// `sysrst_ctrl_detect_sva_12` — `event_detected_pulse_o |=> !event_detected_pulse_o` — needs
/// both: `event_detected_pulse_o` is a combinational output of state AND inputs, so it is neither
/// a state cell nor a primary input.
///
/// `neg_ante` is the `¬A` disjunct **as it appears in the formula**, not `A`; the antecedent is
/// recovered as `not(compile(neg_ante))`. Taking it in that form is what lets a compound
/// antecedent through unchanged — whatever `¬A` is, the same compiler handles it.
///
/// Emits `bad = mununu_ante_prev ∧ ¬C`, where `mununu_ante_prev` ([`ANTE_PREV_SYMBOL`]) is a fresh
/// 1-bit latch with `init 0`, `next = A`.
///
/// # Soundness
///
/// `bad` is reachable ⟺ ∃ a trace and a cycle `t` with `A` true at `t` and `C` false at `t+1` —
/// exactly a violation of `AG (A → AX C)`. Three things make that equivalence hold:
///
/// - **`init 0` fabricates no obligation at reset.** Nothing precedes cycle 0, so the latch must
///   read false there; an `init 1` would report a violation on a design that never asserted `A`.
/// - **The latch is a pure monitor.** It adds no input, no constraint, and never feeds back into
///   the original transition relation, so the reachable-state set of the design is unchanged and
///   the counterexample projects back to the original registers.
/// - **A free next-cycle input is the correct reading of `[] C`.** `C` combinational means its
///   value at `t+1` depends on the inputs at `t+1`; `bad` reachability quantifies existentially
///   over those, which is precisely `¬(∀ successors. C)`. SVA agrees: `A |=> C` is violated iff
///   SOME trace has `A` then `¬C`. This is why the monitor decides a property the predicate cube
///   cannot — the cube must pin an input-dependent label to a cube dimension and abstains instead.
///
/// Note the contrast with [`crate::adapter::btor2::concrete_oracle::ag_implies_next`], which
/// REFUSES an input-dependent consequent (`ag_implies_next_rejects_input_consequent`). That is a
/// real limitation of an oracle that evaluates the consequent as a function of the next STATE: it
/// has no next-cycle input to evaluate against. A `bad` monitor has no such gap — the next cycle's
/// inputs are part of the trace — which is why this path decides properties the oracle cannot
/// cross-check, and why the oracle is not the differential for them.
pub fn emit_ag_implies_next_compound_monitor(
    content: &str,
    formula: &crate::mu_calculus::Formula,
    neg_ante: crate::mu_calculus::NodeId,
    cons: crate::mu_calculus::NodeId,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    // A = ¬(¬A) — the formula carries the negated antecedent, so undo it here.
    let neg_ante_nid = compile_boolean_subtree(
        formula,
        neg_ante,
        &file,
        reset_pinned,
        bool_sort,
        &mut next_nid,
        &mut appended,
    )?;
    let ante = next_nid;
    next_nid += 1;
    appended.push(format!("{ante} not {bool_sort} {neg_ante_nid}"));

    // ¬C, evaluated at the cycle the latch exposes.
    let cons_nid = compile_boolean_subtree(
        formula,
        cons,
        &file,
        reset_pinned,
        bool_sort,
        &mut next_nid,
        &mut appended,
    )?;
    let cons_fail = next_nid;
    next_nid += 1;
    appended.push(format!("{cons_fail} not {bool_sort} {cons_nid}"));

    // mununu_ante_prev latch: init 0, next = A. (`init` requires the state NID > the value NID,
    // so `zero` is emitted before the state — the ordering emit_ag_implies_next_monitor relies on.)
    let zero_bool = next_nid;
    next_nid += 1;
    appended.push(format!("{zero_bool} zero {bool_sort}"));
    let ante_prev = next_nid;
    next_nid += 1;
    appended.push(format!("{ante_prev} state {bool_sort} {ANTE_PREV_SYMBOL}"));
    let ante_prev_init = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{ante_prev_init} init {bool_sort} {ante_prev} {zero_bool}"
    ));
    let ante_prev_next = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{ante_prev_next} next {bool_sort} {ante_prev} {ante}"
    ));

    // bad = ante_prev && !C; named for `observe`.
    let bad_cond = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{bad_cond} and {bool_sort} {ante_prev} {cons_fail} {BAD_COND_SYMBOL}"
    ));
    let bad_line = next_nid;
    appended.push(format!("{bad_line} bad {bad_cond}"));

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// mununu#492 — append a `bad` monitor for `AG (COMPOUND)` where `COMPOUND` is a
/// boolean expression tree over `And`/`Or`/`Not`/`True`/`False`/`Predicate` mu-calc
/// nodes; each `Predicate` leaf resolves via state cell → output net → primary input.
///
/// Widens [`emit_ag_state_atom_monitor`] to (a) compound boolean invariants, and
/// (b) atoms bound to primary inputs (essential for zero-state models where every
/// atom is input-only). The bad line is `bad = !(compound_expr)`; the final NOT
/// node carries [`BAD_COND_SYMBOL`].
///
/// Signal-resolution fallback is state → output → **input**: the primary-input
/// fallback is what zero-state models need. Under safety-monitor semantics this
/// is sound — a `bad` reachable under some input assignment IS a real
/// counterexample (the SVA property is `AG(compound)` universally-quantified over
/// inputs), no different from a witness on a stateful design where the counterexample
/// is `(state_trajectory, input_schedule)`.
pub fn emit_ag_boolean_invariant_monitor(
    content: &str,
    formula: &crate::mu_calculus::Formula,
    root: crate::mu_calculus::NodeId,
    reset_pinned: bool,
) -> Result<String, AdapterError> {
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/bad_monitor: {}", e.message);
        e
    })?;

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let bool_sort = find_or_make_bool_sort(&file, &mut next_nid, &mut appended);

    let inv_expr = compile_boolean_subtree(
        formula,
        root,
        &file,
        reset_pinned,
        bool_sort,
        &mut next_nid,
        &mut appended,
    )?;

    // bad = !(compound); the final NOT carries BAD_COND_SYMBOL.
    let bad_cond = next_nid;
    next_nid += 1;
    appended.push(format!(
        "{bad_cond} not {bool_sort} {inv_expr} {BAD_COND_SYMBOL}"
    ));
    let bad_line = next_nid;
    appended.push(format!("{bad_line} bad {bad_cond}"));

    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast;
    use crate::adapter::btor2::concrete_oracle::{
        AgOracle, ag_implies_next, ag_state_atom, reachable_register_states,
    };
    use std::collections::HashMap;

    // 2-bit FSM cycling 0→1→2→0 (never 3); deterministic (input-free). Same shape
    // as concrete_oracle's CYCLE_FSM so the monitor + the oracle see one design.
    const CYCLE_FSM: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 state
4 zero 2
5 one 2
6 constd 2 2
7 eq 1 3 6
8 add 2 3 5
9 ite 2 7 4 8
10 next 2 3 9
11 init 2 3 4
";

    /// Is `mununu_bad` true at any reachable state of the augmented design?
    /// (Re-enumerates, so any synthesised latch state is included.)
    fn bad_is_reachable(augmented: &str) -> bool {
        let file = parser::parse(augmented).expect("augmented btor2 parses");
        let reach = reachable_register_states(&file, 256, 8).expect("reach");
        let observe = vec![BAD_COND_SYMBOL.to_string()];
        for st in &reach.states {
            let regs: HashMap<String, u128> = st.iter().map(|(k, v)| (k.clone(), *v)).collect();
            let out = bit_blast::simulate_one_step_observe(&file, &regs, &HashMap::new(), &observe)
                .expect("observe step");
            if out.observed.get(BAD_COND_SYMBOL).copied().unwrap_or(0) != 0 {
                return true;
            }
        }
        false
    }

    #[test]
    fn state_atom_monitor_parses_and_has_one_bad() {
        let out = emit_ag_state_atom_monitor(CYCLE_FSM, "state", CmpOp::Ne, 3, false).unwrap();
        let file = parser::parse(&out).expect("parses");
        let bads = file
            .lines
            .iter()
            .filter(|l| matches!(l.node, Node::Bad { .. }))
            .count();
        assert_eq!(bads, 1, "exactly one bad monitor");
    }

    #[test]
    fn state_atom_monitor_matches_oracle_holds() {
        // AG(state != 3): 3 unreachable → oracle Holds → bad must NOT be reachable.
        assert_eq!(
            ag_state_atom(
                &parser::parse(CYCLE_FSM).unwrap(),
                "state",
                CmpOp::Ne,
                3,
                256,
                8,
                false
            )
            .unwrap(),
            AgOracle::Holds
        );
        let out = emit_ag_state_atom_monitor(CYCLE_FSM, "state", CmpOp::Ne, 3, false).unwrap();
        assert!(!bad_is_reachable(&out), "Holds ⇒ bad unreachable");
    }

    #[test]
    fn state_atom_monitor_matches_oracle_violated() {
        // AG(state != 1): 1 reachable → oracle Violated → bad must be reachable.
        assert!(matches!(
            ag_state_atom(
                &parser::parse(CYCLE_FSM).unwrap(),
                "state",
                CmpOp::Ne,
                1,
                256,
                8,
                false
            )
            .unwrap(),
            AgOracle::Violated(_)
        ));
        let out = emit_ag_state_atom_monitor(CYCLE_FSM, "state", CmpOp::Ne, 1, false).unwrap();
        assert!(bad_is_reachable(&out), "Violated ⇒ bad reachable");
    }

    #[test]
    fn implies_next_monitor_matches_oracle_holds() {
        // AG(state==0 |=> state==1): from 0 the next is always 1 → Holds → no bad.
        let ante = OracleAtom::new("state", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state", CmpOp::Eq, 1);
        assert_eq!(
            ag_implies_next(
                &parser::parse(CYCLE_FSM).unwrap(),
                &ante,
                &cons,
                256,
                8,
                false
            )
            .unwrap(),
            AgOracle::Holds
        );
        let out = emit_ag_implies_next_monitor(CYCLE_FSM, &ante, &cons, false).unwrap();
        assert!(!bad_is_reachable(&out), "Holds ⇒ bad unreachable");
    }

    #[test]
    fn implies_next_monitor_matches_oracle_violated() {
        // AG(state==0 |=> state==2): from 0 the next is 1, not 2 → Violated → bad.
        let ante = OracleAtom::new("state", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state", CmpOp::Eq, 2);
        assert!(matches!(
            ag_implies_next(
                &parser::parse(CYCLE_FSM).unwrap(),
                &ante,
                &cons,
                256,
                8,
                false
            )
            .unwrap(),
            AgOracle::Violated(_)
        ));
        let out = emit_ag_implies_next_monitor(CYCLE_FSM, &ante, &cons, false).unwrap();
        assert!(bad_is_reachable(&out), "Violated ⇒ bad reachable");
    }

    #[test]
    fn implies_next_synthesises_the_latch() {
        let ante = OracleAtom::new("state", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state", CmpOp::Eq, 1);
        let out = emit_ag_implies_next_monitor(CYCLE_FSM, &ante, &cons, false).unwrap();
        let file = parser::parse(&out).expect("parses");
        let symbols = parser::collect_symbols(&file);
        assert!(
            symbols.values().any(|s| s == ANTE_PREV_SYMBOL),
            "the |=> monitor latches the antecedent"
        );
    }

    #[test]
    fn non_state_signal_is_rejected() {
        assert!(emit_ag_state_atom_monitor(CYCLE_FSM, "nope", CmpOp::Eq, 0, false).is_err());
    }
}
