//! μ-Calculus infrastructure: intermediate representation, guards, and parser.
//!
//! This module provides the typed representation that other components (parsers,
//! evaluators, optimisers) can share when working with μ-calculus formulas that
//! reference CLTS labels, controllability metadata, and variable valuations.

use crate::clts::{Clts, IdStorage, StateId, Transition};

pub mod env_enforce;
pub mod evaluator;
pub mod gr1;
pub mod gr1_build;
pub mod invert;
mod memo;
pub mod nnf;
pub mod parity_game;
pub mod parity_game_3v;
pub mod parity_game_3v_build;
pub mod parity_game_3v_solve;
pub mod parity_game_3v_solve3v;
pub mod parity_game_3v_subgame;
pub mod parser;
pub mod simplify;
pub mod symbolic;
pub mod trit;

pub use evaluator::{
    ApproximantView, Environment, EvalResult, EvaluationError, EvaluationOptions,
    FixpointConvergenceCallback, FixpointPolarity, PriorApproximant, Signature, WitnessMap,
    evaluate, evaluate_tri, evaluate_tri_with_options, evaluate_with_options,
    evaluate_with_options_and_automaton, evaluate_with_witnesses,
};
pub use parity_game_3v::{
    FailureSubgame, GameEvaluation, Position3v, clts_has_hyper_must_transitions,
    clts_has_non_sharp_transitions, evaluate_3v_game, evaluate_3v_game_with_options,
};
pub use simplify::simplify;
pub use trit::{Trit, TritSet};

/// Classification of a μ-calculus formula by its fixpoint structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyClass {
    /// Pure greatest-fixpoint (nu-only): invariance / safety properties.
    Safety,
    /// Pure least-fixpoint (mu-only): reachability / guarantee properties.
    Reachability,
    /// Mixed fixpoints with alternation depth ≥ 2: liveness / fairness / GR(1).
    Liveness,
    /// Has fixpoints but no alternation (depth 1, mixed mu/nu at same level).
    Mixed,
    /// No fixpoints at all (propositional).
    Propositional,
}

/// Convenience methods for inspecting and traversing [`Node`] values.
pub trait NodeOps {
    /// Returns `true` if this node is a fixpoint (`mu` or `nu`).
    fn is_fixpoint(&self) -> bool;
}

/// Identifier referencing a node inside a [`Formula`] arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) usize);

impl NodeId {
    /// Returns the zero-based index for this node within the arena.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Identifier assigned to a bound fixpoint variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FormulaVarId(pub(crate) usize);

impl FormulaVarId {
    /// Returns the zero-based index of the fixpoint variable inside the formula.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Complete μ-calculus formula captured as an arena of nodes plus fixpoint metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formula {
    root: NodeId,
    nodes: Vec<Node>,
    vars: Vec<FormulaVar>,
}

impl Formula {
    /// Creates a new formula from its constituent arena.
    pub(crate) fn new(root: NodeId, nodes: Vec<Node>, vars: Vec<FormulaVar>) -> Self {
        Self { root, nodes, vars }
    }

