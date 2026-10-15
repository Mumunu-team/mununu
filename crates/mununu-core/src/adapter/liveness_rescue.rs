//! P2 liveness-to-safety — reduce the **response** liveness property
//! `AG(a → AF b)` to a BTOR2 `bad` monitor and decide it with the reachability
//! portfolio at scale.
//!
//! # The property and why it reduces
//!
//! `AG(a → AF b)` — "whenever `a` holds, `b` is eventually reached on *every*
//! path" — is the canonical request/response (request/grant) liveness property.
//! In the modal-μ calculus it is
//!
//! ```text
//!   ν Z. ((¬a ∨ μ Y. (b ∨ [] Y)) ∧ [] Z)
//! ```
//!
//! a νμ alternation (`PropertyClass::Liveness`). The **exact** 3-valued KMTS engine
//! decides it directly but only within its enumeration cap; this module reduces it
//! to a single `bad`-reachability query the scalable portfolio (native ⊕ spacer ⊕
//! btormc ⊕ Pono) decides symbolically on wide designs, and the exact engine
//! cross-checks the small cases.
//!
//! # Liveness-to-safety (Biere–Artho–Schuppan)
//!
//! `AG(a → AF b)` **fails** iff the design has a *reachable* infinite path on which
//! some `a` is never followed by `b` — equivalently, a **reachable lasso** (a
//! reachable cycle) that contains a state where `a` holds and contains **no** state
//! where `b` holds (on the cycle the outstanding request is never granted). The l2s
//! construction ([`crate::adapter::btor2::l2s_monitor::emit_response_l2s_monitor`])
//! makes "a reachable `b`-free cycle containing an `a`" a `bad` state via a
//! nondeterministically-snapshotted shadow copy of the state, so:
//!
//! - `bad` **reachable** ⇒ such a lasso exists ⇒ `AG(a → AF b)` **VIOLATED**;
//! - `bad` **unreachable** (a portfolio safety proof) ⇒ no such lasso ⇒ **HOLDS**.
//!
//! # A note on `F` in mununu's LTL front-end
//!
//! mununu's LTL translator lowers `G(a → F b)` with the **diamond** reading of `F`
//! (`μ Y. (b ∨ <> Y)` = `EF`), giving `AG(a → EF b)` — the weaker *recoverability*
//! response ("`b` is reachable"), a νμ property that does **not** collapse to a
//! single safety query. This module targets the **box** reading `AF b`
//! (`μ Y. (b ∨ [] Y)` — "`b` on every path"), the true "always eventually granted"
//! liveness that l2s reduces. Author the property as the μ-calculus formula above
//! (box recursion in the inner `μ`), not via the LTL `F`.
//!
//! # Conservative by construction (soundness-critical)
//!
//! [`reduce_response_af`] returns `Some` ONLY for the exact single-atom shape above
//! with **unconstrained** boxes and both `a`, `b` single register-comparison atoms.
//! Every other shape returns `None` — a sound abstention (the caller leaves the
//! property to the exact engine), never a mis-reduction.
//!
//! # Status — validated + user-surfaced (2026-07-08)
//!
//! [`response_liveness_rescue`] is proven correct end-to-end by the differential
//! tests below: on both a responder (HOLDS) and a staller (VIOLATED) the
//! l2s → portfolio verdict **matches** the exact 3-valued engine
//! ([`crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict`]) — the
//! roadmap's "every reduced verdict is cross-checked by the exact engine".
//!
//! The reduction is user-invocable via [`response_liveness_rescue_atoms`] +
//! [`parse_response_atom`] across all three surfaces: CLI `mununu btor2
//! verify-liveness --request … --grant …`, API `POST /api/v1/btor2/verify-liveness`,
//! and the UI `runBtor2VerifyLiveness` client. Auto-routing `verify_auto` to *try*
//! this reduction when a translated SVA liveness property escapes the exact engine's
//! cap is a further follow-up (it needs a real SVA liveness target to validate under
//! the mununu-sva toolchain, per the [`crate::adapter::reach_rescue`] no-target
//! lesson).

use crate::adapter::btor2::l2s_monitor::{
    emit_l2s_monitor_from_formula, emit_response_l2s_monitor,
    emit_response_l2s_monitor_under_fairness,
};
use crate::adapter::btor2::parser;
use crate::adapter::btor2::predicate_expr::{
    CmpOp, PredicateExpr, parse_predicate_atom_bool, parse_predicate_expr,
};
use crate::adapter::reach_portfolio::{ReachOutcome, ReachVerdict, decide_reach_portfolio};
use crate::mu_calculus::{Formula, FormulaVarId, Guard, ModalKind, Node, NodeId};

/// A single register-comparison atom `signal ⋈ value` (the `a` / `b` of the
/// response property).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    /// The register (or input) the atom compares.
    pub signal: String,
    /// The comparison operator.
    pub op: CmpOp,
    /// The literal right-hand side.
    pub value: u128,
}

/// A reducible response-liveness property `AG(a → AF b)` over two single atoms, or the
/// antecedent-free `AG AF b` (`ante: None` — every cycle is a request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseAf {
    /// The antecedent `a` — the request. `None` for `AG AF b` (`assert property (s_eventually
    /// b)`), where the l2s monitor's request is the constant `1`.
    pub ante: Option<Atom>,
    /// The consequent `b` — the grant that must eventually follow on every path.
    pub cons: Atom,
    /// `m` in `AG(a → AXᵐ AF b)` — the boxes between the implication and the eventuality, as
    /// `a |=> s_eventually b` (m = 1) and `a |-> ##[m:$] b` translate. `0` for the plain response.
    /// The monitor reads the request through an m-cycle delay line (`a_d`), and
    /// `AG(a → AXᵐ AF b)` ≡ `AG(a_d → AF b)` on the product (see `emit_l2s_monitor`).
    pub delay: u32,
}

