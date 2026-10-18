//! ⊥-rescue bridge — reduce a mu-calculus **safety invariant** to a BTOR2 `bad`
//! monitor and decide it with the subprocess reachability portfolio.
//!
//! verify_auto's internal engines (exact-symbolic BDD, cube CEGAR) leave a
//! property ⊥ when it escapes their reach — a datapath wider than the exact
//! engine's 40-bit cone cap, or an input the cube abstraction cannot pin. The
//! subprocess members (btormc's k-induction, Pono's IC3) decide such a property
//! SYMBOLICALLY, with no enumeration. This module is the bridge: it turns the
//! `AG(state ⋈ value)` fragment of a ⊥ property into the `bad` monitor
//! [`crate::adapter::btor2::bad_monitor`] emits, decides it via
//! [`crate::adapter::reach_portfolio`], and maps the reachability verdict back to
//! HOLDS / VIOLATED.
//!
//! # Conservative by construction (soundness-critical)
//!
//! [`reduce_ag_invariant`] returns `Some` ONLY for the exact
//! `nu X. ((signal ⋈ value) && [] X)` shape — a single literal comparison of one
//! register under an UNCONSTRAINED box. Every other shape returns `None`:
//!
//! - an implication (`|->` → `nu X. ((!a || b) && []X)`, `|=>` →
//!   `nu X. ((!a || []b) && []X)`) — the body is an `Or`, not a bare atom;
//! - an `EF` cover (`mu X. (b || <>X)`) — the root is `Mu`, not `Nu`;
//! - a compound / relational body (`a && b`, `x == y`) — not a single `Cmp`;
//! - a **guarded** box (`[(labels=…)] X`) — a stronger property than the
//!   all-reachable-states monitor checks.
//!
//! A `None` is a sound abstention: the caller leaves the property ⊥. A monitor
//! built for the WRONG shape would be a soundness bug, so the classifier errs
//! toward `None` — it never guesses. Once reduced, the monitor's verdict is sound
//! because every reachability-portfolio member is sound (Bruns–Godefroid for the
//! exact engine within its cap; k-induction / IC3 for the subprocess members).
//!
//! # Status — validated primitive, no current target (2026-07-07)
//!
//! This bridge is **proven sound but not yet wired into `verify_auto`**, because a
//! payoff hunt found no real target for it:
//!
//! - The **reducibility census** ([`e2e_reach_rescue_reducibility_census`] in
//!   `tests/differential_oracle_e2e.rs`) measured the whole OpenTitan FSM corpus in
//!   the `mununu-sva` image: the internal portfolio (exact-symbolic → cube CEGAR)
//!   decides **all 19 safety properties across 18 designs — 0 ⊥**. Nothing to rescue.
//! - A literature/RTL hunt for a wide single-register `AG(reg ⋈ const)` invariant
//!   genuinely beyond the cube abstraction came up structurally empty: wide
//!   invariants that HOLD are almost always 1-inductive (the cube engine closes them
//!   in one QF_BV query), and the ones beyond cube are relational/conditional (this
//!   reducer rejects them) or have astronomically deep counterexamples. The one lead
//!   (ibex PMP bounds) turned out purely combinational (0 state) — btormc "deciding"
//!   it is a trivial bound-0 SAT, not the deep reachability the portfolio targets.
//!
//! So the module is **banked**: the primitive + the census are kept as a standing
//! re-anchoring instrument (the census fires the moment a future corpus design goes
//! ⊥-and-reducible), but no user-facing `portfolio-mc` surface is wired — that would
//! be surface for a capability with no demonstrated real payoff. The mechanism is
//! validated end-to-end by the `#[ignore]`d `e2e_rescue_*` tests below (both verdict
//! directions + the beyond-40-bit-cap case, with live btormc/Pono).

use crate::adapter::btor2::bad_monitor::{
    emit_ag_boolean_invariant_monitor, emit_ag_state_atom_monitor,
};
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_expr};
use crate::adapter::reach_portfolio::{ReachOutcome, ReachVerdict, decide_reach_portfolio};
use crate::mu_calculus::{Formula, FormulaVarId, Guard, ModalKind, Node, NodeId};

/// A reducible single-atom AG safety invariant: `AG(signal ⋈ value)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgInvariant {
    /// The register the invariant constrains (a state cell, checked by the
    /// monitor emitter — a non-state signal makes the rescue abstain).
    pub signal: String,
    /// The comparison operator (`==`, `!=`, `<`, …); the monitor watches its
    /// NEGATION (the violation).
    pub op: CmpOp,
    /// The literal right-hand side of the comparison.
    pub value: u128,
}

/// The rescue verdict for a single property, from the reachability portfolio's
/// verdict on the property's `bad` monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescueVerdict {
    /// `bad` proven unreachable ⇒ the invariant holds in every reachable state.
    Holds,
    /// `bad` reachable ⇒ a reachable state violates the invariant.
    Violated,
    /// Undecided by every member, or a soundness contradiction alarm — never a
    /// silently-picked side.
    Inconclusive,
}

/// Conservatively classify `formula` as the simple AG invariant
/// `nu X. ((signal ⋈ value) && [] X)`. Returns `None` for every other shape (see
/// the module docs) so the reduction never mis-reduces a property it does not
/// fully recognise.
pub fn reduce_ag_invariant(formula: &Formula) -> Option<AgInvariant> {
    // Root must be `nu X. BODY` (a greatest fixpoint — the AG shape; an `EF`
    // cover is `mu X.`, rejected here).
    let Node::Nu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    // BODY must be `atom && [] X` — an `And` of the invariant body and the box
    // recursion. An implication body is an `Or`, so this rejects `|->` / `|=>`.
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    // Exactly one side is the box-to-var recursion; the other is the atom.
    let (atom_id, modal_id) = classify_and(formula, *l, *r, *var)?;
    // The box must be UNCONSTRAINED (a guarded box is a different, stronger
    // property than the monitor — which checks EVERY reachable state — verifies).
    let Node::Modal {
        kind: ModalKind::Box,
        guard,
        target,
    } = formula.node(modal_id)
    else {
        return None;
    };
    if *guard != Guard::default() {
        return None;
    }
    match formula.node(*target) {
        Node::Variable(v) if *v == *var => {}
        _ => return None,
    }
    // The atom must be a single literal comparison of one register.
    let Node::Predicate(atom) = formula.node(atom_id) else {
        return None;
    };
    match parse_predicate_expr(atom) {
        Ok(PredicateExpr::Cmp {
            register,
            op,
            value,
        }) => Some(AgInvariant {
            signal: register,
            op,
            value: value as u128,
        }),
        // A relational (`CmpReg`), compound, or unparseable atom is not reducible.
        _ => None,
    }
}

/// mununu#492 — reduce `nu X. (COMPOUND && [] X)` to the [`NodeId`] of `COMPOUND`
/// when `COMPOUND` is a boolean tree over `And`/`Or`/`Not`/`True`/`False`/`Predicate`
/// nodes, with each `Predicate` leaf parseable as a single register-comparison
/// (`PredicateExpr::Cmp`) — widening [`reduce_ag_invariant`] which accepts only a
/// single atomic body. Non-boolean leaves (`CmpReg`, `CmpRegAddend`, `Select`) are
/// rejected here so the compound emitter never receives a leaf it cannot compile;
/// modal / fixpoint nodes inside `COMPOUND` are rejected — the outer `AG` is the only
/// modality allowed.
///
/// **Soundness rationale.** The compound body compiles to a pure boolean expression
/// over the (state × input) universe; the emitted `bad` is its negation, so a
/// reachable `bad` is a genuine reachable state × input assignment falsifying the
/// invariant. `AG(COMPOUND)` is universally-quantified over ALL trajectories AND
/// input schedules, so the counterexample transfers unchanged. Same soundness
/// argument as [`emit_ag_state_atom_monitor`], generalised from a single leaf.
pub fn reduce_ag_boolean_body(formula: &Formula) -> Option<NodeId> {
    // Outer shape must be `nu X. (BODY && [] X)` — same as reduce_ag_invariant.
    let Node::Nu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (compound_id, modal_id) = classify_and(formula, *l, *r, *var)?;
    let Node::Modal {
        kind: ModalKind::Box,
        guard,
        target,
    } = formula.node(modal_id)
    else {
        return None;
    };
    if *guard != Guard::default() {
        return None;
    }
    match formula.node(*target) {
        Node::Variable(v) if *v == *var => {}
        _ => return None,
    }
    // The compound subtree must be pure boolean-of-single-atom-leaves — no
    // modals / fixpoints / variables, and every Predicate leaf parses as Cmp.
    if !is_compilable_boolean_body(formula, compound_id) {
        return None;
    }
    Some(compound_id)
}

