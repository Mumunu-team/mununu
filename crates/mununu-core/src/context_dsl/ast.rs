use crate::context_dsl::token::Span;
use crate::ltl::LtlFormula;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct ContextDoc {
    pub name: Ident,
    pub alphabet: Vec<AlphabetEntry>,
    pub constants: Vec<ConstantEntry>,
    pub ranges: Vec<RangeEntry>,
    pub enums: Vec<EnumDecl>,
    pub automata: Vec<Automaton>,
    pub compositions: Vec<Composition>,
    pub controllers: Vec<Controller>,
    pub mu_formulas: Vec<MuFormula>,
    pub span: Span,
    /// Side-channel structured state valuations from adapter cross-product enumeration.
    /// Keyed by `automaton_name → state_name → { variable → display_value }`.
    /// Not encoded in CTXDSL text — injected after parsing by adapter callers.
    pub state_valuations: HashMap<String, HashMap<String, BTreeMap<String, String>>>,
    /// Side-channel per-transition Mealy observations from adapters that model
    /// input-dependent outputs. Keyed by `automaton_name → list of observation
    /// rows`. Not encoded in CTXDSL text — injected after parsing by adapter
    /// callers and consumed by trace renderers (CLI counterexample / counterstrategy).
    pub transition_observations: HashMap<String, Vec<crate::adapter::TransitionObservation>>,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: Ident,
    pub variants: Vec<Ident>,
}

#[derive(Debug, Clone, Default)]
pub struct ControllerOptions {
    pub minimize: Option<bool>,
    pub diagnostics: Option<DiagnosticsConfig>,
}