/// The response-liveness shape `ν Z. ((¬a ∨ []ᵐ μ Y. (b ∨ [] Y)) ∧ [] Z)` — or `AG AXᵐ AF b` with
/// no antecedent — with `a` and `b` any BOOLEAN subtrees over register comparisons (`And` / `Or` /
/// `Not` / constants), as node ids of the formula. The SVA translator's `(req && st == IDLE) |->
/// s_eventually done` lands here; the single-atom [`ResponseAf`] is the special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseShape {
    /// The request subtree (`a`, not `¬a`); `None` for `AG AF b`.
    pub ante: Option<NodeId>,
    /// The grant subtree `b`.
    pub cons: NodeId,
    /// `m` in `AG(a → AXᵐ AF b)`.
    pub delay: u32,
}

/// Conservatively recognise [`ResponseShape`]. Returns `None` for every other shape — in
/// particular a guarded box, a diamond inner recursion (`EF`, recoverability), and a leaf the
/// boolean compiler cannot bind (relational / addend / select atoms) — so the reduction never
/// mis-reduces a property it does not fully recognise.
pub fn reduce_response_shape(formula: &Formula) -> Option<ResponseShape> {
    use crate::adapter::reach_rescue::is_compilable_boolean_body;
    // Root must be `ν Z. BODY` (the outer `AG`).
    let Node::Nu { var: z, body } = formula.node(formula.root()) else {
        return None;
    };
    // BODY = `core ∧ [] Z`.
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (core_id, _boxz_id) = split_and_box_to_var(formula, *l, *r, *z)?;
    // core = `¬a ∨ []ᵐ (μ Y. (b ∨ [] Y))`, or the antecedent-free `[]ᵐ μ Y. (b ∨ [] Y)`.
    let (ante, boxed_af_id) = match formula.node(core_id) {
        Node::Or(cl, cr) => {
            let (neg_ante_id, af_id) = classify_or_notpred_boxed_mu(formula, *cl, *cr)?;
            let Node::Not(a_id) = formula.node(neg_ante_id) else {
                return None;
            };
            if !is_compilable_boolean_body(formula, *a_id) {
                return None;
            }
            (Some(*a_id), af_id)
        }
        _ => (None, core_id),
    };
    let (delay, af_id) = strip_unconstrained_boxes(formula, boxed_af_id);
    // AF b = `μ Y. (b ∨ [] Y)`.
    let Node::Mu {
        var: y,
        body: mbody,
    } = formula.node(af_id)
    else {
        return None;
    };
    let Node::Or(ml, mr) = formula.node(*mbody) else {
        return None;
    };
    let (cons_id, _boxy_id) = split_and_box_to_var(formula, *ml, *mr, *y)?;
    if !is_compilable_boolean_body(formula, cons_id) {
        return None;
    }
    Some(ResponseShape {
        ante,
        cons: cons_id,
        delay,
    })
}

/// Conservatively classify `formula` as the response-liveness shape
/// `ν Z. ((¬a ∨ []ᵐ μ Y. (b ∨ [] Y)) ∧ [] Z)` with SINGLE-ATOM `a`, `b` (or no `a`) — the
/// [`ResponseShape`] whose request and grant are each one register comparison, as the
/// `verify-liveness` surface takes them. Returns `None` for every other shape.
pub fn reduce_response_af(formula: &Formula) -> Option<ResponseAf> {
    let shape = reduce_response_shape(formula)?;
    let ante = match shape.ante {
        Some(id) => Some(predicate_atom(formula, id)?),
        None => None,
    };
    Some(ResponseAf {
        ante,
        cons: predicate_atom(formula, shape.cons)?,
        delay: shape.delay,
    })
}

/// Decide `formula` if it is a [`ResponseShape`] — compound request / grant included — under an
/// optional conjunctive fairness assumption. The entry `verify_auto`'s ⊥ escalation uses.
pub fn response_liveness_rescue_formula(
    design_btor2: &str,
    formula: &Formula,
    fairness_atoms: &[Atom],
    reset_pinned: bool,
) -> Option<(LivenessVerdict, ReachOutcome)> {
    let shape = reduce_response_shape(formula)?;
    let fair_tuples: Vec<(&str, CmpOp, u128)> = fairness_atoms
        .iter()
        .map(|a| (a.signal.as_str(), a.op, a.value))
        .collect();
    let monitored = emit_l2s_monitor_from_formula(
        design_btor2,
        formula,
        shape.ante,
        shape.delay,
        shape.cons,
        &fair_tuples,
        reset_pinned,
    )
    .ok()?;
    let file = parser::parse(&monitored).ok()?;
    let outcome = decide_reach_portfolio(&file);
    let verdict = match outcome.verdict {
        ReachVerdict::Unreachable => LivenessVerdict::Holds,
        ReachVerdict::Reachable => LivenessVerdict::Violated,
        ReachVerdict::Unknown | ReachVerdict::Contradiction => LivenessVerdict::Inconclusive,
    };
    Some((verdict, outcome))
}

/// Return `(other, box_to_var)` iff exactly one of `l` / `r` is an unconstrained
/// `[] var` box and the other is not — the unambiguous "`X ∧ [] var`" /
/// "`X ∨ [] var`" split. `None` if neither or both look like the box recursion.
fn split_and_box_to_var(
    formula: &Formula,
    l: NodeId,
    r: NodeId,
    var: FormulaVarId,
) -> Option<(NodeId, NodeId)> {
    match (
        is_unconstrained_box_to_var(formula, l, var),
        is_unconstrained_box_to_var(formula, r, var),
    ) {
        (true, false) => Some((r, l)),
        (false, true) => Some((l, r)),
        _ => None,
    }
}

