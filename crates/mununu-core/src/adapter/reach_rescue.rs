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
fn is_compilable_boolean_body(formula: &Formula, id: NodeId) -> bool {
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
                "the formula matched neither `AG(atom)` nor `AG(boolean-of-atoms)`"
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
                let root =
                    reduce_ag_boolean_body(formula).ok_or(RescueDecline::ShapeNotReducible)?;
                emit_ag_boolean_invariant_monitor(design_btor2, formula, root, reset_pinned)
                    .map_err(|e| RescueDecline::MonitorEmissionFailed(e.message))?
            }
        }
    } else {
        // Non-single-atom shape — try compound.
        let root = reduce_ag_boolean_body(formula).ok_or(RescueDecline::ShapeNotReducible)?;
        emit_ag_boolean_invariant_monitor(design_btor2, formula, root, reset_pinned)
            .map_err(|e| RescueDecline::MonitorEmissionFailed(e.message))?
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

/// The concrete counterexample trace for an AG-invariant the reachability portfolio
/// found VIOLATED: rebuild the same `bad`-monitor [`reach_portfolio_rescue`] uses and
/// run native BMC to extract the `Init → ¬invariant` path. The bad-monitor adds no
/// state, so the trace is over the ORIGINAL design's registers (no projection).
/// Returns `None` if the formula is not a reducible AG-invariant or no bounded
/// counterexample is found within `max_k`.
pub fn ag_invariant_witness(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
    max_k: u32,
) -> Option<crate::adapter::btor2::native_bmc::BmcTrace> {
    let inv = reduce_ag_invariant(formula)?;
    let monitored =
        emit_ag_state_atom_monitor(design_btor2, &inv.signal, inv.op, inv.value, reset_pinned)
            .ok()?;
    let file = parser::parse(&monitored).ok()?;
    match crate::adapter::btor2::native_bmc::bmc_bad_reachable_witness(&file, max_k).ok()? {
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
        // An 80-bit invariant the exact BDD engine cannot touch (over its auto-cap
        // ceiling of 64) — proving the subprocess members extend reach past the cap. The
        // exact member must ABSTAIN; a subprocess member must carry the verdict.
        let f = parse("nu X. ((big == 0) && [] X)");
        let (verdict, outcome) =
            reach_portfolio_rescue(WIDE_STATE, &f, false).expect("reducible + monitor builds");
        assert_eq!(verdict, RescueVerdict::Holds, "outcome: {outcome:?}");
        assert!(
            !outcome.unreachable_by.contains(&"exact"),
            "the exact engine must abstain on the 80-bit (over-cap) design; got {outcome:?}"
        );
        assert!(
            outcome
                .unreachable_by
                .iter()
                .any(|e| *e == "btormc" || *e == "pono"),
            "a subprocess member must carry the beyond-cap verdict; got {outcome:?}"
        );
    }
}
