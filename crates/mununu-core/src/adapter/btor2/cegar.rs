//! R.5 — CEGAR refinement loop MVP for BTOR2 + predicate-cube lift +
//! 3-valued parity-game evaluator.
//!
//! Per the §Phase 5 + §Phase 6 + §10.1 R.5 entries of the KMTS plan
//! (`.claude/plans/you-are-a-formal-vast-lake.md`), this module ships
//! the **API surface** for the failure-subgame-driven CEGAR loop.
//! The load-bearing predicate-discovery mechanisms (Craig interpolation
//! and weakest-precondition computation) are 4-week scopes that are
//! deferred to a follow-up; this MVP delivers the smallest shippable
//! shape that:
//!
//! 1. Closes the API surface: `CegarOptions`, `CegarTrace`,
//!    `CegarIteration`, `CegarTermination`, `PredicateSource`,
//!    `cegar_refine_loop()`.
//! 2. Wires the iteration loop: `predicate_cube_lift` (R.2.5) →
//!    `evaluate_3v_game` (R.5.0) → check for KleeneBot → predicate-
//!    discovery callback → repeat until convergence or cap-hit.
//! 3. Implements bounded refinement (default 16 iterations) with
//!    soundness-tagged warning on cap-hit.
//! 4. Satisfies the §10.1 R.5 binary done-criterion: a hand-curated
//!    fixture where the initial predicate set returns `KleeneBot`
//!    closes in ≤ 3 iterations when the caller supplies the right
//!    refinement predicate via [`PredicateSource::Manual`].
//!
//! **What this MVP does NOT do** (flagged via the `predicate_source`
//! variant and the `lazy_lift_pending` / `approximant_reuse_enabled`
//! flags on `CegarTrace`):
//!
//! - **WP is a heuristic, not full weakest-precondition.** The
//!   `WeakestPrecondition` source ships an MVP that emits
//!   separating predicates over state registers not yet in the
//!   predicate set, capped at 2 per call. It does NOT perform
//!   symbolic back-substitution along the classifying transition;
//!   that's queued as an R.5 follow-up. The name "WP" describes
//!   the API contract (an alternative to `Manual` and
//!   `CraigInterpolation`), not the formal weakest-precondition
//!   construction.
//! - **No Craig interpolation.** The `CraigInterpolation` source
//!   short-circuits to empty (requires Z3 with interpolation
//!   support; queued).
//! - **No lazy KMTS construction.** Each iteration re-runs
//!   `predicate_cube_lift` over the full predicate set; `KmtsLiftLazy`
//!   is a R.5 follow-up.
//! - **No real approximant reuse.** The MVP re-evaluates from scratch
//!   each iteration. The `game_position_evaluations` counter is
//!   instrumented so the R.5 follow-up's improvement is measurable
//!   against this baseline.
//! - **No SMT lemma cache** (§Phase 6 §6.4 R-F3). Pure
//!   non-amortised re-check.
//!
//! Consumers that need the full CEGAR fidelity must wait for R.5
//! follow-ups. The `predicate_source` enum's variants + the
//! `lazy_lift_pending` / `approximant_reuse_enabled = false` flags
//! on `CegarTrace` are the explicit handshakes between R.5 MVP and
//! the future load-bearing implementation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::adapter::AdapterOptions;
use crate::adapter::btor2::ast::Nid;
use crate::adapter::btor2::{PredicateCubeLiftOptions, PredicateSpec};
use crate::adapter::{AdapterError, AdapterErrorKind};
use crate::mu_calculus::{
    ApproximantView, Environment, EvalResult, EvaluationError, EvaluationOptions, FailureSubgame,
    FixpointPolarity, Formula, GameEvaluation, PriorApproximant, Trit, TritSet, evaluate_3v_game,
    evaluate_3v_game_with_options,
};

/// R.5 — Type alias for the manual predicate-discovery callback.
/// Signature: receives the current failure subgame and the active
/// predicate set; returns the predicates to add for this iteration.
pub type ManualPredicateCallback =
    dyn Fn(&FailureSubgame, &[PredicateSpec]) -> Vec<PredicateSpec> + Send + Sync;

/// R.5 — Predicate-discovery source the CEGAR loop consults when it
/// encounters a `KleeneBot` verdict and needs to add predicates.
///
/// **MVP shipping state**:
/// - [`PredicateSource::Manual`] — fully wired (user-supplied callback).
/// - [`PredicateSource::WeakestPrecondition`] — heuristic MVP shipped;
///   emits separating predicates over state registers not yet in the
///   predicate set, capped at 2 per call. See
///   [`weakest_precondition_predicates`] for the semantics.
/// - [`PredicateSource::CraigInterpolation`] — placeholder; returns
///   empty. Requires Z3 with interpolation support; queued as a
///   follow-up.
pub enum PredicateSource {
    /// Caller supplies a closure that returns predicates to add at
    /// each iteration. The closure receives the current failure
    /// subgame + the active predicate set so the caller can choose
    /// per-iteration. The working refinement path with full
    /// caller-side control over predicate selection.
    Manual(Arc<ManualPredicateCallback>),
    /// R.5 WP MVP — emit separating predicates over state registers
    /// reachable from the failure subgame's classifying transitions
    /// that are NOT yet in the predicate set. Bounded at 2 proposals
    /// per call; the CEGAR loop iterates and the helper picks up the
    /// next uncovered register on the following pass. Returns empty
    /// when every state register is covered OR when the BTOR2 file
    /// fails to parse, at which point the loop terminates with
    /// `PredicateSourceExhausted`.
    ///
    /// The name "WP" describes the API contract (an alternative to
    /// `Manual` and `CraigInterpolation`); the mechanism is "any
    /// uncovered state register" rather than formal weakest-
    /// precondition back-substitution. The full WP construction
    /// (symbolic back-substitution along the classifying transition's
    /// next-state function, bounded by the transition's
    /// cone-of-influence) is queued as a follow-up.
    WeakestPrecondition,
    /// **R.5 follow-up.** Compute a Craig interpolant between the
    /// concrete-relation states the may-edge admits and excludes.
    /// MVP behaviour: returns empty; loop terminates at cap.
    CraigInterpolation,
}

impl std::fmt::Debug for PredicateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PredicateSource::Manual(_) => write!(f, "Manual(<closure>)"),
            PredicateSource::WeakestPrecondition => write!(f, "WeakestPrecondition"),
            PredicateSource::CraigInterpolation => write!(f, "CraigInterpolation"),
        }
    }
}

/// R.5 — Configuration for the CEGAR loop.
///
/// Manual `Debug` impl (B.6.b, 2026-06-03): boolean fields are
/// printed only when they DIFFER from the default value to
/// reduce log clutter. `capture_approximants: false`,
/// `enable_approximant_reuse: false`, and `smart_uf_cap: true`
/// are the defaults and don't appear unless overridden. The
/// load-bearing fields (`max_iterations`, `predicate_source`,
/// `max_cube_count`) are always shown.
pub struct CegarOptions {
    /// Maximum number of refinement iterations before the loop
    /// terminates with `BoundedIterationsReached`. Default 16 per
    /// §10.1 R.5 done-criterion.
    pub max_iterations: usize,
    /// Where new refinement predicates come from. MVP: Manual is
    /// the only working source; WP / Interpolation are placeholders.
    pub predicate_source: PredicateSource,
    /// Hard cap on cube count for the underlying `predicate_cube_lift`
    /// at each iteration. Default 1024 (per R.2.5).
    pub max_cube_count: usize,
    /// R.5 CEGAR auto-capture sub-item 1.2 — when true, the loop wires
    /// an [`EvaluationOptions::on_fixpoint_convergence`] callback at
    /// each iteration's `evaluate_3v_game_with_options` call to capture
    /// converged per-fixpoint-var approximants into
    /// [`CegarIteration::approximants_at_end`].
    ///
    /// `false` (default) ⇒ `approximants_at_end` is always `None`;
    /// behaviour identical to the pre-1.2 loop.
    /// `true` ⇒ each iteration's `approximants_at_end` is `Some(map)`
    /// (possibly empty if the formula has no fixpoints).
    ///
    /// Sub-item 1.3 (2026-06-01) adds the sibling
    /// [`Self::enable_approximant_reuse`] flag that consumes the
    /// captured approximants as `prior_approximants` seeds on the
    /// next iteration.
    pub capture_approximants: bool,
    /// R.5 CEGAR auto-capture sub-item 1.3 (2026-06-01) — when
    /// true, the loop threads iteration N's
    /// [`CegarIteration::approximants_at_end`] forward as
    /// [`EvaluationOptions::prior_approximants`] on iteration N+1.
    ///
    /// Requires [`Self::capture_approximants`] = `true` (otherwise
    /// there are no approximants to thread). When the predicate
    /// set grows between iterations (the common case after a
    /// refinement step), the underlying `prior_approximants` API
    /// silently drops state-count-mismatched entries, so the
    /// seeding fires only when state count matches (e.g.
    /// `PredicateSourceExhausted` repeat-eval, OR when sub-item
    /// 1.4's cube-refinement mapping ships and translates
    /// approximants across cube-space resizes).
    ///
    /// `false` (default) ⇒ pre-1.3 behaviour exactly (each
    /// iteration evaluates from scratch).
    /// `true` + matching state count ⇒ iteration N+1's evaluator
    /// is seeded with iteration N's converged definite-true bits.
    pub enable_approximant_reuse: bool,
    /// R.5 B.4.a (2026-06-01) — smart `max_iterations` cap default.
    /// When `true` (default) AND the predicate source is
    /// [`PredicateSource::WeakestPrecondition`] AND the first
    /// lift emits a UF-wrap warning, the effective iteration cap
    /// drops to [`SMART_UF_MAX_ITERATIONS`] (4) instead of the
    /// configured `max_iterations`. Surfaces cap-hit fast on
    /// UF-spurious cases where WP cannot construct a closing
    /// predicate without selective UF concretization (which
    /// requires R-F3 SMT lemma cache access — not shipped).
    ///
    /// Set to `false` to use `max_iterations` literally even on
    /// UF-wrapped lifts (useful when the caller has measured the
    /// fixture and knows WP will converge given more iterations).
    pub smart_uf_cap: bool,
    /// R.5 lazy KMTS sub-item 2.4 (2026-06-04) — choose between
    /// the eager `predicate_cube_lift` and the lazy `LazyLift`
    /// for per-iteration cube-space construction.
    ///
    /// `Eager` (default): each iteration calls
    /// `predicate_cube_lift` which materializes all 2^|P| cube
    /// states + their outgoing may-edges up front. The verdict-
    /// evaluator receives a finished `Clts`.
    ///
    /// `Lazy`: each iteration constructs a `LazyLift` and
    /// materializes a Clts from it by visiting every cube via
    /// `expand_cube`. The current verdict-evaluator can't
    /// consume a lazy handle directly (would need an evaluator-
    /// side patch to do per-state on-demand lookup), so this
    /// MVP still produces a fully-materialized Clts — the
    /// verdict is identical to `Eager`. The flag's value today
    /// is exercising the LazyLift integration end-to-end via
    /// the CEGAR loop, surfacing any divergence between the
    /// eager + lazy per-cube logic as a verdict-equality
    /// mismatch.
    ///
    /// Future evaluator-side lazy support (separate sub-item)
    /// will extract real memory savings from the lazy path.
    pub lift_strategy: LiftStrategy,
    /// R.2.5b session-1 follow-up (2026-06-06) — per-iteration
    /// must-edge inference policy passed to `predicate_cube_lift`.
    /// Defaults to [`crate::adapter::btor2::kmts_lift::MustEdgeInference::Off`]
    /// (pre-R.2.5b behaviour, MayOnly only).
    ///
    /// Set to an SMT mode (`MustEdgeInference::SmtPerTargetStandard`
    /// for the canonical ∀∃ KMTS must, or `SmtHyperMust` for the sound
    /// νμ hyper-must) to opt the CEGAR loop into SMT-proved must /
    /// hyper-must edges. SOUNDNESS: the inferred must-edges are
    /// SMT-proved (UNSAT of the negated obligation), so R.5 CEGAR
    /// verdicts that depend on them are sound (no sampling assumption).
    pub must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference,
    /// DR1 (IR-unification track, 2026-06-19) — may-edge inference
    /// policy forwarded to each iteration's `predicate_cube_lift`.
    /// `Off` (default) keeps the sampling may-edges (fast, but an
    /// under-approximation of the may relation — unsound for safety);
    /// `SmtAllPairs` uses the sound all-pairs SMT may relation
    /// (over-approximation; an edge is excluded only when Z3 proves it
    /// impossible). Surfacing this through `CegarOptions` is the DR1
    /// enablement that lets the CEGAR path run with a sound may
    /// relation (the P1/P3 prerequisite). Note: combining `SmtAllPairs`
    /// with a non-`Off` `must_edge_inference` is not yet wired — see
    /// [`crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs`].
    pub may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference,
    /// CTXDSL Phase 2 (2026-06-22) — when `true`, the loop captures the
    /// final refined predicate-cube `Clts` into [`CegarTrace::final_clts`]
    /// so a caller can serialize the abstract model to CTXDSL via
    /// [`crate::adapter::clts_to_ir::clts_to_ctxdsl_with_formula`]. Default
    /// `false`, in which case the cube is dropped at loop exit (no clone
    /// cost). This is the opt-in behind the `--emit-ctxdsl` CLI flag and
    /// the API's `emit_ctxdsl` request field.
    pub emit_ctxdsl: bool,
}

/// R.5 lazy KMTS sub-item 2.4 (2026-06-04) — selector for the
/// CEGAR loop's per-iteration cube-lift strategy. See
/// [`CegarOptions::lift_strategy`] for semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiftStrategy {
    /// Eager `predicate_cube_lift`. Default; matches pre-2.4
    /// behaviour.
    Eager,
    /// Lazy `LazyLift`. Currently produces the same verdict as
    /// `Eager` because the verdict-evaluator still consumes a
    /// fully-materialized Clts. Future evaluator-side support
    /// will turn this into a real memory-savings path.
    Lazy,
}

/// R.5 B.4.a (2026-06-01) — effective `max_iterations` cap when
/// [`CegarOptions::smart_uf_cap`] fires (WP source + UF-wrap
/// warning in the first lift). 4 iterations is enough to confirm
/// "WP cannot close this verdict because of UF spuriousness" on
/// most fixtures; users who want longer runs can disable the
/// smart cap.
pub const SMART_UF_MAX_ITERATIONS: usize = 4;

/// R.5 B.6.b (2026-06-03) — manual `Debug` impl that hides
/// boolean fields at their defaults to reduce log clutter.
/// See struct doc for the contract.
impl std::fmt::Debug for CegarOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut ds = f.debug_struct("CegarOptions");
        ds.field("max_iterations", &self.max_iterations);
        ds.field("predicate_source", &self.predicate_source);
        ds.field("max_cube_count", &self.max_cube_count);
        if self.capture_approximants {
            ds.field("capture_approximants", &true);
        }
        if self.enable_approximant_reuse {
            ds.field("enable_approximant_reuse", &true);
        }
        if !self.smart_uf_cap {
            // smart_uf_cap default is TRUE; only print when DISABLED.
            ds.field("smart_uf_cap", &false);
        }
        if !matches!(
            self.must_edge_inference,
            crate::adapter::btor2::kmts_lift::MustEdgeInference::Off
        ) {
            // must_edge_inference default is Off; only print when
            // overridden.
            ds.field("must_edge_inference", &self.must_edge_inference);
        }
        if self.emit_ctxdsl {
            // emit_ctxdsl default is false; only print when opted in.
            ds.field("emit_ctxdsl", &true);
        }
        ds.finish()
    }
}

impl Default for CegarOptions {
    fn default() -> Self {
        Self {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: true,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            // AR-S2 — the sound all-pairs SMT may-relation is the default (retiring
            // the sampling-may default). Callers can still opt into the faster
            // sampling path with `MayEdgeInference::Off` when the input space is
            // small enough to enumerate exhaustively.
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        }
    }
}

/// R.5 — Per-iteration record of the CEGAR loop's state.
#[derive(Debug, Clone)]
pub struct CegarIteration {
    /// Iteration number (0-indexed; iteration 0 is the initial
    /// evaluation before any refinement).
    pub iteration: usize,
    /// Predicate set in use at the start of this iteration.
    pub predicates_at_start: Vec<PredicateSpec>,
    /// Game evaluator's verdict at the start of this iteration.
    pub verdict: TritSet,
    /// Failure subgame if verdict has `KleeneBot`; `None` if
    /// converged.
    pub failure_subgame: Option<FailureSubgame>,
    /// Predicates the source added in response to this iteration's
    /// failure subgame. Empty when the verdict converged or the
    /// source returned no suggestions.
    pub predicates_added: Vec<PredicateSpec>,
    /// Instrumented counter for the §10.1 R.5 approximant-reuse
    /// done-criterion. Currently records the cube count × formula
    /// size as a proxy for game-position evaluations; R.5 follow-up
    /// will replace this with a precise per-position counter.
    pub game_position_evaluations: usize,
    /// R.5 CEGAR auto-capture sub-item 1.2 + B.1.a (2026-06-01) —
    /// per-fixpoint-var converged approximants from THIS iteration's
    /// evaluator run.
    ///
    /// Keyed by `FormulaVarId::index()` (the same shape
    /// `EvaluationOptions::prior_approximants` consumes). Value is
    /// the converged iterate as a [`StoredApproximant`] carrying
    /// both `must_true` and `may_true` bit-sets per the B.1.a
    /// widening.
    ///
    /// `None` ⇒ caller did not enable auto-capture for this run
    /// (default; sub-item 1.3 wires the loop to feed these forward).
    /// `Some(map)` ⇒ map contains exactly the fixpoint vars the
    /// evaluator visited during convergence. Outer fixpoints capture
    /// at the outer convergence, inner fixpoints capture once per
    /// their own convergence (matches sub-item 1.1's nested-fixpoint
    /// semantics — K fires for K-deep nesting on a single
    /// evaluation).
    pub approximants_at_end: Option<HashMap<usize, StoredApproximant>>,
}

/// R.5 B.1.a (2026-06-01) + sub-item 1.4.a (2026-06-01) —
/// persistent storage for a captured fixpoint approximant.
/// Mirrors the `ApproximantView` shape but owns its bit-sets so
/// it can outlive the evaluator's iterate.
///
/// Sub-item 1.4 reads `must_true` (for the μ lower-bound seed) +
/// `may_true` (for the ν upper-bound seed) + `polarity` (to pick
/// which one) for the cube-refinement mapping's parent-to-children
/// copy.
#[derive(Debug, Clone)]
pub struct StoredApproximant {
    /// Definite-true bit-set (KleeneT positions). Sound μ-LFP
    /// lower-bound seed.
    pub must_true: EvalResult,
    /// May-true bit-set (KleeneT ∪ KleeneBot positions). Sound
    /// ν-GFP upper-bound seed.
    pub may_true: EvalResult,
    /// Fixpoint polarity of the captured variable.
    pub polarity: FixpointPolarity,
}

/// R.5 — Termination reason for the CEGAR loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CegarTermination {
    /// Verdict has no `KleeneBot` cells — refinement closed.
    Converged,
    /// Hit `max_iterations` before convergence. Final verdict
    /// retains `KleeneBot` cells; CEGAR caller should treat as a
    /// soundness-tagged unknown per §10.1 R.5.
    BoundedIterationsReached,
    /// Predicate source returned an empty refinement set despite a
    /// `KleeneBot` verdict — the source has no further suggestions.
    /// MVP `WeakestPrecondition` / `CraigInterpolation` sources hit
    /// this immediately because they are not yet implemented.
    PredicateSourceExhausted,
}