/// Peel unconstrained `[]` modalities (`[]ᵐ φ`) and return `(m, φ)`. A guarded box is not
/// peeled: it is a stronger property than the delay line encodes.
fn strip_unconstrained_boxes(formula: &Formula, mut id: NodeId) -> (u32, NodeId) {
    let mut m = 0;
    while let Node::Modal {
        kind: ModalKind::Box,
        guard,
        target,
    } = formula.node(id)
    {
        if *guard != Guard::default() {
            break;
        }
        m += 1;
        id = *target;
    }
    (m, id)
}

/// Return `(not_pred, boxed_mu)` iff exactly one of `l` / `r` is a `Not` node and the other is a
/// `Mu` fixpoint behind zero or more unconstrained boxes — the `¬a ∨ []ᵐ AF b` split.
fn classify_or_notpred_boxed_mu(
    formula: &Formula,
    l: NodeId,
    r: NodeId,
) -> Option<(NodeId, NodeId)> {
    let is_not = |id: NodeId| matches!(formula.node(id), Node::Not(_));
    let is_mu = |id: NodeId| {
        let (_, inner) = strip_unconstrained_boxes(formula, id);
        matches!(formula.node(inner), Node::Mu { .. })
    };
    match (is_not(l) && is_mu(r), is_not(r) && is_mu(l)) {
        (true, false) => Some((l, r)),
        (false, true) => Some((r, l)),
        _ => None,
    }
}

/// A `[] var` box whose target is exactly `var` and whose guard is unconstrained
/// (a guarded box is a stronger property than the monitor checks).
fn is_unconstrained_box_to_var(formula: &Formula, id: NodeId, var: FormulaVarId) -> bool {
    matches!(
        formula.node(id),
        Node::Modal { kind: ModalKind::Box, guard, target }
            if *guard == Guard::default()
                && matches!(formula.node(*target), Node::Variable(v) if *v == var)
    )
}

/// A `<> var` diamond whose target is exactly `var` and whose guard is unconstrained —
/// the inner recursion of the `EF` in `AG EF good`, the branching content that
/// distinguishes νμ recoverability from the box-AF response.
fn is_unconstrained_diamond_to_var(formula: &Formula, id: NodeId, var: FormulaVarId) -> bool {
    matches!(
        formula.node(id),
        Node::Modal { kind: ModalKind::Diamond, guard, target }
            if *guard == Guard::default()
                && matches!(formula.node(*target), Node::Variable(v) if *v == var)
    )
}

/// Conservatively recognize the recoverability shape `AG EF good` =
/// `ν Y. ((μ X. (good ∨ <> X)) ∧ [] Y)` with a single-atom `good`, returning that atom.
/// Returns `None` for every other shape — in particular the box-AF response (whose
/// inner recursion is `[] Y`, not `<> X`) — so a caller can route a νμ recoverability ⊥
/// to the cube + `smt-hyper-must` path while leaving box-AF to l2s. The mirror of
/// [`reduce_response_af`] for the diamond (`EF`) inner fixpoint.
pub fn reduce_ag_ef_target(formula: &Formula) -> Option<Atom> {
    // ν Y. BODY
    let Node::Nu { var: y, body } = formula.node(formula.root()) else {
        return None;
    };
    // BODY = core ∧ [] Y
    let Node::And(l, r) = formula.node(*body) else {
        return None;
    };
    let (core_id, _box_y) = split_and_box_to_var(formula, *l, *r, *y)?;
    // core = μ X. (good ∨ <> X)
    let Node::Mu {
        var: x,
        body: mbody,
    } = formula.node(core_id)
    else {
        return None;
    };
    let Node::Or(ml, mr) = formula.node(*mbody) else {
        return None;
    };
    // Exactly one side is `<> X`; the other is the `good` predicate.
    let good_id = match (
        is_unconstrained_diamond_to_var(formula, *ml, *x),
        is_unconstrained_diamond_to_var(formula, *mr, *x),
    ) {
        (true, false) => *mr,
        (false, true) => *ml,
        _ => return None,
    };
    predicate_atom(formula, good_id)
}

/// Parse a `Predicate` node as a single register-comparison atom. `None` for a
/// relational (`reg ⋈ reg`), compound, or unparseable atom.
///
/// A bare identifier (`grant`) is the 1-bit atom `grant != 0` — the shape the SVA translator
/// emits for a 1-bit signal (`a |-> s_eventually b` → `(!(a) || mu Y. (b || [] Y))`). Before
/// 2026-10-15 the strict parser refused it, so the most common SVA liveness spelling never
/// reached this reduction.
fn predicate_atom(formula: &Formula, id: NodeId) -> Option<Atom> {
    let Node::Predicate(atom) = formula.node(id) else {
        return None;
    };
    match parse_predicate_atom_bool(atom) {
        Ok(PredicateExpr::Cmp {
            register,
            op,
            value,
        }) => Some(Atom {
            signal: register,
            op,
            value: value as u128,
        }),
        _ => None,
    }
}

/// The response-liveness verdict, from the reachability portfolio's verdict on the
/// l2s `bad` monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessVerdict {
    /// `bad` proven unreachable ⇒ no `b`-free request-pending lasso ⇒ the response
    /// property holds.
    Holds,
    /// `bad` reachable ⇒ a reachable lasso leaves a request forever ungranted.
    Violated,
    /// Undecided by every portfolio member, or a soundness contradiction alarm —
    /// never a silently-picked side.
    Inconclusive,
}