    /// Returns the root node identifier.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Provides read-only access to all owned nodes.
    /// The value of a formula with NO atoms — `nu X. (true && [] X)`, the shape an
    /// `ASSERT_KNOWN` becomes once `$isunknown` folds to `false` — on any model whose transition
    /// relation is TOTAL (a BTOR2 design: every state has a successor), or `None` if the formula
    /// reads a predicate or a guarded modality.
    ///
    /// An atom-less formula cannot tell two states apart, and with a total relation every state is
    /// bisimilar to the one-state self-loop, so the formula's value there is its value everywhere:
    /// `[] φ` and `<> φ` are `φ`, and a fixpoint settles within two iterations over the two-point
    /// lattice. Deciding it here spares the engines a cone that is the WHOLE design (no atom to
    /// restrict it) and, over the bit cap, a skip for a property that holds trivially.
    pub fn constant_value(&self) -> Option<bool> {
        fn eval(f: &Formula, id: NodeId, env: &mut Vec<(FormulaVarId, bool)>) -> Option<bool> {
            Some(match f.node(id) {
                Node::True => true,
                Node::False => false,
                Node::Predicate(_) => return None,
                Node::Variable(v) => env.iter().rev().find(|(w, _)| w == v)?.1,
                Node::Not(a) => !eval(f, *a, env)?,
                Node::And(a, b) => eval(f, *a, env)? & eval(f, *b, env)?,
                Node::Or(a, b) => eval(f, *a, env)? | eval(f, *b, env)?,
                Node::Modal { guard, target, .. } => {
                    if *guard != Guard::default() {
                        return None;
                    }
                    eval(f, *target, env)?
                }
                Node::Mu { var, body } | Node::Nu { var, body } => {
                    let mut cur = matches!(f.node(id), Node::Nu { .. });
                    for _ in 0..3 {
                        env.push((*var, cur));
                        let next = eval(f, *body, env);
                        env.pop();
                        let next = next?;
                        if next == cur {
                            break;
                        }
                        cur = next;
                    }
                    cur
                }
            })
        }
        eval(self, self.root(), &mut Vec::new())
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Returns the node associated with `id`.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// Provides read-only access to the fixpoint variable table.
    pub fn vars(&self) -> &[FormulaVar] {
        &self.vars
    }

    /// Returns metadata for the requested fixpoint variable.
    pub fn var(&self, id: FormulaVarId) -> &FormulaVar {
        &self.vars[id.0]
    }

    /// Returns fixpoint variables in nesting order (outermost first) with their
    /// polarity. This defines the signature structure for strategy extraction.
    ///
    /// Each entry is `(FormulaVarId, is_mu)` where `is_mu = true` for least
    /// fixpoints and `false` for greatest fixpoints. The ordering determines
    /// significance in the lexicographic signature comparison: earlier entries
    /// (outer fixpoints) are more significant.
    pub fn fixpoint_nesting_order(&self) -> Vec<(FormulaVarId, bool)> {
        let mut result = Vec::new();
        self.collect_fixpoint_order(self.root, &mut result);
        result
    }

    /// Returns the FormulaVarIds of all mu-fixpoints in nesting order
    /// (outermost first). These are the "obligations" a memory-aware
    /// controller rotates through to ensure each mu-fixpoint receives fair
    /// progress under alternation. Used by `ControllerMode::ProductGame`.
    pub fn mu_obligations(&self) -> Vec<FormulaVarId> {
        self.fixpoint_nesting_order()
            .into_iter()
            .filter_map(|(v, is_mu)| if is_mu { Some(v) } else { None })
            .collect()
    }

    fn collect_fixpoint_order(&self, id: NodeId, out: &mut Vec<(FormulaVarId, bool)>) {
        match self.node(id) {
            Node::Mu { var, body } => {
                out.push((*var, true));
                self.collect_fixpoint_order(*body, out);
            }
            Node::Nu { var, body } => {
                out.push((*var, false));
                self.collect_fixpoint_order(*body, out);
            }
            Node::And(l, r) | Node::Or(l, r) => {
                self.collect_fixpoint_order(*l, out);
                self.collect_fixpoint_order(*r, out);
            }
            Node::Not(inner) => self.collect_fixpoint_order(*inner, out),
            Node::Modal { target, .. } => self.collect_fixpoint_order(*target, out),
            Node::True | Node::False | Node::Predicate(_) | Node::Variable(_) => {}
        }
    }

    /// Computes the alternation depth of the formula.
    ///
    /// Alternation depth counts the maximum nesting of mu inside nu (or vice
    /// versa). Depth 0 = no fixpoints, depth 1 = single-type fixpoints (safety
    /// or reachability), depth 2+ = requires memory for correct strategy
    /// extraction (liveness, GR(1), parity).
    pub fn alternation_depth(&self) -> usize {
        self.ad_node(self.root, &mut vec![None; self.nodes.len()])
    }

    /// Recursive helper for alternation depth computation.
    /// `cache[node_index]` caches computed depths.
    fn ad_node(&self, id: NodeId, cache: &mut Vec<Option<usize>>) -> usize {
        if let Some(d) = cache[id.0] {
            return d;
        }
        let depth = match &self.nodes[id.0] {
            Node::True | Node::False | Node::Predicate(_) | Node::Variable(_) => 0,
            Node::Not(inner) => self.ad_node(*inner, cache),
            Node::And(l, r) | Node::Or(l, r) => {
                self.ad_node(*l, cache).max(self.ad_node(*r, cache))
            }
            Node::Modal { target, .. } => self.ad_node(*target, cache),
            Node::Mu { body, .. } => {
                let body_depth = self.ad_node(*body, cache);
                // Check if the body contains a nu — that's an alternation
                if self.contains_opposite_fixpoint(*body, true) {
                    body_depth.max(1) + 1
                } else {
                    body_depth.max(1)
                }
            }
            Node::Nu { body, .. } => {
                let body_depth = self.ad_node(*body, cache);
                // Check if the body contains a mu — that's an alternation
                if self.contains_opposite_fixpoint(*body, false) {
                    body_depth.max(1) + 1
                } else {
                    body_depth.max(1)
                }
            }
        };
        cache[id.0] = Some(depth);
        depth
    }

    /// Number of `Box` (`[]`) modalities anywhere in the formula.
    ///
    /// A box quantifies universally over successors, so it is evaluated
    /// against the **may** relation. When the may relation is an
    /// under-approximation (the sampling `MayEdgeInference::Off` path), a
    /// missing may-successor makes `[]φ` vacuously easier to satisfy — a
    /// definite `KleeneT` on a box property is then unsound. Callers use this
    /// to decide whether to emit an A.4 sampling-may soundness warning.
    pub fn box_modality_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| {
                matches!(
                    n,
                    Node::Modal {
                        kind: ModalKind::Box,
                        ..
                    }
                )
            })
            .count()
    }