/// mununu#503 — the `|=>` shape: `nu X. ((¬A ∨ [] C) ∧ [] X)`, i.e. `AG (A → AX C)`.
///
/// Returns `(neg_ante_id, cons_id)` — the `¬A` disjunct **as written** and the boolean subtree
/// under the consequent box. The caller recovers `A` by negating; see
/// [`crate::adapter::btor2::bad_monitor::emit_ag_implies_next_compound_monitor`].
///
/// This is the sibling of [`reduce_ag_boolean_body`], which handles the OVERLAPPED `|->`
/// (`nu X. ((¬A ∨ C) ∧ [] X)` — no inner box). The two are distinguished by exactly one thing:
/// whether a disjunct of the implication is a box.
///
/// # What it deliberately does not match
///
/// - **Both disjuncts boxes** (`[] p ∨ [] q`) — ambiguous about which is the consequent, so the
///   shape match stays unambiguous rather than guessing.
/// - **A nested box consequent** (`a |=> ##1 b` ⇒ `[] [] b`, and multi-element antecedents, which
///   [`crate::adapter::slang::translate`] emits as nested `(!(b) || [] (!(a) || [] c))`). The inner
///   box is not a compilable boolean body, so these decline — honestly, rather than by
///   mis-compiling a modality into a boolean.
/// - **`[] X` as the consequent.** The recursion variable is not a boolean body either, so a
///   degenerate `nu X. ((¬A ∨ [] X) ∧ [] X)` cannot be mistaken for an implication.
pub fn reduce_ag_implies_next(formula: &Formula) -> Option<(NodeId, NodeId)> {
    let Node::Nu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (implication_id, modal_id) = classify_and(formula, *l, *r, *var)?;
    // The outer recursion box must be unconstrained, as in reduce_ag_boolean_body.
    let Node::Modal {
        kind: ModalKind::Box,
        guard,
        target,
    } = formula.node(modal_id)
    else {
        return None;
    };
    if *guard != Guard::default() {
        return None;
    }
    match formula.node(*target) {
        Node::Variable(v) if *v == *var => {}
        _ => return None,
    }

    // `¬A ∨ [] C` — exactly one disjunct is a box (the consequent).
    let Node::Or(a, b) = formula.node(implication_id) else {
        return None;
    };
    let (neg_ante, cons_modal) = match (is_plain_box(formula, *a), is_plain_box(formula, *b)) {
        (true, false) => (*b, *a),
        (false, true) => (*a, *b),
        _ => return None,
    };
    let Node::Modal {
        kind: ModalKind::Box,
        guard,
        target: cons,
        ..
    } = formula.node(cons_modal)
    else {
        return None;
    };
    if *guard != Guard::default() {
        return None;
    }
    // Both sides must compile to pure boolean expressions — a modal or fixpoint inside either is
    // out of the fragment.
    if !is_compilable_boolean_body(formula, neg_ante) || !is_compilable_boolean_body(formula, *cons)
    {
        return None;
    }
    Some((neg_ante, *cons))
}

/// A `[…]` box node, whatever its target — as opposed to [`is_box_to_var`], which additionally
/// requires the target to be the recursion variable.
fn is_plain_box(formula: &Formula, id: NodeId) -> bool {
    matches!(
        formula.node(id),
        Node::Modal {
            kind: ModalKind::Box,
            ..
        }
    )
}

/// Walk the subtree; verify it is only And/Or/Not/True/False/Predicate and every
/// Predicate leaf parses as `PredicateExpr::Cmp`. A single modal / fixpoint /
/// variable / CmpReg / CmpRegAddend / Select rejects the whole tree.
///
/// mununu#503 — uses [`parse_predicate_atom_bool`], NOT the strict
/// [`parse_predicate_expr`]. A **bare 1-bit identifier** (`trigger_active`,
/// `event_detected_o`) is how the SVA translator emits "signal is true", and the strict
/// parser requires an operator, so it returns `Err` and rejected the ENTIRE tree. That is
/// why every sysrst `|->` property was classified `safety-shape-not-reducible` and never
/// reached the rescue lane at all — a whole property class turned away at the gate by a
/// leaf-parsing detail.
///
/// The relaxed reading is `signal != 0`, not `== 1`: for a multi-bit signal `== 1` would
/// spuriously exclude the truthy values 2, 3, … (see that function's own soundness note).
/// [`crate::adapter::btor2::bad_monitor`]'s compound compiler uses the same entry point, so
/// the gate and the compiler cannot disagree about what a leaf means — the disagreement
/// being precisely this bug.
pub(crate) fn is_compilable_boolean_body(formula: &Formula, id: NodeId) -> bool {
    match formula.node(id) {
        Node::True | Node::False => true,
        Node::Not(inner) => is_compilable_boolean_body(formula, *inner),
        Node::And(l, r) | Node::Or(l, r) => {
            is_compilable_boolean_body(formula, *l) && is_compilable_boolean_body(formula, *r)
        }
        Node::Predicate(atom) => {
            matches!(
                crate::adapter::btor2::predicate_expr::parse_predicate_atom_bool(atom),
                Ok(PredicateExpr::Cmp { .. })
            )
        }
        _ => false,
    }
}

/// Plan 2 / PR C (2026-10-18) — the bounded-chain safety shape: `nu X. (BODY && [] X)` whose BODY
/// is a bounded-depth combination of boolean blocks, `&&`, `||` and unguarded boxes in which every
/// `||` has at most one box-bearing side ([`bad_monitor::chain_is_exact`]) — the SVA translator's
/// `$rose(x) |-> ##[0:k] y` (a disjunct at every level), `a |=> ##n c` (nested boxes), the
/// `throughout` conjunctions, and the one-box `|=>` the sibling reducer handles — optionally ending
/// in ONE weak-until block `nu Z. (e && (b || [] Z))`, the lowering of `e throughout b[->1]`
/// ([`bad_monitor::weak_until_parts`]; the compiler judges it with a pending latch). Returns the
/// BODY node, or `None` for any other shape or a depth over [`MAX_CHAIN_DEPTH`]. On these shapes
/// the one-path monitor ([`bad_monitor::emit_bounded_chain_monitor`]) is EXACT, so the reach
/// portfolio DECIDES them: `bad` reachable ⇔ some path falsifies the body at some cycle. Before
/// this reducer the rescue declined every one of them in 0 ms — on the OpenTitan `edn` IP, 21 of 54
/// non-trivial rows (the five duplicated-counter fault-flag chains, the sixteen 32-bit
/// bus-stability rows) and csrng's ten control checks never reached an engine.
pub fn reduce_bounded_chain(formula: &Formula) -> Option<NodeId> {
    let Node::Nu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (block, recur) = classify_and(formula, *l, *r, *var)?;
    if !matches!(formula.node(recur), Node::Modal { guard, .. } if *guard == Guard::default()) {
        return None;
    }
    let depth = crate::adapter::btor2::bad_monitor::linear_depth(formula, block)?;
    // A box-free body is the boolean reducer's (it ran first) — unless a weak-until block sits in
    // it (`A |-> (e throughout b[->1])`), which only the chain compiler judges.
    let has_until =
        !crate::adapter::btor2::bad_monitor::weak_until_sites(formula, block).is_empty();
    if (depth == 0 && !has_until) || depth > MAX_CHAIN_DEPTH {
        return None;
    }
    crate::adapter::btor2::bad_monitor::chain_is_exact(formula, block).then_some(block)
}

/// The deepest chain the rescue reduces — the SVA translator's own unroll cap (`MAX_SEQ_UNROLL`),
/// so every shape it emits is in reach; a deeper hand-written formula declines rather than
/// growing an unbounded monitor.
pub const MAX_CHAIN_DEPTH: usize = 64;