/// Reduce a response-liveness property `AG(a → AF b)` to an l2s `bad` monitor over
/// `design_btor2` and decide it with the reachability portfolio.
///
/// Returns `None` when the property is not the reducible response shape (the caller
/// leaves it to the exact engine) or the monitor cannot be built (an atom binds no
/// signal). `design_btor2` MUST be the same reset/config-pinned BTOR2 the exact
/// engine reasoned over, so the reachable-state set matches; `reset_pinned` mirrors
/// that reset-gating so the atoms resolve through the async-reset mux consistently.
///
/// On `Some`, the second component is the raw [`ReachOutcome`] (which engines decided
/// each side) for diagnostics / a witness note.
pub fn response_liveness_rescue(
    design_btor2: &str,
    formula: &Formula,
    reset_pinned: bool,
) -> Option<(LivenessVerdict, ReachOutcome)> {
    response_liveness_rescue_formula(design_btor2, formula, &[], reset_pinned)
}

/// Like [`response_liveness_rescue`] but with the request / grant atoms given
/// directly (the caller asserts the `AG(ante → AF cons)` shape) — the entry point
/// for the `verify-liveness` surface, where the user names the two atoms rather than
/// authoring the full μ-calculus formula.
///
/// Returns `None` only when the l2s monitor cannot be built (an atom binds no
/// signal). See [`response_liveness_rescue`] for the `design_btor2` / `reset_pinned`
/// contract and the returned [`ReachOutcome`].
pub fn response_liveness_rescue_atoms(
    design_btor2: &str,
    ante: &Atom,
    cons: &Atom,
    reset_pinned: bool,
) -> Option<(LivenessVerdict, ReachOutcome)> {
    let monitored = emit_response_l2s_monitor(
        design_btor2,
        (&ante.signal, ante.op, ante.value),
        (&cons.signal, cons.op, cons.value),
        reset_pinned,
    )
    .ok()?;
    let file = parser::parse(&monitored).ok()?;
    let outcome = decide_reach_portfolio(&file);
    let verdict = match outcome.verdict {
        ReachVerdict::Unreachable => LivenessVerdict::Holds,
        ReachVerdict::Reachable => LivenessVerdict::Violated,
        ReachVerdict::Unknown | ReachVerdict::Contradiction => LivenessVerdict::Inconclusive,
    };
    Some((verdict, outcome))
}

/// mununu#477 Option B — decide `(⋀_j GF fair_j) → AG(ante → AF cons)` via the
/// fair-cycle l2s (Emerson–Lei extension of Biere–Artho–Schuppan). Same contract
/// as [`response_liveness_rescue_atoms`] plus a slice of primary-input / state
/// fairness atoms; an empty `fairness_atoms` slice recovers
/// [`response_liveness_rescue_atoms`] exactly.
///
/// The construction — one `fair_j_seen` latch per fairness atom, mirroring the
/// existing `b_seen`; `bad = looped ∧ ¬b_seen ∧ ⋀_j fair_j_seen` — is documented
/// in [`emit_response_l2s_monitor_under_fairness`] with the soundness /
/// completeness argument.
///
/// Returns `None` only when the l2s monitor cannot be built (an atom binds no
/// signal). On `Some`, the second component is the raw [`ReachOutcome`] (which
/// engines decided) for diagnostics / a witness note.
pub fn response_liveness_rescue_under_fairness(
    design_btor2: &str,
    ante: &Atom,
    cons: &Atom,
    fairness_atoms: &[Atom],
    reset_pinned: bool,
) -> Option<(LivenessVerdict, ReachOutcome)> {
    let fair_tuples: Vec<(&str, CmpOp, u128)> = fairness_atoms
        .iter()
        .map(|a| (a.signal.as_str(), a.op, a.value))
        .collect();
    let monitored = emit_response_l2s_monitor_under_fairness(
        design_btor2,
        (&ante.signal, ante.op, ante.value),
        (&cons.signal, cons.op, cons.value),
        &fair_tuples,
        reset_pinned,
    )
    .ok()?;
    let file = parser::parse(&monitored).ok()?;
    let outcome = decide_reach_portfolio(&file);
    let verdict = match outcome.verdict {
        ReachVerdict::Unreachable => LivenessVerdict::Holds,
        ReachVerdict::Reachable => LivenessVerdict::Violated,
        ReachVerdict::Unknown | ReachVerdict::Contradiction => LivenessVerdict::Inconclusive,
    };
    Some((verdict, outcome))
}

/// Parse repeatable `"REG op VALUE"` fairness-atom strings (the
/// `verify-liveness-under-fairness` surface) into [`Atom`]s. Reuses
/// [`parse_response_atom`] so the grammar matches the request / grant atoms
/// exactly; each entry must be a single register-comparison.
pub fn parse_fairness_atoms(specs: &[String]) -> Result<Vec<Atom>, String> {
    specs.iter().map(|s| parse_response_atom(s)).collect()
}