    /// Whether the formula contains ANY modality (`[]` or `<>`). A modal formula's
    /// definite verdict depends on the may relation's completeness in BOTH directions —
    /// a definite `KleeneT` on `[]φ` (all may-successors) and a definite `KleeneF` on
    /// `<>φ` / `EF` reachability (no may-path) both become unsound if the may relation is
    /// an UNDER-approximation (the sampling `MayEdgeInference::Off` path with a capped
    /// input enumeration). A purely propositional formula is unaffected. Callers use this
    /// to decide whether a cube definite must be downgraded to `⊥` under sampling-may.
    pub fn has_modality(&self) -> bool {
        self.nodes.iter().any(|n| matches!(n, Node::Modal { .. }))
    }

    /// Returns true if the subtree rooted at `id` contains a fixpoint of the
    /// opposite kind. `looking_for_nu = true` means we're inside a mu and
    /// looking for a nu (and vice versa).
    fn contains_opposite_fixpoint(&self, id: NodeId, looking_for_nu: bool) -> bool {
        match &self.nodes[id.0] {
            Node::True | Node::False | Node::Predicate(_) | Node::Variable(_) => false,
            Node::Not(inner) => self.contains_opposite_fixpoint(*inner, looking_for_nu),
            Node::And(l, r) | Node::Or(l, r) => {
                self.contains_opposite_fixpoint(*l, looking_for_nu)
                    || self.contains_opposite_fixpoint(*r, looking_for_nu)
            }
            Node::Modal { target, .. } => self.contains_opposite_fixpoint(*target, looking_for_nu),
            Node::Nu { .. } if looking_for_nu => true,
            Node::Mu { .. } if !looking_for_nu => true,
            Node::Mu { body, .. } | Node::Nu { body, .. } => {
                self.contains_opposite_fixpoint(*body, looking_for_nu)
            }
        }
    }

    /// Classifies this formula into a [`PropertyClass`] based on its fixpoint
    /// structure.
    pub fn property_class(&self) -> PropertyClass {
        let (has_mu, has_nu) = self.fixpoint_kinds(self.root);
        let ad = self.alternation_depth();

        match (has_mu, has_nu, ad) {
            (false, false, _) => PropertyClass::Propositional,
            (false, true, _) => PropertyClass::Safety,
            (true, false, _) => PropertyClass::Reachability,
            (true, true, d) if d >= 2 => PropertyClass::Liveness,
            _ => PropertyClass::Mixed,
        }
    }

    /// Returns (has_mu, has_nu) for the subtree rooted at `id`.
    fn fixpoint_kinds(&self, id: NodeId) -> (bool, bool) {
        match &self.nodes[id.0] {
            Node::True | Node::False | Node::Predicate(_) | Node::Variable(_) => (false, false),
            Node::Not(inner) => self.fixpoint_kinds(*inner),
            Node::And(l, r) | Node::Or(l, r) => {
                let (lm, ln) = self.fixpoint_kinds(*l);
                let (rm, rn) = self.fixpoint_kinds(*r);
                (lm || rm, ln || rn)
            }
            Node::Modal { target, .. } => self.fixpoint_kinds(*target),
            Node::Mu { body, .. } => {
                let (_, bn) = self.fixpoint_kinds(*body);
                (true, bn)
            }
            Node::Nu { body, .. } => {
                let (bm, _) = self.fixpoint_kinds(*body);
                (bm, true)
            }
        }
    }
}