/// Plan 2 / B2 (2026-10-18) — the plain reachability shape `mu Z. (p || <> Z)` (`EF p`) with `p` a
/// compilable boolean body: the node of `p`, or `None` when the formula is any other shape. Its
/// DUAL is the AG invariant `nu Z. (!p && [] Z)`, which [`reach_portfolio_rescue_diagnosed`] already
/// reduces to a `bad := p` monitor — so a ⊥ `EF p` is decided by the reach portfolio on the dual:
/// `bad` reachable ⇒ `EF p` HOLDS (with the path), `bad` proved unreachable ⇒ `EF p` VIOLATED.
/// `<>` here is "some successor over some input", exactly BTOR2 reachability — the same reading the
/// exact engine gives `EF`.
pub fn reduce_ef_target(formula: &Formula) -> Option<NodeId> {
    let Node::Mu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    let Node::Or(l, r) = formula.node(*body) else {
        return None;
    };
    let is_diamond_to_var = |id: NodeId| {
        matches!(
            formula.node(id),
            Node::Modal { kind: ModalKind::Diamond, guard, target }
                if *guard == Guard::default()
                    && matches!(formula.node(*target), Node::Variable(v) if *v == *var)
        )
    };
    let p = match (is_diamond_to_var(*l), is_diamond_to_var(*r)) {
        (true, false) => *r,
        (false, true) => *l,
        _ => return None,
    };
    is_compilable_boolean_body(formula, p).then_some(p)
}

/// Return `(atom_node, modal_node)` iff exactly one of `l` / `r` is a box-to-`var`
/// and the other is not. `None` if neither or both look like the box recursion,
/// keeping the shape match unambiguous.
fn classify_and(
    formula: &Formula,
    l: NodeId,
    r: NodeId,
    var: FormulaVarId,
) -> Option<(NodeId, NodeId)> {
    match (
        is_box_to_var(formula, l, var),
        is_box_to_var(formula, r, var),
    ) {
        (true, false) => Some((r, l)),
        (false, true) => Some((l, r)),
        _ => None,
    }
}

/// A `[…] X` box whose target is exactly the fixpoint variable `var` (guard
/// unchecked here — [`reduce_ag_invariant`] enforces the unconstrained guard).
fn is_box_to_var(formula: &Formula, id: NodeId, var: FormulaVarId) -> bool {
    matches!(
        formula.node(id),
        Node::Modal { kind: ModalKind::Box, target, .. }
            if matches!(formula.node(*target), Node::Variable(v) if *v == var)
    )
}

/// Reduce a ⊥ safety property to a `bad` monitor over the lifted design and decide
/// it with the reachability portfolio.
///
/// Returns `None` when the property is not a reducible AG invariant (the caller
/// leaves it ⊥) or the monitor cannot be built (e.g. the signal does not resolve
/// to a state cell — the emitter's own check). `design_btor2` MUST be the same
/// reset/config-pinned BTOR2 verify_auto reasoned over, so the reachable-state set
/// matches; `reset_pinned` mirrors verify_auto's reset-gating (whether recognized
/// resets were pinned inactive) so the monitor resolves the signal through the
/// async-reset mux consistently.
///
/// On `Some`, the second component is the raw [`ReachOutcome`] (which engines
/// decided each side), for diagnostics / a witness note.
/// mununu#503 — why the safety-rescue lane declined a property, when it did.
///
/// Every decline used to be a bare `None`, and one of them threw away a fully descriptive
/// `AdapterError` via `.ok()?`. A consumer following the `bottom-reason` note's advice — *"check
/// for a sibling `bit-cap` / `abstained` note"* — found nothing, because the lane emitted nothing.
/// The trail ended, and a residual ⊥ could not be attributed at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescueDecline {
    /// Neither reducer matched the formula's shape (`AG(atom)` / `AG(boolean-of-atoms)`).
    ShapeNotReducible,
    /// The shape reduced, but the monitor emitter refused it — carries the emitter's own
    /// message, which names the offending leaf. This is the one that was being discarded.
    MonitorEmissionFailed(String),
    /// The emitted monitor did not re-parse as BTOR2 (an internal inconsistency).
    MonitoredDesignUnparseable(String),
}

impl std::fmt::Display for RescueDecline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RescueDecline::ShapeNotReducible => write!(
                f,
                "the formula matched none of `AG(atom)`, `AG(boolean-of-atoms)`, `A |=> C`, or a \
                 bounded box chain with at most one box-bearing side per `||` and at most one \
                 `e throughout b[->1]` block, at the bottom (depth ≤ {})",
                MAX_CHAIN_DEPTH
            ),
            RescueDecline::MonitorEmissionFailed(m) => {
                write!(f, "the bad-monitor emitter refused the reduced body: {m}")
            }
            RescueDecline::MonitoredDesignUnparseable(m) => {
                write!(f, "the emitted monitor did not re-parse: {m}")
            }
        }
    }
}

/// The compound-shape emission chain: `AG(boolean)` first, then `AG(A → AX C)`.
///
/// Ordered so the shipped `|->` / plain-invariant path emits byte-for-byte as before; the `|=>`
/// reducer is reached only when the body is not a pure boolean invariant. A property matching
/// NEITHER declines as [`RescueDecline::ShapeNotReducible`], which is what the `bottom-reason`
/// note reports.
fn emit_boolean_or_implies_next(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
) -> Result<String, RescueDecline> {
    if let Some(root) = reduce_ag_boolean_body(formula) {
        return emit_ag_boolean_invariant_monitor(design_btor2, formula, root, reset_pinned)
            .map_err(|e| RescueDecline::MonitorEmissionFailed(e.message));
    }
    if let Some((neg_ante, cons)) = reduce_ag_implies_next(formula) {
        return crate::adapter::btor2::bad_monitor::emit_ag_implies_next_compound_monitor(
            design_btor2,
            formula,
            neg_ante,
            cons,
            reset_pinned,
        )
        .map_err(|e| RescueDecline::MonitorEmissionFailed(e.message));
    }
    // PR C — the bounded chains (nested boxes, a boolean beside each): the one-path monitor is a
    // decider on them (`chain_is_exact`).
    let block = reduce_bounded_chain(formula).ok_or(RescueDecline::ShapeNotReducible)?;
    crate::adapter::btor2::bad_monitor::emit_bounded_chain_monitor(
        design_btor2,
        formula,
        block,
        reset_pinned,
    )
    .map(|(monitored, _depth)| monitored)
    .map_err(|e| RescueDecline::MonitorEmissionFailed(e.message))
}

/// Thin wrapper preserving the original signature for existing callers.
pub fn reach_portfolio_rescue(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
) -> Option<(RescueVerdict, ReachOutcome)> {
    reach_portfolio_rescue_diagnosed(design_btor2, formula, reset_pinned).ok()
}

/// As [`reach_portfolio_rescue`], but reports WHY it declined instead of returning a bare `None`.
pub fn reach_portfolio_rescue_diagnosed(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
) -> Result<(RescueVerdict, ReachOutcome), RescueDecline> {
    // Try the existing single-atom reducer first — preserves byte-for-byte
    // emission on the shipped case. Fall back to the compound-atom reducer
    // (mununu#492) for `AG(compound_boolean_expression)` and for atoms bound
    // to primary inputs (essential for zero-state models).
    let monitored = if let Some(inv) = reduce_ag_invariant(formula) {
        // bad = !(signal ⋈ value); the emitter validates `signal` resolves to a
        // state cell / output net (a non-state / non-output signal ⇒ Err ⇒
        // fall through to the compound path below).
        match emit_ag_state_atom_monitor(design_btor2, &inv.signal, inv.op, inv.value, reset_pinned)
        {
            Ok(s) => s,
            Err(_) => {
                // The single-atom emitter refused the signal (zero-state atom
                // on a primary input, say). Try the compound path — its leaf
                // resolution includes primary inputs.
                emit_boolean_or_implies_next(design_btor2, formula, reset_pinned)?
            }
        }
    } else {
        // Non-single-atom shape — try compound, then the `|=>` implication.
        emit_boolean_or_implies_next(design_btor2, formula, reset_pinned)?
    };
    let file = parser::parse(&monitored)
        .map_err(|e| RescueDecline::MonitoredDesignUnparseable(e.message))?;
    let outcome = decide_reach_portfolio(&file);
    let verdict = match outcome.verdict {
        ReachVerdict::Unreachable => RescueVerdict::Holds,
        ReachVerdict::Reachable => RescueVerdict::Violated,
        // Undecided, or a contradiction alarm — never silently pick a side.
        ReachVerdict::Unknown | ReachVerdict::Contradiction => RescueVerdict::Inconclusive,
    };
    Ok((verdict, outcome))
}