/// Decide a **conjunction** of response-liveness properties `⋀_i AG(a_i → AF b_i)`
/// over one `design_btor2`, by deciding each response independently via
/// [`response_liveness_rescue_atoms`] and combining the verdicts:
///
/// - **any `Violated` ⇒ `Violated`** — a real `b_i`-free lasso through a pending `a_i`
///   is a genuine path violating the conjunction (the counterexample to conjunct `i`
///   is a counterexample to `⋀`), so `Violated` dominates.
/// - else **any `Inconclusive` ⇒ `Inconclusive`** — that conjunct might yet be
///   violated, so the conjunction cannot be declared to hold.
/// - else **all `Holds` ⇒ `Holds`**.
///
/// This is **correct by composition**: each single-response query is a sound + complete
/// l2s reduction (see [`response_liveness_rescue`]), a conjunction violation is exactly
/// *some* conjunct's violation (which its query finds), and each query still scales via
/// the reachability portfolio. It is the closed-system shape — verifying a
/// multi-guarantee controller (e.g. an arbiter's per-client no-starvation) in place,
/// where the environment is concrete and any GR(1) assumptions are already discharged.
/// The open-system Streett shape `(⋀ GF a_i) → (⋀ GF b_j)` — where live assumptions
/// couple the guarantees, so this N-query decomposition does NOT hold — is a separate
/// follow-up (the Emerson–Lei fair-cycle l2s).
///
/// Returns `None` (matching [`response_liveness_rescue_atoms`]) if `pairs` is empty or
/// any monitor cannot be built (an atom binds no signal). On `Some`, the second
/// component is the per-response [`ReachOutcome`] (same order as `pairs`) for diagnostics.
pub fn response_liveness_rescue_conjunction(
    design_btor2: &str,
    pairs: &[(Atom, Atom)],
    reset_pinned: bool,
) -> Option<(LivenessVerdict, Vec<ReachOutcome>)> {
    if pairs.is_empty() {
        return None;
    }
    let mut outcomes = Vec::with_capacity(pairs.len());
    let mut any_violated = false;
    let mut any_inconclusive = false;
    for (ante, cons) in pairs {
        let (verdict, outcome) =
            response_liveness_rescue_atoms(design_btor2, ante, cons, reset_pinned)?;
        match verdict {
            LivenessVerdict::Violated => any_violated = true,
            LivenessVerdict::Inconclusive => any_inconclusive = true,
            LivenessVerdict::Holds => {}
        }
        outcomes.push(outcome);
    }
    // Violated dominates Inconclusive dominates Holds (conjunction semantics).
    let verdict = if any_violated {
        LivenessVerdict::Violated
    } else if any_inconclusive {
        LivenessVerdict::Inconclusive
    } else {
        LivenessVerdict::Holds
    };
    Some((verdict, outcomes))
}

/// Parse a request/grant atom string (`"REG op VALUE"`, e.g. `"st == 2"`) into an
/// [`Atom`], for the CLI / API `verify-liveness` surface. Reuses the same predicate
/// grammar as the classifier, so a relational (`reg ⋈ reg`) or malformed atom is
/// rejected identically.
pub fn parse_response_atom(s: &str) -> Result<Atom, String> {
    match parse_predicate_expr(s) {
        Ok(PredicateExpr::Cmp {
            register,
            op,
            value,
        }) => Ok(Atom {
            signal: register,
            op,
            value: value as u128,
        }),
        Ok(_) => Err(format!(
            "`{s}` is not a single register-comparison atom (`REG op VALUE`); relational or \
             compound atoms are out of the response fragment"
        )),
        Err(e) => Err(format!("cannot parse response atom `{s}`: {e:?}")),
    }
}

/// Parse repeatable `"ANTE => CONS"` response strings (the `verify-liveness-all`
/// surface) into `(ante, cons)` [`Atom`] pairs for
/// [`response_liveness_rescue_conjunction`]. Each string is split on the first literal
/// `=>`; both sides are trimmed and parsed with [`parse_response_atom`]. Errors if a
/// string lacks `=>`, or either side is not a single register-comparison atom.
///
/// Reused by the CLI, HTTP API, and SV-direct surfaces so the `--response` grammar is
/// identical everywhere.
pub fn parse_response_pairs(responses: &[String]) -> Result<Vec<(Atom, Atom)>, String> {
    responses
        .iter()
        .map(|r| {
            let (ante, cons) = r.split_once("=>").ok_or_else(|| {
                format!(
                    "response `{r}` must contain `=>` separating the antecedent and consequent \
                     (e.g. `\"req == 1 => grant == 1\"`)"
                )
            })?;
            Ok((
                parse_response_atom(ante.trim())?,
                parse_response_atom(cons.trim())?,
            ))
        })
        .collect()
}

/// Render a list of `"ANTE => CONS"` response strings as the display property
/// `AG((a) -> AF (b)) && AG((c) -> AF (d))`, echoed for provenance across the
/// `verify-liveness-all` surfaces. Splits each string on the first `=>` (falling back
/// to the raw trimmed text when absent — a malformed entry is caught by
/// [`parse_response_pairs`], so this is display-only).
pub fn response_conjunction_property(responses: &[String]) -> String {
    responses
        .iter()
        .map(|r| match r.split_once("=>") {
            Some((ante, cons)) => format!("AG(({}) -> AF ({}))", ante.trim(), cons.trim()),
            None => r.trim().to_string(),
        })
        .collect::<Vec<_>>()
        .join(" && ")
}