/// PO-2 (cube-modal soundness audit, 2026-06-23) — scan a formula for
/// modality forms that fall OUTSIDE the one audited-sound predicate-cube
/// fragment (`Control::All`, bare/label-agnostic, unbounded — the only
/// slice the §4.5 preservation theorem covers over a cube; see
/// `.claude/reviews/cube-modal-soundness/`). Returns one human-readable
/// soundness warning per offending modality (deduplicated).
///
/// `cube_labels` is the set of label symbols the cube actually carries: a
/// label-specific modality on one of those is sound (it equals the bare
/// modality), while one on any other label is *vacuous* because the cube
/// collapses every concrete action onto its own label(s). The `control`
/// and `bounded` checks are cube-label-independent (out-of-fragment over
/// any cube).
///
/// Used by the CEGAR loop ([`crate::adapter::btor2::cegar`]) to surface a
/// warning on the `btor2 cegar` / `sv cegar` / `/cegar` cube surfaces,
/// making the out-of-fragment boundary explicit rather than silently
/// returning an unsound/unaudited/vacuous verdict.
pub fn cube_modality_soundness_warnings(
    formula: &Formula,
    cube_labels: &std::collections::HashSet<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for node in formula.nodes() {
        let Node::Modal { kind, guard, .. } = node else {
            continue;
        };
        let op = match kind {
            ModalKind::Box => "[]",
            ModalKind::Diamond => "<>",
        };
        if guard.control != Control::All {
            out.push(format!(
                "{op} modality with ctrl={:?} over a predicate cube: the per-player \
                 (controller × environment) 3-valued game semantics is SOUND as of \
                 PO-3 / R.6.8 (de Alfaro–Godefroid–Jagadeesan LICS 2004; evaluate_tri \
                 routes the controllability arms through the per-player rule). BUT over a \
                 plain *verification* cube the controllability partition is a build-time \
                 default (the lone `step` label is Controllable-by-default), so a \
                 controllability verdict is only MEANINGFUL over a controllability-aware \
                 (R.6.6) cube with declared controllable inputs. Use a bare {op} for plain \
                 verification.",
                guard.control
            ));
        }
        if let Some(k) = guard.max_steps {
            out.push(format!(
                "{op} bounded modality (steps={k}) over a predicate cube: the may/must \
                 (3-valued) filter is NOT applied to bounded modal steps (PO-4 / R.6.3.b), \
                 so the 3-valued soundness of the bounded verdict is not established. Use an \
                 unbounded {op} for a sound cube verdict."
            ));
        }
        for lbl in &guard.labels {
            if !cube_labels.contains(lbl.as_str()) {
                out.push(format!(
                    "{op}{{{lbl}}} label-specific modality over a predicate cube: the cube \
                     collapses every concrete action onto its own label(s) {cube_labels:?}, so \
                     a modality on '{lbl}' matches no transition and is VACUOUS. Only bare \
                     (label-agnostic) {op} modalities are sound over a cube."
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Arena entry describing a single fixpoint variable (name only for now).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormulaVar {
    pub name: String,
}

/// Typed μ-calculus node inside a [`Formula`] arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    True,
    False,
    Predicate(String),
    Variable(FormulaVarId),
    Not(NodeId),
    And(NodeId, NodeId),
    Or(NodeId, NodeId),
    Modal {
        kind: ModalKind,
        guard: Guard,
        target: NodeId,
    },
    Mu {
        var: FormulaVarId,
        body: NodeId,
    },
    Nu {
        var: FormulaVarId,
        body: NodeId,
    },
}

impl NodeOps for Node {
    #[inline]
    fn is_fixpoint(&self) -> bool {
        matches!(self, Node::Mu { .. } | Node::Nu { .. })
    }
}

/// Distinguishes box vs. diamond modalities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    Box,
    Diamond,
}

/// Controllability guard applied to modalities.
///
/// - `All`: no controllability distinction (all transitions treated equally)
/// - `Controllable`: system perspective — all uncontrollable must satisfy, some controllable must
/// - `Environment`: environment perspective (dual of Controllable) — some uncontrollable satisfies OR all controllable satisfy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Control {
    #[default]
    All,
    Controllable,
    Environment,
}

/// Variable guard describing required/forbidden symbols.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VariableGuard {
    pub required: Vec<String>,
    pub forbidden: Vec<String>,
}

/// Aggregated guard information for label/variable/controllability constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub labels: Vec<String>,
    pub current: VariableGuard,
    pub next: VariableGuard,
    pub control: Control,
    pub max_steps: Option<u32>,
}

impl Default for Guard {
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            current: VariableGuard::default(),
            next: VariableGuard::default(),
            control: Control::All,
            max_steps: None,
        }
    }
}