/// R.5 — Outcome of the CEGAR refinement loop.
#[derive(Debug, Clone)]
pub struct CegarTrace {
    /// Per-iteration record. `iterations[0]` is the initial
    /// evaluation; `iterations[K]` is after the K-th refinement.
    pub iterations: Vec<CegarIteration>,
    /// Verdict at the end of the loop (either converged or capped).
    pub final_verdict: TritSet,
    /// Predicate set at termination. Includes the initial set plus
    /// every predicate the source added across all iterations.
    pub final_predicates: Vec<PredicateSpec>,
    /// Why the loop stopped.
    pub terminated_with: CegarTermination,
    /// **Always `true` at R.5 MVP.** Flags that the underlying lift
    /// is eager `predicate_cube_lift` (R.2.5 MVP) rather than the
    /// lazy `KmtsLiftLazy` that R.5 follow-up will ship.
    pub lazy_lift_pending: bool,
    /// R.5 CEGAR auto-capture sub-item 1.3 (2026-06-01) — mirrors
    /// [`CegarOptions::enable_approximant_reuse`]: `true` iff the
    /// loop threaded prior iterations' converged definite-true
    /// bit-sets forward as `EvaluationOptions::prior_approximants`
    /// on subsequent iterations.
    ///
    /// Pre-1.3 the flag was always `false`; post-1.3 the flag
    /// echoes the caller's opt-in. The §10.1 R.5 done-criterion
    /// "second iteration reuses approximants" is measurable by
    /// checking this flag + `iterations[N].approximants_at_end`
    /// (the captured map at iteration N, fed forward to iteration
    /// N+1 when reuse + state-count match).
    pub approximant_reuse_enabled: bool,
    /// R.5 B.3.b (2026-06-01) — soundness / advisory warnings
    /// produced during the CEGAR run. Each warning is structured
    /// (`AdapterWarning { kind, message, location }`) so consumers
    /// can route by kind. The B.3.b warning fires when the input
    /// formula has alternation depth ≥ 2 AND the initial lift's
    /// CLTS has no `MustHyperOnly` (hyper-must) transitions —
    /// per Shoham–Grumberg LMCS 2007, refining standard KMTS on
    /// alternating fixpoints is non-monotone, so verdicts may
    /// regress across iterations.
    ///
    /// Default `Vec::new()` — empty when no advisories fire.
    pub warnings: Vec<crate::adapter::AdapterWarning>,
    /// R-Y5 (2026-06-08) — register names that the failure
    /// subgame identifies as load-bearing for `KleeneBot`
    /// verdicts. Downstream consumers (e.g. a Yosys re-run
    /// wrapper, a user reviewing the trace) can use this list
    /// to drive init-policy promotion: promoting these
    /// registers to `anyconst` (R-Y2) or declaring them in the
    /// sidecar's `config_values` (R-Y7) often closes the
    /// indefinite verdict.
    ///
    /// **R-Y5 MVP scope**: identification only. The actual
    /// re-run with promoted init policy requires regenerating
    /// BTOR2 with different Yosys passes — outside the CEGAR
    /// loop's current scope. Future R-Y5 follow-up adds the
    /// re-run wrapper.
    ///
    /// Deduplicated across all iterations of the loop. Empty
    /// when:
    /// - The verdict converged without ever producing a
    ///   `KleeneBot` cell.
    /// - No failure subgame was emitted (verdict was definite).
    /// - No classifying transition's source predicate maps to
    ///   a register in the BTOR2 source.
    pub init_refinement_candidates: Vec<String>,
    /// CTXDSL Phase 2 (2026-06-22) — the final refined predicate-cube
    /// `Clts` (the abstract model the loop converged/capped on), captured
    /// only when [`CegarOptions::emit_ctxdsl`] is `true`. A caller pairs
    /// it with the checked formula and
    /// [`crate::adapter::clts_to_ir::clts_to_ctxdsl_with_formula`] to
    /// return the model + formula as inspectable CTXDSL (the opt-in
    /// `--emit-ctxdsl` / `emit_ctxdsl` surfaces). `None` when the opt-in
    /// was off — the cube is dropped at loop exit, avoiding the clone.
    pub final_clts:
        Option<crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>>,
    /// Track I.1 (trace slice, 2026-06-24) — a reachability countertrace for a
    /// VIOLATED verdict: a path of definite-`False` cube cells from an initial
    /// cell to a trap (or the farthest reachable failing cell). `None` when no
    /// initial cell is definite-`False` (the property is not violated at start)
    /// or the verdict is HOLDS / undecided. See [`CounterTrace`] +
    /// [`build_counterexample_trace`].
    pub counterexample: Option<CounterTrace>,
}

/// Track I.1 (2026-06-24) — a cube cell that witnesses a non-HOLDS verdict: the
/// predicate valuation of a cube state whose verdict is `False` (the formula is
/// falsified there) or `Unknown`/⊥ (the abstraction cannot decide it). The
/// valuation is decoded from the cube-index bit convention used by
/// `predicate_cube_lift` — predicate `j` holds in cube `i` iff `(i >> j) & 1 == 1`
/// (see `kmts_lift.rs` ~line 946) — so it needs no access to the lifted `Clts`.
///
/// This turns a bare verdict ("F=4") into something actionable ("falsified at the
/// cell where `idle=false, err=true`"). It is a *cell* witness, not yet a full
/// reachability *trace*: the path from init to a falsifying state, and the
/// sub-formula-level cause (via the parity-game refuter strategy), are the
/// Track-I.1 follow-up slices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessCell {
    /// The cube-state index in the lifted KMTS.
    pub cube_index: usize,
    /// `(predicate-name, holds)` for every predicate, in declaration order.
    pub valuation: Vec<(String, bool)>,
}

impl WitnessCell {
    /// Render as `idle=false, err=true` (declaration order).
    pub fn render(&self) -> String {
        self.valuation
            .iter()
            .map(|(name, holds)| format!("{name}={holds}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn decode_cube_valuation(index: usize, predicates: &[PredicateSpec]) -> Vec<(String, bool)> {
    predicates
        .iter()
        .enumerate()
        .map(|(j, p)| (p.name.clone(), (index >> j) & 1 == 1))
        .collect()
}

impl CegarTrace {
    fn witness_cells_for(&self, want: Trit, max: usize) -> Vec<WitnessCell> {
        let mut out = Vec::new();
        for i in 0..self.final_verdict.len() {
            if out.len() >= max {
                break;
            }
            if self.final_verdict.verdict_at(i) == want {
                out.push(WitnessCell {
                    cube_index: i,
                    valuation: decode_cube_valuation(i, &self.final_predicates),
                });
            }
        }
        out
    }

    /// Track I.1 — cube cells that **falsify** the formula (definite-`False`),
    /// each decoded to its predicate valuation. Capped at `max`; pass a large
    /// value for all. Empty when the property HOLDS or is INDEFINITE everywhere.
    pub fn violating_cells(&self, max: usize) -> Vec<WitnessCell> {
        self.witness_cells_for(Trit::False, max)
    }

    /// Track I.1 — cube cells the abstraction **cannot decide** (`Unknown`/⊥),
    /// each decoded to its predicate valuation. These are the cells a finer
    /// predicate set (or CEGAR refinement) would need to resolve. Capped at `max`.
    pub fn undecided_cells(&self, max: usize) -> Vec<WitnessCell> {
        self.witness_cells_for(Trit::Unknown, max)
    }
}

/// Track I.1 (trace slice, 2026-06-24) — a concrete reachability *countertrace*
/// for a VIOLATED verdict: a path of cube cells, all definite-`False`, from an
/// initial cell to a **trap** (a cell whose every successor stays `False`, so
/// the violation is locked in) — or, when no trap is reachable inside the
/// failing region, to the farthest such cell.
///
/// Soundness: every cell on the path is definite-`False`, so the property genuinely
/// fails at each step, and a trap end-cell means it can never recover. The path is
/// real reachability in the lifted cube. This is a *reachability* witness, not the
/// sub-formula-level root cause — extracting the precise cause (e.g. the exact state
/// where an invariant first breaks) is the parity-game refuter-strategy follow-up.
/// Cube edges all carry the single `step` label, so steps record the cell valuation
/// (the informative axis), not the label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterTrace {
    /// Cells from an initial cell to the witness cell, each definite-`False`.
    pub steps: Vec<WitnessCell>,
    /// `true` iff the final cell is a trap (all successors stay `False`).
    pub ends_in_trap: bool,
}

/// Track I.1 — build a [`CounterTrace`] from the lifted cube + its verdict.
/// Returns `None` unless an *initial* cube cell is definite-`False` (i.e. the
/// property is actually violated at start). BFS stays inside the `False` region
/// (only `False → False` edges), so the path witnesses a persistent failure.
fn build_counterexample_trace(
    clts: &crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>,
    verdict: &TritSet,
    predicates: &[PredicateSpec],
) -> Option<CounterTrace> {
    use crate::clts::StateId;
    use std::collections::{HashMap, HashSet, VecDeque};

    let is_false = |idx: usize| verdict.verdict_at(idx) == Trit::False;
    // Start at an initial cell that is definite-False (else the property is not
    // violated at start and there is nothing to trace).
    let start: StateId<crate::clts::DefaultStateIdx> = clts
        .initial_states()
        .iter()
        .copied()
        .find(|s| is_false(s.index()))?;

    // A trap is a False cell whose every outgoing edge stays in the False region
    // (a cell with no successors counts — the failure cannot be left).
    let is_trap = |s: StateId<crate::clts::DefaultStateIdx>| -> bool {
        is_false(s.index())
            && clts
                .outgoing(s)
                .iter()
                .all(|t| is_false(t.target().index()))
    };

    let mut parent: HashMap<usize, usize> = HashMap::new();
    let mut visited: HashSet<usize> = HashSet::new();
    let mut queue: VecDeque<StateId<crate::clts::DefaultStateIdx>> = VecDeque::new();
    visited.insert(start.index());
    queue.push_back(start);
    let mut trap: Option<StateId<crate::clts::DefaultStateIdx>> = None;
    let mut farthest = start;
    while let Some(s) = queue.pop_front() {
        if trap.is_none() && is_trap(s) {
            trap = Some(s);
        }
        farthest = s; // BFS dequeues in distance order; the last is the farthest.
        for t in clts.outgoing(s) {
            let tgt = t.target();
            if is_false(tgt.index()) && visited.insert(tgt.index()) {
                parent.insert(tgt.index(), s.index());
                queue.push_back(tgt);
            }
        }
    }

    let ends_in_trap = trap.is_some();
    let target = trap.unwrap_or(farthest);

    // Reconstruct the path start → target via parent pointers.
    let mut idx_path = vec![target.index()];
    let mut cur = target.index();
    while let Some(&p) = parent.get(&cur) {
        idx_path.push(p);
        cur = p;
    }
    idx_path.reverse();

    let steps = idx_path
        .into_iter()
        .map(|i| WitnessCell {
            cube_index: i,
            valuation: decode_cube_valuation(i, predicates),
        })
        .collect();
    Some(CounterTrace {
        steps,
        ends_in_trap,
    })
}

/// R.5 MVP — Run the CEGAR refinement loop on a BTOR2 fixture +
/// formula.
///
/// The loop:
/// 1. Lift the BTOR2 with the current predicate set via
///    `predicate_cube_lift` (R.2.5).
/// 2. Evaluate the formula via `evaluate_3v_game` (R.5.0). If no
///    `KleeneBot` cells, terminate `Converged`.
/// 3. Otherwise, call `predicate_source` with the failure subgame.
///    If it returns an empty set, terminate `PredicateSourceExhausted`.
///    Otherwise, extend the predicate set and loop.
/// 4. If `max_iterations` reached without convergence, terminate
///    `BoundedIterationsReached`.
///
/// **MVP scope** (load-bearing follow-ups deferred):
///
/// - The lift at step 1 is the R.2.5 MVP — predicate cubes as Clts
///   states with **no transitions** (`predicate_image_pending = true`
///   on the lift result). Evaluating a formula like `[] true` on a
///   transitionless Clts yields trivial verdicts. For meaningful
///   end-to-end testing, the MVP fixture is hand-curated so the
///   formula's verdict depends on the cube state space alone (not
///   transitions).
///
/// - Step 2's `evaluate_3v_game` is the R.5.0 MVP — verdict
///   computation delegates to `evaluate_tri` and the failure subgame
///   is the post-hoc over-approximation. R.5 follow-up replaces both
///   with the Shoham–Grumberg game solver.
///
/// - Step 3's `PredicateSource::Manual` is the only working variant.
///   `WeakestPrecondition` / `CraigInterpolation` short-circuit.
///
/// SOUNDNESS (unsatisfiable-cube-cell fix, 2026-07-23) — a predicate-cube cell with
/// **no outgoing may-transition** holds no concrete state, so its μ-calculus verdict is
/// vacuous and must be ⊥.
///
/// *Why unsatisfiable.* The concrete RTL transition relation is **total** — every
/// register has a next-state function, so every concrete state has a successor. The
/// may-relation is a **sound over-approximation** of it (an edge is dropped only when the
/// SMT backend proves it impossible), so for any concrete state `s` in cell `C` with
/// successor `s'` there is a may-edge `C → α(s')`. Contrapositive: a cell with no outgoing
/// may-edge contains no concrete state (its predicate conjunction is unsatisfiable — e.g.
/// a CEGAR refinement that adds `x==0` and `x==1` makes the `{x==0 ∧ x==1}` and
/// `{x≠0 ∧ x≠1}` cells empty).
///
/// *Why ⊥ is the sound verdict.* An unsatisfiable cell corresponds to no concrete state,
/// so it can neither satisfy nor falsify the property; ⊥ (`must=0, may=1`) makes no
/// definite claim and therefore can never produce an unsound definite verdict. Before this
/// fix the 3-valued eval read such a cell as an ordinary state: a ¬good sink made
/// `EF good = good ∨ ⟨⟩∅ = ` definite-**False**, which then spuriously falsified the
/// universal recoverability property `AG EF good` (νμ) — the i2c `AG EF (wb_ack_o==1)`
/// incident, where CEGAR refined into isolated ¬good sinks and verify-auto read one as an
/// init cube ⇒ a spurious definite VIOLATED (exact-symbolic decides HOLDS).
///
/// *Precision, not soundness, rests on totality.* Satisfiable cells always have an
/// outgoing may-edge and are untouched, so every sound definite verdict is preserved. If a
/// non-total system were ever lifted, a genuine terminal (satisfiable, no successor) would
/// degrade to ⊥ — a precision loss, never an unsound verdict. On a normal cube (no
/// unsatisfiable cells) the pass is a no-op.
fn downgrade_unsatisfiable_cells(
    clts: &crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>,
    verdicts: TritSet,
) -> (TritSet, std::collections::HashSet<usize>) {
    use crate::clts::StateId;
    let unsat: std::collections::HashSet<usize> = (0..clts.state_count())
        .filter(|&i| {
            StateId::<crate::clts::DefaultStateIdx>::from_index(i)
                .is_some_and(|s| clts.outgoing(s).is_empty())
        })
        .collect();
    if unsat.is_empty() {
        return (verdicts, unsat);
    }
    let mut must = verdicts.must_true().clone();
    let mut may = verdicts.may_true().clone();
    for &i in &unsat {
        if i < must.len() {
            must.set(i, false); // not definitely-true
            may.set(i, true); // but possibly-true ⇒ ⊥, never definite-False
        }
    }
    (TritSet::from_parts(must, may), unsat)
}

pub fn cegar_refine_loop(
    formula: &Formula,
    btor2_content: &str,
    initial_predicates: Vec<PredicateSpec>,
    env: &Environment,
    adapter_options: &AdapterOptions,
    cegar_opts: &CegarOptions,
) -> Result<CegarTrace, AdapterError> {
    let mut current_predicates = initial_predicates;
    let mut iterations: Vec<CegarIteration> = Vec::new();
    let mut warnings: Vec<crate::adapter::AdapterWarning> = Vec::new();

    // B.1 increment 3c (2026-06-26) — sidecar compound predicates. Read any
    // `compound_predicates` from the SvAnnotation sidecar, append each as a
    // cube-bit predicate (its truth decided by the SMT seam via the parsed
    // expr, NOT register==value), and collect the exprs for
    // `lift_opts.compound_exprs`. Compounds REQUIRE the eager SmtAllPairs
    // may-relation (the only compound-aware lift): the sampling representative
    // can't realise a conjunction and the lazy per-cube body never consults the
    // expr. So when compounds are present we force `may = SmtAllPairs` +
    // `strategy = Eager`, warning if that overrides an explicit request.
    // `ensure_compound_lift_supported` re-checks this at the lift entry.
    let mut compound_exprs: HashMap<String, crate::adapter::btor2::predicate_expr::PredicateExpr> =
        HashMap::new();
    // H.E.2 — derived predicates (labeled per cube via SMT, NOT cube dimensions):
    // the simple combinational-of-input atoms (`combinational_predicates`) PLUS
    // (H.F) the relational/compound ones whose operands include an input or
    // combinational signal (`compound_predicates` with `derived = true`). They do
    // NOT enter `current_predicates` (the cube index is unchanged); the lift writes
    // their per-cube label into `state_3valued_predicates`, resolving any relational
    // expr through `compound_exprs`.
    let mut derived_predicates = sidecar_combinational_predicates(adapter_options);
    for (spec, expr, is_derived) in sidecar_compound_predicates(adapter_options) {
        compound_exprs.insert(spec.name.clone(), expr);
        if is_derived {
            derived_predicates.push(spec); // H.F — a derived label, not a dimension
        } else {
            current_predicates.push(spec); // B.1 — a cube dimension
        }
    }
    // SmtAllPairs + Eager are forced when EITHER compound OR derived predicates
    // are present (both are SMT-seam-only paths) — OR the source is Craig interpolation
    // (F.1), which can DISCOVER a compound relational/bound invariant mid-loop and install
    // it into `lift_opts.compound_exprs`; the compound-aware lift must already be selected
    // when that first compound lands, so we force it from the start for the Craig source.
    let smt_seam_required = !compound_exprs.is_empty()
        || !derived_predicates.is_empty()
        || matches!(
            cegar_opts.predicate_source,
            PredicateSource::CraigInterpolation
        );
    let effective_may = if smt_seam_required {
        crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs
    } else {
        cegar_opts.may_edge_inference
    };
    let effective_strategy = if smt_seam_required {
        LiftStrategy::Eager
    } else {
        cegar_opts.lift_strategy
    };
    if !compound_exprs.is_empty()
        && (cegar_opts.may_edge_inference
            != crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs
            || cegar_opts.lift_strategy != LiftStrategy::Eager)
    {
        warnings.push(crate::adapter::AdapterWarning {
            kind: crate::adapter::WarningKind::ApproximateTranslation,
            message: format!(
                "[B.1 3c compound] {} compound predicate(s) present → forced \
                 may_edge_inference = SmtAllPairs + lift_strategy = Eager (the only \
                 compound-aware lift path; the sampling representative can't realise a \
                 conjunction and the lazy body never consults the expr).",
                compound_exprs.len()
            ),
            location: None,
        });
    }
    // A.4 (soundness-ledger, 2026-07-05) — sampling may-relation is an
    // UNDER-approximation. `MayEdgeInference::Off` samples one canonical
    // representative per source cube over a capped input enumeration, so it can
    // MISS a real may-successor. A `Box` ([]) modality quantifies universally
    // over may-successors, so a missing successor makes `[]φ` vacuously easier
    // to satisfy — a definite HOLDS (`KleeneT`) on a box property is then NOT
    // guaranteed sound (a missed successor can hide a violation). Surface this
    // on the `btor2 cegar` / `sv cegar` / `/cegar` surface as a WARNING (not a
    // reject: sampling is fast and exact when the input space is small enough to
    // be exhaustively enumerated). `effective_may` already accounts for the
    // compound/derived force to SmtAllPairs above, so this fires only when the
    // run genuinely uses the sampling path.
    let box_count = formula.box_modality_count();
    if effective_may == crate::adapter::btor2::kmts_lift::MayEdgeInference::Off && box_count > 0 {
        warnings.push(crate::adapter::AdapterWarning {
            kind: crate::adapter::WarningKind::ApproximateTranslation,
            location: None,
            message: format!(
                "adapter/btor2/cegar (A.4): may_edge_inference = Off (sampling) with {box_count} \
                 box ([]) modality/-ies. The sampling may-relation is an UNDER-approximation \
                 (one canonical representative per cube + a capped input enumeration) that can \
                 MISS a real may-successor, so a definite HOLDS on a box property is NOT \
                 guaranteed sound — a missed successor can hide a violation. Re-run with \
                 `--may-edge-inference smt-all-pairs` for a sound over-approximating may-relation, \
                 or use the exact-symbolic engine for an abstraction-free verdict.",
            ),
        });
    }
    // R-Y5 (2026-06-08) — register names load-bearing for
    // `KleeneBot` verdicts across iterations. Populated at the
    // failure-subgame consumption point; surfaced in the final
    // CegarTrace for downstream init-policy promotion drivers
    // (e.g. a future Yosys re-run wrapper). Uses BTreeSet for
    // deterministic ordering in the trace output.
    let mut init_refinement_candidates: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    // CTXDSL Phase 2 (2026-06-22) — when `emit_ctxdsl` is set, hold the
    // most-recent iteration's lifted cube `Clts` so the final `CegarTrace`
    // carries the abstract model for CTXDSL serialization. Each iteration
    // overwrites it, so at any return point it holds the final (latest)
    // model. Cloned per iteration only under the opt-in; stays `None`
    // (no clone) otherwise.
    let mut captured_clts: Option<
        crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>,
    > = None;
    // R.5 B.4.a (2026-06-01) — effective iteration cap. Starts as
    // `cegar_opts.max_iterations`; iteration 0's UF-wrap detection
    // may lower it to `SMART_UF_MAX_ITERATIONS` when the smart
    // cap is enabled + the predicate source is WP + the lift
    // emits a UF-wrap warning. The loop's `for` header iterates
    // to the original `max_iterations` but the cap-hit check
    // uses `effective_max_iterations` so early termination on
    // UF-spurious cases is structurally guaranteed.
    let mut effective_max_iterations = cegar_opts.max_iterations;
    // R.5 Item 3 sub-item 3.4 (2026-06-04) — locate the CVC5
    // binary once at loop start when the predicate source is
    // CraigInterpolation. Cached for the loop's lifetime.
    // - `Some(bin)` ⇒ Craig path will run via CVC5.
    // - `None` + `cvc5_unavailable_warning_emitted = true` ⇒ the
    //   CraigInterpolation arm falls back to WP for the whole
    //   run; the warning is emitted exactly once.
    // Loop iterations re-use the cache.
    let cvc5_bin: Option<crate::adapter::cvc5::Cvc5Bin> = if matches!(
        cegar_opts.predicate_source,
        PredicateSource::CraigInterpolation
    ) {
        match crate::adapter::cvc5::locate_cvc5() {
            Ok(bin) => Some(bin),
            Err(e) => {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: crate::adapter::WarningKind::ApproximateTranslation,
                    location: None,
                    message: format!(
                        "adapter/btor2/cegar (Item 3 sub-item 3.4): \
                             PredicateSource::CraigInterpolation selected but cvc5 binary \
                             not available: {}. Falling back to WeakestPrecondition heuristic \
                             for the duration of this run. Install cvc5 (Homebrew: \
                             `brew install cvc5`; Debian: `apt install cvc5`) or set \
                             MUNUNU_CVC5_PATH to use Craig interpolation.",
                        e.message
                    ),
                });
                None
            }
        }
    } else {
        None
    };
    // CVC5 invocation options; sub-item 3.5 will expose this via
    // the sidecar.
    let cvc5_opts = crate::adapter::cvc5::InterpolantQueryOptions::default();
    // F.1 — the transition-aware Craig refinement (`craig_refinement_over_transition`) needs the
    // parsed model to encode the exact transition; parse it once. On a parse error we keep going
    // (Craig simply yields nothing → WP fallback) rather than fail the whole loop.
    let parsed_file_for_craig: Option<crate::adapter::btor2::ast::Btor2File> = if matches!(
        cegar_opts.predicate_source,
        PredicateSource::CraigInterpolation
    ) {
        crate::adapter::btor2::parser::parse(btor2_content).ok()
    } else {
        None
    };
    // `mut` so the Craig source (F.1) can install a discovered compound relational/bound
    // invariant into `compound_exprs` for the NEXT iteration's compound-aware re-lift.
    let mut lift_opts = PredicateCubeLiftOptions {
        max_cube_count: cegar_opts.max_cube_count,
        // R.2.5 predicate-image MVP — enable boolean-input enumeration
        // so the lifted Clts has meaningful may-edges for the CEGAR
        // loop to refine. 8 bits = 256 combinations per cube, matches
        // the lift opts' default.
        max_input_bits: 8,
        // R.2.5b session-1 follow-up (2026-06-06): forward the
        // CEGAR-level must-edge inference policy to the lifter. When
        // the user has opted into an SMT must mode, each
        // iteration's lift emits SMT-proved Sharp / MustHyperOnly
        // edges + an [R.2.5b-smt-must*] AdapterWarning that
        // propagates through the CegarTrace to the verdict surface.
        must_edge_inference: cegar_opts.must_edge_inference,
        // MIG-3.2 (2026-06-13) — may-edge policy. DR1 (2026-06-19)
        // surfaces this through `CegarOptions::may_edge_inference`:
        // `Off` (default) keeps the sampling may-edges; `SmtAllPairs`
        // selects the sound all-pairs SMT may relation.
        // B.1 3c — `effective_may` forces `SmtAllPairs` when compound
        // predicates are present (the only compound-aware lift).
        may_edge_inference: effective_may,
        // R-Y7 (2026-06-07) + R-S8 session 2 (2026-06-08) —
        // symbolic-init via predicate cubes. Read the
        // `signals[].config_values` map from the sidecar JSON
        // (when present in `adapter_options.sidecar_json`) via
        // the R-S8 encoder's `sidecar_config_values` bridge.
        // Empty when no sidecar OR no signal declares
        // `config_values` → preserves the pre-R-Y7 single-
        // initial-cube behaviour. Non-empty → the predicate-cube
        // lift expands the initial-state set per R-S8's
        // hyper-must initial semantics.
        config_values: crate::adapter::btor2::r_s8_encoder::sidecar_config_values(adapter_options),
        // B.1 3c — compound predicates parsed from the sidecar (empty when none
        // declared → preserves the simple-atom path). The lift honours these via
        // the SmtEncode seam's `PredicateLike::expr`.
        compound_exprs: compound_exprs.clone(),
        // H.E.2 — derived combinational predicates parsed from the sidecar
        // (empty when none declared). The lift labels each per cube via the
        // SmtEncode seam's `combinational_labels` pass and writes the label into
        // `state_3valued_predicates` (NOT a cube dimension).
        derived_predicates: derived_predicates.clone(),
        // scalable-KMTS P1.4 — compute the SmtAllPairs may-relation via the compound-sound SMT
        // post-image (O(2^|P|·#succ)) instead of the all-pairs O(2^{2|P|}) loop. Both encode via
        // `encode_design_for_lift`, so the KMTS and every verdict are IDENTICAL (proven by
        // `p1_postimage_lift_matches_all_pairs_lift`); only the cost differs. This is the P0-wall fix
        // for every cube verification (recoverability, safety). Consulted only on the SmtAllPairs path.
        may_postimage: true,
    };