// The surface verdict label comes from the canonical
// [`crate::verdict::PropertyVerdict`] (`From<LivenessVerdict>`), so
// `btor2 verify-liveness` reports the same `holds`/`violated`/`unknown` vocabulary as
// every other verify surface (`Inconclusive` folds to `unknown`).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::parser as mu_parser;

    fn parse(s: &str) -> Formula {
        mu_parser::parse(s).unwrap_or_else(|e| panic!("parse `{s}`: {e:?}"))
    }

    // The canonical response shape, both atom orders inside the inner Or.
    #[test]
    fn reduces_response_af() {
        let f = parse("nu Z. ((!(req == 1) || mu Y. ((grant == 1) || [] Y)) && [] Z)");
        let r = reduce_response_af(&f).expect("reducible");
        let ante = r.ante.as_ref().expect("has an antecedent");
        assert_eq!(ante.signal, "req");
        assert_eq!(ante.op, CmpOp::Eq);
        assert_eq!(ante.value, 1);
        assert_eq!(r.cons.signal, "grant");
        assert_eq!(r.cons.op, CmpOp::Eq);
        assert_eq!(r.cons.value, 1);
    }

    // Order independence: `[] Z && core`, `[] Y || b`, `AF b || !a`.
    #[test]
    fn reduces_response_af_reordered() {
        let f = parse("nu Z. (([] Z) && ((mu Y. (([] Y) || (b != 0))) || !(a == 3)))");
        let r = reduce_response_af(&f).expect("reducible under reordering");
        let ante = r.ante.as_ref().expect("has an antecedent");
        assert_eq!(ante.signal, "a");
        assert_eq!(ante.value, 3);
        assert_eq!(r.cons.signal, "b");
        assert_eq!(r.cons.op, CmpOp::Ne);
    }

    /// The SVA translator writes a 1-bit signal as a BARE atom (`a |-> s_eventually b` →
    /// `(!(a) || mu Y. (b || [] Y))`); the reducer reads it as `sig != 0`, as the exact engine
    /// does. It used the strict parser before 2026-10-15 and returned `None` here.
    #[test]
    fn reduce_response_af_accepts_bare_boolean_atoms() {
        let f = parse("nu X. (((!(req) || (mu Y. (grant || [] Y)))) && [] X)");
        let r = reduce_response_af(&f).expect("bare 1-bit atoms reduce");
        let ante = r.ante.expect("has an antecedent");
        assert_eq!(
            (ante.signal.as_str(), ante.op, ante.value),
            ("req", CmpOp::Ne, 0)
        );
        assert_eq!(
            (r.cons.signal.as_str(), r.cons.op, r.cons.value),
            ("grant", CmpOp::Ne, 0)
        );
    }

    /// `assert property (s_eventually b)` translates to `AG AF b` — no antecedent. It reduces
    /// with `ante: None` (every cycle is a request).
    #[test]
    fn reduce_response_af_accepts_antecedent_free_af() {
        let f = parse("nu X. (((mu Y. ((st == 2) || [] Y))) && [] X)");
        let r = reduce_response_af(&f).expect("AG AF b reduces");
        assert!(r.ante.is_none());
        assert_eq!((r.cons.signal.as_str(), r.cons.value), ("st", 2));
        // The diamond (recoverability, AG EF) is still not this shape.
        let ef = parse("nu X. (((mu Y. ((st == 2) || <> Y))) && [] X)");
        assert!(reduce_response_af(&ef).is_none());
    }

    /// `AG AF b` through l2s agrees with the exact engine both ways on the responder: `st == 0`
    /// recurs on every path (HOLDS), but `st == 2` does not — the environment can keep `go` low
    /// and idle forever (VIOLATED). The second is the case an antecedent-ful monitor could not
    /// express: no request ever has to occur.
    #[test]
    fn antecedent_free_af_l2s_matches_exact_both_ways() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        for (target, want_exact, want_l2s) in [
            ("st == 0", ExactVerdict::Holds, LivenessVerdict::Holds),
            ("st == 2", ExactVerdict::Violated, LivenessVerdict::Violated),
        ] {
            let f = parse(&format!("nu X. (((mu Y. (({target}) || [] Y))) && [] X)"));
            let exact = exact_symbolic_verdict(RESPONDER, &f).expect("exact decides");
            assert_eq!(exact, want_exact, "exact on AG AF ({target})");
            let (v, out) = response_liveness_rescue(RESPONDER, &f, false).expect("reducible");
            assert_eq!(v, want_l2s, "l2s on AG AF ({target}): {out:?}");
        }
    }

    /// `AG(a → AXᵐ AF b)` through the request delay line agrees with the exact engine. On the
    /// responder, `st == 2 → AF (st == 2)` holds trivially (b is now), but `st == 2 → AX AF
    /// (st == 2)` is VIOLATED (grant → idle, and the environment may idle forever); the
    /// one-cycle delay is what tells the two apart. `st == 1 → AX AF (st == 2)` holds.
    #[test]
    fn delayed_response_l2s_matches_exact() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        for (formula, want_delay, want_exact, want_l2s) in [
            (
                "nu X. ((!(st == 2) || mu Y. ((st == 2) || [] Y)) && [] X)",
                0,
                ExactVerdict::Holds,
                LivenessVerdict::Holds,
            ),
            (
                "nu X. ((!(st == 2) || [] (mu Y. ((st == 2) || [] Y))) && [] X)",
                1,
                ExactVerdict::Violated,
                LivenessVerdict::Violated,
            ),
            (
                "nu X. ((!(st == 1) || [] (mu Y. ((st == 2) || [] Y))) && [] X)",
                1,
                ExactVerdict::Holds,
                LivenessVerdict::Holds,
            ),
            (
                "nu X. ((!(st == 1) || [] [] (mu Y. ((st == 0) || [] Y))) && [] X)",
                2,
                ExactVerdict::Holds,
                LivenessVerdict::Holds,
            ),
        ] {
            let f = parse(formula);
            let r = reduce_response_af(&f).unwrap_or_else(|| panic!("{formula} reduces"));
            assert_eq!(r.delay, want_delay, "{formula}");
            let exact = exact_symbolic_verdict(RESPONDER, &f).expect("exact decides");
            assert_eq!(exact, want_exact, "exact on {formula}");
            let (v, out) = response_liveness_rescue(RESPONDER, &f, false).expect("reducible");
            assert_eq!(v, want_l2s, "l2s on {formula}: {out:?}");
        }
        // A GUARDED box is not peeled — it is not a plain delay.
        let guarded = parse(
            "nu X. ((!(st == 1) || [(ctrl = controllable)] (mu Y. ((st == 2) || [] Y))) && [] X)",
        );
        assert!(reduce_response_af(&guarded).is_none());
    }

    /// A COMPOUND request and grant (`(req && st == IDLE) |-> s_eventually done`, the usual SVA
    /// antecedent) reduce through `reduce_response_shape` and the boolean compiler, and agree with
    /// the exact engine. State-only atoms, so the exact engine is the oracle.
    #[test]
    fn compound_response_l2s_matches_exact() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        for (formula, want_exact, want_l2s) in [
            // (st==0 || st==1) → AF st==2: idle can idle forever → VIOLATED.
            (
                "nu X. ((!((st == 0) || (st == 1)) || mu Y. ((st == 2) || [] Y)) && [] X)",
                ExactVerdict::Violated,
                LivenessVerdict::Violated,
            ),
            // (st==1 && !(st==0)) → AX AF (st==0 || st==2): holds.
            (
                "nu X. ((!((st == 1) && (!(st == 0))) || [] (mu Y. (((st == 0) || (st == 2)) || [] Y))) && [] X)",
                ExactVerdict::Holds,
                LivenessVerdict::Holds,
            ),
        ] {
            let f = parse(formula);
            let shape = reduce_response_shape(&f).unwrap_or_else(|| panic!("{formula} reduces"));
            assert!(shape.ante.is_some());
            assert!(
                reduce_response_af(&f).is_none(),
                "not single-atom: the atom reducer abstains"
            );
            let exact = exact_symbolic_verdict(RESPONDER, &f).expect("exact decides");
            assert_eq!(exact, want_exact, "exact on {formula}");
            let (v, out) =
                response_liveness_rescue_formula(RESPONDER, &f, &[], false).expect("reducible");
            assert_eq!(v, want_l2s, "l2s on {formula}: {out:?}");
        }
        // A relational leaf cannot be compiled by the monitor — still abstains.
        let rel = parse("nu X. ((!(st == 1) || mu Y. ((x == y) || [] Y)) && [] X)");
        assert!(reduce_response_shape(&rel).is_none());
    }

    /// A primary input in the request — what the exact engine refuses on this path — is a leaf
    /// the boolean compiler binds. On the responder `(go && st == 0) → AX AF (st == 2)` holds:
    /// `go` in idle moves to 1, and 1 always moves to 2.
    #[test]
    fn input_in_the_request_reduces_and_decides() {
        let f =
            parse("nu X. ((!((go == 1) && (st == 0)) || [] (mu Y. ((st == 2) || [] Y))) && [] X)");
        let (v, out) =
            response_liveness_rescue_formula(RESPONDER, &f, &[], false).expect("reducible");
        assert_eq!(v, LivenessVerdict::Holds, "{out:?}");
        let g =
            parse("nu X. ((!((go == 1) && (st == 0)) || [] (mu Y. ((st == 0) || [] Y))) && [] X)");
        let (v, _) =
            response_liveness_rescue_formula(RESPONDER, &g, &[], false).expect("reducible");
        assert_eq!(v, LivenessVerdict::Holds, "1 → 2 → 0: idle recurs");
    }

    // The DIAMOND response (mununu's LTL `G(a->F b)` = AG(a->EF b)) is NOT this
    // shape — the inner recursion is `<> Y`, not `[] Y`. Must abstain.
    #[test]
    fn rejects_diamond_ef_response() {
        let f = parse("nu Z. ((!(req == 1) || mu Y. ((grant == 1) || <> Y)) && [] Z)");
        assert!(reduce_response_af(&f).is_none());
    }

    // A bare AG invariant (no inner Mu) is not a response.
    #[test]
    fn rejects_ag_invariant() {
        let f = parse("nu Z. ((cnt == 3) && [] Z)");
        assert!(reduce_response_af(&f).is_none());
    }

    // A guarded outer box is a stronger property than the monitor checks.
    #[test]
    fn rejects_guarded_box() {
        let f = parse(
            "nu Z. ((!(req == 1) || mu Y. ((grant == 1) || [] Y)) && [(ctrl = controllable)] Z)",
        );
        assert!(reduce_response_af(&f).is_none());
    }

    // A relational consequent (`reg ⋈ reg`) is not a single literal atom.
    #[test]
    fn rejects_relational_atom() {
        let f = parse("nu Z. ((!(req == 1) || mu Y. ((x == y) || [] Y)) && [] Z)");
        assert!(reduce_response_af(&f).is_none());
    }

    // --- Differential correctness: l2s + portfolio vs the exact 3-valued engine ---
    //
    // Both atoms are STATE predicates (`st == k`) so the state-based exact engine can
    // serve as the ground-truth oracle. `go` is a free input (nondeterministic
    // idle→req choice) — allowed; it is not an atom.

    // 3-state responder: st 0=idle, 1=req, 2=grant. idle -go-> req; req -> grant
    // (deterministic); grant -> idle. AG((st==1) → AF (st==2)) HOLDS.
    const RESPONDER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 eq 2 3 4