/// Builder for constructing μ-calculus formulas programmatically.
#[derive(Debug, Default)]
pub struct FormulaBuilder {
    nodes: Vec<Node>,
    vars: Vec<FormulaVar>,
}

impl FormulaBuilder {
    /// Adds a node to the formula and returns its identifier.
    pub fn push_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len());
        self.nodes.push(node);
        id
    }

    /// Adds a fixpoint variable to the formula and returns its identifier.
    pub fn push_var(&mut self, name: String) -> FormulaVarId {
        let id = FormulaVarId(self.vars.len());
        self.vars.push(FormulaVar { name });
        id
    }

    /// Returns the node associated with the given identifier.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    /// Consumes the builder and creates a formula with the given root node.
    pub fn into_formula(self, root: NodeId) -> Formula {
        Formula::new(root, self.nodes, self.vars)
    }
}

// ---------------------------------------------------------------------------
// Shared guard filter — used by both the evaluator and the parity game
// ---------------------------------------------------------------------------

/// Returns `true` if `transition` passes the label and state-variable sub-filters
/// of `guard`.
///
/// This is the common "phase 2–4" filter shared by:
/// - [`evaluator::EvalContext::guard_matches`] (which additionally handles
///   controllability at the `eval_modal` dispatch level before calling here).
/// - [`parity_game::transition_matches_guard`] (which checks controllability
///   upfront, then delegates here for the label and variable filters).
///
/// Checks performed (in order):
/// 1. **Label-name filter**: the transition must carry AT LEAST ONE of the names in
///    `guard.labels` — a label set is any-of, the textbook reading of a labelled modality
///    `[K] φ` and what the Mu-Calculus Reference documents. (Until mununu#590 it was all-of,
///    a synchronisation-vector match, by accident of the loop: `[labels = {a, c}] φ` was
///    vacuously true wherever no single edge carried both `a` and `c`.) A guard that wants
///    exactly a vector `{a, b}` is the conjunction of its single-label guards on the same
///    edge, which no guard can express; "this exact vector is enabled" is a transition
///    property, not a modality.
/// 2. **Current-state variable filter**: `guard.current.required` names must
///    all be present in the source state's variable set; `guard.current.forbidden`
///    names must all be absent.
/// 3. **Next-state variable filter**: same logic applied to the target state.
///
/// Controllability (`guard.control`) is **not** checked here. Callers are
/// responsible for controllability filtering before calling this function.
pub(crate) fn guard_matches_labels_and_vars<S, L>(
    source: StateId<S>,
    transition: &Transition<S, L>,
    guard: &Guard,
    clts: &Clts<S, L>,
) -> bool
where
    S: IdStorage,
    L: IdStorage,
{
    // 1. Label-name filter: the transition carries at least one of the guard's labels
    //    (any-of; mununu#590).
    if !guard.labels.is_empty() {
        let found = guard.labels.iter().any(|wanted| {
            transition.labels().iter().any(|label_id| {
                clts.label_bitset(*label_id)
                    .is_some_and(|bitset| bitset.test(wanted.as_str()))
            })
        });
        if !found {
            return false;
        }
    }

    // 2. Current-state variable filter.
    if !guard.current.required.is_empty() || !guard.current.forbidden.is_empty() {
        let state_vars = clts.state_variable_bitset(source);
        if guard
            .current
            .required
            .iter()
            .any(|var| !state_vars.contains(var.as_str()))
        {
            return false;
        }
        if guard
            .current
            .forbidden
            .iter()
            .any(|var| state_vars.contains(var.as_str()))
        {
            return false;
        }
    }

    // 3. Next-state variable filter.
    if !guard.next.required.is_empty() || !guard.next.forbidden.is_empty() {
        let target_vars = clts.state_variable_bitset(transition.target());
        if guard
            .next
            .required
            .iter()
            .any(|var| !target_vars.contains(var.as_str()))
        {
            return false;
        }
        if guard
            .next
            .forbidden
            .iter()
            .any(|var| target_vars.contains(var.as_str()))
        {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::PropertyClass;
    use super::parser;

    #[test]
    fn parses_basic_formula() {
        let parsed = parser::parse("true").expect("formula parses");
        let root = parsed.root();
        assert!(matches!(parsed.node(root), super::Node::True));
    }

    /// PO-2 — the cube-modal soundness linter flags out-of-fragment
    /// modality forms (controllability / bounded / non-cube-label) and
    /// stays silent on the audited-sound fragment (bare `Control::All`).
    #[test]
    fn po2_cube_modality_soundness_warnings() {
        use super::cube_modality_soundness_warnings;
        let cube_labels: std::collections::HashSet<&str> = ["step"].into_iter().collect();
        let warn = |s: &str| {
            cube_modality_soundness_warnings(&parser::parse(s).expect("parse"), &cube_labels)
        };

        // Sound fragment: no warnings.
        assert!(warn("<>p").is_empty(), "bare diamond is in-fragment");
        assert!(warn("[]p").is_empty(), "bare box is in-fragment");
        assert!(
            warn("<step>p").is_empty(),
            "<step> == bare over a single-`step` cube ⇒ sound"
        );
        assert!(
            warn("nu X. (p && [] X)").is_empty(),
            "alternation-free bare safety is in-fragment"
        );

        // Controllability ⇒ semantics SOUND (PO-3 / R.6.8 closed), but the
        // partition is meaningful only over a controllability-aware cube — still
        // one advisory warning.
        let w = warn("[ (ctrl = controllable) ] p");
        assert_eq!(w.len(), 1, "one ctrl warning: {w:?}");
        assert!(w[0].contains("SOUND") && w[0].contains("R.6.8") && w[0].contains("MEANINGFUL"));

        // Bounded ⇒ 3-valued-blind (PO-4).
        let w = warn("< (steps = 5) > p");
        assert_eq!(w.len(), 1, "one bounded warning: {w:?}");
        assert!(w[0].contains("steps=5") && w[0].contains("R.6.3.b"));

        // Label-specific on a non-cube label ⇒ vacuous.
        let w = warn("<foo>p");
        assert_eq!(w.len(), 1, "one label warning: {w:?}");
        assert!(w[0].contains("VACUOUS") && w[0].contains("foo"));

        // Combined on one node ⇒ both control + bounded fire; dedup keeps
        // distinct messages.
        let w = warn("< (steps = 2, ctrl = controllable) > p");
        assert_eq!(w.len(), 2, "control + bounded both flagged: {w:?}");
    }

    #[test]
    fn alternation_depth_propositional() {
        let f = parser::parse("true && !false").unwrap();
        assert_eq!(f.alternation_depth(), 0);
        assert_eq!(f.property_class(), PropertyClass::Propositional);
    }

    #[test]
    fn alternation_depth_safety() {
        // nu X. (p && [] X) — pure greatest fixpoint, depth 1
        let f = parser::parse("nu X. (p && [] X)").unwrap();
        assert_eq!(f.alternation_depth(), 1);
        assert_eq!(f.property_class(), PropertyClass::Safety);
    }

    #[test]
    fn alternation_depth_reachability() {
        // mu X. (p || <> X) — pure least fixpoint, depth 1
        let f = parser::parse("mu X. (p || <> X)").unwrap();
        assert_eq!(f.alternation_depth(), 1);
        assert_eq!(f.property_class(), PropertyClass::Reachability);
    }

    #[test]
    fn alternation_depth_liveness() {
        // nu X. (mu Y. (p || <> Y)) && [] X — mu inside nu, depth 2
        let f = parser::parse("nu X. ((mu Y. (p || <> Y)) && [] X)").unwrap();
        assert_eq!(f.alternation_depth(), 2);
        assert_eq!(f.property_class(), PropertyClass::Liveness);
    }

    #[test]
    fn alternation_depth_nested_same_kind() {
        // nu X. (nu Y. (p && [] Y) && [] X) — nested nu, no alternation, depth 1
        let f = parser::parse("nu X. ((nu Y. (p && [] Y)) && [] X)").unwrap();
        assert_eq!(f.alternation_depth(), 1);
        assert_eq!(f.property_class(), PropertyClass::Safety);
    }

    #[test]
    fn mu_obligations_returns_all_mu_vars_in_nesting_order() {
        // GR(1)-like: nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [] X)
        let f = parser::parse("nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [] X)")
            .unwrap();
        let obligations = f.mu_obligations();
        assert_eq!(
            obligations.len(),
            2,
            "two mu-fixpoints (Y1, Y2) expected as obligations"
        );
        // Confirm the names are the mu vars (Y1 outer, Y2 inner) — order is
        // outermost-first per fixpoint_nesting_order.
        let names: Vec<&str> = obligations
            .iter()
            .map(|v| f.var(*v).name.as_str())
            .collect();
        assert_eq!(names, vec!["Y1", "Y2"]);
    }

    #[test]
    fn mu_obligations_empty_for_pure_nu_safety() {
        let f = parser::parse("nu X. (p && [] X)").unwrap();
        assert!(f.mu_obligations().is_empty());
    }

    #[test]
    fn mu_obligations_single_for_pure_reachability() {
        let f = parser::parse("mu X. (p || <> X)").unwrap();
        assert_eq!(f.mu_obligations().len(), 1);
    }

    #[test]
    fn box_modality_count_counts_only_boxes() {
        // A.4 (2026-07-05) — counts `[]` nodes; ignores `<>` and non-modal nodes.
        assert_eq!(parser::parse("true").unwrap().box_modality_count(), 0);
        assert_eq!(
            parser::parse("mu X. (p || <> X)")
                .unwrap()
                .box_modality_count(),
            0
        );
        assert_eq!(
            parser::parse("nu X. (p && [] X)")
                .unwrap()
                .box_modality_count(),
            1
        );
        // GR(1)-like: one outer [] plus two diamonds → box count = 1.
        assert_eq!(
            parser::parse("nu X. ((mu Y1. (A || <> Y1)) && (mu Y2. (B || <> Y2)) && [] X)")
                .unwrap()
                .box_modality_count(),
            1
        );
        // Two nested boxes → count = 2.
        assert_eq!(
            parser::parse("nu X. ((nu Y. (p && [] Y)) && [] X)")
                .unwrap()
                .box_modality_count(),
            2
        );
    }

    #[test]
    fn has_modality_detects_box_and_diamond_but_not_propositional() {
        // `has_modality` — any `[]` OR `<>` makes a formula modal (its verdict depends on the
        // transition relation); a purely propositional formula is not. (Originally the A.4
        // ⊥-guard's modal gate, retired in AR-S2; kept as a general Formula utility.)
        assert!(!parser::parse("p").unwrap().has_modality());
        assert!(!parser::parse("(p || q) && !r").unwrap().has_modality());
        assert!(parser::parse("mu X. (p || <> X)").unwrap().has_modality()); // EF (diamond)
        assert!(parser::parse("nu X. (p && [] X)").unwrap().has_modality()); // AG (box)
        // AG EF — the ibex_controller shape: both box and diamond.
        assert!(
            parser::parse("nu Y. ((mu X. (p or <> X)) and [] Y)")
                .unwrap()
                .has_modality()
        );
    }
}

#[cfg(test)]
mod constant_value_tests {
    /// mununu#722 — an atom-less formula has one value on every total model; a predicate or a
    /// guarded modality leaves it to the engines.
    #[test]
    fn an_atom_less_formula_is_decided_without_a_model() {
        let v = |s: &str| {
            crate::mu_calculus::parser::parse(s)
                .unwrap()
                .constant_value()
        };
        assert_eq!(v("nu X. (true && [] X)"), Some(true));
        assert_eq!(v("nu X. ((!(true)) || [] X)"), Some(true));
        assert_eq!(v("nu X. (false && [] X)"), Some(false));
        assert_eq!(v("mu X. (false || <> X)"), Some(false));
        assert_eq!(v("nu X. ((mu Y. (true || [] Y)) && [] X)"), Some(true));
        assert_eq!(v("nu X. ((a == 1) && [] X)"), None);
        assert_eq!(v("nu X. (a && [] X)"), None);
    }
}