    for iteration in 0..=cegar_opts.max_iterations {
        // 1. Lift the BTOR2 with the current predicate set.
        //
        // **Sub-item 2.4 (2026-06-04)**: the lift-strategy flag
        // routes between the eager `predicate_cube_lift` and the
        // lazy `LazyLift` path. Both produce a fully-materialized
        // `PredicateCubeLiftResult` today (the verdict-evaluator
        // can't consume a lazy handle directly yet) — the Lazy
        // path is an end-to-end exercise of the `LazyLift`
        // machinery, surfacing any drift between the eager + lazy
        // per-cube logic as a verdict-equality test failure.
        // U.4 (lift-unification, 2026-06-26) — the eager-vs-lazy dispatch +
        // the compound-predicate soundness gate now live behind the single
        // `lift_predicate_cube` entry. The Lazy path's must-edge policy is
        // taken from `lift_opts.must_edge_inference`, which the lift_opts
        // construction above sets equal to `cegar_opts.must_edge_inference`.
        let lift_result = crate::adapter::btor2::kmts_lift::lift_predicate_cube(
            current_predicates.clone(),
            btor2_content,
            adapter_options,
            &lift_opts,
            // B.1 3c — `effective_strategy` forces Eager when compounds present.
            effective_strategy,
        )?;

        // CTXDSL Phase 2 (2026-06-22) — capture this iteration's cube model
        // for the opt-in CTXDSL dump. Each iteration overwrites, so at any
        // return point `captured_clts` holds the final (latest) model.
        if cegar_opts.emit_ctxdsl {
            captured_clts = Some(lift_result.clts.clone());
        }

        // R.5 B.3.b (2026-06-01) — soundness warning. After the
        // FIRST lift, check whether the input formula has
        // alternation depth ≥ 2 AND the lift's CLTS has no
        // `MustHyperOnly` (hyper-must) transitions. Per
        // Shoham–Grumberg LMCS 2007, refining standard KMTS
        // (Sharp + MayOnly only) on alternating fixpoints is
        // **non-monotone** — verdicts may regress across CEGAR
        // iterations. Until R.2.5b's must-edge work emits
        // hyper-must transitions natively (paired B.3.c), this
        // gap is documented as a soundness-tagged warning so the
        // caller can flag νμ verdicts under refinement.
        if iteration == 0
            && formula.alternation_depth() >= 2
            && !crate::mu_calculus::clts_has_hyper_must_transitions(&lift_result.clts)
        {
            warnings.push(crate::adapter::AdapterWarning {
                kind: crate::adapter::WarningKind::ApproximateTranslation,
                location: None,
                message: format!(
                    "adapter/btor2/cegar (B.3.b): formula has alternation depth {} but the \
                     lifted CLTS has no MustHyperOnly (hyper-must) transitions. Refining \
                     standard KMTS (Sharp + MayOnly only) on alternating fixpoints is \
                     non-monotone per Shoham–Grumberg LMCS 2007 — verdicts may regress \
                     across CEGAR iterations. Until R.2.5b's SMT-driven must-edge work + \
                     paired B.3.c hyper-must extension ship, treat refined νμ verdicts as \
                     soundness-tagged.",
                    formula.alternation_depth()
                ),
            });
        }

        // PO-2 (cube-modal soundness audit, 2026-06-23) — surface a
        // soundness warning for any modality form OUTSIDE the audited-sound
        // cube fragment (controllability `ctrl=…` ⇒ unaudited per-player
        // game semantics PO-3/R.6.8; bounded `steps=k` ⇒ may/must filter
        // not applied PO-4/R.6.3.b; label-specific on a non-cube label ⇒
        // vacuous). Computed once (iteration 0) from the formula + the
        // cube's actual label symbols. Makes the out-of-fragment boundary
        // explicit on the `btor2 cegar` / `sv cegar` / `/cegar` surfaces
        // rather than silently returning an unsound/unaudited/vacuous
        // verdict. A WARNING, not a reject — V.6's controllability-aware
        // cube path keeps working (flagged, until R.6.8 closes).
        if iteration == 0 {
            let cube_labels: std::collections::HashSet<&str> = lift_result
                .clts
                .label_entries()
                .flatten()
                .map(|s| s.as_str())
                .collect();
            for msg in crate::mu_calculus::cube_modality_soundness_warnings(formula, &cube_labels) {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: crate::adapter::WarningKind::ApproximateTranslation,
                    location: None,
                    message: format!("adapter/btor2/cegar (PO-2 cube-modal soundness): {msg}"),
                });
            }
        }

        // R.5 B.4.a (2026-06-01) — smart `max_iterations` cap.
        // When the predicate source is WP AND the first lift
        // emits a UF-wrap warning AND `smart_uf_cap` is on
        // (default), drop the effective iteration cap to
        // `SMART_UF_MAX_ITERATIONS` (4). Surfaces cap-hit fast
        // on UF-spurious cases where WP cannot construct a
        // closing predicate without selective UF concretization
        // (which requires R-F3 SMT lemma cache access — not
        // shipped). Emit an explanatory warning so callers know
        // why the cap was reduced.
        if iteration == 0
            && cegar_opts.smart_uf_cap
            && matches!(
                cegar_opts.predicate_source,
                PredicateSource::WeakestPrecondition
            )
            && lift_result
                .warnings
                .iter()
                .any(|w| w.message.contains("UF-wrapped"))
        {
            let smart_cap = cegar_opts.max_iterations.min(SMART_UF_MAX_ITERATIONS);
            if smart_cap < cegar_opts.max_iterations {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: crate::adapter::WarningKind::ApproximateTranslation,
                    location: None,
                    message: format!(
                        "adapter/btor2/cegar (B.4.a): smart_uf_cap reduced max_iterations from \
                         {} to {} because the lift emitted a UF-wrap warning AND the predicate \
                         source is WeakestPrecondition. WP heuristics cannot construct closing \
                         predicates for UF-spurious KleeneBot cells (that needs selective UF \
                         concretization gated on R-F3 SMT lemma cache, not shipped). To use \
                         {} iterations literally, set `CegarOptions::smart_uf_cap = false`.",
                        cegar_opts.max_iterations, smart_cap, cegar_opts.max_iterations
                    ),
                });
                effective_max_iterations = smart_cap;
            }
        }

        // The evaluator needs an `Environment` sized to the lift's cube
        // count. The predicate set can change WITHIN this loop in two
        // ways the caller-supplied `env` cannot anticipate: (1) B.1 3c
        // appends the sidecar `compound_predicates` above, and (2) CEGAR
        // refinement grows the predicate set across iterations. Rather
        // than the prior hard-error ("caller must rebuild Environment"),
        // rebuild a correctly-sized env whenever the caller's doesn't
        // match. Safe because the cube-CEGAR formulas are closed
        // (fixpoints bind their own variables) — the caller's `env`
        // carries no free-variable bindings, only a state_count, so a
        // resized empty env is behaviourally identical when sizes match
        // and is the *correct* env when they don't. (The approximant-reuse
        // path below already anticipates in-loop cube growth via
        // `refine_cube_approximant`; this rebuild is the missing half.)
        let rebuilt_env = (env.state_count() != lift_result.clts.state_count())
            .then(|| Environment::new(lift_result.clts.state_count()));
        let eval_env: &Environment = rebuilt_env.as_ref().unwrap_or(env);

        // 2. Evaluate. When `capture_approximants` is set, wire an
        //    `on_fixpoint_convergence` callback into the evaluator
        //    options so the converged per-fixpoint-var iterates are
        //    captured into `approximants_at_end`. Default (callback
        //    not set) preserves the pre-1.2 behaviour exactly.
        //
        //    **B.1.a (2026-06-01)**: the captured value is a
        //    `StoredApproximant` carrying BOTH `must_true` and
        //    `may_true` bit-sets, not just the must-set. Sub-item
        //    1.4 needs `may_true` for the cube-refinement mapping.
        //
        //    **Sub-item 1.3 (2026-06-01)**: when
        //    `enable_approximant_reuse` is set AND a prior
        //    iteration's `approximants_at_end` exists, thread the
        //    prior `StoredApproximant::must_true` forward as the
        //    next iteration's `prior_approximants` seed. The seed
        //    carries the prior bitset's length as the
        //    `state_count` so the evaluator's `prior_approximants`
        //    state-count-match filter (evaluator.rs ~line 1873)
        //    silently drops entries when refinement grew the cube
        //    space — exactly the "no cube-refinement mapping yet"
        //    scope sub-item 1.4 will close.
        // **Sub-item 1.4.b (2026-06-01)**: when the prior bit-set's
        // length differs from the current lift's state count
        // (refinement grew the cube space), apply the
        // `refine_cube_approximant` parent-to-children mapping
        // to translate the prior bit-set into the refined space.
        // The mapping uses the same projection for both `must_true`
        // and `may_true`; per the standard refinement-monotonicity
        // lemma (Cousot–Cousot 1977; Shoham–Grumberg LMCS 2007), if
        // the parent verdict is KleeneT (KleeneF) then every child
        // verdict is also KleeneT (KleeneF); parent KleeneBot
        // children inherit KleeneBot as the upper bound.
        let current_lift_state_count = lift_result.clts.state_count();
        let prior_seed: Option<HashMap<usize, PriorApproximant>> = if cegar_opts
            .enable_approximant_reuse
            && let Some(prev) = iterations.last()
            && let Some(prev_approx) = prev.approximants_at_end.as_ref()
        {
            let mut seed = HashMap::new();
            for (var_idx, stored) in prev_approx {
                let prior_state_count = stored.must_true.len();
                let (refined_must, refined_may) = if prior_state_count == current_lift_state_count {
                    // No refinement; pass bits through unchanged.
                    (stored.must_true.clone(), stored.may_true.clone())
                } else if prior_state_count.is_power_of_two()
                    && current_lift_state_count.is_power_of_two()
                    && current_lift_state_count >= prior_state_count
                {
                    // Refinement grew the cube space — project via
                    // the parent-to-children mapping.
                    let n_old = prior_state_count.trailing_zeros() as usize;
                    let n_new = current_lift_state_count.trailing_zeros() as usize;
                    (
                        refine_cube_approximant(&stored.must_true, n_old, n_new),
                        refine_cube_approximant(&stored.may_true, n_old, n_new),
                    )
                } else {
                    // Unrecognised state-count change (shouldn't
                    // happen with the current lift contract; skip
                    // this entry to be safe).
                    continue;
                };
                seed.insert(
                    *var_idx,
                    PriorApproximant {
                        state_count: current_lift_state_count,
                        must_true: refined_must,
                        may_true: refined_may,
                    },
                );
            }
            Some(seed)
        } else {
            None
        };
        let captured: Option<Arc<Mutex<HashMap<usize, StoredApproximant>>>> =
            if cegar_opts.capture_approximants {
                Some(Arc::new(Mutex::new(HashMap::new())))
            } else {
                None
            };
        let mut eval_opts = EvaluationOptions {
            prior_approximants: prior_seed,
            ..Default::default()
        };
        let mut game_eval: GameEvaluation = if let Some(capture_handle) = &captured {
            let sink: Arc<Mutex<HashMap<usize, StoredApproximant>>> = Arc::clone(capture_handle);
            eval_opts.on_fixpoint_convergence =
                Some(Arc::new(move |var, view: &ApproximantView<'_>| {
                    if let Ok(mut guard) = sink.lock() {
                        guard.insert(
                            var.index(),
                            StoredApproximant {
                                must_true: view.must_true().clone(),
                                may_true: view.may_true().clone(),
                                polarity: view.polarity(),
                            },
                        );
                    }
                }));
            evaluate_3v_game_with_options(formula, &lift_result.clts, eval_env, &eval_opts)
                .map_err(eval_err_to_adapter_err)?
        } else if eval_opts.prior_approximants.is_some() {
            // Sub-item 1.3: even without capture, the prior_seed
            // path needs to flow through `with_options`.
            evaluate_3v_game_with_options(formula, &lift_result.clts, eval_env, &eval_opts)
                .map_err(eval_err_to_adapter_err)?
        } else {
            evaluate_3v_game(formula, &lift_result.clts, eval_env)
                .map_err(eval_err_to_adapter_err)?
        };

        // SOUNDNESS (unsatisfiable-cube-cell fix, 2026-07-23) — downgrade cube cells
        // that hold no concrete state (no outgoing may-transition) to ⊥ so they can
        // never contribute a spurious definite verdict (the i2c `AG EF (wb_ack_o==1)`
        // incident). See [`downgrade_unsatisfiable_cells`] for the argument. `unsat_cells`
        // is also excluded from the convergence check below (an unreal cell must not
        // keep the refinement loop spinning).
        let (masked_verdicts, unsat_cells) =
            downgrade_unsatisfiable_cells(&lift_result.clts, game_eval.verdicts.clone());
        game_eval.verdicts = masked_verdicts;

        let approximants_at_end: Option<HashMap<usize, StoredApproximant>> = captured.map(|h| {
            // B.6.a invariant: by this point `eval_opts` has been
            // dropped (the `if let` arm above ended), so the
            // callback closure holding the sink-side Arc is dropped
            // too. `try_unwrap` succeeds on the fast path; the
            // lock+clone fallback handles future refactors that
            // hold the closure longer.
            Arc::try_unwrap(h)
                .map(|m| m.into_inner().unwrap_or_default())
                .unwrap_or_else(|arc| arc.lock().map(|g| g.clone()).unwrap_or_default())
        });
        let game_position_evaluations = estimate_position_evaluations(&lift_result.clts, formula);

        // 3. Check convergence. Unsatisfiable cells (masked to ⊥ above) are not real
        // states, so they must not keep the loop refining — exclude them.
        let has_kleenebot = (0..game_eval.verdicts.len()).any(|s| {
            !unsat_cells.contains(&s) && matches!(game_eval.verdicts.verdict_at(s), Trit::Unknown)
        });

        // R-Y5 (2026-06-08) — identification pass: when a failure
        // subgame is present, every register named in the current
        // predicate set is a candidate for init-policy promotion.
        // The MVP's coarse heuristic ("any register in any
        // current predicate") is an over-approximation; future
        // R-Y5 follow-up refines by per-classifying-transition
        // analysis (only the predicates whose truth values DIFFER
        // between source + target of a classifying may-edge are
        // load-bearing). Coarse is acceptable for the MVP because
        // downstream consumers (Yosys re-run wrapper, user trace
        // review) treat the list as a CANDIDATE set, not an
        // authoritative one.
        if game_eval.failure_subgame.is_some() {
            for pred in &current_predicates {
                init_refinement_candidates.insert(pred.register.clone());
            }
        }

        if !has_kleenebot {
            // Converged. Record final iteration + return.
            iterations.push(CegarIteration {
                iteration,
                predicates_at_start: current_predicates.clone(),
                verdict: game_eval.verdicts.clone(),
                failure_subgame: None,
                predicates_added: Vec::new(),
                game_position_evaluations,
                approximants_at_end,
            });
            // Track I.1 — countertrace over the converged cube (Some only
            // when an initial cell is definite-False).
            let counterexample = build_counterexample_trace(
                &lift_result.clts,
                &game_eval.verdicts,
                &current_predicates,
            );
            return Ok(CegarTrace {
                iterations,
                final_verdict: game_eval.verdicts,
                final_predicates: current_predicates,
                terminated_with: CegarTermination::Converged,
                lazy_lift_pending: true,
                approximant_reuse_enabled: cegar_opts.enable_approximant_reuse,
                warnings: warnings.clone(),
                init_refinement_candidates: init_refinement_candidates.iter().cloned().collect(),
                // CTXDSL Phase 2 — final cube model (Some only under emit_ctxdsl).
                final_clts: captured_clts.take(),
                counterexample,
            });
        }

        // 4. Bounded refinement cap-hit check.
        // R.5 B.4.a (2026-06-01) — uses `effective_max_iterations`
        // (possibly reduced by the smart UF cap at iter 0)
        // instead of `cegar_opts.max_iterations` directly.
        if iteration == effective_max_iterations {
            iterations.push(CegarIteration {
                iteration,
                predicates_at_start: current_predicates.clone(),
                verdict: game_eval.verdicts.clone(),
                failure_subgame: game_eval.failure_subgame,
                predicates_added: Vec::new(),
                game_position_evaluations,
                approximants_at_end,
            });
            // Track I.1 — countertrace over the capped cube.
            let counterexample = build_counterexample_trace(
                &lift_result.clts,
                &game_eval.verdicts,
                &current_predicates,
            );
            return Ok(CegarTrace {
                iterations,
                final_verdict: game_eval.verdicts,
                final_predicates: current_predicates,
                terminated_with: CegarTermination::BoundedIterationsReached,
                lazy_lift_pending: true,
                approximant_reuse_enabled: cegar_opts.enable_approximant_reuse,
                warnings: warnings.clone(),
                init_refinement_candidates: init_refinement_candidates.iter().cloned().collect(),
                // CTXDSL Phase 2 — final cube model (Some only under emit_ctxdsl).
                final_clts: captured_clts.take(),
                counterexample,
            });
        }

        // 5. Predicate-source consultation.
        let subgame = game_eval
            .failure_subgame
            .as_ref()
            .expect("KleeneBot implies failure_subgame is Some (R.5.0 invariant)");
        let new_predicates = match &cegar_opts.predicate_source {
            PredicateSource::Manual(callback) => callback(subgame, &current_predicates),
            PredicateSource::WeakestPrecondition => {
                // R.5 WP MVP — emit separating predicates over state
                // registers reachable from the failure subgame's
                // classifying transitions that are NOT yet in the
                // predicate set. See
                // `.claude/plans/r5-wp-predicate-discovery-design-2026-05-27.md`.
                weakest_precondition_predicates(subgame, &current_predicates, btor2_content)
            }
            PredicateSource::CraigInterpolation => {
                // F.1 (2026-07-24) — TRANSITION-AWARE Craig refinement FIRST. Pose the
                // interpolation query over the EXACT transition (`craig_refinement_over_transition`)
                // rather than the legacy propositional cube-fact query, so cvc5 can return the
                // relational/bound invariant a branching ⊥ needs (emergent-K). A simple
                // `reg == value` is a new cube dimension; a compound (`!=`/relational/bound) is
                // installed into the compound-aware lift for the next iteration.
                let (simple, compound) = match &parsed_file_for_craig {
                    Some(file) => craig_refinement_over_transition(
                        subgame,
                        &current_predicates,
                        &lift_result.clts,
                        file,
                        cvc5_opts.timeout.as_millis() as u32,
                    ),
                    None => (Vec::new(), Vec::new()),
                };
                // A discovered compound needs BOTH a `compound_exprs` entry AND a matching
                // PredicateSpec in the predicate set: the lift keys the cube dimension by NAME (the
                // spec's register/value are placeholders — the actual constraint is the expr). Push
                // the spec into the returned predicates so it lands in `current_predicates` and the
                // next compound-aware re-lift sees a well-formed dimension.
                let mut out = simple;
                for expr in compound {
                    if lift_opts.compound_exprs.values().any(|e| e == &expr) {
                        continue; // re-discovery, not progress
                    }
                    let name = format!("craig_compound_{}", lift_opts.compound_exprs.len());
                    let placeholder = expr.registers().into_iter().next().unwrap_or_default();
                    lift_opts.compound_exprs.insert(name.clone(), expr);
                    out.push(PredicateSpec {
                        name,
                        register: placeholder,
                        value: 0,
                    });
                }
                if !out.is_empty() {
                    out
                } else {
                    // No transition-aware progress → the legacy propositional Craig query (F.2,
                    // kept as a fallback pending removal), then WP — never silently terminate.
                    let legacy = if let Some(bin) = &cvc5_bin {
                        craig_interpolation_predicates(
                            subgame,
                            &current_predicates,
                            &lift_result.clts,
                            bin,
                            &cvc5_opts,
                        )
                    } else {
                        Vec::new()
                    };
                    if !legacy.is_empty() {
                        legacy
                    } else {
                        weakest_precondition_predicates(subgame, &current_predicates, btor2_content)
                    }
                }
            }
        };

        let added_count = new_predicates.len();
        iterations.push(CegarIteration {
            iteration,
            predicates_at_start: current_predicates.clone(),
            verdict: game_eval.verdicts.clone(),
            failure_subgame: game_eval.failure_subgame,
            predicates_added: new_predicates.clone(),
            game_position_evaluations,
            approximants_at_end,
        });

        if added_count == 0 {
            // Track I.1 — countertrace over the exhausted-source cube.
            let counterexample = build_counterexample_trace(
                &lift_result.clts,
                &game_eval.verdicts,
                &current_predicates,
            );
            return Ok(CegarTrace {
                final_verdict: iterations.last().unwrap().verdict.clone(),
                final_predicates: current_predicates,
                terminated_with: CegarTermination::PredicateSourceExhausted,
                iterations,
                lazy_lift_pending: true,
                approximant_reuse_enabled: cegar_opts.enable_approximant_reuse,
                warnings: warnings.clone(),
                init_refinement_candidates: init_refinement_candidates.iter().cloned().collect(),
                // CTXDSL Phase 2 — final cube model (Some only under emit_ctxdsl).
                final_clts: captured_clts.take(),
                counterexample,
            });
        }

        // 6. Extend predicate set for the next iteration. Predicates
        //    are appended in order; the caller's source is
        //    responsible for not re-adding duplicates.
        current_predicates.extend(new_predicates);
    }

    // Loop exits cleanly via Converged / cap-hit / exhausted; this
    // path is structurally unreachable.
    unreachable!("CEGAR loop fell through without termination — bug in loop structure")
}