10 eq 2 3 7
11 ite 1 6 7 4
12 ite 1 10 8 4
13 ite 1 9 11 12
14 next 1 3 13
";

    // 4-state staller: st 0=idle, 1=req, 3=stuck; 2=grant is UNREACHABLE. idle -go->
    // req; req -> stuck; stuck -> stuck. AG((st==1) → AF (st==2)) VIOLATED — a req
    // enters the stuck cycle and grant never comes.
    const STALLER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 ite 1 6 7 4
13 ite 1 11 9 3
14 ite 1 10 12 13
15 next 1 3 14
";

    fn response_formula() -> Formula {
        parse("nu Z. ((!(st == 1) || mu Y. ((st == 2) || [] Y)) && [] Z)")
    }

    #[test]
    fn l2s_holds_matches_exact_on_responder() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let f = response_formula();
        // Exact 3-valued engine — the ground-truth oracle.
        let exact = exact_symbolic_verdict(RESPONDER, &f).expect("exact decides");
        assert_eq!(
            exact,
            ExactVerdict::Holds,
            "responder: AG(req→AF grant) holds"
        );
        // l2s → portfolio must agree.
        let (v, out) = response_liveness_rescue(RESPONDER, &f, false).expect("reducible");
        assert_eq!(v, LivenessVerdict::Holds, "l2s outcome: {out:?}");
    }

    #[test]
    fn l2s_violated_matches_exact_on_staller() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        let f = response_formula();
        let exact = exact_symbolic_verdict(STALLER, &f).expect("exact decides");
        assert_eq!(
            exact,
            ExactVerdict::Violated,
            "staller: AG(req→AF grant) violated"
        );
        let (v, out) = response_liveness_rescue(STALLER, &f, false).expect("reducible");
        assert_eq!(v, LivenessVerdict::Violated, "l2s outcome: {out:?}");
    }

    // The atoms-based surface entry (the CLI / API path) agrees with the formula path.
    #[test]
    fn atoms_entry_matches_formula_entry() {
        let ante = parse_response_atom("st == 1").expect("atom parses");
        let cons = parse_response_atom("st == 2").expect("atom parses");
        assert_eq!(ante.op, CmpOp::Eq);
        assert_eq!(cons.value, 2);
        let (v_r, _) = response_liveness_rescue_atoms(RESPONDER, &ante, &cons, false).expect("ok");
        assert_eq!(v_r, LivenessVerdict::Holds);
        let (v_s, _) = response_liveness_rescue_atoms(STALLER, &ante, &cons, false).expect("ok");
        assert_eq!(v_s, LivenessVerdict::Violated);
    }

    // --- Multi-response conjunction (correct by composition) ---

    // Two responses on RESPONDER, each individually Holds — req→grant and grant→idle —
    // so the conjunction Holds, and the conjunction verdict matches the composition of
    // the individual single-response verdicts.
    #[test]
    fn conjunction_all_hold_is_holds() {
        let p1 = (
            parse_response_atom("st == 1").unwrap(),
            parse_response_atom("st == 2").unwrap(),
        );
        let p2 = (
            parse_response_atom("st == 2").unwrap(),
            parse_response_atom("st == 0").unwrap(),
        );
        let (v1, _) = response_liveness_rescue_atoms(RESPONDER, &p1.0, &p1.1, false).unwrap();
        let (v2, _) = response_liveness_rescue_atoms(RESPONDER, &p2.0, &p2.1, false).unwrap();
        assert_eq!(v1, LivenessVerdict::Holds, "req→grant holds");
        assert_eq!(v2, LivenessVerdict::Holds, "grant→idle holds");
        let (v, outs) =
            response_liveness_rescue_conjunction(RESPONDER, &[p1, p2], false).expect("decides");
        assert_eq!(v, LivenessVerdict::Holds, "both hold ⇒ conjunction holds");
        assert_eq!(outs.len(), 2, "one outcome per response");
    }

    // On STALLER the req→grant response is Violated; conjoined with a trivially-holding
    // response, the conjunction is Violated (Violated dominates).
    #[test]
    fn conjunction_one_violated_is_violated() {
        let p_bad = (
            parse_response_atom("st == 1").unwrap(),
            parse_response_atom("st == 2").unwrap(),
        );
        let p_ok = (
            parse_response_atom("st == 0").unwrap(),
            parse_response_atom("st == 0").unwrap(),
        );
        let (vb, _) = response_liveness_rescue_atoms(STALLER, &p_bad.0, &p_bad.1, false).unwrap();
        assert_eq!(
            vb,
            LivenessVerdict::Violated,
            "req→grant violated on staller"
        );
        let (v, _) =
            response_liveness_rescue_conjunction(STALLER, &[p_ok, p_bad], false).expect("decides");
        assert_eq!(
            v,
            LivenessVerdict::Violated,
            "one violated conjunct ⇒ conjunction violated"
        );
    }

    #[test]
    fn conjunction_empty_is_none() {
        assert!(response_liveness_rescue_conjunction(RESPONDER, &[], false).is_none());
    }

    #[test]
    fn parse_response_atom_rejects_relational() {
        assert!(
            parse_response_atom("x == y").is_err(),
            "relational atom rejected"
        );
        assert!(
            parse_response_atom("not an atom !!").is_err(),
            "garbage rejected"
        );
    }

    // The surface label comes from the canonical PropertyVerdict — Inconclusive folds
    // to the shared `unknown`, not a bespoke `inconclusive`.
    #[test]
    fn liveness_verdict_maps_to_canonical_vocabulary() {
        use crate::verdict::PropertyVerdict;
        assert_eq!(
            PropertyVerdict::from(LivenessVerdict::Holds).as_str(),
            "holds"
        );
        assert_eq!(
            PropertyVerdict::from(LivenessVerdict::Violated).as_str(),
            "violated"
        );
        assert_eq!(
            PropertyVerdict::from(LivenessVerdict::Inconclusive).as_str(),
            "unknown"
        );
    }
}