#[derive(Debug, Clone, Default)]
pub struct DiagnosticsConfig {
    pub counterexample: Option<bool>,
    pub deadlock_traces: Option<bool>,
    pub max_counter_traces: Option<u32>,
    pub proof_obligations: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

impl Ident {
    pub fn new(name: String, span: Span) -> Self {
        Self { name, span }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub id: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlphabetEntry {
    pub name: Ident,
    pub display: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConstantEntry {
    pub name: Ident,
    pub value: i64,
}

#[derive(Debug, Clone)]
pub struct RangeEntry {
    pub name: Ident,
    pub lower: Expr,
    pub upper: Expr,
}

#[derive(Debug, Clone)]
pub struct Automaton {
    pub name: Ident,
    pub meta: Meta,
    pub parameters: Vec<Parameter>,
    pub alphabet: Vec<AlphabetRef>,
    pub controllable: Vec<AlphabetRef>,
    pub internal: Vec<AlphabetRef>,
    pub controllable_declared: bool,
    pub internal_declared: bool,
    pub variables: Vec<VariableDecl>,
    pub state_groups: Vec<StateGroup>,
    pub states: Vec<StateDecl>,
    pub transitions: Vec<TransitionDecl>,
    pub predicates: Vec<PredicateDecl>,
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub name: Ident,
    pub spec: RangeSpec,
}

#[derive(Debug, Clone)]
pub enum RangeSpec {
    Named(Ident),
    Bounds { lower: Expr, upper: Expr },
}

#[derive(Debug, Clone)]
pub struct AlphabetRef {
    pub name: Ident,
    pub index: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct VariableDecl {
    pub name: Ident,
    pub index: Option<Expr>,
    pub ty: TypeName,
    pub init: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeName {
    Bool,
    I64,
    Enum(String),
}

#[derive(Debug, Clone)]
pub struct StateDecl {
    pub name: Ident,
    pub index: Option<StateIndexSpec>,
    pub is_initial: bool,
    /// Per-state variable initialiser overrides, written as
    /// `vars { x = 1; y = 2; }` inside the optional outer block on a state
    /// declaration. These flow through the unrolling pipeline and become
    /// part of the unrolled state space.
    pub overrides: Vec<Assignment>,
    /// Per-state structured valuations, written as `valuations { is_red = 1; … }`
    /// inside the optional outer block on a state declaration. These do not
    /// affect the formal model — they are display-only metadata that the
    /// realize layer registers on the CLTS via `Clts::with_valuation_for_state`
    /// and that the trace renderer prints alongside state names in
    /// counterexamples / counterstrategies.
    pub valuations: Vec<Assignment>,
    /// Per-state 3-valued (Kleene) predicate labels, written as
    /// `predicates_3v { p = unknown; q = true; r = false; }` inside the optional
    /// outer block on a state declaration. Unlike the 2-valued `predicates`
    /// block (predicate ⇒ true at a state), these carry a full `Tristate`
    /// (`true` / `false` / `unknown`) and realize into
    /// [`crate::clts::Clts::with_3valued_predicate`] — the round-trippable
    /// surface for a predicate-cube KMTS's `state_3valued_predicates`.
    pub three_valued: Vec<ThreeValuedDecl>,
}

/// One `<predicate> = <tristate>` entry inside a state's `predicates_3v { … }`
/// block.
#[derive(Debug, Clone)]
pub struct ThreeValuedDecl {
    pub name: Ident,
    pub value: TristateLit,
}

/// CTXDSL surface literal for a Kleene tristate, mapping to
/// [`crate::clts::Tristate`] at the realize step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TristateLit {
    /// `true` ⇒ `Tristate::KleeneT`.
    True,
    /// `false` ⇒ `Tristate::KleeneF`.
    False,
    /// `unknown` ⇒ `Tristate::KleeneBot`.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct StateGroup {
    pub name: Ident,
    pub members: Vec<StateSelector>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StateIndexSpec {
    Range { symbol: Ident, range: Ident },
    Expr(Expr),
}

/// R.5 Item K sub-item K.1 (2026-06-04) — CTXDSL-level modality
/// annotation on a transition declaration. Maps to
/// [`crate::clts::TransitionModality`] at the realize step (K.2).
///
/// Syntax: `transition s -> t on label a [<modality>];` where
/// `<modality>` is one of `may`, `must`, or `sharp`. The
/// attribute is OPTIONAL — transitions without it default to
/// `Sharp`, matching the pre-K.1 grammar exactly (backward
/// compatibility for every existing CTXDSL fixture).
///
/// **K.1 MVP scope.** Each modality variant maps to a single-
/// target transition. The `Must` variant maps to
/// [`crate::clts::TransitionModality::MustHyperOnly`] with a
/// singleton target at the realize step. A K.1 follow-up
/// (likely K.1b) will add multi-target hyper-must syntax
/// (`transition s -> [t1, t2, t3] on a [must];`) for true GKMTS
/// hyper-must edges.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TransitionModalitySpec {
    /// Default; matches pre-K.1 grammar. Realizes to
    /// `TransitionModality::Sharp`.
    #[default]
    Sharp,
    /// `[may]` attribute. Realizes to
    /// `TransitionModality::MayOnly`. Used by KMTS-aware
    /// adapters (or hand-authored CTXDSL) to express "this
    /// transition is in the over-approximation but not the
    /// under-approximation."
    MayOnly,
    /// `[must]` attribute. Realizes to
    /// `TransitionModality::MustHyperOnly` with a singleton
    /// target set. The multi-target hyper-must syntax is a K.1
    /// follow-up.
    MustOnly,
}

#[derive(Debug, Clone)]
pub struct TransitionDecl {
    pub source: StateSelector,
    pub target: StateSelector,
    pub label: TransitionLabel,
    pub additional_labels: Vec<TransitionLabel>,
    pub guard: Option<Expr>,
    pub effects: Vec<Assignment>,
    /// R.5 Item K sub-item K.1 (2026-06-04) — modality
    /// attribute. Defaults to `Sharp` when the source CTXDSL
    /// omits the `[may]` / `[must]` / `[sharp]` attribute on
    /// the transition, preserving pre-K.1 behaviour.
    pub modality: TransitionModalitySpec,
    /// R.5 Item K sub-item K.1b (2026-06-06) — additional
    /// hyper-must target states for the multi-target
    /// `transition s -> [t1, t2, t3] on a [must];` syntax.
    /// The primary `target` is the first hyper-target (`t1`);
    /// this field carries `[t2, t3, ...]`. Default `Vec::new()`
    /// — matches pre-K.1b grammar (singleton hyper-must when
    /// `modality == MustOnly`).
    ///
    /// Only load-bearing for `modality == MustOnly`; ignored by
    /// realize when modality is `Sharp` or `MayOnly`. The
    /// emitter writes the bracketed-list syntax iff
    /// `!additional_targets.is_empty()`.
    pub additional_targets: Vec<StateSelector>,
}

#[derive(Debug, Clone)]
pub struct PredicateDecl {
    pub name: Ident,
    pub target: PredicateTarget,
}

#[derive(Debug, Clone)]
pub enum PredicateTarget {
    State(StateRef),
}

#[derive(Debug, Clone)]
pub enum StateSelector {
    Named(StateRef),
    Group(Ident),
    Wildcard(WildcardPattern),
}

#[derive(Debug, Clone)]
pub struct WildcardPattern {
    pub pattern: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StateRef {
    Simple(Ident),
    Indexed { name: Ident, indices: Vec<Expr> },
}

#[derive(Debug, Clone)]
pub enum TransitionLabel {
    Named { name: Ident, index: Option<Expr> },
    Epsilon(Span),
}

#[derive(Debug, Clone)]
pub struct Assignment {
    pub target: Ident,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub struct MemberRef {
    pub name: Ident,
    pub index: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct Composition {
    pub name: Ident,
    pub meta: Meta,
    pub kind: CompositionKind,
    pub members: Vec<MemberRef>,
    /// `allow_dead_vectors;` — a compound action no partner can match is a warning, not a
    /// realization error (mununu#570).
    pub allow_dead_vectors: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionKind {
    Synchronous,
    Asynchronous,
    Superset,
}

#[derive(Debug, Clone)]
pub struct Controller {
    pub name: Ident,
    pub meta: Meta,
    pub source: Ident,
    pub formula: Ident,
    pub export: Option<String>,
    pub options: ControllerOptions,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MuFormula {
    pub name: Ident,
    pub meta: Meta,
    pub targets: FormulaTargets,
    pub body: FormulaExpr,
}

#[derive(Debug, Clone)]
pub enum FormulaTargets {
    All(Span),
    Named(Vec<Ident>),
}

/// Formula expression that can be either μ-calculus or LTL.
#[derive(Debug, Clone)]
pub enum FormulaExpr {
    /// Existing μ-calculus syntax
    MuCalculus(MuExpr),
    /// New LTL syntax
    Ltl(LtlExpr),
}

/// μ-calculus expression (raw string representation).
#[derive(Debug, Clone)]
pub struct MuExpr {
    pub raw: String,
    pub span: Span,
}

/// LTL expression with parsed formula and source span.
#[derive(Debug, Clone)]
pub struct LtlExpr {
    pub formula: LtlFormula,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Integer(i64),
    Ident(Ident),
    Index {
        target: Ident,
        expr: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Group(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