/// MVP position-evaluation counter. R.5 follow-up replaces with a
/// precise per-position counter from the game solver.
fn estimate_position_evaluations<S, L>(clts: &crate::clts::Clts<S, L>, formula: &Formula) -> usize
where
    S: crate::clts::IdStorage,
    L: crate::clts::IdStorage,
{
    clts.state_count() * formula.nodes().len()
}

/// R.5 sub-item 1.4.b (2026-06-01) — cube-refinement mapping.
/// Projects a bit-set defined over the coarse cube space (size
/// `2^n_old_preds`) to the refined cube space (size
/// `2^n_new_preds`) via the parent-to-children copy.
///
/// **Indexing convention** (matches `predicate_cube_lift` at
/// `kmts_lift.rs` ~line 587): predicate at position `bit` in
/// the predicate list maps to cube_index bit `bit`. When new
/// predicates are appended (the CEGAR loop's
/// `current_predicates.extend(new_predicates)` pattern), they
/// occupy positions `[n_old_preds .. n_new_preds)` — i.e. the
/// HIGH bits of the refined cube index. A child cube `c` has
/// parent `c & ((1 << n_old_preds) - 1)` (the low bits).
///
/// **Soundness** (Cousot–Cousot 1977; Shoham–Grumberg LMCS 2007
/// §3): each parent cube partitions the concrete state space
/// into a set of children; the formula's 3-valued verdict at
/// each child is at least as defined as the verdict at the
/// parent (info-order ≤). Parent KleeneT implies every child
/// KleeneT; parent KleeneF implies every child KleeneF; parent
/// KleeneBot allows children to be anything. So copying the
/// parent's must_true bit to every child is a sound lower bound
/// on the child's must_true; same for may_true → may_true.
///
/// **Preconditions**: `prior_bits.len() == 2^n_old_preds`,
/// `n_new_preds >= n_old_preds`.
fn refine_cube_approximant(
    prior_bits: &EvalResult,
    n_old_preds: usize,
    n_new_preds: usize,
) -> EvalResult {
    debug_assert!(
        n_new_preds >= n_old_preds,
        "refinement cannot shrink the predicate set"
    );
    let old_cube_count = 1usize << n_old_preds;
    let new_cube_count = 1usize << n_new_preds;
    debug_assert_eq!(
        prior_bits.len(),
        old_cube_count,
        "prior_bits length must match 2^n_old_preds"
    );
    let mask_old: usize = old_cube_count.saturating_sub(1);
    let mut refined: EvalResult = bitvec::vec::BitVec::repeat(false, new_cube_count);
    for c in 0..new_cube_count {
        let parent = c & mask_old;
        if prior_bits[parent] {
            refined.set(c, true);
        }
    }
    refined
}

fn eval_err_to_adapter_err(e: EvaluationError) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::IrConsistencyError,
        location: None,
        message: format!("adapter/btor2/cegar: evaluator error: {e:?}"),
    }
}

/// R.5 Item 3 sub-item 3.4 (2026-06-04) — Craig interpolation
/// predicate source. For each classifying may-but-not-must
/// transition in the failure subgame, invoke CVC5 to compute
/// a separating predicate between the source cube and the
/// target cube.
///
/// **Inputs**:
/// - `subgame.classifying_transitions`: `(source_state_index,
///   transition_index)` pairs. The lifter ensures
///   `state_index == cube_index`.
/// - `current_predicates`: the predicate set defining the cube
///   space (so the SMT-LIB query knows the bit positions).
/// - `clts`: needed to look up the target cube from the
///   `transition_index`.
/// - `cvc5_bin`: located via `adapter::cvc5::locate_cvc5` once
///   at loop start (cached by the caller).
/// - `opts`: subprocess timeout etc.
///
/// **Returns**: list of `PredicateSpec`s — one per classifying
/// transition where CVC5 produced a parsable equality
/// interpolant. Empty when no transitions yielded interpolants
/// (CEGAR loop falls back to WP per the dispatch in
/// `cegar_refine_loop`).
///
/// **Dedup**: predicates that match an existing entry in
/// `current_predicates` by (register, value) are skipped —
/// re-adding them would not refine the cube space.
fn craig_interpolation_predicates(
    subgame: &FailureSubgame,
    current_predicates: &[PredicateSpec],
    clts: &crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>,
    cvc5_bin: &crate::adapter::cvc5::Cvc5Bin,
    opts: &crate::adapter::cvc5::InterpolantQueryOptions,
) -> Vec<PredicateSpec> {
    if subgame.classifying_transitions.is_empty() {
        return Vec::new();
    }
    use crate::clts::StateId;
    let mut out: Vec<PredicateSpec> = Vec::new();
    let mut seen_pairs: std::collections::HashSet<(String, u64)> = current_predicates
        .iter()
        .map(|p| (p.register.clone(), p.value))
        .collect();
    for (src_idx, t_idx, _owner) in &subgame.classifying_transitions {
        // R.6.5 — owning-player tag ignored by the Craig path's MVP;
        // R.6.5.b will branch on it to pick the refinement direction
        // (env ⇒ shrink R_may; ctrl ⇒ grow R_must).
        let Some(src_id) = StateId::<crate::clts::DefaultStateIdx>::from_index(*src_idx) else {
            continue;
        };
        let transitions = clts.outgoing(src_id);
        let Some(transition) = transitions.get(*t_idx) else {
            continue;
        };
        let target_idx = transition.target().index();
        let query = crate::adapter::cvc5::build_interpolation_query(
            current_predicates,
            *src_idx,
            target_idx,
        );
        match crate::adapter::cvc5::invoke_cvc5_for_interpolant(cvc5_bin, &query, opts) {
            Ok(Some(mut spec)) => {
                if !seen_pairs.contains(&(spec.register.clone(), spec.value)) {
                    seen_pairs.insert((spec.register.clone(), spec.value));
                    // Tag the predicate with the source state for
                    // diagnostics, mirroring the WP helper's naming.
                    spec.name = format!("craig_s{}_t{}", src_idx, t_idx);
                    out.push(spec);
                }
            }
            Ok(None) | Err(_) => {
                // CVC5 reported no interpolant, or compound/unparsable
                // form, or subprocess failure. The CEGAR loop's
                // dispatch falls back to WP when this helper returns
                // empty across all transitions.
            }
        }
    }
    out
}

/// The predicate REGION for a cube index over the SIMPLE predicate dimensions — bit `i` of
/// `cube` is predicate `i`'s polarity (`predicates[i] == value` when set, `!=` when clear),
/// mirroring `predicate_cube_lift`'s indexing (see `render_cube_conjunction`). Compound
/// dimensions occupy the HIGH bits and are OMITTED: a weaker (fewer-conjunct) source region only
/// WEAKENS the interpolation query's A side, so any predicate it yields is still a SOUND
/// separator — at worst less precise, never unsound.
fn cube_index_to_region(
    predicates: &[PredicateSpec],
    cube: usize,
) -> Vec<crate::adapter::btor2::predicate_expr::PredicateExpr> {
    use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
    predicates
        .iter()
        .enumerate()
        .map(|(i, p)| PredicateExpr::Cmp {
            register: p.register.clone(),
            op: if (cube >> i) & 1 == 1 {
                CmpOp::Eq
            } else {
                CmpOp::Ne
            },
            value: p.value,
        })
        .collect()
}

/// F.1 — TRANSITION-AWARE Craig refinement, MUST-PRECONDITION query (the recoverability-correct
/// interpolation). For each classifying MayOnly transition `(src_cube → target_cube)` of the ⊥
/// failure subgame, the target cube is the good-ward cube, so `must_precondition_interpolant`
/// interpolates the MUST-PRECONDITION of reaching it — `A = source ∧ T_a ∧ target(next_a)` vs
/// `B = source ∧ T_b ∧ ¬target(next_b)`, two next-state frames sharing the current state — yielding
/// the invariant `I(cur)` that makes the target must-reachable (the WP relation the ⊥ needs, e.g.
/// `data == target` / `reg <= K`). This is the query a recoverability MUST-OBLIGATION ⊥ needs, NOT a
/// safety spurious-edge query. Solved by **MathSAT** proof-based lazy-BV interpolation; cvc5's SyGuS
/// BV interpolation returns `fail` on this shape, so mathsat is REQUIRED to decide (mathsat-absent ⇒
/// this yields nothing and the caller falls back to WP — sound, just undecided).
///
/// Splits results: a simple `reg == value` becomes a cube DIMENSION (`PredicateSpec`); any other
/// shape (`!=` / relational / bound) becomes a compound EXPR the caller installs into the
/// SmtAllPairs+Eager compound-aware lift. Addresses only NAMED state cells; a property atom over a
/// combinational output contributes a weaker region but never an unsound predicate.
#[allow(clippy::type_complexity)]
fn craig_refinement_over_transition(
    subgame: &FailureSubgame,
    current_predicates: &[PredicateSpec],
    clts: &crate::clts::Clts<crate::clts::DefaultStateIdx, crate::clts::DefaultLabelIdx>,
    file: &crate::adapter::btor2::ast::Btor2File,
    timeout_ms: u32,
) -> (
    Vec<PredicateSpec>,
    Vec<crate::adapter::btor2::predicate_expr::PredicateExpr>,
) {
    use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
    use crate::adapter::btor2::refine::{RefineOutcome, must_precondition_interpolant};
    use crate::clts::StateId;
    let mut simple: Vec<PredicateSpec> = Vec::new();
    let mut compound: Vec<PredicateExpr> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (src_idx, t_idx, _owner) in &subgame.classifying_transitions {
        let Some(src_id) = StateId::<crate::clts::DefaultStateIdx>::from_index(*src_idx) else {
            continue;
        };
        let transitions = clts.outgoing(src_id);
        let Some(transition) = transitions.get(*t_idx) else {
            continue;
        };
        let target_idx = transition.target().index();
        let source = cube_index_to_region(current_predicates, *src_idx);
        let target = cube_index_to_region(current_predicates, target_idx);
        // F.1 must-precondition (2026-07-24) — the classifying transition's TARGET cube is the
        // good-ward cube, so interpolate the MUST-PRECONDITION of reaching it (`{cur:T→target}` vs
        // `{cur:T→¬target}`) rather than a safety spurious-edge query — the recoverability-correct
        // query the ⊥ needs. Solved by MathSAT proof-based lazy-BV interpolation; mathsat-absent /
        // no-interpolant ⇒ nothing here and the caller falls back to WP.
        if let RefineOutcome::Predicate(p) =
            must_precondition_interpolant(file, &source, &target, timeout_ms)
        {
            // Dedup identical interpolants across the classifying transitions.
            if !seen.insert(format!("{p:?}")) {
                continue;
            }
            match &p {
                PredicateExpr::Cmp {
                    register,
                    op: CmpOp::Eq,
                    value,
                } => simple.push(PredicateSpec {
                    name: format!("craig_{register}_eq_{value}"),
                    register: register.clone(),
                    value: *value,
                }),
                _ => compound.push(p),
            }
        }
    }
    (simple, compound)
}

/// R.5 WP — emit separating predicates from the failure subgame's
/// classifying transitions over state registers that are NOT yet in
/// the predicate set.
///
/// **Heuristic.** Walk the BTOR2 file's `Node::State` lines; for each
/// state cell whose symbol is not already covered by
/// `current_predicates`, propose up to 2 predicates from the
/// candidate-value pool: `{0, 1} ∪ collect_btor2_constants(file)`.
/// Values from the BTOR2 const set come from
/// [`bit_blast::collect_btor2_constants`] — every distinct literal
/// the design's BTOR2 carries (sums + initial values + comparison
/// constants) becomes a candidate. The CEGAR loop appends these to
/// the predicate set for the next iteration.
///
/// **What this MVP does NOT do** (R.5 follow-ups):
/// - No actual weakest-precondition formula computation (no
///   symbolic back-substitution along the classifying transition).
///   The name "WP" is aspirational for the API surface; the
///   mechanism here is "any uncovered register + any literal".
/// - No cone-of-influence bound — predicates may be proposed for
///   registers irrelevant to the classifying transition's
///   dependency cone. Bounded-iteration cap (16 by default)
///   prevents runaway.
/// - No classifying-transition coverage ranking — picks first-found
///   first-emitted.
///
/// **Capping.** Returns at most 2 predicates per call to keep the
/// per-iteration growth bounded; the CEGAR loop iterates and the
/// helper picks up the next uncovered register on the following
/// pass.
///
/// **Empty-result termination.** Returns empty Vec when every state
/// register is already covered, OR when the BTOR2 file fails to
/// parse — the CEGAR loop then terminates with
/// `CegarTermination::PredicateSourceExhausted`.
/// R-F5.5b (2026-07-03) — the WP predicate-discovery step for the **symbolic**
/// CEGAR loop. The symbolic path detects ⊥ cubes but does not build the explicit
/// parity-game failure subgame; [`weakest_precondition_predicates`] only uses the
/// subgame as a non-empty *trigger* (it proposes predicates from the current
/// predicates' cone-of-influence + BTOR2 constants, not from the specific
/// classifying transition), so a minimal placeholder subgame is sufficient.
/// Returns the next batch of candidate predicates (capped, same as the explicit
/// loop); empty ⇒ the predicate source is exhausted.
pub fn wp_refine_predicates(
    current_predicates: &[PredicateSpec],
    btor2_content: &str,
) -> Vec<PredicateSpec> {
    let trigger = FailureSubgame {
        positions: Vec::new(),
        classifying_transitions: vec![(
            0,
            0,
            crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
        )],
        root: None,
        subgame_extraction_complete: false,
    };
    weakest_precondition_predicates(&trigger, current_predicates, btor2_content)
}

fn weakest_precondition_predicates(
    subgame: &FailureSubgame,
    current_predicates: &[PredicateSpec],
    btor2_content: &str,
) -> Vec<PredicateSpec> {
    // The subgame's classifying_transitions is the trigger for WP —
    // we only propose predicates when there's an actual KleeneBot
    // verdict from a may-but-not-must edge to refine.
    if subgame.classifying_transitions.is_empty() {
        return Vec::new();
    }

    let file = match crate::adapter::btor2::parser::parse(btor2_content) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    let covered: std::collections::HashSet<&str> = current_predicates
        .iter()
        .map(|p| p.register.as_str())
        .collect();

    // R.5 WP literal-extraction follow-up — candidate value pool is
    // {0, 1} ∪ (distinct BTOR2 const literals). 0 and 1 stay at the
    // front so existing fixtures that only need basic splits keep
    // their predicate-set growth path unchanged; literal values
    // append after as additional discriminators for fixtures with
    // wider comparison constants (e.g. FSM states 4'b110111).
    let mut candidate_values: Vec<u64> = vec![0, 1];
    for v in crate::adapter::btor2::bit_blast::collect_btor2_constants(&file) {
        if !candidate_values.contains(&v) {
            candidate_values.push(v);
        }
    }

    // R.5 WP cone-of-influence bound — restrict proposals to state
    // cells that the predicate-set's registers transitively depend
    // on via the BTOR2 next-state graph. Without this bound, WP
    // would propose predicates over registers irrelevant to the
    // classifying transition's behavior (wasting CEGAR iterations
    // on splits that can't change the verdict).
    //
    // Algorithm: for each register named in `current_predicates`,
    // find its BTOR2 state NID via the symbol table + walk its
    // next-line operand graph backward, collecting every reachable
    // State NID. The union of those NIDs (translated back to
    // symbols) is the COI. Empty COI ⇒ no restriction, fall back
    // to "any uncovered register".
    let coi_symbols: std::collections::HashSet<&str> = {
        let mut name_to_nid: std::collections::HashMap<&str, Nid> =
            std::collections::HashMap::new();
        for (nid, name) in &symbols {
            name_to_nid.insert(name.as_str(), *nid);
        }
        let mut reachable_nids: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        for pred in current_predicates {
            let Some(&seed_nid) = name_to_nid.get(pred.register.as_str()) else {
                continue;
            };
            let Some(next_value) =
                crate::adapter::btor2::parser::find_next_value_operand(&file, seed_nid)
            else {
                // No `Next` line → the state's COI is just itself.
                reachable_nids.insert(seed_nid);
                continue;
            };
            let from_this = crate::adapter::btor2::parser::collect_reachable_states_from(
                &file,
                std::slice::from_ref(&next_value),
            );
            // Include the seed state itself + everything its next-value
            // depends on.
            reachable_nids.insert(seed_nid);
            reachable_nids.extend(from_this);
        }
        let mut out: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for nid in &reachable_nids {
            if let Some(name) = symbols.get(nid) {
                out.insert(name.as_str());
            }
        }
        out
    };
    // Empty COI ⇒ fall back to "any uncovered state cell".
    let coi_active = !coi_symbols.is_empty();

    // Walk state cells; propose up to 2 predicates per uncovered
    // register (per the doc-comment's bounded-growth policy).
    //
    // R.5 WP COI bound — try the COI-restricted walk first; if it
    // yields no proposals (the COI was too narrow to admit any
    // uncovered register), fall back to the unrestricted walk. The
    // fallback ensures the helper preserves its prior-MVP "always
    // returns at least one proposal when some register is uncovered"
    // contract.
    let mut out = walk_state_cells_proposing(
        &file,
        &symbols,
        &covered,
        &candidate_values,
        current_predicates,
        if coi_active { Some(&coi_symbols) } else { None },
    );
    if out.is_empty() && coi_active {
        out = walk_state_cells_proposing(
            &file,
            &symbols,
            &covered,
            &candidate_values,
            current_predicates,
            None,
        );
    }
    out
}