/// The concrete counterexample for a VIOLATED AG-safety property: the SHALLOWEST `Init → bad`
/// path native BMC finds within `max_k` steps (plus the property's own depth), with the input
/// assignment at every step.
///
/// The body may be any bounded-depth combination of boolean blocks, `&&`, `||` and boxes — every
/// `|->` / `|=>` / `##n` shape the SVA translator emits — compiled by
/// [`emit_bounded_safety_witness_monitor`](crate::adapter::btor2::bad_monitor::emit_bounded_safety_witness_monitor),
/// which reads it along ONE path. A path that falsifies that reading also falsifies the
/// formula (a box holds only if every successor satisfies its target), so a returned trace is
/// always a genuine counterexample; the converse can fail (`[] a || [] b` can be false at a state
/// although every single path satisfies `a` or `b` next), and then there is no trace. The monitor's
/// history latches (`mununu_wit_…`) appear in the trace beside the design's registers.
pub fn safety_witness(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
    max_k: u32,
) -> Option<crate::adapter::btor2::native_bmc::BmcTrace> {
    let Node::Nu { var, body } = formula.node(formula.root()) else {
        return None;
    };
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (block, recur) = classify_and(formula, *l, *r, *var)?;
    if !matches!(formula.node(recur), Node::Modal { guard, .. } if *guard == Guard::default()) {
        return None;
    }
    let (monitored, depth) =
        crate::adapter::btor2::bad_monitor::emit_bounded_safety_witness_monitor(
            design_btor2,
            formula,
            block,
            reset_pinned,
        )
        .ok()?;
    let file = parser::parse(&monitored).ok()?;
    let k = max_k.saturating_add(u32::try_from(depth).ok()?);
    match crate::adapter::btor2::native_bmc::bmc_bad_reachable_witness(&file, k).ok()? {
        (crate::adapter::btor2::native_bmc::BmcOutcome::Violated { .. }, trace) => trace,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::parser as mu_parser;

    fn parse(s: &str) -> Formula {
        mu_parser::parse(s).unwrap_or_else(|e| panic!("parse `{s}`: {e:?}"))
    }

    /// 2-bit counter 0→1→2→3→0.
    const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                           5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";

    fn design_states(
        trace: &crate::adapter::btor2::native_bmc::BmcTrace,
    ) -> Vec<Vec<(String, u64)>> {
        trace
            .states
            .iter()
            .map(|st| {
                st.iter()
                    .filter(|(k, _)| {
                        !k.starts_with(crate::adapter::btor2::bad_monitor::WITNESS_LATCH_PREFIX)
                    })
                    .cloned()
                    .collect()
            })
            .collect()
    }

    /// A two-step consequent (`cnt == 1 |-> ##2 cnt != 3`, which the counter breaks): the witness
    /// is the counting path from reset to the violation, ending where the consequent fails.
    #[test]
    fn a_multi_step_safety_violation_has_a_witness() {
        let f = parse("nu X. (((!((cnt == 1))) || [] [] (cnt != 3)) && [] X)");
        let trace = safety_witness(COUNTER, &f, false, 10).expect("a witness");
        let cnt = |v: u64| vec![("cnt".to_string(), v)];
        assert_eq!(design_states(&trace), vec![cnt(0), cnt(1), cnt(2), cnt(3)]);
        assert_eq!(trace.inputs.len(), trace.states.len());
        // The property that holds (`##2 cnt == 3`) has none.
        let holds = parse("nu X. (((!((cnt == 1))) || [] [] (cnt == 3)) && [] X)");
        assert!(safety_witness(COUNTER, &holds, false, 10).is_none());
    }

    /// No state at all: `a |=> b` over two inputs fails at depth 1, and the witness names the
    /// input at EVERY frame — `a` where the attempt starts, `!b` at the frame `bad` reads.
    #[test]
    fn an_input_only_violation_names_the_last_frames_input() {
        let design = "1 sort bitvec 1\n2 input 1 a\n3 input 1 b\n";
        let f = parse("nu X. (((!(a)) || [] b) && [] X)");
        let trace = safety_witness(design, &f, false, 5).expect("a witness");
        assert_eq!(trace.inputs.len(), 2);
        let at = |j: usize, n: &str| {
            trace.inputs[j]
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| *v)
        };
        assert_eq!(at(0, "a"), Some(1));
        assert_eq!(at(1, "b"), Some(0));
    }

    /// A fixpoint inside the body (`AG AF`) is not a bounded shape: no witness, never a wrong one.
    #[test]
    fn an_unbounded_body_has_no_safety_witness() {
        let f = parse("nu X. ((mu Y. ((cnt == 3) || [] Y)) && [] X)");
        assert!(safety_witness(COUNTER, &f, false, 10).is_none());
    }

    /// PR C — a bounded response window the one-box reducer declines: on the 2-bit counter
    /// (0→1→2→3→0, no input) `cnt == 1 |-> ##[0:2] cnt == 3` HOLDS (cnt reaches 3 two cycles later)
    /// and `cnt == 1 |-> ##[0:1] cnt == 3` is VIOLATED. Before the chain reducer both declined.
    #[test]
    fn bounded_chain_decides_a_response_window() {
        let holds = parse(
            "nu X. (((!((cnt == 1))) || ((cnt == 3) || [] ((cnt == 3) || [] (cnt == 3)))) && [] X)",
        );
        assert!(reduce_bounded_chain(&holds).is_some());
        let (v, _) = reach_portfolio_rescue_diagnosed(COUNTER, &holds, false).expect("reduces");
        assert_eq!(v, RescueVerdict::Holds);
        let violated = parse("nu X. (((!((cnt == 1))) || ((cnt == 3) || [] (cnt == 3))) && [] X)");
        let (v2, _) = reach_portfolio_rescue_diagnosed(COUNTER, &violated, false).expect("reduces");
        assert_eq!(v2, RescueVerdict::Violated);
        assert!(safety_witness(COUNTER, &violated, false, 10).is_some());
    }

    /// PR C — `!en |=> ##1 $stable(q)` is a two-box chain with a register-equality leaf
    /// (`q == q__past`), which the one-box reducer's boolean gate rejects: with `q` free (`next q
    /// = d`) it is VIOLATED; with `q` held it HOLDS.
    #[test]
    fn bounded_chain_accepts_a_register_equality_leaf() {
        const FREE: &str = "1 sort bitvec 1\n2 input 1 en\n3 input 1 d\n4 zero 1\n5 state 1 q\n\
                            6 init 1 5 4\n7 next 1 5 3\n8 state 1 q__past\n9 init 1 8 4\n\
                            10 next 1 8 5\n";
        const HELD: &str = "1 sort bitvec 1\n2 input 1 en\n3 input 1 d\n4 zero 1\n5 state 1 q\n\
                            6 init 1 5 4\n7 next 1 5 5\n8 state 1 q__past\n9 init 1 8 4\n\
                            10 next 1 8 5\n";
        let f = parse("nu X. (((!((!(en != 0)))) || [] [] (q == q__past)) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_none(),
            "the one-box reducer declines two boxes"
        );
        assert!(reduce_bounded_chain(&f).is_some());
        let (v, _) = reach_portfolio_rescue_diagnosed(FREE, &f, false).expect("reduces");
        assert_eq!(v, RescueVerdict::Violated);
        let (v2, _) = reach_portfolio_rescue_diagnosed(HELD, &f, false).expect("reduces");
        assert_eq!(v2, RescueVerdict::Holds);
    }

    /// PR C — `A |=> (e throughout b[->1])` lowers to a box over a weak-until block
    /// (`nu Z. (e && (b || [] Z))`, mununu#731), which the chain compiler judges with a pending
    /// latch. A 1-bit `q` with its `q__past` shadow, a `hold` latch set by `start`: when `hold`
    /// is released by the FIRST `req` after `start`, `q` is stable from the cycle after `start` up
    /// to and including that `req` — HOLDS; when a `req` in the `start` cycle itself releases it,
    /// `q` may change while the obligation is still pending — VIOLATED, with a witness.
    #[test]
    fn bounded_chain_decides_a_weak_until_consequent() {
        // q' = (start | hold) ? q : d ; hold' = start | (hold & !req)
        const HELD_UNTIL_REQ: &str = "1 sort bitvec 1\n2 input 1 start\n3 input 1 req\n\
                                      4 input 1 d\n5 zero 1\n6 state 1 q\n7 init 1 6 5\n\
                                      8 state 1 q__past\n9 init 1 8 5\n10 next 1 8 6\n\
                                      11 state 1 hold\n12 init 1 11 5\n13 or 1 2 11\n\
                                      14 ite 1 13 6 4\n15 next 1 6 14\n16 not 1 3\n\
                                      17 and 1 11 16\n18 or 1 2 17\n19 next 1 11 18\n";
        // hold' = (start | hold) & !req — a `req` in the start cycle releases one cycle early
        const RELEASED_EARLY: &str = "1 sort bitvec 1\n2 input 1 start\n3 input 1 req\n\
                                      4 input 1 d\n5 zero 1\n6 state 1 q\n7 init 1 6 5\n\
                                      8 state 1 q__past\n9 init 1 8 5\n10 next 1 8 6\n\
                                      11 state 1 hold\n12 init 1 11 5\n13 or 1 2 11\n\
                                      14 ite 1 13 6 4\n15 next 1 6 14\n16 not 1 3\n\
                                      17 and 1 13 16\n18 next 1 11 17\n";
        let f = parse(
            "nu X. (((!((start == 1))) || [] (nu Z. ((q == q__past) && ((req == 1) || [] Z)))) \
             && [] X)",
        );
        assert!(
            reduce_ag_implies_next(&f).is_none(),
            "the one-box reducer declines a fixpoint consequent"
        );
        assert!(reduce_bounded_chain(&f).is_some());
        let (v, _) = reach_portfolio_rescue_diagnosed(HELD_UNTIL_REQ, &f, false).expect("reduces");
        assert_eq!(v, RescueVerdict::Holds);
        let (v2, _) = reach_portfolio_rescue_diagnosed(RELEASED_EARLY, &f, false).expect("reduces");
        assert_eq!(v2, RescueVerdict::Violated);
        assert!(safety_witness(RELEASED_EARLY, &f, false, 10).is_some());
        // two weak-until blocks, or one with a deeper box beside it, stay declined
        let two = parse(
            "nu X. (((nu Z. ((q == 1) && ((req == 1) || [] Z))) && (nu W. ((q == 0) && \
             ((req == 1) || [] W)))) && [] X)",
        );
        assert!(reduce_bounded_chain(&two).is_none());
        let shallower =
            parse("nu X. (((nu Z. ((q == 1) && ((req == 1) || [] Z))) && [] (q == 0)) && [] X)");
        assert!(reduce_bounded_chain(&shallower).is_none());
    }

    /// PR C — the shapes the one-path reading does not decide stay declined: `[] a || [] b`, a
    /// negated box, and a chain deeper than the cap.
    #[test]
    fn bounded_chain_declines_what_the_one_path_reading_cannot_decide() {
        let two_boxes = parse("nu X. (([] (cnt == 1) || [] (cnt == 2)) && [] X)");
        assert!(reduce_bounded_chain(&two_boxes).is_none());
        assert_eq!(
            reach_portfolio_rescue_diagnosed(COUNTER, &two_boxes, false),
            Err(RescueDecline::ShapeNotReducible)
        );
        let not_box = parse("nu X. ((!([] (cnt == 1))) && [] X)");
        assert!(reduce_bounded_chain(&not_box).is_none());
        let mut deep = "(cnt == 3)".to_string();
        for _ in 0..(MAX_CHAIN_DEPTH + 1) {
            deep = format!("((cnt == 3) || [] {deep})");
        }
        let too_deep = parse(&format!("nu X. (((!((cnt == 1))) || {deep}) && [] X)"));
        assert!(reduce_bounded_chain(&too_deep).is_none());
        let at_cap = {
            let mut d = "(cnt == 3)".to_string();
            for _ in 0..(MAX_CHAIN_DEPTH - 1) {
                d = format!("((cnt == 3) || [] {d})");
            }
            parse(&format!("nu X. (((!((cnt == 1))) || {d}) && [] X)"))
        };
        assert!(reduce_bounded_chain(&at_cap).is_some());
    }

    /// PR C — the seam between the translator's shapes and the reducer: on a 3-bit counter with a
    /// free `en` input, a dozen bounded chains (`##[m:n]` windows, `|=> ##n`, `throughout`
    /// conjunctions, a `##[0:k]` disjunct chain) decided by the portfolio on the monitor agree with
    /// the exact ROBDD engine on the formula — both polarities present.
    #[test]
    fn the_chain_monitor_agrees_with_the_exact_engine() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        const COUNTER_EN: &str = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                                  5 input 2 en\n6 state 1 cnt\n7 init 1 6 3\n8 add 1 6 4\n\
                                  9 ite 1 5 8 6\n10 next 1 6 9\n";
        let formulas = [
            // cnt == 1 |-> ##[0:2] cnt == 3  — en may stay low: violated
            "nu X. (((!((cnt == 1))) || ((cnt == 3) || [] ((cnt == 3) || [] (cnt == 3)))) && [] X)",
            // cnt == 7 |=> cnt == 0 || cnt == 7  — holds (wrap or hold)
            "nu X. (((!((cnt == 7))) || [] ((cnt == 0) || (cnt == 7))) && [] X)",
            // cnt == 1 |=> ##1 cnt != 0  — holds (two steps from 1 reach at most 3)
            "nu X. (((!((cnt == 1))) || [] [] (cnt != 0)) && [] X)",
            // cnt == 1 |-> (cnt != 0) throughout ##2  — holds
            "nu X. (((!((cnt == 1))) || ((cnt != 0) && [] ((cnt != 0) && [] (cnt != 0)))) && [] X)",
            // cnt == 6 |=> ##1 cnt != 0  — violated (6→7→0 with en high)
            "nu X. (((!((cnt == 6))) || [] [] (cnt != 0)) && [] X)",
            // cnt == 2 |-> ##[1:2] cnt == 3  — violated (en low)
            "nu X. (((!((cnt == 2))) || [] ((cnt == 3) || [] (cnt == 3))) && [] X)",
            // cnt == 2 |-> ##[0:1] cnt <= 3  (as != 4..7) — holds
            "nu X. (((!((cnt == 2))) || (((cnt == 2) || (cnt == 3)) || [] ((cnt == 2) || (cnt == 3)))) && [] X)",
            // always: cnt != 4 |=> cnt != 5  — violated (4→5 after a step from 3? 3→4 then 4→5): from cnt==3 (≠4) next could be 4 (≠5 ✓) … from cnt==4 (antecedent false) — the only way to 5 is from 4, so holds
            "nu X. (((!((cnt != 4))) || [] (cnt != 5)) && [] X)",
            // nested boxes with a conjunct: cnt == 0 |=> (cnt != 3) && [] (cnt != 3)  — holds
            "nu X. (((!((cnt == 0))) || [] ((cnt != 3) && [] (cnt != 3))) && [] X)",
            // cnt == 0 |=> ##1 cnt == 2  — violated (en low)
            "nu X. (((!((cnt == 0))) || [] [] (cnt == 2)) && [] X)",
            // cnt == 1 |=> (cnt != 0) throughout (cnt == 4)[->1]  — holds (0 comes after 7)
            "nu X. (((!((cnt == 1))) || [] (nu Z. ((cnt != 0) && ((cnt == 4) || [] Z)))) && [] X)",
            // cnt == 1 |=> (cnt != 3) throughout (cnt == 4)[->1]  — violated (3 precedes 4)
            "nu X. (((!((cnt == 1))) || [] (nu Z. ((cnt != 3) && ((cnt == 4) || [] Z)))) && [] X)",
            // cnt == 5 |=> (cnt != 1) throughout (cnt == 0)[->1]  — holds (6, 7, 0)
            "nu X. (((!((cnt == 5))) || [] (nu Z. ((cnt != 1) && ((cnt == 0) || [] Z)))) && [] X)",
            // cnt == 6 |=> (cnt != 0) throughout (cnt == 2)[->1]  — violated (7, 0 before 2)
            "nu X. (((!((cnt == 6))) || [] (nu Z. ((cnt != 0) && ((cnt == 2) || [] Z)))) && [] X)",
            // cnt == 3 |-> ((cnt != 0) throughout (cnt == 5)[->1]) at depth 0 beside a boolean — holds
            "nu X. (((!((cnt == 3))) || ((cnt == 3) && (nu Z. ((cnt != 0) && ((cnt == 5) || [] Z))))) && [] X)",
        ];
        let (mut holds_seen, mut violated_seen) = (0, 0);
        let mut disagreements = Vec::new();
        for text in formulas {
            let f = parse(text);
            assert!(reduce_bounded_chain(&f).is_some(), "{text}");
            let exact = exact_symbolic_verdict(COUNTER_EN, &f).expect("exact decides 3 bits");
            let (rescued, _) =
                reach_portfolio_rescue_diagnosed(COUNTER_EN, &f, false).expect("reduces");
            let want = match exact {
                ExactVerdict::Holds => {
                    holds_seen += 1;
                    RescueVerdict::Holds
                }
                ExactVerdict::Violated => {
                    violated_seen += 1;
                    RescueVerdict::Violated
                }
            };
            if rescued != want {
                disagreements.push((text, exact, rescued));
            }
        }
        assert!(disagreements.is_empty(), "{disagreements:?}");
        assert!(
            holds_seen >= 3 && violated_seen >= 3,
            "{holds_seen} / {violated_seen}"
        );
    }

    #[test]
    fn reduces_simple_eq_invariant() {
        // `assert property (cnt == 3)` → `nu X. ((cnt == 3) && [] X)`.
        let inv = reduce_ag_invariant(&parse("nu X. ((cnt == 3) && [] X)")).expect("reducible");
        assert_eq!(
            inv,
            AgInvariant {
                signal: "cnt".to_string(),
                op: CmpOp::Eq,
                value: 3
            }
        );
    }

    #[test]
    fn reduces_bare_boolean_invariant() {
        // A bare-boolean `assert property (b)` translates to `(b != 0)`.
        let inv = reduce_ag_invariant(&parse("nu X. ((flag != 0) && [] X)")).expect("reducible");
        assert_eq!(inv.signal, "flag");
        assert_eq!(inv.op, CmpOp::Ne);
        assert_eq!(inv.value, 0);
    }

    #[test]
    fn reduces_relational_literal_invariant() {
        // `AG(cnt <= 7)` is still a single literal comparison — reducible.
        let inv = reduce_ag_invariant(&parse("nu X. ((cnt <= 7) && [] X)")).expect("reducible");
        assert_eq!(inv.op, CmpOp::Le);
        assert_eq!(inv.value, 7);
    }

    #[test]
    fn atom_operand_order_is_symmetric() {
        // The classifier must accept `[] X && atom` as well as `atom && [] X`.
        let inv = reduce_ag_invariant(&parse("nu X. (([] X) && (cnt == 1))")).expect("reducible");
        assert_eq!(inv.value, 1);
    }

    #[test]
    fn abstains_on_implication_shapes() {
        // `a |-> b` and `a |=> b` — the body is an `Or`, not a bare atom.
        assert!(reduce_ag_invariant(&parse("nu X. ((!(a != 0) || (b != 0)) && [] X)")).is_none());
        assert!(
            reduce_ag_invariant(&parse("nu X. ((!(a != 0) || [] (b != 0)) && [] X)")).is_none()
        );
    }

    #[test]
    fn abstains_on_ef_cover() {
        // `cover property (b)` → `mu X. (b || <> X)` — a least fixpoint, not `Nu`.
        assert!(reduce_ag_invariant(&parse("mu X. ((b != 0) || <> X)")).is_none());
    }

    #[test]
    fn abstains_on_compound_body() {
        // A conjunctive invariant body is not a single `Cmp`.
        assert!(reduce_ag_invariant(&parse("nu X. (((a == 1) && (b == 2)) && [] X)")).is_none());
    }

    #[test]
    fn abstains_on_guarded_box() {
        // A box constrained to specific labels is a stronger property than the
        // all-reachable-states monitor — the classifier must abstain.
        let f = parse("nu X. ((cnt == 1) && [(labels = {step})] X)");
        assert!(
            reduce_ag_invariant(&f).is_none(),
            "a guarded box must not reduce to the all-states monitor"
        );
    }

    #[test]
    fn rescue_abstains_on_non_reducible_formula() {
        // The full rescue returns None (no subprocess spawned) when the shape is
        // not reducible — the design text is never even parsed.
        let f = parse("mu X. ((b != 0) || <> X)");
        assert!(reach_portfolio_rescue("1 sort bitvec 1\n", &f, false).is_none());
    }

    // ---- mununu#492 Part A: compound-atom safety reducer + integration ----

    #[test]
    fn compound_reducer_accepts_exclusion_shape() {
        // `AG(!a || !b)` — the exclusion safety `!(a && b)`, a compound Or-of-Nots.
        let f = parse("nu X. (((!(a == 1)) || (!(b == 1))) && [] X)");
        assert!(
            reduce_ag_boolean_body(&f).is_some(),
            "the exclusion safety must reduce under the compound reducer"
        );
        // And the single-atom reducer must still reject it — the widening is
        // strictly additive on the single-atom lane.
        assert!(
            reduce_ag_invariant(&f).is_none(),
            "the single-atom reducer must stay strict on compound shapes"
        );
    }

    #[test]
    /// mununu#503 — a body whose leaves are BARE 1-bit identifiers must be reducible.
    ///
    /// This is the shape every SVA `|->` produces once the translator emits `sig` rather than
    /// `sig == 1`. The strict `parse_predicate_expr` requires an operator, so it returned `Err`
    /// and rejected the whole tree — which is why every sysrst `|->` property was classified
    /// `safety-shape-not-reducible` and never reached the rescue lane. The gate and the monitor
    /// compiler now share `parse_predicate_atom_bool`, so they cannot disagree about a leaf.
    fn bare_identifier_leaves_are_reducible() {
        // `AG(!(a && b) || c)` with every leaf a bare identifier — the `|->` shape.
        let f = crate::mu_calculus::parser::parse(
            "nu X. (((!((trigger_active && cfg_enable_i))) || event_detected_o) && [] X)",
        )
        .expect("formula parses");
        assert!(
            reduce_ag_boolean_body(&f).is_some(),
            "bare 1-bit identifiers are the SVA translator's `signal is true` form; rejecting \
             them turns away an entire property class at the reducibility gate"
        );
    }

    #[test]
    /// The relaxation must not widen what else is accepted: a relational leaf (`CmpReg`) is
    /// still rejected, because the compound monitor cannot compile it.
    fn relational_leaves_are_still_rejected() {
        let f = crate::mu_calculus::parser::parse("nu X. ((cnt_q >= cnt_q__past) && [] X)")
            .expect("formula parses");
        assert!(
            reduce_ag_boolean_body(&f).is_none(),
            "a reg-vs-reg relational leaf has no compound-monitor encoding and must stay rejected"
        );
    }

    #[test]
    fn compound_reducer_accepts_implication_shape() {
        // `AG(!a || b)` — the classical implication safety `AG(a -> b)`.
        let f = parse("nu X. (((!(a == 1)) || (b == 1)) && [] X)");
        assert!(reduce_ag_boolean_body(&f).is_some());
    }

    #[test]
    fn compound_reducer_accepts_conjunctive_body() {
        // `AG(a && b)` — a conjunctive safety.
        let f = parse("nu X. (((a == 1) && (b == 1)) && [] X)");
        assert!(reduce_ag_boolean_body(&f).is_some());
    }

    #[test]
    fn compound_reducer_rejects_modal_inside_body() {
        // A modal (box / diamond) inside the compound is rejected — the compound
        // emitter compiles a pure boolean expression, not a modal one.
        let f = parse("nu X. (((a == 1) && [] (b == 1)) && [] X)");
        assert!(
            reduce_ag_boolean_body(&f).is_none(),
            "a compound with an embedded modal must not reduce"
        );
    }

    #[test]
    fn compound_reducer_rejects_relational_leaf() {
        // A `CmpReg` leaf (register-vs-register) is out of the compilable set;
        // the compound emitter would reject it, so the reducer must too.
        let f = parse("nu X. (((a == 1) && (b == c)) && [] X)");
        assert!(reduce_ag_boolean_body(&f).is_none());
    }

    #[test]
    fn compound_reducer_rejects_guarded_box() {
        // Same guard-box refusal as reduce_ag_invariant.
        let f = parse("nu X. (((a == 1) && (b == 1)) && [(labels = {step})] X)");
        assert!(reduce_ag_boolean_body(&f).is_none());
    }

    // A zero-state model — 3 primary inputs, no registers. `AG(!a || !b)` is
    // violated when a=b=1; `AG(!a || b)` is violated when a=1,b=0; `AG(a || !a)`
    // is trivially true. These are exactly the ticket-#492 shapes on the
    // stateless `mem_router` contrast pair.
    const ZERO_STATE_MODEL: &str = "\
1 sort bitvec 1
2 input 1 a
3 input 1 b
4 input 1 c
";

    /// mununu#503 — a design whose leaf is a NAMED INTERNAL NET, as yosys emits them
    /// (`<nid> uext <sort> <src> 0 <name>` — a no-op alias line carrying the symbol).
    ///
    /// `hot` aliases `a`, so `AG(!(a == 1) || !(hot == 1))` is FALSIFIED by `a = 1`: the two
    /// leaves are the same net, so they are true together. This is the NEGATIVE control for the
    /// internal-net resolver — the check that actually matters, because a verdict comparison
    /// cannot tell a correct encoding from one that silently observes the wrong node. A resolver
    /// that mis-binds the leaf would most likely MISS this violation and report `Holds`.
    const NAMED_INTERNAL_NET: &str = "\
1 sort bitvec 1
2 input 1 a
3 input 1 b
4 uext 1 2 0 hot
";

    #[test]
    fn internal_net_leaf_is_observed_by_the_monitor() {
        // `hot` IS `a`, so `!(a==1) || !(hot==1)` is false whenever a = 1 ⇒ VIOLATED.
        let f = parse("nu X. (((!(a == 1)) || (!(hot == 1))) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(NAMED_INTERNAL_NET, &f, false)
            .expect("an internal-net leaf must resolve, so the rescue fires");
        assert_eq!(
            verdict,
            RescueVerdict::Violated,
            "`hot` aliases `a`, so a = 1 falsifies the invariant. A monitor that mis-binds the \
             internal net would miss this and report Holds — which is why the negative control \
             is the test that matters here, not a verdict comparison"
        );
    }

    #[test]
    /// The positive twin: over the SAME design an invariant that genuinely holds must still hold,
    /// so the resolver is not simply making everything violated.
    fn internal_net_leaf_holds_when_the_invariant_is_true() {
        // `!(hot==1) || (a==1)` — `hot` IS `a`, so this is a tautology ⇒ HOLDS.
        let f = parse("nu X. (((!(hot == 1)) || (a == 1)) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(NAMED_INTERNAL_NET, &f, false)
            .expect("an internal-net leaf must resolve");
        assert_eq!(
            verdict,
            RescueVerdict::Holds,
            "`hot` aliases `a`, so `hot -> a` is a tautology over the net"
        );
    }

    #[test]
    fn zero_state_exclusion_safety_gets_violated_via_compound_rescue() {
        // `AG(!(a == 1) || !(b == 1))` — a=1,b=1 falsifies. Expected: VIOLATED.
        let f = parse("nu X. (((!(a == 1)) || (!(b == 1))) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(ZERO_STATE_MODEL, &f, false)
            .expect("rescue must fire on the compound path");
        assert_eq!(
            verdict,
            RescueVerdict::Violated,
            "a=1 && b=1 falsifies AG(!a || !b); expected VIOLATED, got {verdict:?}"
        );
    }

    #[test]
    fn zero_state_implication_safety_gets_violated_via_compound_rescue() {
        // `AG(!(a == 1) || (b == 1))` — a=1,b=0 falsifies. Expected: VIOLATED.
        let f = parse("nu X. (((!(a == 1)) || (b == 1)) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(ZERO_STATE_MODEL, &f, false)
            .expect("rescue must fire on the compound path");
        assert_eq!(verdict, RescueVerdict::Violated);
    }

    #[test]
    fn zero_state_tautology_gets_holds_via_compound_rescue() {
        // `AG((a == 1) || (a != 1))` — trivially true. Expected: HOLDS.
        let f = parse("nu X. (((a == 1) || (a != 1)) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(ZERO_STATE_MODEL, &f, false)
            .expect("rescue must fire on the compound path");
        assert_eq!(
            verdict,
            RescueVerdict::Holds,
            "AG(a || !a) is trivially true; expected HOLDS, got {verdict:?}"
        );
    }

    #[test]
    fn zero_state_conjunctive_safety_gets_violated_via_compound_rescue() {
        // `AG(a==1 && b==1)` — a=0 falsifies. Expected: VIOLATED.
        let f = parse("nu X. (((a == 1) && (b == 1)) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(ZERO_STATE_MODEL, &f, false)
            .expect("rescue must fire on the compound path");
        assert_eq!(verdict, RescueVerdict::Violated);
    }

    // The single-atom regression: the widened `reach_portfolio_rescue` must still
    // route a `AG(cnt != 3)` on the WIDE_INPUT_FSM through the existing
    // single-atom emitter (byte-equivalent). We can't assert on emitted bytes
    // here (the file text isn't returned), but we can assert the verdict is the
    // same as it was before the widening — VIOLATED on `AG(cnt != 3)` at k=2.
    #[test]
    fn single_atom_regression_still_decides_via_the_original_lane() {
        let f = parse("nu X. ((cnt == 0) && [] X)");
        // cnt starts at 0, then transitions to 1 → clearly violated at k=1.
        let (verdict, _) = reach_portfolio_rescue(WIDE_INPUT_FSM, &f, false)
            .expect("single-atom lane must still rescue");
        assert_eq!(verdict, RescueVerdict::Violated);
    }

    // ---- docker-validated (`mununu-sva`): full rescue path with live btormc/Pono ----
    // These exercise reduce_ag_invariant → emit monitor → reach_portfolio →
    // verdict with the real subprocess members. Run with `--ignored` in mununu-sva.

    /// A 2-bit counter `cnt` cycling 0→1→2→0 (input `w` wide but irrelevant).
    /// `AG(cnt != 3)` HOLDS (3 is never reached); `AG(cnt != 2)` is VIOLATED.
    const WIDE_INPUT_FSM: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 sort bitvec 8
4 input 3 w
5 zero 2
6 state 2 cnt
7 one 2
8 constd 2 2
9 eq 1 6 8
10 add 2 6 7
11 ite 2 9 5 10
12 next 2 6 11
13 init 2 6 5
";

    /// A 300-bit register `big` that stays 0 — `AG(big == 0)` HOLDS and is
    /// 1-inductive. The 300-bit cone EXCEEDS the exact engine's auto-cap ceiling (192), so
    /// the exact member abstains and only the subprocess members (btormc k-induction /
    /// Pono IC3) can decide it — the "beyond the BDD cap" value proof.
    const WIDE_STATE: &str = "\
1 sort bitvec 300
2 zero 1
3 state 1 big
4 init 1 3 2
5 next 1 3 3
";

    #[test]
    #[ignore = "requires btormc + pono (mununu-sva); run with --ignored"]
    fn e2e_rescue_flips_true_invariant_to_holds() {
        // AG(cnt != 3) is TRUE — the rescue reduces it and the portfolio proves
        // the `bad` (cnt == 3) unreachable ⇒ Holds.
        let f = parse("nu X. ((cnt != 3) && [] X)");
        let (verdict, outcome) =
            reach_portfolio_rescue(WIDE_INPUT_FSM, &f, false).expect("reducible + monitor builds");
        assert_eq!(verdict, RescueVerdict::Holds, "outcome: {outcome:?}");
        assert!(
            !outcome.unreachable_by.is_empty(),
            "at least one engine must prove unreachable; got {outcome:?}"
        );
    }

    #[test]
    #[ignore = "requires btormc + pono (mununu-sva); run with --ignored"]
    fn e2e_rescue_flips_false_invariant_to_violated() {
        // AG(cnt != 2) is FALSE — cnt reaches 2, so `bad` (cnt == 2) is reachable
        // ⇒ Violated. Confirms the other verdict direction through the full path.
        let f = parse("nu X. ((cnt != 2) && [] X)");
        let (verdict, outcome) =
            reach_portfolio_rescue(WIDE_INPUT_FSM, &f, false).expect("reducible + monitor builds");
        assert_eq!(verdict, RescueVerdict::Violated, "outcome: {outcome:?}");
        assert!(
            !outcome.reachable_by.is_empty(),
            "at least one engine must find the violating state; got {outcome:?}"
        );
    }

    #[test]
    #[ignore = "requires btormc + pono (mununu-sva); run with --ignored"]
    fn e2e_rescue_decides_beyond_exact_cap() {
        // A 300-bit invariant the exact BDD engine cannot touch (over its auto-cap ceiling) —
        // proving the subprocess members extend reach past the cap. The exact member must
        // ABSTAIN, and the portfolio must decide HOLDS.
        //
        // mununu#625 — which member CARRIES the portfolio's verdict is a race since #607
        // (the first trusted decider cancels the rest after a short grace): on a fast host
        // `native` and `spacer` decide this trivial model inside the grace and the subprocess
        // members are cancelled before they answer, so asserting on `unreachable_by` encoded
        // "who was first", not the claim. The claim — btormc and pono DECIDE beyond the cap —
        // is checked by running each directly on the same monitored model.
        let f = parse("nu X. ((big == 0) && [] X)");
        let (verdict, outcome) =
            reach_portfolio_rescue(WIDE_STATE, &f, false).expect("reducible + monitor builds");
        assert_eq!(verdict, RescueVerdict::Holds, "outcome: {outcome:?}");
        assert!(
            !outcome.unreachable_by.contains(&"exact"),
            "the exact engine must abstain on the 300-bit (over-cap) design; got {outcome:?}"
        );
        let inv = reduce_ag_invariant(&f).expect("single-atom invariant");
        let monitored =
            emit_ag_state_atom_monitor(WIDE_STATE, &inv.signal, inv.op, inv.value, false)
                .expect("monitor builds");
        let file = parser::parse(&monitored).expect("monitored model parses");
        use crate::adapter::btormc::{self, McVerdict};
        use crate::adapter::pono;
        let by_btormc =
            btormc::decide_via_btormc(&file, btormc::DEFAULT_KMAX, btormc::DEFAULT_TIMEOUT, None)
                .expect("btormc runs");
        let by_pono =
            pono::decide_via_pono(&file, pono::DEFAULT_ENGINE, pono::DEFAULT_TIMEOUT, None)
                .expect("pono runs");
        eprintln!("beyond-cap subprocess verdicts: btormc={by_btormc:?} pono={by_pono:?}");
        // The claim is "a subprocess member decides beyond the cap", as the portfolio needs:
        // btormc's plain k-induction returns Unknown here (the monitor's flag register is not
        // 1-inductive without strengthening — measured), pono's IC3 proves it.
        assert!(
            by_btormc == McVerdict::Safe || by_pono == McVerdict::Safe,
            "neither btormc nor pono proves the 300-bit invariant beyond the exact cap: \
             btormc={by_btormc:?} pono={by_pono:?}"
        );
        assert_ne!(by_btormc, McVerdict::Violated);
        assert_ne!(by_pono, McVerdict::Violated);
    }

    // `q` toggles every cycle (q' = !q), so a high `q` is ALWAYS followed by a low one.
    // `zero` precedes `state` because BTOR2 `init` needs the value NID below the state NID.
    const TOGGLING_PULSE: &str = "\
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 not 1 3
6 next 1 3 5
";

    // Same shape, but `q` latches a FREE input, so it can stay high two cycles running.
    const FREE_PULSE: &str = "\
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 input 1 d
6 next 1 3 5
7 input 1 e
";

    #[test]
    fn implies_next_matches_the_non_overlapped_shape() {
        // `q |=> !q`  →  nu X. ((!(q==1) || [] (!(q==1))) && [] X)
        let f = parse("nu X. (((!(q == 1)) || [] (!(q == 1))) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_some(),
            "the `|=>` shape must match the implies-next reducer"
        );
        assert!(
            reduce_ag_boolean_body(&f).is_none(),
            "and must NOT match the boolean-body reducer — the inner box is not a boolean, so a \
             reducer that accepted it would be compiling a modality away"
        );
    }

    #[test]
    fn implies_next_declines_the_overlapped_shape() {
        // `q |-> !q` has no inner box; that is reduce_ag_boolean_body's shape, not this one.
        let f = parse("nu X. (((!(q == 1)) || (!(q == 1))) && [] X)");
        assert!(reduce_ag_implies_next(&f).is_none(), "`|->` is not `|=>`");
        assert!(
            reduce_ag_boolean_body(&f).is_some(),
            "the overlapped form still belongs to the boolean-body reducer"
        );
    }

    #[test]
    fn implies_next_declines_a_nested_box_consequent() {
        // `q |=> ##1 r` ⇒ `[] [] r`. The inner box is not a compilable boolean body.
        let f = parse("nu X. (((!(q == 1)) || [] [] (r == 1)) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_none(),
            "a nested box must decline rather than be mis-compiled into a boolean"
        );
    }

    #[test]
    fn implies_next_declines_when_both_disjuncts_are_boxes() {
        let f = parse("nu X. ((([] (q == 1)) || [] (r == 1)) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_none(),
            "which disjunct is the consequent is ambiguous — the match must not guess"
        );
    }

    #[test]
    fn implies_next_declines_a_recursion_variable_consequent() {
        // Degenerate `nu X. ((!A || [] X) && [] X)` must not read as an implication.
        let f = parse("nu X. (((!(q == 1)) || [] X) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_none(),
            "the recursion variable is not a boolean consequent"
        );
    }

    #[test]
    /// NEGATIVE CONTROL — the test that actually proves the monitor fires.
    ///
    /// `q` latches a free input, so `q` high twice running is reachable and `q |=> !q` is
    /// genuinely VIOLATED. A latch wired wrong (init 1, or `next` reading the consequent) would
    /// report Holds here, and a monitor that never fires would pass every positive test.
    fn implies_next_catches_a_pulse_that_can_stay_high() {
        let f = parse("nu X. (((!(q == 1)) || [] (!(q == 1))) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(FREE_PULSE, &f, false)
            .expect("the `|=>` shape must now reach the rescue lane");
        assert_eq!(
            verdict,
            RescueVerdict::Violated,
            "`q` can stay high for two cycles, so the property is violated"
        );
    }

    #[test]
    /// The positive twin: over a design where the pulse genuinely cannot repeat, the same property
    /// must HOLD — so the monitor is not simply reporting everything violated.
    fn implies_next_holds_on_a_toggling_pulse() {
        let f = parse("nu X. (((!(q == 1)) || [] (!(q == 1))) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(TOGGLING_PULSE, &f, false)
            .expect("the `|=>` shape must reach the rescue lane");
        assert_eq!(
            verdict,
            RescueVerdict::Holds,
            "`q` toggles, so a high `q` is always followed by a low one"
        );
    }

    #[test]
    /// `init 0` must fabricate no obligation at reset. Over the toggling design the antecedent is
    /// false at cycle 0 (q inits low); a latch initialised to 1 would report a violation at the
    /// very first step on a design that never asserted the antecedent.
    fn implies_next_makes_no_obligation_at_reset() {
        // `!q |=> q` — holds on the toggler (low is always followed by high), and would be
        // violated at cycle 0 by a latch that came up asserted.
        let f = parse("nu X. (((!(q == 0)) || [] (q == 1)) && [] X)");
        let (verdict, _) = reach_portfolio_rescue(TOGGLING_PULSE, &f, false).expect("reducible");
        assert_eq!(
            verdict,
            RescueVerdict::Holds,
            "an `init 1` latch would manufacture a cycle-0 violation here"
        );
    }

    #[test]
    /// The compound-antecedent axis — `emit_ag_implies_next_monitor` took a single atom, which is
    /// half of why it never reached a real property.
    fn implies_next_accepts_a_compound_antecedent() {
        let f = parse("nu X. ((!((q == 1) && (e == 1)) || [] (q == 0)) && [] X)");
        assert!(
            reduce_ag_implies_next(&f).is_some(),
            "a compound antecedent must match"
        );
        let (verdict, _) = reach_portfolio_rescue(FREE_PULSE, &f, false)
            .expect("a compound antecedent must reach the rescue lane");
        assert_eq!(
            verdict,
            RescueVerdict::Violated,
            "`e` is free, so `q && e` then `q` again is reachable"
        );
    }
}