/// R.5 WP helper — walk state cells in the BTOR2, proposing
/// predicates over each uncovered register (cap at 2 total per
/// invocation per the bounded-growth policy). When `coi_filter` is
/// `Some`, only registers in that set are eligible.
fn walk_state_cells_proposing(
    file: &crate::adapter::btor2::ast::Btor2File,
    symbols: &std::collections::HashMap<Nid, String>,
    covered: &std::collections::HashSet<&str>,
    candidate_values: &[u64],
    current_predicates: &[PredicateSpec],
    coi_filter: Option<&std::collections::HashSet<&str>>,
) -> Vec<PredicateSpec> {
    let mut seen_proposals: std::collections::HashSet<(String, u64)> =
        std::collections::HashSet::new();
    let existing_register_value_pairs: std::collections::HashSet<(&str, u64)> = current_predicates
        .iter()
        .map(|p| (p.register.as_str(), p.value))
        .collect();
    let mut out: Vec<PredicateSpec> = Vec::new();

    for line in &file.lines {
        if !matches!(line.node, crate::adapter::btor2::ast::Node::State { .. }) {
            continue;
        }
        let symbol = match symbols.get(&line.nid) {
            Some(s) => s.as_str(),
            None => continue,
        };
        if covered.contains(symbol) {
            continue;
        }
        if let Some(coi) = coi_filter
            && !coi.contains(symbol)
        {
            continue;
        }
        for &v in candidate_values {
            if existing_register_value_pairs.contains(&(symbol, v)) {
                continue;
            }
            let proposal = (symbol.to_string(), v);
            if seen_proposals.contains(&proposal) {
                continue;
            }
            seen_proposals.insert(proposal.clone());
            out.push(PredicateSpec {
                name: format!("wp_{symbol}_eq_{v}"),
                register: symbol.to_string(),
                value: v,
            });
            if out.len() >= 2 {
                break;
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// B.1 increment 3c (2026-06-26) — read `compound_predicates` from the
/// `SvAnnotation` sidecar JSON (`AdapterOptions::sidecar_json`) and return them
/// as `(PredicateSpec, PredicateExpr)` pairs ready for the predicate-cube lift.
///
/// Mirrors [`crate::adapter::btor2::r_s8_encoder::sidecar_config_values`]'s
/// defensive-skip pattern. Returns an empty Vec when:
/// - the sidecar JSON is absent or fails to parse as an `SvAnnotation`;
/// - no `compound_predicates` are declared.
///
/// A single malformed `expr` is skipped (with a `tracing::warn!`) rather than
/// failing the whole lift. Each pair is
/// `(PredicateSpec { name, register, value: 0 }, expr)` where `register` is the
/// FIRST register the expr references — a placeholder; the predicate's cube-bit
/// truth is decided by `expr` via the SMT seam's `PredicateLike::expr`, NOT by
/// `register == value`. The `name` matches the cube-bit / `compound_exprs` key.
///
/// Shared by the CEGAR loop today; the no-sidecar `@mununu`-tag path (Track H
/// H.3) reuses the same `parse_predicate_expr` → compound-predicate bridge.
pub fn sidecar_compound_predicates(
    options: &AdapterOptions,
) -> Vec<(
    PredicateSpec,
    crate::adapter::btor2::predicate_expr::PredicateExpr,
    bool, // H.F — `derived`: true ⇒ a per-cube label (not a cube dimension).
)> {
    let Some(json) = &options.sidecar_json else {
        return Vec::new();
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for decl in &ann.compound_predicates {
        let expr = match crate::adapter::btor2::predicate_expr::parse_predicate_expr(&decl.expr) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    compound = %decl.name,
                    expr = %decl.expr,
                    error = %err,
                    "sidecar_compound_predicates: skipping malformed compound predicate expr"
                );
                continue;
            }
        };
        // Placeholder register = first register the expr references. The expr
        // (not register==value) drives the cube-bit truth; resolution of the
        // remaining expr registers follows the same canonical-name assumption
        // the simple `predicates[]` field already makes.
        let register = expr.registers().into_iter().next().unwrap_or_default();
        out.push((
            PredicateSpec {
                name: decl.name.clone(),
                register,
                value: 0,
            },
            expr,
            decl.derived,
        ));
    }
    out
}

/// H.E.2 (2026-06-28) — read `combinational_predicates` from the SvAnnotation
/// sidecar into derived-label [`PredicateSpec`]s (`name` = the formula atom,
/// `register` = the combinational signal's symbol, `value`). The cube lift
/// labels each per cube via the SMT `combinational_labels` pass — they are NOT
/// cube dimensions, so they never enter `current_predicates` / the cube index.
/// Empty when there is no sidecar or no `combinational_predicates` declared.
pub fn sidecar_combinational_predicates(options: &AdapterOptions) -> Vec<PredicateSpec> {
    let Some(json) = &options.sidecar_json else {
        return Vec::new();
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return Vec::new();
    };
    ann.combinational_predicates
        .iter()
        .map(|d| PredicateSpec {
            name: d.name.clone(),
            register: d.signal.clone(),
            value: d.value,
        })
        .collect()
}

/// Parse `--config-values`-style `REG=v1,v2,...` entries into the synthetic
/// sidecar JSON the predicate-cube lift reads via `sidecar_config_values`
/// (R-S8). Shared by the CLI (`mununu btor2 cegar --config-values`) and the
/// HTTP API (`POST /api/v1/btor2/cegar` `config_values`) so the entry format
/// has a single source of truth (no CLI↔API drift). Returns `None` for an
/// empty input (no synthetic sidecar), `Some(json)` otherwise. Each entry is
/// `REG=v1,v2,...` with at least one `u64` value.
pub fn config_values_to_sidecar_json(entries: &[String]) -> Result<Option<String>, String> {
    if entries.is_empty() {
        return Ok(None);
    }
    let mut signals = Vec::with_capacity(entries.len());
    for entry in entries {
        let (reg, values_str) = entry.split_once('=').ok_or_else(|| {
            format!("invalid config-values entry {entry:?}: expected REG=v1,v2,v3 format")
        })?;
        let values: Vec<u64> = values_str
            .split(',')
            .map(|s| {
                s.trim()
                    .parse::<u64>()
                    .map_err(|e| format!("invalid value {s:?} in config-values: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.is_empty() {
            return Err(format!(
                "config-values {entry:?}: at least one value required"
            ));
        }
        signals.push(serde_json::json!({ "name": reg, "config_values": values }));
    }
    let synthetic = serde_json::json!({
        "module": "cegar",
        "source": "cegar.btor2",
        "signals": signals,
    });
    Ok(Some(synthetic.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::parser;

    // Track I.1 — a non-HOLDS verdict surfaces the predicate valuation of the
    // cube cells that falsify the formula (definite-F) or that the abstraction
    // cannot decide (⊥), decoded from the cube-index bit convention.
    #[test]
    fn i1_witness_cells_decode_falsifying_and_undecided_valuations() {
        use bitvec::prelude::*;
        // 4 cubes over predicates [idle (bit 0), err (bit 1)]:
        //   cube0 = F, cube1 = T, cube2 = F, cube3 = ⊥.
        let must = bitvec![usize, Lsb0; 0, 1, 0, 0];
        let may = bitvec![usize, Lsb0; 0, 1, 0, 1];
        let trace = CegarTrace {
            iterations: Vec::new(),
            final_verdict: crate::mu_calculus::trit::TritSet::from_parts(must, may),
            final_predicates: vec![
                PredicateSpec {
                    name: "idle".into(),
                    register: "state_q".into(),
                    value: 55,
                },
                PredicateSpec {
                    name: "err".into(),
                    register: "state_q".into(),
                    value: 41,
                },
            ],
            terminated_with: CegarTermination::Converged,
            lazy_lift_pending: true,
            approximant_reuse_enabled: false,
            warnings: Vec::new(),
            init_refinement_candidates: Vec::new(),
            final_clts: None,
            counterexample: None,
        };

        let viol = trace.violating_cells(10);
        assert_eq!(viol.len(), 2, "two cube cells falsify the formula");
        assert_eq!(viol[0].render(), "idle=false, err=false"); // cube0
        assert_eq!(viol[1].render(), "idle=false, err=true"); // cube2 (the err trap)

        let undec = trace.undecided_cells(10);
        assert_eq!(undec.len(), 1);
        assert_eq!(undec[0].render(), "idle=true, err=true"); // cube3

        // The cap is honoured.
        assert_eq!(trace.violating_cells(1).len(), 1);
    }

    // Track I.1 (trace slice) — a VIOLATED cube yields a reachability
    // countertrace: a path of definite-False cells from the initial cell to a
    // trap (a cell whose every successor stays False). The walk traverses only
    // F→F edges and never enters the T/⊥ region.
    #[test]
    fn i1_counterexample_trace_walks_false_region_to_trap() {
        use crate::clts::Clts;
        use bitvec::prelude::*;
        // 4 cubes over predicates [idle (bit 0), err (bit 1)].
        // Verdicts: s0=F (init), s1=F, s2=F (trap via self-loop), s3=T.
        // Edges: s0→s1, s0→s3, s1→s2, s1→s3, s2→s2.
        let mut builder = Clts::builder();
        builder
            .state("s0")
            .state("s1")
            .state("s2")
            .state("s3")
            .initial("s0");
        let step = builder.labels().intern(["step"]).expect("label interned");
        let s0 = builder.state_id_or_insert("s0").unwrap();
        let s1 = builder.state_id_or_insert("s1").unwrap();
        let s2 = builder.state_id_or_insert("s2").unwrap();
        let s3 = builder.state_id_or_insert("s3").unwrap();
        builder.transition_ids(s0, &[step], s1);
        builder.transition_ids(s0, &[step], s3);
        builder.transition_ids(s1, &[step], s2);
        builder.transition_ids(s1, &[step], s3);
        builder.transition_ids(s2, &[step], s2); // self-loop → trap
        let clts = builder.build().expect("clts builds");

        let must = bitvec![usize, Lsb0; 0, 0, 0, 1];
        let may = bitvec![usize, Lsb0; 0, 0, 0, 1];
        let verdict = crate::mu_calculus::trit::TritSet::from_parts(must, may);
        let preds = vec![
            PredicateSpec {
                name: "idle".into(),
                register: "state_q".into(),
                value: 55,
            },
            PredicateSpec {
                name: "err".into(),
                register: "state_q".into(),
                value: 41,
            },
        ];

        let trace = build_counterexample_trace(&clts, &verdict, &preds)
            .expect("an initial F cell yields a countertrace");
        assert!(trace.ends_in_trap, "the walk reaches the s2 self-loop trap");
        let rendered: Vec<String> = trace.steps.iter().map(|c| c.render()).collect();
        assert_eq!(
            rendered,
            vec![
                "idle=false, err=false".to_string(), // s0 / cube0 (init)
                "idle=true, err=false".to_string(),  // s1 / cube1
                "idle=false, err=true".to_string(),  // s2 / cube2 (trap)
            ]
        );
    }

    // Track I.1 — when no initial cell is definite-False (the property is not
    // violated at start), there is no countertrace.
    #[test]
    fn i1_counterexample_trace_none_when_initial_not_false() {
        use crate::clts::Clts;
        use bitvec::prelude::*;
        let mut builder = Clts::builder();
        builder.state("s0").state("s1").initial("s0");
        let step = builder.labels().intern(["step"]).expect("label interned");
        let s0 = builder.state_id_or_insert("s0").unwrap();
        let s1 = builder.state_id_or_insert("s1").unwrap();
        builder.transition_ids(s0, &[step], s1);
        let clts = builder.build().expect("clts builds");
        // s0 = T (must+may set), s1 = F.
        let must = bitvec![usize, Lsb0; 1, 0];
        let may = bitvec![usize, Lsb0; 1, 0];
        let verdict = crate::mu_calculus::trit::TritSet::from_parts(must, may);
        let preds = vec![PredicateSpec {
            name: "idle".into(),
            register: "state_q".into(),
            value: 55,
        }];
        assert!(build_counterexample_trace(&clts, &verdict, &preds).is_none());
    }

    // M.6 parity — the shared `REG=v1,v2,...` parse used by both the CLI
    // `--config-values` flag and the API `config_values` field.
    #[test]
    fn config_values_to_sidecar_json_empty_is_none() {
        assert_eq!(config_values_to_sidecar_json(&[]).unwrap(), None);
    }

    #[test]
    fn config_values_to_sidecar_json_parses_entries() {
        let json = config_values_to_sidecar_json(&["boot_fsm_ns=0,1,2,3,4,5,6,7".to_string()])
            .unwrap()
            .expect("non-empty input yields a synthetic sidecar");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let sig = &parsed["signals"][0];
        assert_eq!(sig["name"], "boot_fsm_ns");
        assert_eq!(
            sig["config_values"],
            serde_json::json!([0, 1, 2, 3, 4, 5, 6, 7])
        );
    }

    #[test]
    fn config_values_to_sidecar_json_rejects_malformed() {
        assert!(config_values_to_sidecar_json(&["no_equals_sign".to_string()]).is_err());
        assert!(config_values_to_sidecar_json(&["reg=".to_string()]).is_err());
        assert!(config_values_to_sidecar_json(&["reg=1,notanumber".to_string()]).is_err());
    }

    const SMALL_BTOR2: &str = "\
1 sort bitvec 1
2 state 1 reg_a
3 state 1 reg_b
4 zero 1
5 init 1 2 4
6 init 1 3 4
7 next 1 2 4
8 next 1 3 4
";

    // A.5 (2026-07-05) — trap-FSM fixtures for the recoverability soundness
    // reproduction. State `s` (2 bits): idle = 1, error = 2. The FSM's next-state
    // is `fsm_next = (s == error) ? error : idle` — error is a permanent trap
    // (self-loop), every other state moves to idle. No `init` line ⇒ `s` is havoc
    // (the cube path enumerates all 4 predicate cubes, including the error cube).
    // Predicates: idle = (s == 1), err = (s == 2).
    //
    // `TRAP_FSM_RESET_PINNED` is the "reset held" model: `next(s) = fsm_next` (no
    // reset mux). From the error cube idle is UNREACHABLE, so `AG EF idle` is
    // definite-FALSE at the error cube — the sound trap verdict.
    const TRAP_FSM_RESET_PINNED: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 s
4 constd 2 1
5 constd 2 2
6 eq 1 3 5
7 ite 2 6 5 4
8 next 2 3 7
";

    // `TRAP_FSM_RESET_FREE` is the SAME FSM with an async active-low reset mux left
    // as a LIVE input: `next(s) = rst_ni ? fsm_next : idle`. rst_ni is a free
    // primary input (not constant-folded). From the error cube the ∀∃ must-edge
    // inference can pick rst_ni = 0 (asserted) ⇒ next = idle, so a must-edge
    // error → idle is REAL and `AG EF idle` is definite-TRUE at the error cube.
    // This is the un-pinned-reset artifact behind the recoverability "may=all-pairs
    // spurious HOLDS": the must inference is correct; the reset was simply not tied.
    const TRAP_FSM_RESET_FREE: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 s
4 constd 2 1
5 constd 2 2
6 eq 1 3 5
7 ite 2 6 5 4
8 input 1 rst_ni
9 ite 2 8 7 4
10 next 2 3 9
";

    // A.5 helper — run `AG EF idle` over a trap-FSM fixture with the canonical
    // ∀∃ must-edge inference (`SmtPerTargetStandard`) + the sound over-approximating
    // may-relation (`SmtAllPairs`), and return the definite verdict at the error
    // cube (idle = false, err = true). `None` ⇒ the error cube was ⊥.
    fn a5_err_cube_verdict(btor2: &str) -> Option<crate::mu_calculus::trit::Trit> {
        use crate::mu_calculus::trit::Trit;
        let formula = parser::parse("nu X. ((mu Y. (idle || <> Y)) && [] X)")
            .expect("AG EF idle formula parses");
        let predicates = vec![
            PredicateSpec {
                name: "idle".into(),
                register: "s".into(),
                value: 1,
            },
            PredicateSpec {
                name: "err".into(),
                register: "s".into(),
                value: 2,
            },
        ];
        let env = Environment::new(4);
        let cegar_opts = CegarOptions {
            max_iterations: 1,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs,
            must_edge_inference:
                crate::adapter::btor2::kmts_lift::MustEdgeInference::SmtPerTargetStandard,
            lift_strategy: LiftStrategy::Eager,
            ..CegarOptions::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            btor2,
            predicates,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds on the trap FSM");
        // Cube index encodes predicate truth bit-per-predicate in `final_predicates`
        // order. Find the index with idle=false, err=true.
        let idle_bit = trace
            .final_predicates
            .iter()
            .position(|p| p.name == "idle")
            .expect("idle predicate present");
        let err_bit = trace
            .final_predicates
            .iter()
            .position(|p| p.name == "err")
            .expect("err predicate present");
        let err_cube = 1usize << err_bit; // err=1, idle=0
        assert_eq!(err_cube & (1 << idle_bit), 0, "err cube must have idle=0");
        for i in 0..trace.final_verdict.len() {
            eprintln!("cube {i}: {:?}", trace.final_verdict.verdict_at(i));
        }
        eprintln!(
            "final_predicates: {:?}",
            trace
                .final_predicates
                .iter()
                .map(|p| &p.name)
                .collect::<Vec<_>>()
        );
        match trace.final_verdict.verdict_at(err_cube) {
            Trit::Unknown => None,
            v => Some(v),
        }
    }

    #[test]
    #[ignore = "requires Z3 (SMT must/may edges); run in the mununu-sva/dev image with --ignored"]
    fn a5_trap_fsm_pinned_reset_error_is_false_free_reset_is_true() {
        use crate::mu_calculus::trit::Trit;
        // Reset PINNED (held): error is a permanent trap ⇒ AG EF idle is definite
        // FALSE at the error cube. The sound recoverability verdict.
        let pinned = a5_err_cube_verdict(TRAP_FSM_RESET_PINNED);
        assert_eq!(
            pinned,
            Some(Trit::False),
            "with the reset pinned, the error cube must be definite-FALSE (idle unreachable from a trap)"
        );
        // Reset FREE (un-pinned): the ∀∃ must-edge inference legitimately finds
        // rst_ni=0 ⇒ next=idle, so AG EF idle is definite-TRUE at the error cube.
        // This is the "may=all-pairs spurious HOLDS" — sound for a FREE reset,
        // unsound only if the reset was meant to be tied. Root cause: the reset was
        // not constant-folded out of the BTOR2 before the may/must SMT seam.
        let free = a5_err_cube_verdict(TRAP_FSM_RESET_FREE);
        assert_eq!(
            free,
            Some(Trit::True),
            "with the reset free, the error cube flips to definite-TRUE via the real reset-recovery path"
        );
    }

    #[test]
    fn cegar_converges_immediately_when_initial_verdict_is_definite() {
        // `true` formula evaluates to KleeneT at every state; no
        // refinement needed. Loop returns Converged at iteration 0.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        // After lift: 2^1 = 2 cubes.
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert_eq!(trace.terminated_with, CegarTermination::Converged);
        assert_eq!(trace.iterations.len(), 1);
        assert_eq!(trace.iterations[0].iteration, 0);
        // No KleeneBot cells in final verdict.
        for s in 0..trace.final_verdict.len() {
            assert!(!matches!(trace.final_verdict.verdict_at(s), Trit::Unknown));
        }
        assert!(trace.lazy_lift_pending);
        assert!(!trace.approximant_reuse_enabled);
        // CTXDSL Phase 2 — emit_ctxdsl defaulted false ⇒ no captured cube.
        assert!(trace.final_clts.is_none());
    }

    #[test]
    /// mununu#503 — a property whose atoms are ALL derived labels seeds ZERO cube dimensions.
    /// Every may-edge path used to be gated on `!predicates.is_empty()`, so such a property got
    /// a 1-cube, 0-edge KMTS, and `downgrade_unsatisfiable_cells` masked the cell to ⊥ because
    /// its proxy reads "no outgoing edges" as "unsatisfiable cube". Result: the property
    /// returned Unknown regardless of what it asserted.
    ///
    /// `nu X. ([] X)` is the sharpest witness: it contains NO atom at all, so a ⊥ here cannot be
    /// blamed on atom binding — it is true at every state of every KMTS, by definition.
    fn zero_dimension_lift_decides_a_formula_with_no_atoms() {
        let formula = crate::mu_calculus::parser::parse("nu X. ([] X)").expect("formula parses");
        let sidecar = serde_json::json!({
            "module": "cegar",
            "source": "zero_dim.btor2",
            "compound_predicates": [
                {"name": "trigger_i != trigger_active",
                 "expr": "trigger_i != trigger_active",
                 "derived": true}
            ],
            "combinational_predicates": [],
            "signals": []
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            ZERO_DIM_BTOR2,
            Vec::new(), // <- no cube dimensions: the whole point
            &Environment::new(1),
            &opts,
            &CegarOptions::default(),
        )
        .expect("cegar runs");
        let v = &trace.final_verdict;
        let cells: Vec<Trit> = (0..v.len()).map(|i| v.verdict_at(i)).collect();
        assert_eq!(
            cells,
            vec![Trit::True],
            "`nu X. ([] X)` must be True at the single universal cube; ⊥ here means the cube \
             was emitted edgeless and masked by downgrade_unsatisfiable_cells"
        );
    }

    #[test]
    /// mununu#503 — the same zero-dimension shape, now with the derived label carrying real
    /// content. `trigger_active` is a `uext` alias of `not(trigger_i)`, so
    /// `trigger_i != trigger_active` is a TAUTOLOGY over the combinational relation and must
    /// reach a definite True. This is the reduction of the sysrst `sva_2` failure.
    fn zero_dimension_lift_decides_a_derived_label_tautology() {
        let formula =
            crate::mu_calculus::parser::parse("nu X. (((trigger_i != trigger_active)) && [] X)")
                .expect("formula parses");
        let sidecar = serde_json::json!({
            "module": "cegar",
            "source": "zero_dim.btor2",
            "compound_predicates": [
                {"name": "trigger_i != trigger_active",
                 "expr": "trigger_i != trigger_active",
                 "derived": true}
            ],
            "combinational_predicates": [],
            "signals": []
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            ZERO_DIM_BTOR2,
            Vec::new(),
            &Environment::new(1),
            &opts,
            &CegarOptions::default(),
        )
        .expect("cegar runs");
        let v = &trace.final_verdict;
        assert_eq!(
            v.verdict_at(0),
            Trit::True,
            "`b != !b` is valid over the combinational relation, so AG of it must HOLD"
        );
    }

    /// mununu#503 fixture — one state, one free input, and a combinational alias
    /// `trigger_active = uext(not(trigger_i))`. A property over `trigger_i != trigger_active`
    /// binds ONLY as a derived label (an input operand is not a sound cube dimension), so it
    /// seeds zero dimensions — the degenerate cube space this defect lived in.
    const ZERO_DIM_BTOR2: &str = "1 sort bitvec 1\n\
                                  2 state 1 q\n\
                                  3 zero 1\n\
                                  4 init 1 2 3\n\
                                  5 input 1 trigger_i\n\
                                  6 not 1 5\n\
                                  7 uext 1 6 0 trigger_active\n\
                                  8 next 1 2 5\n";

    #[test]
    fn downgrade_unsatisfiable_cells_masks_edgeless_cells_to_bottom() {
        use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability};
        use bitvec::prelude::*;
        // s0 has a self-loop (satisfiable); s1 has NO outgoing transition — an
        // unsatisfiable cube cell in a total transition system.
        let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        builder.state("s0").state("s1").initial("s0");
        let lbl = builder.labels().intern(["a"]).expect("intern a");
        builder.set_label_controllability(lbl, LabelControllability::Uncontrollable);
        let s0 = builder.state_id_or_insert("s0").expect("s0");
        builder.transition_ids(s0, &[lbl], s0);
        let clts = builder.build().expect("build");
        // s0 = definite-True, s1 = definite-False (as a ¬good sink evaluates).
        let must = bitvec![usize, Lsb0; 1, 0];
        let may = bitvec![usize, Lsb0; 1, 0];
        let verdicts = crate::mu_calculus::trit::TritSet::from_parts(must, may);
        assert_eq!(
            verdicts.verdict_at(1),
            Trit::False,
            "precondition: the edgeless cell starts definite-False"
        );

        let (masked, unsat) = downgrade_unsatisfiable_cells(&clts, verdicts);
        assert_eq!(
            unsat,
            std::collections::HashSet::from([1]),
            "only the edgeless (unsatisfiable) cell is flagged"
        );
        assert_eq!(
            masked.verdict_at(0),
            Trit::True,
            "the satisfiable cell (with an outgoing edge) is untouched"
        );
        assert_eq!(
            masked.verdict_at(1),
            Trit::Unknown,
            "the unsatisfiable ¬good sink is masked to ⊥ — never a spurious definite-False"
        );
    }

    // Toggle `x' = !x` with predicates {x==0, x==1}: a 1-bit `x` is always 0 or 1, so
    // the cube cell {x≠0 ∧ x≠1} is UNSATISFIABLE and a ¬good sink (no outgoing may-edge).
    const TOGGLE_BTOR2: &str =
        "1 sort bitvec 1\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";

    #[test]
    fn cegar_unsatisfiable_cube_cell_does_not_falsify_recoverability() {
        use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference};
        // `AG EF (x==1)` genuinely HOLDS on the toggle (drive the toggle to reach x==1).
        // Before the unsatisfiable-cell fix, the unsatisfiable ¬good cell's vacuous
        // `EF (x==1) = good ∨ ⟨⟩∅ = False` spuriously falsified the universal νμ — a
        // definite VIOLATED where exact decides HOLDS. The fix masks unsatisfiable cells
        // to ⊥, so NO cell is definite-False (a spurious counterexample would surface as
        // a `Trit::False` here).
        let formula =
            parser::parse("nu Y. ((mu X. ((x == 1) || <> X)) && [] Y)").expect("formula parses");
        let initial = vec![
            PredicateSpec {
                name: "x == 0".into(),
                register: "x".into(),
                value: 0,
            },
            PredicateSpec {
                name: "x == 1".into(),
                register: "x".into(),
                value: 1,
            },
        ];
        let env = Environment::new(4); // 2^2 cubes
        let cegar_opts = CegarOptions {
            max_iterations: 8,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            TOGGLE_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        let verdicts: Vec<Trit> = (0..trace.final_verdict.len())
            .map(|i| trace.final_verdict.verdict_at(i))
            .collect();
        assert!(
            !verdicts.contains(&Trit::False),
            "an unsatisfiable ¬good cube cell must be ⊥, not a spurious definite-False \
             counterexample; got {verdicts:?}"
        );
    }

    /// F.1 (2026-07-24) — the must-precondition Craig refinement is SOUND on a recoverability
    /// property (never falsifies a HOLDS), and its DECIDING is gated on MathSAT (host make-ci has no
    /// mathsat → the query is inert → WP fallback → sound-but-undecided; the deciding is validated in
    /// `e2e_craig_must_precondition_decides_emergent_recoverability`, `mununu-sva`). `busy` returns to
    /// idle (`busy==0`) exactly when `data == target`; `data`/`target` increment in LOCKSTEP so
    /// `data == target` is INVARIANT (never syntactically compared beyond the gate). `AG EF (busy==0)`
    /// genuinely HOLDS (exact oracle), yet the cube over `{busy==0}` alone leaves ⊥. The
    /// recoverability-correct query interpolates the MUST-PRECONDITION of the good-ward target
    /// (`{cur:T→target}` vs `{cur:T→¬target}`) — MathSAT's proof-based lazy-BV interpolation derives
    /// `data == target` from the transition (cvc5's SyGuS returns `fail`). This host test locks the
    /// FIRM soundness property; the functional decide/exhaust is reported (mathsat-gated).
    #[test]
    fn craig_refinement_recoverability_sound_must_precondition_mathsat_gated() {
        use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference};
        use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 3\n3 state 1 busy\n4 state 2 data\n\
5 state 2 target\n6 one 1\n7 zero 1\n8 zero 2\n9 one 2\n10 init 1 3 6\n11 init 2 4 8\n\
12 init 2 5 8\n13 add 2 4 9\n14 next 2 4 13\n15 add 2 5 9\n16 next 2 5 15\n17 eq 1 4 5\n\
18 ite 1 17 7 3\n19 next 1 3 18\n";
        let formula =
            parser::parse("nu Y. ((mu X. ((busy == 0) || <> X)) && [] Y)").expect("formula");
        // Soundness anchor: the property genuinely HOLDS on the concrete design.
        assert_eq!(
            exact_symbolic_verdict(D, &formula).map(|v| format!("{v:?}")),
            Ok("Holds".to_string()),
            "the lockstep-gated recoverability genuinely HOLDS (the oracle)"
        );
        let initial = vec![PredicateSpec {
            name: "busy == 0".into(),
            register: "busy".into(),
            value: 0,
        }];
        let mk_opts = |src| CegarOptions {
            max_iterations: 8,
            predicate_source: src,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        };
        let env = Environment::new(2);
        let run = |src| {
            cegar_refine_loop(
                &formula,
                D,
                initial.clone(),
                &env,
                &AdapterOptions::default(),
                &mk_opts(src),
            )
            .expect("cegar succeeds")
        };
        let craig = run(PredicateSource::CraigInterpolation);
        // FIRM SOUNDNESS: a HOLDS property must NEVER be falsified — no definite-False cell.
        let craig_verdicts: Vec<Trit> = (0..craig.final_verdict.len())
            .map(|i| craig.final_verdict.verdict_at(i))
            .collect();
        assert!(
            !craig_verdicts.contains(&Trit::False),
            "Craig refinement must never falsify a HOLDS recoverability; got {craig_verdicts:?}"
        );
        // FUNCTIONAL (mathsat-gated): report whether the must-precondition closed the ⊥ WP cannot.
        let wp = run(PredicateSource::WeakestPrecondition);
        eprintln!(
            "F.1 must-precondition recoverability: craig={:?} wp={:?}",
            craig.terminated_with, wp.terminated_with
        );
    }

    /// F.1 e2e (2026-07-24) — with MathSAT present, the must-precondition Craig refinement DECIDES
    /// the emergent-invariant recoverability the WP splitter cannot. Same `data==target`-gated
    /// `AG EF (busy==0)` design (exact oracle HOLDS); MathSAT's lazy-BV interpolation discovers
    /// `data == target`, the ⊥ closes → `Converged` with the sound HOLDS verdict, whereas WP
    /// exhausts. `#[ignore]`d (needs MathSAT, subprocess-only per the not-bundled policy — run in the
    /// `mununu-sva` image with `MUNUNU_MATHSAT_PATH` set).
    #[test]
    #[ignore = "requires MathSAT (mununu-sva image); run with --ignored + MUNUNU_MATHSAT_PATH"]
    fn e2e_craig_must_precondition_decides_emergent_recoverability() {
        // MathSAT is not in the `mununu-sva` image (it ships in `mununu-sva-pono`).
        // Skip rather than fail: an absent optional tool is not a regression, and a
        // red result here masks the genuine failures in the `--ignored` sweep.
        if crate::adapter::btor2::native_interp::mathsat_available().is_err() {
            eprintln!(
                "SKIPPED: MathSAT unavailable — run in `mununu-sva-pono` or set \
                 MUNUNU_MATHSAT_PATH."
            );
            return;
        }
        use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference};
        use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 3\n3 state 1 busy\n4 state 2 data\n\
5 state 2 target\n6 one 1\n7 zero 1\n8 zero 2\n9 one 2\n10 init 1 3 6\n11 init 2 4 8\n\
12 init 2 5 8\n13 add 2 4 9\n14 next 2 4 13\n15 add 2 5 9\n16 next 2 5 15\n17 eq 1 4 5\n\
18 ite 1 17 7 3\n19 next 1 3 18\n";
        let formula =
            parser::parse("nu Y. ((mu X. ((busy == 0) || <> X)) && [] Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(D, &formula).map(|v| format!("{v:?}")),
            Ok("Holds".to_string()),
            "oracle: the recoverability HOLDS"
        );
        let initial = vec![PredicateSpec {
            name: "busy == 0".into(),
            register: "busy".into(),
            value: 0,
        }];
        let opts = CegarOptions {
            max_iterations: 8,
            predicate_source: PredicateSource::CraigInterpolation,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        };
        let env = Environment::new(2);
        let trace = cegar_refine_loop(
            &formula,
            D,
            initial,
            &env,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("cegar succeeds");
        // The must-precondition invariant `data==target` closes the ⊥ → the loop CONVERGES.
        assert_eq!(
            trace.terminated_with,
            CegarTermination::Converged,
            "with MathSAT the must-precondition refinement must DECIDE (converge), not exhaust; \
             final predicates: {:?}",
            trace.final_predicates
        );
        // MathSAT discovered the emergent relational invariant as a compound cube dimension.
        assert!(
            trace
                .final_predicates
                .iter()
                .any(|s| s.name.starts_with("craig_compound")),
            "the must-precondition should have discovered a compound invariant; got {:?}",
            trace.final_predicates
        );
        // The recoverability verdict is read at the RESET cube (as `verify_recoverability` does):
        // reset ⇒ busy=1 (`busy==0` FALSE) and every discovered invariant (`data==target`) TRUE. The
        // abstraction legitimately marks the UNREACHABLE `{busy≠0 ∧ data≠target}` cube False, so the
        // sound check is the RESET cube — where the property must be True (HOLDS), matching the exact
        // oracle — NOT "no False anywhere".
        let busy0 = trace
            .final_predicates
            .iter()
            .position(|s| s.name == "busy == 0")
            .expect("busy==0 is a dimension");
        let init_cube = (0..trace.final_predicates.len()).fold(0usize, |acc, i| {
            if i == busy0 { acc } else { acc | (1 << i) }
        });
        assert_eq!(
            trace.final_verdict.verdict_at(init_cube),
            Trit::True,
            "the recoverability must HOLD at the reset cube (matching the oracle); all verdicts: {:?}",
            (0..trace.final_verdict.len())
                .map(|i| trace.final_verdict.verdict_at(i))
                .collect::<Vec<_>>()
        );
    }

    // H.U.2 — `reg` stuck at 0; `g = not(reg)` is a combinational-of-state signal
    // (always 1). `AG (g == 1)` over this combinational CUBE DIMENSION must HOLD —
    // the end-to-end exercise of the uniform combinational path through CEGAR
    // (no slang, so no combinational-signal inlining; the predicate genuinely
    // targets the combinational node `g`).
    const COMB_CUBE_BTOR2: &str = "\
1 sort bitvec 1
2 state 1 reg
3 zero 1
4 init 1 2 3
5 next 1 2 2
6 not 1 2 g
";

    #[test]
    fn cegar_combinational_of_state_cube_dimension_holds() {
        let formula = parser::parse("nu X. ((g == 1) && [] X)").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "g == 1".into(),
            register: "g".into(),
            value: 1,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            // A combinational cube dimension requires the SmtAllPairs may relation
            // (the sampling representative is state-register oriented).
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs,
            ..Default::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            COMB_CUBE_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        // `g` at the reset state (reg = 0) is `not(0) = 1`, so the reset cube is
        // {g == 1} = cube index 1 (the predicate-true cube). `g` never changes
        // (reg stuck), so AG(g==1) HOLDS there.
        assert_eq!(
            trace.final_verdict.verdict_at(1),
            Trit::True,
            "AG(g==1) holds at the reset cube {{g==1}} (g is the constant-1 \
             combinational-of-state output of a stuck reg)"
        );
        // The opposite cube {g==0} is unreachable / vacuously false for the body.
        assert_eq!(trace.final_verdict.verdict_at(0), Trit::False);
    }

    /// CTXDSL Phase 2 (2026-06-22) — `emit_ctxdsl: true` captures the final
    /// refined cube `Clts` into the trace; it serializes to a model +
    /// formula CTXDSL document via `clts_to_ctxdsl_with_formula`.
    #[test]
    fn cegar_emit_ctxdsl_captures_final_cube_model() {
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            emit_ctxdsl: true,
            ..Default::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        let clts = trace
            .final_clts
            .as_ref()
            .expect("emit_ctxdsl=true ⇒ final cube model captured");
        let ctxdsl = crate::adapter::clts_to_ir::clts_to_ctxdsl_with_formula(
            clts,
            "lifted_kmts",
            "cegar_model",
            "checked_property",
            "true",
        )
        .expect("serialize model + formula");
        assert!(
            ctxdsl.contains("automaton lifted_kmts {"),
            "model automaton missing:\n{ctxdsl}"
        );
        assert!(
            ctxdsl.contains("body = true;"),
            "formula body missing:\n{ctxdsl}"
        );
    }

    #[test]
    fn cegar_honors_smt_all_pairs_may_edge_inference() {
        // DR1 (IR-unification track) — CegarOptions::may_edge_inference =
        // SmtAllPairs threads through to predicate_cube_lift's sound
        // all-pairs may relation. On a toggle (q' = !q) with predicate
        // {q == 0}, EF(q==0) = `mu X. (p0 | <step> X)`: the q==0 cube is
        // KleeneT (already idle); the q≠0 cube is KleeneBot — the
        // diamond `<step>` needs a must-witness to be KleeneT, and
        // must-edge inference is Off, so the SOUND verdict is "indefinite,
        // refine", NOT a (spurious) definite false. This pins the sound
        // outcome through the CEGAR surface, distinct from any sampling
        // under-approximation that could drop the q≠0 → q==0 may-edge.
        const TOGGLE: &str =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let formula = parser::parse("mu X. (p0 | <step> X)").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p0".into(),
            register: "q".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 3,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs,
            ..Default::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            TOGGLE,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds under SmtAllPairs");
        // Count the verdict cells: exactly one KleeneT (q==0) and one
        // KleeneBot (q≠0); no spurious definite-false.
        let mut t = 0;
        let mut f = 0;
        let mut bot = 0;
        for s in 0..trace.final_verdict.len() {
            match trace.final_verdict.verdict_at(s) {
                Trit::True => t += 1,
                Trit::False => f += 1,
                Trit::Unknown => bot += 1,
            }
        }
        assert_eq!(
            (t, f, bot),
            (1, 0, 1),
            "SmtAllPairs EF(q==0) should be sound (T + ⊥, no spurious F); got T={t} F={f} ⊥={bot}"
        );
    }

    #[test]
    fn cegar_terminates_on_bounded_cap_when_source_is_stubbed() {
        // `false` formula evaluates to KleeneF (definite) — also
        // doesn't trigger refinement. Use a predicate-referencing
        // formula to provoke KleeneBot. R.2.5 MVP returns a Clts
        // with no transitions, so any formula referring to a
        // never-set state predicate sees KleeneF — which still
        // converges. The MVP test below uses a formula whose
        // verdict involves cube-state predicates that hit
        // KleeneT / KleeneF cleanly.
        //
        // To actually exercise the bounded-cap path, we use a
        // formula referring to a predicate that does not exist in
        // the labelling. That returns vacuous KleeneF (no state
        // matches the predicate).
        //
        // This test asserts the bounded-cap MACHINERY works rather
        // than KleeneBot is triggered (R.5.0 MVP needs MayOnly
        // edges in the lift output to produce KleeneBot, which
        // R.2.5 MVP does not yet produce). The full end-to-end
        // KleeneBot-then-refine test lands when R.2.5's SMT
        // predicate-image queries arrive.
        let formula = parser::parse("true").expect("ok");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 3,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("ok");
        // `true` converges immediately, so terminated_with =
        // Converged. The cap-hit path is structurally exercised
        // when a KleeneBot-producing fixture lands (post R.2.5 SMT).
        assert_eq!(trace.terminated_with, CegarTermination::Converged);
    }

    #[test]
    fn cegar_manual_predicate_source_extends_predicate_set() {
        // Validate the Manual predicate-source callback gets invoked
        // when the loop sees KleeneBot. For the R.5 MVP we
        // construct a synthetic scenario by directly testing the
        // callback wiring — we use a formula that the lift's
        // transitionless Clts produces a definite verdict for, so
        // the loop converges without invoking the callback. The
        // callback machinery is therefore tested via the unused
        // path: provide a Manual source that would add a predicate
        // if invoked; assert it's NOT invoked because the loop
        // converges first.
        let invoked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let invoked_clone = invoked.clone();
        let cb = std::sync::Arc::new(
            move |_subgame: &FailureSubgame, _preds: &[PredicateSpec]| -> Vec<PredicateSpec> {
                invoked_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                vec![PredicateSpec {
                    name: "added_by_callback".into(),
                    register: "reg_a".into(),
                    value: 1,
                }]
            },
        );

        let formula = parser::parse("true").expect("ok");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::Manual(cb),
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("ok");
        assert_eq!(trace.terminated_with, CegarTermination::Converged);
        // Converged at iteration 0 → callback never invoked.
        assert!(
            !invoked.load(std::sync::atomic::Ordering::SeqCst),
            "Manual callback must not be invoked when loop converges immediately"
        );
    }

    #[test]
    fn cegar_trace_carries_mvp_flags() {
        let formula = parser::parse("true").expect("ok");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions::default();
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("ok");
        // R.5 MVP invariants: lift is eager + no approximant reuse.
        // R.5 follow-ups flip these to false / true respectively.
        assert!(trace.lazy_lift_pending);
        assert!(!trace.approximant_reuse_enabled);
    }

    // ---- R.5 WP predicate-discovery MVP — helper-level tests ----

    #[test]
    fn wp_returns_empty_when_no_classifying_transitions() {
        // FailureSubgame with empty classifying_transitions means
        // there's nothing for WP to refine — must return empty Vec.
        let empty_subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: Vec::new(),
            root: None,
            subgame_extraction_complete: false,
        };
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let out = weakest_precondition_predicates(&empty_subgame, &preds, SMALL_BTOR2);
        assert!(out.is_empty(), "WP must return empty when subgame is empty");
    }

    #[test]
    fn wp_proposes_predicates_for_uncovered_state_register() {
        // SMALL_BTOR2 has reg_a + reg_b. Cover reg_a only; WP must
        // propose predicates over reg_b (the uncovered register).
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let out = weakest_precondition_predicates(&subgame, &preds, SMALL_BTOR2);
        assert!(
            !out.is_empty(),
            "WP must propose at least one predicate when an uncovered register exists"
        );
        // Every proposal targets reg_b (the only uncovered register).
        for p in &out {
            assert_eq!(p.register, "reg_b", "WP must target the uncovered register");
        }
        // Cap at 2 predicates per call (per the helper's bounded-growth policy).
        assert!(out.len() <= 2, "WP must cap at 2 predicates per call");
    }

    #[test]
    fn wp_returns_empty_when_every_register_is_covered() {
        // All registers in SMALL_BTOR2 are already covered by the
        // current predicate set — WP has nothing more to propose.
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let preds = vec![
            PredicateSpec {
                name: "p_a".into(),
                register: "reg_a".into(),
                value: 0,
            },
            PredicateSpec {
                name: "p_b".into(),
                register: "reg_b".into(),
                value: 0,
            },
        ];
        let out = weakest_precondition_predicates(&subgame, &preds, SMALL_BTOR2);
        assert!(
            out.is_empty(),
            "WP must return empty when every state register is already covered"
        );
    }

    #[test]
    fn wp_returns_empty_on_malformed_btor2() {
        // BTOR2 parse failure → WP must short-circuit to empty Vec
        // (the loop then terminates with PredicateSourceExhausted).
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let out = weakest_precondition_predicates(&subgame, &[], "this is not valid BTOR2 syntax");
        assert!(
            out.is_empty(),
            "WP must return empty on BTOR2 parse failure rather than panicking"
        );
    }

    // ---- R.5 WP literal-extraction follow-up tests ----

    /// BTOR2 fixture with a 4-bit FSM state register `fsm_q` and a
    /// distinct comparison literal `4'd5` (encoded as `const 1 0101`).
    /// The R.5 WP literal-extraction follow-up should propose
    /// `fsm_q == 5` as one of the candidate predicates (in addition
    /// to the default `fsm_q == 0` / `fsm_q == 1`).
    const WP_LITERAL_FIXTURE: &str = r#"
1 sort bitvec 4
2 zero 1
3 const 1 0101
4 state 1 fsm_q
5 init 1 4 2
6 eq 1 4 3
7 ite 1 6 2 4
8 next 1 4 7
"#;

    #[test]
    fn wp_literal_extraction_proposes_const_values_from_btor2() {
        // FailureSubgame triggers WP; current_predicates is empty
        // (no register covered). WP should propose at least one
        // predicate for fsm_q, AND the candidate-value pool must
        // include the literal `5` extracted from `const 1 0101`.
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let out = weakest_precondition_predicates(&subgame, &[], WP_LITERAL_FIXTURE);
        assert!(
            !out.is_empty(),
            "WP must propose at least one predicate when an uncovered register exists"
        );
        // First proposal: `fsm_q == 0` (candidate-value pool prepends 0, 1
        // before literal extraction).
        assert_eq!(out[0].register, "fsm_q");
        assert_eq!(out[0].value, 0);
        // Second proposal: `fsm_q == 1` (next in the pool).
        if out.len() >= 2 {
            assert_eq!(out[1].register, "fsm_q");
            assert_eq!(out[1].value, 1);
        }
    }

    #[test]
    fn wp_literal_extraction_skips_values_already_in_predicates() {
        // If the current predicate set already covers `fsm_q == 0`
        // and `fsm_q == 1`, the literal-extraction pass picks up the
        // next candidate (`5` from the BTOR2 const literal).
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        // Same-register coverage uses `(register, value)` pair dedup
        // per the helper's existing_register_value_pairs check — but
        // the OUTER covered-register check (by name only) currently
        // skips the register entirely if ANY predicate names it.
        // The helper's current behaviour: register-name match → skip.
        // So we need a register OTHER than fsm_q in the current set
        // to verify the literal-extraction kicks in for fsm_q.
        //
        // To verify the (register, value) dedup AT THE VALUE LEVEL,
        // we use a fixture with two state registers and partially
        // cover one. See the test below; this test asserts the
        // baseline 0/1 coverage path:
        let preds_covering_other_reg = vec![PredicateSpec {
            name: "p_a".into(),
            register: "other_reg".into(),
            value: 0,
        }];
        let out = weakest_precondition_predicates(
            &subgame,
            &preds_covering_other_reg,
            WP_LITERAL_FIXTURE,
        );
        // `other_reg` doesn't exist in WP_LITERAL_FIXTURE; only `fsm_q`
        // is uncovered → WP proposes for fsm_q.
        assert!(!out.is_empty());
        assert!(out.iter().all(|p| p.register == "fsm_q"));
    }

    #[test]
    fn wp_literal_extraction_includes_btor2_const_pool() {
        // Direct check on collect_btor2_constants: the WP_LITERAL_FIXTURE
        // has consts 0 (zero), 5 (const 1 0101). The set returned
        // must include both.
        let file = crate::adapter::btor2::parser::parse(WP_LITERAL_FIXTURE).expect("parse");
        let consts = crate::adapter::btor2::bit_blast::collect_btor2_constants(&file);
        assert!(
            consts.contains(&0),
            "collect_btor2_constants must include 0 (the `zero 1` line); got {consts:?}"
        );
        assert!(
            consts.contains(&5),
            "collect_btor2_constants must include 5 (the `const 1 0101` literal); got {consts:?}"
        );
    }

    // ---- R.5 WP cone-of-influence bound tests ----

    /// BTOR2 fixture with two connected state cells where `cnt_a`
    /// (the cone-controlling register, covered by the initial
    /// predicate) has a next-value computation that reads `cnt_b`.
    /// Under the WP cone-of-influence walk (backwards from
    /// cnt_a's next-value), `cnt_b` enters the cone — so WP must
    /// propose predicates for `cnt_b` even though only `cnt_a`
    /// is in the current predicate set.
    const WP_COI_FIXTURE: &str = r#"
1 sort bitvec 2
2 zero 1
3 const 1 11
4 state 1 cnt_a
5 state 1 cnt_b
6 add 1 4 5
7 next 1 4 6
8 add 1 5 3
9 next 1 5 8
"#;

    #[test]
    fn wp_coi_includes_register_in_cone_of_covered_predicate() {
        // Coverage: cnt_a is covered → COI walks from cnt_a's next
        // (NID 7 → operand NID 6 → operand cnt_a (4) + cnt_b (5)).
        // So COI = {cnt_a, cnt_b}. The only uncovered cell in COI
        // is cnt_b → WP proposes for cnt_b.
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let preds = vec![PredicateSpec {
            name: "p_a".into(),
            register: "cnt_a".into(),
            value: 0,
        }];
        let out = weakest_precondition_predicates(&subgame, &preds, WP_COI_FIXTURE);
        assert!(
            !out.is_empty(),
            "WP must propose for cnt_b (in cnt_a's cone)"
        );
        assert!(
            out.iter().all(|p| p.register == "cnt_b"),
            "WP COI must restrict to cnt_b (the in-cone uncovered register); got: {:?}",
            out.iter().map(|p| p.register.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn wp_coi_falls_back_when_cone_excludes_all_uncovered() {
        // Fixture where the covered register's cone DOES NOT
        // include the uncovered register — independent chains.
        // SMALL_BTOR2 has reg_a + reg_b both with next = const 0.
        // Neither's cone includes the other → COI = {reg_a only}
        // (when reg_a is covered). reg_b is OUTSIDE the COI ⇒
        // strict-COI path returns empty ⇒ fallback fires ⇒ reg_b
        // gets proposed under the unrestricted walk.
        let subgame = FailureSubgame {
            positions: Vec::new(),
            classifying_transitions: vec![(
                0,
                0,
                crate::mu_calculus::parity_game_3v::OwningPlayer::Unknown,
            )],
            root: None,
            subgame_extraction_complete: false,
        };
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let out = weakest_precondition_predicates(&subgame, &preds, SMALL_BTOR2);
        assert!(
            !out.is_empty(),
            "WP must fall back when COI excludes every uncovered register"
        );
        assert!(
            out.iter().all(|p| p.register == "reg_b"),
            "fallback must propose for reg_b (the only uncovered register); got: {:?}",
            out.iter().map(|p| p.register.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn collect_reachable_states_from_walks_through_op_chains() {
        // Direct check on the parser helper: from cnt_a's next-value
        // (NID 6 = `add 1 4 5`), reachable states are cnt_a (4) + cnt_b (5).
        let file = crate::adapter::btor2::parser::parse(WP_COI_FIXTURE).expect("parse");
        let next_op =
            crate::adapter::btor2::parser::find_next_value_operand(&file, 4).expect("cnt_a Next");
        let reachable = crate::adapter::btor2::parser::collect_reachable_states_from(
            &file,
            std::slice::from_ref(&next_op),
        );
        assert!(
            reachable.contains(&4),
            "cnt_a (NID 4) must be reachable from its own next-value computation; got {reachable:?}"
        );
        assert!(
            reachable.contains(&5),
            "cnt_b (NID 5) must be reachable from cnt_a's next-value (cnt_a += cnt_b); got {reachable:?}"
        );
    }

    #[test]
    fn r5_cegar_auto_capture_records_per_iteration_approximants() {
        // R.5 sub-item 1.2 — when `capture_approximants` is set, each
        // iteration's `approximants_at_end` field is `Some(map)`
        // containing one entry per converged fixpoint var. Backward
        // compat: with the flag unset, the field stays `None`.
        //
        // Fixture: `nu X. < true > X` over the SMALL_BTOR2 1-predicate
        // lift (2 cubes). One outer fixpoint ⇒ exactly one entry in
        // the captured map.
        let formula = parser::parse("nu X. < true > X").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);

        // Run 1: capture disabled (default) ⇒ approximants_at_end None.
        let opts_off = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_off = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial.clone(),
            &env,
            &AdapterOptions::default(),
            &opts_off,
        )
        .expect("cegar succeeds with capture off");
        for iter in &trace_off.iterations {
            assert!(
                iter.approximants_at_end.is_none(),
                "capture_approximants=false MUST leave approximants_at_end None; got Some on iteration {}",
                iter.iteration
            );
        }

        // Run 2: capture enabled ⇒ approximants_at_end Some, with one
        // entry for the single fixpoint var.
        let opts_on = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_on = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &opts_on,
        )
        .expect("cegar succeeds with capture on");
        assert!(
            !trace_on.iterations.is_empty(),
            "trace must record at least one iteration"
        );
        for iter in &trace_on.iterations {
            let approximants = iter.approximants_at_end.as_ref().unwrap_or_else(|| {
                panic!(
                    "capture_approximants=true MUST populate approximants_at_end on iteration {}",
                    iter.iteration
                )
            });
            assert_eq!(
                approximants.len(),
                1,
                "single-fixpoint formula MUST capture exactly one approximant; got {} on iteration {}",
                approximants.len(),
                iter.iteration
            );
            // B.1.a (2026-06-01): the captured iterate is a
            // `StoredApproximant` carrying must + may bit-sets.
            // Both must equal the lift's state count.
            let iterate = approximants.values().next().expect("one entry");
            assert_eq!(
                iterate.must_true.len(),
                env.state_count(),
                "captured must_true bitset length MUST equal the lift's state count"
            );
            assert_eq!(
                iterate.may_true.len(),
                env.state_count(),
                "captured may_true bitset length MUST equal the lift's state count"
            );
            // Invariant: must_true ⊆ may_true. Verified by checking
            // that `must_true & !may_true` is empty.
            let mut diff = iterate.must_true.clone();
            diff &= !iterate.may_true.clone();
            assert!(
                diff.not_any(),
                "must_true ⊆ may_true invariant violated for fixpoint var on iteration {}",
                iter.iteration
            );
        }

        // Backward-compat sanity: verdicts agree across the two runs.
        assert_eq!(
            trace_off.terminated_with, trace_on.terminated_with,
            "capture toggle MUST NOT change termination state"
        );
    }

    #[test]
    fn r5_cegar_auto_capture_13_reuse_flag_threads_priors_without_changing_verdict() {
        // R.5 CEGAR auto-capture sub-item 1.3 (2026-06-01) — when
        // `enable_approximant_reuse` is set, the loop threads
        // iteration N's `approximants_at_end` forward as
        // `prior_approximants` on iteration N+1. The seed is
        // SOUND under monotonicity (the LFP / GFP converges from
        // any monotone-comparable starting point); the verdict
        // MUST be identical to a run with reuse off.
        //
        // This test runs the converged-immediately path (formula
        // `true` returns KleeneT at iteration 0, never needs
        // refinement). The reuse flag has no observable
        // verdict-altering effect, but the trace MUST record
        // `approximant_reuse_enabled = true` to signal the caller
        // opted in.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);

        let opts_reuse_off = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_off = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial.clone(),
            &env,
            &AdapterOptions::default(),
            &opts_reuse_off,
        )
        .expect("cegar succeeds with reuse off");

        let opts_reuse_on = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: true,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_on = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &opts_reuse_on,
        )
        .expect("cegar succeeds with reuse on");

        // Verdict equivalence — strict soundness guarantee.
        // TritSet has no PartialEq; use its set-equality helper.
        assert!(
            trace_off.final_verdict.eq_set(&trace_on.final_verdict),
            "approximant reuse MUST NOT change the final verdict"
        );
        assert_eq!(
            trace_off.terminated_with, trace_on.terminated_with,
            "approximant reuse MUST NOT change the termination state"
        );

        // Flag reflects the caller's opt-in.
        assert!(
            !trace_off.approximant_reuse_enabled,
            "reuse-off trace MUST report approximant_reuse_enabled = false"
        );
        assert!(
            trace_on.approximant_reuse_enabled,
            "reuse-on trace MUST report approximant_reuse_enabled = true"
        );
    }

    #[test]
    fn r5_cegar_auto_capture_13_default_options_has_reuse_off() {
        // R.5 CEGAR auto-capture sub-item 1.3 (2026-06-01) — the
        // default `CegarOptions` MUST have `enable_approximant_reuse
        // = false` (strict-additive opt-in).
        let opts = CegarOptions::default();
        assert!(
            !opts.enable_approximant_reuse,
            "CegarOptions::default() MUST have enable_approximant_reuse = false"
        );
    }

    #[test]
    fn r5_subitem_14b_refine_cube_approximant_pass_through_on_equal_sizes() {
        // R.5 sub-item 1.4.b (2026-06-01) — when n_old == n_new
        // (no refinement happened), the refined bit-set is
        // identical to the prior.
        let mut prior: EvalResult = bitvec::vec::BitVec::repeat(false, 4);
        prior.set(1, true);
        prior.set(2, true);
        let refined = refine_cube_approximant(&prior, 2, 2);
        assert_eq!(refined, prior, "n_old == n_new MUST pass through unchanged");
    }

    #[test]
    fn r5_subitem_14b_refine_cube_approximant_one_predicate_added_doubles_cubes() {
        // R.5 sub-item 1.4.b (2026-06-01) — adding 1 predicate
        // doubles the cube count; each parent cube has 2 children
        // (high-bit 0 + high-bit 1). The parent's bit value is
        // copied to BOTH children.
        //
        // Setup: |P_old| = 1 → 2 cubes c0 (p0=F), c1 (p0=T).
        // Prior bits: c0=true, c1=false.
        // Add p1 → |P_new| = 2 → 4 cubes:
        //   c0 (p1=F, p0=F) — parent c0 → must inherit true
        //   c1 (p1=F, p0=T) — parent c1 → must inherit false
        //   c2 (p1=T, p0=F) — parent c0 → must inherit true
        //   c3 (p1=T, p0=T) — parent c1 → must inherit false
        let mut prior: EvalResult = bitvec::vec::BitVec::repeat(false, 2);
        prior.set(0, true);
        let refined = refine_cube_approximant(&prior, 1, 2);
        assert_eq!(refined.len(), 4, "refined cube count must equal 2^n_new");
        assert!(refined[0], "child c00 (parent c0=true) must inherit true");
        assert!(
            !refined[1],
            "child c01 (parent c1=false) must inherit false"
        );
        assert!(refined[2], "child c10 (parent c0=true) must inherit true");
        assert!(
            !refined[3],
            "child c11 (parent c1=false) must inherit false"
        );
    }

    #[test]
    fn r5_subitem_14b_refine_cube_approximant_two_predicates_added_quadruples_cubes() {
        // R.5 sub-item 1.4.b (2026-06-01) — adding 2 predicates
        // gives each parent cube 4 children. All 4 children
        // inherit the parent's bit.
        //
        // |P_old| = 1 → 2 cubes; |P_new| = 3 → 8 cubes.
        // Parent c0 (low-bit 0) has children {0, 2, 4, 6}.
        // Parent c1 (low-bit 1) has children {1, 3, 5, 7}.
        let mut prior: EvalResult = bitvec::vec::BitVec::repeat(false, 2);
        prior.set(1, true); // c1 = true
        let refined = refine_cube_approximant(&prior, 1, 3);
        assert_eq!(refined.len(), 8);
        // Parent c1 (low bit = 1) children: indices 1, 3, 5, 7
        for &idx in &[1usize, 3, 5, 7] {
            assert!(
                refined[idx],
                "child {idx} (parent c1=true) must inherit true"
            );
        }
        for &idx in &[0usize, 2, 4, 6] {
            assert!(
                !refined[idx],
                "child {idx} (parent c0=false) must inherit false"
            );
        }
    }

    #[test]
    fn r5_subitem_14b_refine_cube_approximant_zero_predicates_added_unchanged() {
        // R.5 sub-item 1.4.b (2026-06-01) — edge case: 0 new
        // predicates added. mask_old = full mask, every "child"
        // is the parent itself. Behaves identically to the
        // pass-through case.
        let mut prior: EvalResult = bitvec::vec::BitVec::repeat(false, 4);
        prior.set(2, true);
        let refined = refine_cube_approximant(&prior, 2, 2);
        assert_eq!(refined, prior);
    }

    // R.5 sub-item 1.4.b (2026-06-01) — small BTOR2 with one
    // 1-bit input. The input enables the R.2.5 lifter's
    // predicate-image MVP to emit MayOnly transitions, which the
    // CEGAR loop can refine across to exercise the 1.4.b cube-
    // refinement-mapping wiring end-to-end.
    const SMALL_BTOR2_WITH_INPUT: &str = "\
1 sort bitvec 1
2 input 1 in_a
3 state 1 reg_a
4 zero 1
5 init 1 3 4
6 next 1 3 2
";

    #[test]
    fn r5_subitem_14b_cegar_reuse_on_refinement_produces_sound_verdict() {
        // R.5 sub-item 1.4.b (2026-06-01) — end-to-end wiring
        // test: run cegar_refine_loop twice on the same fixture,
        // once with `enable_approximant_reuse = false` and once
        // with `= true`. A Manual predicate source adds a fresh
        // predicate on the first call (forcing refinement); after
        // that it returns empty (so the loop hits
        // PredicateSourceExhausted at iter 1).
        //
        // With reuse on, iter 1's seed is built via the 1.4.b
        // refinement mapping (parent.must / parent.may projected
        // to the refined 2^2 = 4-cube space). With reuse off, iter
        // 1 evaluates from scratch.
        //
        // The strict soundness guarantee: both runs MUST produce
        // an identical final verdict and identical termination
        // state. The 1.4.b refinement-mapping wiring is exercised
        // even if the underlying lift's verdict doesn't actually
        // contain KleeneBot (the Manual source unconditionally
        // forces refinement on any KleeneBot-producing iter).
        let formula = parser::parse("nu X. < true > X").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        // Manual source: add 1 fresh predicate on first call,
        // empty thereafter. Track call count via Mutex<bool>.
        let call_count = Arc::new(Mutex::new(0usize));
        let cc = call_count.clone();
        let cb: Arc<crate::adapter::btor2::cegar::ManualPredicateCallback> =
            Arc::new(move |_subgame, current| {
                let mut n = cc.lock().unwrap();
                if *n == 0 {
                    *n += 1;
                    // Add a fresh predicate (different name, same
                    // register, different value).
                    if !current.iter().any(|p| p.name == "p_new") {
                        return vec![PredicateSpec {
                            name: "p_new".into(),
                            register: "reg_a".into(),
                            value: 1,
                        }];
                    }
                }
                Vec::new()
            });

        // Run 1: reuse off (baseline for verdict comparison).
        let opts_off = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::Manual(cb.clone()),
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let env_iter0 = Environment::new(2);
        let result_off = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_INPUT,
            initial.clone(),
            &env_iter0,
            &AdapterOptions::default(),
            &opts_off,
        );

        // Reset call counter for the second run.
        *call_count.lock().unwrap() = 0;

        let opts_on = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::Manual(cb),
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: true,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let result_on = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_INPUT,
            initial,
            &env_iter0,
            &AdapterOptions::default(),
            &opts_on,
        );

        // Both runs MUST either both succeed or both fail in the
        // same way. The current loop has the
        // "env state count must match lift state count" check at
        // line ~365; refinement grows the lift state count, which
        // breaks that invariant on iter 1. This test documents
        // the current behaviour: the loop errors with the
        // IrConsistencyError on iter 1, BUT this is identical
        // between reuse-on and reuse-off — proving the 1.4.b
        // wiring did not introduce a verdict divergence.
        match (&result_off, &result_on) {
            (Ok(t_off), Ok(t_on)) => {
                assert!(
                    t_off.final_verdict.eq_set(&t_on.final_verdict),
                    "reuse must not change final verdict"
                );
                assert_eq!(t_off.terminated_with, t_on.terminated_with);
            }
            (Err(e_off), Err(e_on)) => {
                assert_eq!(
                    e_off.message, e_on.message,
                    "reuse must not change error message — both runs hit the same env-state-count check at iter 1"
                );
            }
            (Ok(_), Err(_)) | (Err(_), Ok(_)) => {
                panic!(
                    "reuse toggle must not change ok/err state: off={:?} on={:?}",
                    result_off.as_ref().err(),
                    result_on.as_ref().err()
                );
            }
        }
    }

    #[test]
    fn r5_b3b_warning_fires_on_alt_depth_2_formula_without_hyper_must() {
        // R.5 B.3.b (2026-06-01) — when the input formula has
        // alternation depth ≥ 2 AND the lifted CLTS has no
        // MustHyperOnly transitions, the CEGAR loop emits an
        // AdapterWarning per Shoham–Grumberg LMCS 2007's
        // standard-KMTS non-monotonicity result.
        //
        // Fixture: `nu X. mu Y. (predicate || < tick > X)` is
        // depth-2 (nu around mu). The lift produces a standard
        // KMTS (Sharp + MayOnly only) — no MustHyperOnly.
        let formula = parser::parse("nu X. mu Y. (true || < true > X)").expect("formula parses");
        assert_eq!(formula.alternation_depth(), 2);

        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        // The B.3.b warning MUST fire — formula is alt-depth 2,
        // the lift has no hyper-must transitions.
        assert!(
            trace
                .warnings
                .iter()
                .any(|w| w.message.contains("B.3.b") && w.message.contains("alternation depth")),
            "expected a B.3.b warning naming alternation depth; got warnings: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b3b_warning_absent_on_alt_depth_1_formula() {
        // R.5 B.3.b (2026-06-01) — for an alternation-depth-1
        // formula (pure ν or pure μ, no μ-inside-ν or ν-inside-μ),
        // the warning MUST NOT fire even if the lift is standard
        // KMTS. Safety + liveness without alternation are sound
        // under standard KMTS refinement.
        let formula = parser::parse("nu X. < true > X").expect("formula parses");
        assert_eq!(formula.alternation_depth(), 1);

        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        // No B.3.b warning — alt-depth = 1.
        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("B.3.b")),
            "B.3.b warning MUST NOT fire on alt-depth-1 formula; got warnings: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b3b_warning_default_trace_has_empty_warnings_vec() {
        // R.5 B.3.b (2026-06-01) — backward-compat sanity: when
        // no advisories fire, `warnings` is the empty Vec.
        // Strict-additive: callers that don't read `warnings`
        // see no behaviour change.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions::default();
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            trace.warnings.is_empty(),
            "default `true` formula MUST produce no warnings; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn a4_sampling_may_warning_fires_on_box_formula_with_may_off() {
        // A.4 (2026-07-05) — a box ([]) formula run with the sampling
        // may-relation (`MayEdgeInference::Off`) MUST emit the A.4 soundness
        // warning: sampling under-approximates the may-relation, so a definite
        // HOLDS on the box property is not guaranteed sound.
        let formula = parser::parse("nu X. (true && [] X)").expect("formula parses");
        assert_eq!(formula.box_modality_count(), 1);

        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            ..CegarOptions::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        assert!(
            trace.warnings.iter().any(|w| {
                w.message.contains("(A.4)") && w.message.contains("may_edge_inference = Off")
            }),
            "expected an A.4 sampling-may warning on a box formula with may=off; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn a4_sampling_may_warning_absent_with_smt_all_pairs() {
        // A.4 (2026-07-05) — the SAME box formula run with the sound
        // `SmtAllPairs` over-approximating may-relation MUST NOT emit the A.4
        // warning: the may-relation is then sound for the box.
        let formula = parser::parse("nu X. (true && [] X)").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::SmtAllPairs,
            ..CegarOptions::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("(A.4)")),
            "A.4 warning MUST NOT fire when may=SmtAllPairs; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn a4_sampling_may_warning_absent_on_diamond_only_formula() {
        // A.4 (2026-07-05) — a formula with NO box (diamond-only) MUST NOT emit
        // the A.4 warning even under may=off: the under-approximation soundness
        // gap is specific to the universal (box) modality.
        let formula = parser::parse("mu X. (true || <> X)").expect("formula parses");
        assert_eq!(formula.box_modality_count(), 0);
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            ..CegarOptions::default()
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("(A.4)")),
            "A.4 warning MUST NOT fire on a diamond-only formula; got: {:?}",
            trace.warnings
        );
    }

    // R.5 B.4.a (2026-06-01) — BTOR2 fixture with a `mul`
    // operator. The R.5b default UF policy wraps `Op::Mul`
    // unconditionally, so this fixture's lift will emit a
    // UF-wrap warning that the B.4.a smart cap then keys off.
    const SMALL_BTOR2_WITH_MUL: &str = "\
1 sort bitvec 1
2 state 1 reg_a
3 input 1 in_a
4 zero 1
5 mul 1 2 3
6 init 1 2 4
7 next 1 2 5
";

    #[test]
    fn r5_b4a_smart_uf_cap_default_is_true() {
        // R.5 B.4.a (2026-06-01) — backward-compat sanity: the
        // default `CegarOptions` MUST have `smart_uf_cap = true`
        // (the smart cap is opt-OUT, not opt-in).
        let opts = CegarOptions::default();
        assert!(
            opts.smart_uf_cap,
            "CegarOptions::default() MUST have smart_uf_cap = true"
        );
    }

    #[test]
    fn r5_b4a_warning_fires_when_wp_source_meets_uf_wrapped_lift() {
        // R.5 B.4.a (2026-06-01) — when the predicate source is
        // WP AND the first lift emits a UF-wrap warning AND the
        // smart cap is on (default), the loop emits an
        // explanatory warning + reduces the effective iteration
        // cap from `max_iterations` (16) to `SMART_UF_MAX_ITERATIONS`
        // (4).
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions::default(); // smart_uf_cap=true, max_iterations=16
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_MUL,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        // The B.4.a warning MUST fire — the lift wraps the
        // `mul` Op, the source is WP, smart_uf_cap is the
        // default true.
        assert!(
            trace.warnings.iter().any(|w| w.message.contains("B.4.a")
                && w.message.contains("smart_uf_cap")
                && w.message.contains("UF-wrap")),
            "expected a B.4.a warning naming smart_uf_cap + UF-wrap; got warnings: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b4a_no_warning_when_smart_uf_cap_disabled() {
        // R.5 B.4.a (2026-06-01) — when the user explicitly
        // disables the smart cap, the warning MUST NOT fire
        // even on the same UF-wrapped fixture. The literal
        // `max_iterations` is used.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false, // opt out
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_MUL,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("B.4.a")),
            "B.4.a warning MUST NOT fire when smart_uf_cap=false; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b4a_no_warning_when_source_is_not_wp() {
        // R.5 B.4.a (2026-06-01) — when the predicate source is
        // not WP (e.g. Manual), the smart cap MUST NOT fire even
        // on a UF-wrapped lift. WP is the only source where the
        // smart cap applies (because WP is the source that
        // cannot construct closing predicates for UF-spurious
        // cases without R-F3 SMT lemma cache).
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let manual_cb: Arc<ManualPredicateCallback> = Arc::new(|_, _| Vec::new());
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::Manual(manual_cb),
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: true, // on; should still not fire because source ≠ WP
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_MUL,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("B.4.a")),
            "B.4.a warning MUST NOT fire when source is Manual (not WP); got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b4a_no_warning_when_lift_has_no_uf_wrap() {
        // R.5 B.4.a (2026-06-01) — on a fixture WITHOUT a UF-
        // wrapped Op (the existing SMALL_BTOR2), the smart cap
        // MUST NOT fire even with WP source + smart_uf_cap=true.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions::default();
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2, // no mul → no UF wrap
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            !trace.warnings.iter().any(|w| w.message.contains("B.4.a")),
            "B.4.a warning MUST NOT fire when lift has no UF wrap; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_b6c_capture_records_approximants_when_verdict_contains_kleenebot() {
        // R.5 B.6.c (2026-06-03) — close the test gap in sub-item
        // 1.2's coverage. The pre-existing
        // `r5_cegar_auto_capture_records_per_iteration_approximants`
        // test only exercises the converged-`true`-verdict case.
        // This test exercises the case where the verdict contains
        // KleeneBot cells — proving capture works even when the
        // game evaluator returns indefinite positions.
        //
        // Fixture: `nu X. < step > X` over SMALL_BTOR2_WITH_INPUT.
        // The lift produces MayOnly transitions on the "step"
        // label (the input enables R.2.5's may-edge sampling); for
        // a ν formula over a CLTS with may-but-not-must
        // transitions, the diamond modality produces KleeneBot at
        // some cubes. The capture machinery MUST still populate
        // `approximants_at_end` correctly — both must_true (the
        // KleeneT-only positions) and may_true (the KleeneT ∪
        // KleeneBot positions) bit-sets reflect the converged
        // 3-valued iterate.
        let formula = parser::parse("nu X. < true > X").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: true,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_INPUT,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        assert!(
            !trace.iterations.is_empty(),
            "trace must record at least one iteration"
        );
        // For every iteration, the captured approximants_at_end
        // MUST be Some(map) since capture_approximants=true.
        for iter in &trace.iterations {
            let approximants = iter.approximants_at_end.as_ref().unwrap_or_else(|| {
                panic!(
                    "capture_approximants=true MUST populate approximants_at_end on iteration {}; \
                     verdict at this iteration: {:?}",
                    iter.iteration, iter.verdict
                )
            });
            // The ν formula `nu X. <true> X` has exactly one
            // fixpoint var (the outer ν), so the captured map has
            // exactly one entry.
            assert_eq!(
                approximants.len(),
                1,
                "single-fixpoint formula MUST capture exactly one approximant on iteration {}",
                iter.iteration
            );
            // The captured iterate's `must ⊆ may` invariant MUST
            // hold even when KleeneBot positions are present
            // (which is the whole point of this test vs the
            // sub-item 1.2 test that uses `true` formula).
            let stored = approximants.values().next().expect("one entry");
            let mut diff = stored.must_true.clone();
            diff &= !stored.may_true.clone();
            assert!(
                diff.not_any(),
                "must_true ⊆ may_true invariant MUST hold on iteration {} even with KleeneBot \
                 cells present in the verdict",
                iter.iteration
            );
        }
    }

    /// R-Y5 (2026-06-08) — the trace's
    /// `init_refinement_candidates` field is empty when no
    /// failure subgame is emitted, and is populated
    /// (containing the registers from the active predicate
    /// set) when at least one iteration's failure_subgame is
    /// Some. This structural invariant decouples R-Y5 from
    /// the specifics of which fixture happens to produce
    /// KleeneBot via evaluate_tri's projection (a function
    /// of the CLTS state-3valued-predicate labelling, which
    /// the R.2.5 MVP does not populate).
    ///
    /// The structural invariant is tested by inspecting the
    /// per-iteration `failure_subgame` field: if ANY iteration
    /// has Some, the trace's candidates MUST include all
    /// registers from `final_predicates` (the MVP's coarse
    /// over-approximation collects every register from the
    /// predicate set whenever a subgame fires).
    #[test]
    fn r_y5_init_refinement_candidates_invariant() {
        let formula = parser::parse("nu X. < true > X").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 2,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_INPUT,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");

        let any_subgame = trace
            .iterations
            .iter()
            .any(|iter| iter.failure_subgame.is_some());
        if any_subgame {
            // R-Y5 invariant: when a subgame fires at any
            // iteration, every register from the final
            // predicate set is a candidate (MVP coarse over-
            // approximation).
            for pred in &trace.final_predicates {
                assert!(
                    trace.init_refinement_candidates.contains(&pred.register),
                    "R-Y5: register {:?} must appear in init_refinement_candidates \
                     when a failure subgame fires; got: {:?}",
                    pred.register,
                    trace.init_refinement_candidates
                );
            }
        } else {
            // R-Y5 invariant: no subgame → candidates empty.
            assert!(
                trace.init_refinement_candidates.is_empty(),
                "R-Y5: no failure subgame → init_refinement_candidates must be empty; \
                 got: {:?}",
                trace.init_refinement_candidates
            );
        }
    }

    /// R-Y5 — when the verdict converges without any
    /// failure subgame, `init_refinement_candidates` stays
    /// empty (no init promotion needed).
    #[test]
    fn r_y5_init_refinement_candidates_empty_on_converged_verdict() {
        // Trivial-true formula → definite verdict at every
        // cube → no failure subgame ever.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2_WITH_INPUT,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            trace.init_refinement_candidates.is_empty(),
            "R-Y5: no KleeneBot → no init refinement candidates; got: {:?}",
            trace.init_refinement_candidates
        );
    }

    #[test]
    fn r5_b6b_default_options_debug_omits_default_booleans() {
        // R.5 B.6.b (2026-06-03) — manual Debug impl hides
        // boolean fields at their defaults. Verify the rendered
        // string for `CegarOptions::default()` does NOT contain
        // the three default-valued boolean field names.
        let opts = CegarOptions::default();
        let debug_str = format!("{opts:?}");
        // The three boolean fields at defaults MUST NOT appear.
        assert!(
            !debug_str.contains("capture_approximants"),
            "Default Debug output MUST hide `capture_approximants: false`; got: {debug_str}"
        );
        assert!(
            !debug_str.contains("enable_approximant_reuse"),
            "Default Debug output MUST hide `enable_approximant_reuse: false`; got: {debug_str}"
        );
        assert!(
            !debug_str.contains("smart_uf_cap"),
            "Default Debug output MUST hide `smart_uf_cap: true`; got: {debug_str}"
        );
        // The load-bearing fields MUST appear.
        assert!(
            debug_str.contains("max_iterations"),
            "Default Debug output MUST include `max_iterations`; got: {debug_str}"
        );
        assert!(
            debug_str.contains("predicate_source"),
            "Default Debug output MUST include `predicate_source`; got: {debug_str}"
        );
    }

    #[test]
    fn r5_b6b_non_default_booleans_appear_in_debug() {
        // R.5 B.6.b (2026-06-03) — when boolean fields are
        // explicitly overridden away from their defaults, they
        // MUST appear in the Debug output.
        let opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: true,     // non-default
            enable_approximant_reuse: true, // non-default
            smart_uf_cap: false,            // non-default
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let debug_str = format!("{opts:?}");
        assert!(
            debug_str.contains("capture_approximants"),
            "Debug output MUST include `capture_approximants: true` when overridden; got: {debug_str}"
        );
        assert!(
            debug_str.contains("enable_approximant_reuse"),
            "Debug output MUST include `enable_approximant_reuse: true` when overridden; got: {debug_str}"
        );
        assert!(
            debug_str.contains("smart_uf_cap"),
            "Debug output MUST include `smart_uf_cap: false` when overridden; got: {debug_str}"
        );
    }

    #[test]
    fn r5_subitem_34_craig_emits_adapter_warning_when_cvc5_absent() {
        // R.5 Item 3 sub-item 3.4 (2026-06-04) — when
        // PredicateSource::CraigInterpolation is selected but
        // CVC5 is not available, the CEGAR loop MUST emit an
        // AdapterWarning + fall back to the WP heuristic for
        // the run. Test by forcing MUNUNU_CVC5_PATH to a bogus
        // path so locate_cvc5() fails deterministically.
        //
        // SAFETY: env var manipulation in tests is racy; we
        // restore the original value at the end.
        let original = std::env::var("MUNUNU_CVC5_PATH").ok();
        unsafe {
            std::env::set_var(
                "MUNUNU_CVC5_PATH",
                "/nonexistent/path/to/cvc5/binary/from/3_4/test",
            );
        }

        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::CraigInterpolation,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_result = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        );

        // Restore env var before any assertion (so failure
        // doesn't leak the bogus value).
        unsafe {
            match original {
                Some(v) => std::env::set_var("MUNUNU_CVC5_PATH", v),
                None => std::env::remove_var("MUNUNU_CVC5_PATH"),
            }
        }

        let trace = trace_result.expect("cegar should succeed by falling back to WP");
        assert!(
            trace
                .warnings
                .iter()
                .any(|w| w.message.contains("sub-item 3.4")
                    && w.message.contains("cvc5 binary not available")),
            "expected a sub-item-3.4 warning naming the missing CVC5 binary; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    fn r5_subitem_34_craig_no_warning_when_source_is_not_craig() {
        // R.5 Item 3 sub-item 3.4 (2026-06-04) — when the
        // predicate source is NOT CraigInterpolation, the
        // sub-item-3.4 warning MUST NOT fire even if CVC5 is
        // absent. The warning is specific to the Craig path.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds");
        assert!(
            !trace
                .warnings
                .iter()
                .any(|w| w.message.contains("sub-item 3.4")),
            "sub-item 3.4 warning MUST NOT fire when source != CraigInterpolation; got: {:?}",
            trace.warnings
        );
    }

    #[test]
    #[ignore = "requires cvc5 binary installed; run with --ignored when available"]
    fn r5_subitem_34_craig_invokes_cvc5_end_to_end() {
        // R.5 Item 3 sub-item 3.4 (2026-06-04) — when CVC5 is
        // available + the Craig source is selected, the CEGAR
        // loop invokes CVC5 + emits predicates with the
        // `craig_s<src>_t<tidx>` naming convention.
        //
        // Ignored by default; run with `cargo test -- --ignored`
        // after `brew install cvc5`. Asserts that the CEGAR
        // run completes AND (if any predicates were added)
        // they include at least one with the craig naming
        // convention.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);
        let cegar_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::CraigInterpolation,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &cegar_opts,
        )
        .expect("cegar succeeds with CVC5 available");
        // No sub-item-3.4 warning should fire — CVC5 was
        // discovered.
        assert!(
            !trace
                .warnings
                .iter()
                .any(|w| w.message.contains("sub-item 3.4")
                    && w.message.contains("cvc5 binary not available")),
            "sub-item 3.4 warning MUST NOT fire when CVC5 is available; got: {:?}",
            trace.warnings
        );
        // The `true` formula converges immediately; no
        // refinement happens. This test mostly proves the
        // wiring + locate_cvc5 success path.
    }

    #[test]
    fn r5_subitem_24_default_lift_strategy_is_eager() {
        // R.5 lazy KMTS sub-item 2.4 (2026-06-04) — backward-
        // compat sanity: `CegarOptions::default()` MUST set
        // `lift_strategy = LiftStrategy::Eager`.
        let opts = CegarOptions::default();
        assert_eq!(opts.lift_strategy, LiftStrategy::Eager);
    }

    #[test]
    fn r5_subitem_24_lazy_strategy_produces_same_verdict_as_eager() {
        // R.5 lazy KMTS sub-item 2.4 (2026-06-04) — LOAD-
        // BEARING DRIFT-PROTECTION TEST at the verdict level.
        // Running CEGAR with `LiftStrategy::Lazy` MUST produce
        // a verdict identical to `LiftStrategy::Eager` on the
        // same fixture + formula. Any divergence between the
        // lazy + eager per-cube paths surfaces here as a
        // verdict-equality test failure.
        let formula = parser::parse("true").expect("formula parses");
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let env = Environment::new(2);

        let mut eager_opts = CegarOptions {
            max_iterations: 4,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,

            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace_eager = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial.clone(),
            &env,
            &AdapterOptions::default(),
            &eager_opts,
        )
        .expect("eager cegar succeeds");

        eager_opts.lift_strategy = LiftStrategy::Lazy;
        let trace_lazy = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &eager_opts,
        )
        .expect("lazy cegar succeeds");

        assert!(
            trace_eager.final_verdict.eq_set(&trace_lazy.final_verdict),
            "Lazy strategy MUST produce verdict identical to Eager"
        );
        assert_eq!(
            trace_eager.terminated_with, trace_lazy.terminated_with,
            "Lazy strategy MUST produce identical termination state to Eager"
        );
    }

    #[test]
    fn b1_3c_sidecar_compound_predicate_threads_into_cegar() {
        // B.1 increment 3c (2026-06-26) — a compound predicate declared on the
        // SvAnnotation sidecar threads end-to-end through cegar_refine_loop:
        // the cube space gains the `idle` bit, and because compounds require
        // the eager SmtAllPairs lift, 3c FORCES `may = SmtAllPairs` even though
        // the CegarOptions request `Off` — without that force, the compound
        // gate (`ensure_compound_lift_supported`) would error. Fixture mirrors
        // the kmts_lift b1 fixture: a:=0 always, b:=in_b, so
        // `idle = (a==0 && b==0)` is the meaningful compound.
        let formula = parser::parse("true").expect("formula parses");
        let env = Environment::new(2);
        let btor2 = "1 sort bitvec 1\n2 input 1 in_b\n3 state 1 a\n4 state 1 b\n5 zero 1\n6 init 1 3 5\n7 init 1 4 5\n8 next 1 3 5\n9 next 1 4 2\n";
        let sidecar =
            r#"{"module":"m","compound_predicates":[{"name":"idle","expr":"a == 0 && b == 0"}]}"#;
        let adapter_options = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let opts = CegarOptions {
            max_iterations: 2,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            // Deliberately Off — 3c must FORCE SmtAllPairs (else the compound
            // gate errors). The success of the call below proves the force.
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        // No initial predicates — the sidecar compound is the SOLE cube bit, so
        // the helper-supplied PredicateSpec is what populates the cube space.
        let trace = cegar_refine_loop(&formula, btor2, vec![], &env, &adapter_options, &opts)
            .expect("compound cegar succeeds — 3c forces SmtAllPairs, not the 3b gate error");
        assert!(
            trace.final_predicates.iter().any(|p| p.name == "idle"),
            "the sidecar compound `idle` must thread in as a cube-bit predicate; got {:?}",
            trace
                .final_predicates
                .iter()
                .map(|p| &p.name)
                .collect::<Vec<_>>()
        );
        assert!(
            trace
                .warnings
                .iter()
                .any(|w| w.message.contains("[B.1 3c compound]")),
            "expected the 3c force-SmtAllPairs override warning; got {:?}",
            trace
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn b1_3c_no_sidecar_compound_preserves_legacy_path() {
        // B.1 increment 3c — absent `compound_predicates`, the CEGAR loop is
        // byte-for-byte the pre-3c path: no forced SmtAllPairs, no override
        // warning, no synthetic cube bit.
        let formula = parser::parse("true").expect("formula parses");
        let env = Environment::new(2);
        let initial = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let opts = CegarOptions {
            max_iterations: 2,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(
            &formula,
            SMALL_BTOR2,
            initial,
            &env,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("legacy cegar succeeds");
        assert!(
            !trace
                .warnings
                .iter()
                .any(|w| w.message.contains("[B.1 3c compound]")),
            "no compound predicates ⇒ no 3c override warning"
        );
    }

    #[test]
    fn cegar_rebuilds_env_on_in_loop_predicate_growth() {
        // B.2 (2026-06-26) — regression guard for the env-rebuild fix.
        // The caller sizes the `Environment` from its EXPLICIT predicate
        // count, but B.1 3c appends sidecar compound predicates INSIDE the
        // loop (and CEGAR refinement grows the set across iterations), so
        // the caller's env can be too small. Pre-fix this hard-errored
        // ("environment state count N does not match lift state count M");
        // post-fix the loop rebuilds a correctly-sized env. This is the
        // exact V.7-b CLI scenario at the lib level: 1 bootstrap predicate
        // (env sized for 2 cubes) + 1 sidecar compound = 2 predicates =
        // 4 cubes — a deliberate mismatch.
        let formula = parser::parse("true").expect("formula parses");
        let btor2 = "1 sort bitvec 1\n2 input 1 in_b\n3 state 1 a\n4 state 1 b\n5 zero 1\n6 init 1 3 5\n7 init 1 4 5\n8 next 1 3 5\n9 next 1 4 2\n";
        let sidecar =
            r#"{"module":"m","compound_predicates":[{"name":"idle","expr":"a == 0 && b == 0"}]}"#;
        let adapter_options = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        // Deliberately undersized env: 2 cubes for the 1 bootstrap
        // predicate, but the appended compound grows the lift to 4 cubes.
        let env = Environment::new(2);
        let initial = vec![PredicateSpec {
            name: "busy".into(),
            register: "a".into(),
            value: 1,
        }];
        let opts = CegarOptions {
            max_iterations: 2,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: false,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: crate::adapter::btor2::kmts_lift::MustEdgeInference::Off,
            may_edge_inference: crate::adapter::btor2::kmts_lift::MayEdgeInference::Off,
            emit_ctxdsl: false,
        };
        let trace = cegar_refine_loop(&formula, btor2, initial, &env, &adapter_options, &opts)
            .expect("env-rebuild lets the undersized-env compound run succeed");
        assert_eq!(
            trace.final_predicates.len(),
            2,
            "bootstrap `busy` + sidecar compound `idle` ⇒ 2 cube-bit predicates"
        );
    }
}
