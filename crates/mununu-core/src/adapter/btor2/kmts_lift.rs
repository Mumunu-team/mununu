//! R.2 — BTOR2 → KMTS lifter.
//!
//! Per the KMTS architecture
//! (`docs/design/native-sv-abstraction.md` §6, §7;
//! `docs/design/kmts-theory.md` §6;
//! `docs/design/predicate-abstraction-recipe.md` §2),
//! this module promotes the existing 2-valued
//! [`Btor2Adapter`](super::Btor2Adapter) output into a KMTS-aware
//! shape consumable by the post-R.3 `KleeneDomain` evaluator.
//!
//! **R.2 scope (MVP).** The lifter is a *post-hoc enrichment* over
//! the existing BTOR2 adapter — it does not change the CLTS shape
//! today, it adds a parallel `predicate_labellings` side-map keyed
//! by `(automaton, state, predicate_name) → Tristate`. Each entry
//! is derived from the BTOR2 bit-blaster's per-state register
//! valuations: every `(register, value)` pair becomes a predicate
//! `<register>=<value>` whose `KleeneT` set is the states where the
//! register has that value, and whose `KleeneF` set is every other
//! enumerated state. **No `KleeneBot` values are produced at R.2** —
//! the bit-blaster's explicit-state enumeration is exact, so every
//! predicate has a definite verdict at every state.
//!
//! Modality is also uniform at R.2: every transition the bit-blaster
//! emits has both a may-witness (the abstraction admits the edge)
//! and a must-witness (the bit-blaster's exact enumeration computes
//! the concrete reachability), so every edge is `Sharp`. This
//! matches the legacy semantics exactly; the R.2 lifter produces a
//! KMTS that is *vacuously* 3-valued — the same verdicts as the
//! 2-valued evaluator on every fixture today.
//!
//! **Where R.2 stops being a no-op.** When R.5 (CEGAR) + R.5b (UF
//! abstraction) land, the lifter's predicate-image construction
//! will introduce `KleeneBot` valuations (predicates the abstraction
//! cannot decide) and `MayOnly` transitions (over-approximation
//! edges from UF-abstracted operators). The R.2 surface is the
//! interface those phases plug into; today it ships the enrichment
//! shape and a fixture-sweep regression so the post-R.3 evaluator
//! has something to read.
//!
//! ## Predicate-cube lift architecture (U.1–U.4 unification, 2026-06-26)
//!
//! The predicate-cube path (the R.2.5+ cap-bypassing lift; distinct from
//! the R.2 post-hoc enrichment above) has **one** public entry and one
//! shared per-cube body, after the lift-unification refactor:
//!
//! - [`lift_predicate_cube`] — the single gated entry the CEGAR loop +
//!   verify orchestrator call. Runs the compound soundness gate
//!   ([`ensure_compound_lift_supported`]) once, then dispatches on
//!   [`LiftStrategy`](crate::adapter::btor2::cegar::LiftStrategy):
//!   `Eager` → [`predicate_cube_lift`], `Lazy` → [`LazyLift`] +
//!   [`materialize_clts_from_lazy`].
//! - [`cube_sampling_edges`] — the ONE per-cube sampling body (canonical
//!   representative → input-combo enumeration → UF-rep simulate →
//!   predicate re-evaluation → target cube), shared by the eager
//!   sampling loop + the lazy [`compute_cube_outgoing_edges`]. Returns
//!   `(target, LabelDescriptor)` pairs deduped by `(target, descriptor)`,
//!   which reproduces both effective edge sets. **The eager and lazy
//!   sampling lifts cannot diverge** — the
//!   `u1_eager_lazy_differential_equivalence` test asserts they produce
//!   byte-identical CLTSes.
//! - [`apply_sampled_must_inference`] — the ONE sampled-target must-edge
//!   post-pass (`SmtPerTarget{,Standard}` / `SmtHyperMust`; the ∀∃ and
//!   hyper passes route through the STS-IR seam), shared by both paths.
//!
//! **Strategy matrix.** The may-relation has two shapes:
//! - `MayEdgeInference::SmtAllPairs` — sound global all-pairs SMT
//!   may-relation. **Eager-only** (a global Z3 query, not a per-cube
//!   one) and the **only** path that honours compound predicates (via
//!   the `SmtEncode` seam + `PredicateLike::expr`).
//! - `MayEdgeInference::Off` (sampling) — the per-cube
//!   [`cube_sampling_edges`] body; simple `register==value` atoms only.
//!   Runs on both Eager + Lazy and is the eager≡lazy equivalence corpus.
//!
//! **Compound rule.** Compound predicates (`compound_exprs`) require
//! `Eager` + `SmtAllPairs`: the sampling representative inverse can't
//! realise e.g. `a==0 && b==0`, and the lazy body never consults
//! `compound_exprs`. [`ensure_compound_lift_supported`] enforces this at
//! the entry (and via thin defensive copies in `predicate_cube_lift` +
//! `LazyLift::from_btor2` for the ~39 direct callers).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::adapter::AdapterOptions;
use crate::adapter::btor2::Btor2Adapter;
use crate::adapter::{AdapterError, AdapterOutput, FormatAdapter, SourceFormat, SourceInfo};
use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, Tristate};

/// Per-(automaton, state) 3-valued labelling map.
/// Outer key: automaton name. Middle key: state name. Inner map:
/// predicate name → Kleene verdict at that state.
pub type LabellingMap = HashMap<String, HashMap<String, BTreeMap<String, Tristate>>>;

/// R.2 — Options controlling the BTOR2 → KMTS lift.
#[derive(Debug, Clone, Default)]
pub struct KmtsLiftOptions {
    /// When `true`, the lifter wraps an explicit "no-state-matched"
    /// predicate `__no_state__` that is KleeneT at exactly the
    /// states where every other predicate is KleeneF. Useful for
    /// debugging predicate completeness; off by default.
    pub emit_no_state_predicate: bool,
    /// Cap on the number of predicates synthesised. Each
    /// `(register, value)` pair in the bit-blaster's state
    /// valuations becomes one predicate; designs with wide
    /// registers and many enumerated values can produce hundreds.
    /// `None` (default) means no cap; the R.5+ CEGAR loop is
    /// where predicate cardinality becomes a real concern.
    pub max_predicates: Option<usize>,
}

/// R.2 — One synthesised predicate the lifter produces per
/// `(register, value)` pair found in the bit-blaster's state
/// valuations. The CLTS-layer 3-valued labelling field
/// [`crate::clts::Clts::state_3valued_predicates`] uses the
/// `name` here as the predicate identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiftedPredicate {
    /// Predicate name in the form `<register>=<value>`. Round-trips
    /// directly as the predicate identifier in
    /// `Clts::state_3valued_predicate(state, name)`.
    pub name: String,
    /// Source register / signal the predicate is anchored on.
    pub register: String,
    /// The display value the predicate witnesses.
    pub value: String,
}

/// R.2 — Result of lifting one BTOR2 source through the KMTS-aware
/// shape. Carries the existing 2-valued [`AdapterOutput`] unchanged
/// plus the predicate side-map keyed by `(automaton, state, predicate)`.
#[derive(Debug, Clone)]
pub struct KmtsLiftResult {
    /// The unchanged 2-valued output the legacy BTOR2 adapter would
    /// have produced. Existing CTXDSL emission, state_valuations,
    /// sidecars, partition summaries all survive.
    pub adapter_output: AdapterOutput,
    /// The set of predicates the lifter synthesised from the
    /// bit-blaster's state valuations. Deterministic order
    /// (sorted by name).
    pub predicates: Vec<LiftedPredicate>,
    /// Per-(automaton, state) 3-valued labelling map. The post-R.3
    /// `KleeneDomain` evaluator reads this when the
    /// `Clts::state_3valued_predicates` field is `None` (the legacy
    /// CLTS path today; the lifter intentionally does *not* mutate
    /// the Clts struct at R.2, that hook lands when the evaluator
    /// is on).
    pub predicate_labellings: LabellingMap,
}

impl KmtsLiftResult {
    /// Total number of `(automaton, state, predicate)` triples
    /// the labelling map carries. Used as the R.2 done-criterion
    /// proxy: a fixture "produces KMTS" iff this count is > 0
    /// (the lifter inferred at least one predicate from the
    /// bit-blaster's state valuations).
    pub fn labelling_count(&self) -> usize {
        self.predicate_labellings
            .values()
            .flat_map(|per_state| per_state.values())
            .map(BTreeMap::len)
            .sum()
    }
}

/// R.2 — Lift one BTOR2 source through the KMTS-aware shape.
///
/// Runs the existing [`Btor2Adapter::translate`] to produce the
/// 2-valued output, then walks `state_valuations` to synthesise
/// per-`(register, value)` predicates and per-state labellings.
/// All transitions are implicitly `Sharp` (the bit-blaster's
/// exact-enumeration semantics — both may and must witnesses
/// exist for every emitted edge) and the existing Clts is
/// returned unmodified inside `adapter_output`.
///
/// Errors only when the underlying BTOR2 adapter errors —
/// post-translation enrichment is infallible (or returns an empty
/// labelling map when the bit-blaster did not populate
/// `state_valuations`, which is the case for very small fixtures
/// where every state has a trivial valuation).
pub fn lift_btor2_to_kmts(
    content: &str,
    options: &AdapterOptions,
    lift_opts: &KmtsLiftOptions,
) -> Result<KmtsLiftResult, AdapterError> {
    let adapter_output = Btor2Adapter::translate(content, options).map_err(|mut e| {
        e.message = format!("adapter/btor2/kmts_lift: {}", e.message);
        e
    })?;

    let (predicates, predicate_labellings) =
        synthesise_predicates_and_labellings(&adapter_output, lift_opts);

    Ok(KmtsLiftResult {
        adapter_output,
        predicates,
        predicate_labellings,
    })
}

/// Walk `AdapterOutput.state_valuations` and produce
/// `(predicates, labellings)`. Each `(register, value)` pair found
/// in any state's valuation becomes one predicate; the labelling
/// at each state is `KleeneT` for predicates matching that state's
/// valuation and `KleeneF` for the rest.
fn synthesise_predicates_and_labellings(
    out: &AdapterOutput,
    lift_opts: &KmtsLiftOptions,
) -> (Vec<LiftedPredicate>, LabellingMap) {
    // First pass: collect the universe of (register, value) pairs
    // observed across all automata × states. Deduplicate and sort
    // for determinism — the lifter's output must be stable across
    // runs to keep regression baselines clean.
    let mut universe: std::collections::BTreeSet<(String, String)> =
        std::collections::BTreeSet::new();
    for per_state in out.state_valuations.values() {
        for valuation in per_state.values() {
            for (reg, val) in valuation {
                universe.insert((reg.clone(), val.clone()));
            }
        }
    }

    let mut predicates: Vec<LiftedPredicate> = universe
        .into_iter()
        .map(|(register, value)| LiftedPredicate {
            name: format!("{register}={value}"),
            register,
            value,
        })
        .collect();

    if let Some(cap) = lift_opts.max_predicates
        && predicates.len() > cap
    {
        predicates.truncate(cap);
    }

    if lift_opts.emit_no_state_predicate {
        predicates.push(LiftedPredicate {
            name: "__no_state__".to_string(),
            register: "__synthetic__".to_string(),
            value: "no_state".to_string(),
        });
    }

    // Second pass: for each (automaton, state), build a BTreeMap of
    // predicate_name → Tristate. The bit-blaster's enumeration is
    // exact, so every predicate has a definite verdict (KleeneT or
    // KleeneF) — no KleeneBot at R.2.
    let mut labellings: LabellingMap = HashMap::new();
    for (aut, per_state) in &out.state_valuations {
        let mut per_state_map: HashMap<String, BTreeMap<String, Tristate>> = HashMap::new();
        for (state, valuation) in per_state {
            let mut verdicts: BTreeMap<String, Tristate> = BTreeMap::new();
            for pred in &predicates {
                if pred.register == "__synthetic__" {
                    // The `__no_state__` synthetic predicate is
                    // KleeneT iff every other predicate is KleeneF.
                    // Filled in after the main loop below.
                    continue;
                }
                let verdict = if valuation.get(&pred.register) == Some(&pred.value) {
                    Tristate::KleeneT
                } else {
                    Tristate::KleeneF
                };
                verdicts.insert(pred.name.clone(), verdict);
            }
            // `__no_state__` post-fill: KleeneT iff every other
            // predicate at this state is KleeneF. This happens iff
            // the state's valuation has no matching `(register, value)`
            // in the predicate set — which today is rare (the
            // predicate set is derived from the valuation universe).
            if lift_opts.emit_no_state_predicate {
                let all_false = verdicts.values().all(|v| *v == Tristate::KleeneF);
                verdicts.insert(
                    "__no_state__".to_string(),
                    if all_false {
                        Tristate::KleeneT
                    } else {
                        Tristate::KleeneF
                    },
                );
            }
            per_state_map.insert(state.clone(), verdicts);
        }
        labellings.insert(aut.clone(), per_state_map);
    }

    (predicates, labellings)
}

// ---------------------------------------------------------------------------
// R.2.5 — predicate-cube lift API
// ---------------------------------------------------------------------------
//
// Per the §Phase 5 R.2.5 / §Phase 6 §6.3 / §10.1 plan entries, the R.2
// post-hoc lifter inherits the bit-blaster's `MAX_STATE_BITS = 20` cap.
// R.2.5 ships an alternative API whose abstract states are predicate
// cubes (2^|P|), bypassing the bit-blast enumeration entirely.
//
// **R.2.5 MVP scope** (this commit): the API surface, structural
// state-space construction (2^|P| cubes as `Clts` states with the
// matching `state_3valued_predicates` labelling), the binary capability
// test (a synthetic BTOR2 fixture with > MAX_STATE_BITS total state
// bits but |P| ≤ 10 lifts where R.2 errors), and an explicit
// `predicate_image_pending` flag marking the structural debt that the
// load-bearing SMT-driven must/may edge construction (R.5 / R.5b) will
// close.
//
// **What this MVP does NOT do**: no SMT predicate-image queries; no
// must-edges; no may-edges (the returned Clts has the cube state set
// but no transitions). Verdicts computed over the R.2.5 output today
// are useless (every property evaluates over an isolated set of cube
// states with no dynamics), but the *binary capability* — lifting a
// cap-exceeding fixture into a predicate-cube state space — is real
// and verifiable. The done-criterion in §10.1 R.2.5 is explicitly
// binary; verdict correctness lands when R.5's `KmtsLiftLazy` ships.

/// R.2.5 — A single predicate the cube lifter understands. Today's
/// minimal shape is a register-equality predicate `<register> = <value>`;
/// future iterations can extend this enum to arbitrary BTOR2-expression
/// predicates as SMT integration matures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateSpec {
    /// Display name for the predicate; surfaces in `state_3valued_predicates`
    /// labellings as the key.
    pub name: String,
    /// Source register / signal name (as it appears in the BTOR2 symbol
    /// table). The lifter validates this names a real BTOR2 state.
    pub register: String,
    /// The value the predicate checks against (current MVP: literal
    /// equality only; future extensions could carry an operator).
    pub value: u64,
}

/// B.1 (increment 3b) — a predicate paired with its optional compound
/// expression, the slice element the SMT may/must edge seam consumes.
/// A `Some` expr makes [`crate::adapter::btor2::smt_must_edge::PredicateLike::expr`]
/// return it, so 3a's `build_pred_constraint` encodes the compound boolean
/// instead of the simple `register == value` atom; `None` is the simple atom
/// (behaviour-preserving). Built by [`cube_predicates`] from a `PredicateSpec`
/// list + the lift options' `compound_exprs` map, in predicates order (cube
/// bit `i` = `predicates[i]`).
pub struct CubePredicate<'a> {
    spec: &'a PredicateSpec,
    expr: Option<&'a crate::adapter::btor2::predicate_expr::PredicateExpr>,
}

impl crate::adapter::btor2::smt_must_edge::PredicateLike for CubePredicate<'_> {
    fn register(&self) -> &str {
        &self.spec.register
    }
    fn value(&self) -> u64 {
        self.spec.value
    }
    fn expr(&self) -> Option<&crate::adapter::btor2::predicate_expr::PredicateExpr> {
        self.expr
    }
}

/// B.1 — pair each predicate with its compound expr (if its name is in
/// `compound_exprs`), preserving order so cube bit `i` stays `predicates[i]`.
fn cube_predicates<'a>(
    predicates: &'a [PredicateSpec],
    compound_exprs: &'a std::collections::HashMap<
        String,
        crate::adapter::btor2::predicate_expr::PredicateExpr,
    >,
) -> Vec<CubePredicate<'a>> {
    predicates
        .iter()
        .map(|spec| CubePredicate {
            spec,
            expr: compound_exprs.get(&spec.name),
        })
        .collect()
}

/// R.2.5 — Options for [`predicate_cube_lift`].
#[derive(Debug, Clone)]
pub struct PredicateCubeLiftOptions {
    /// Hard cap on cube count. Defaults to 1024 (|P| ≤ 10). Larger
    /// values are accepted but the wall-clock + memory bound from the
    /// §10.1 R.2.5 done-criterion (< 10 s, < 256 MB) applies only at
    /// the default cap.
    pub max_cube_count: usize,
    /// R.2.5 predicate-image MVP — max number of 1-bit BTOR2 inputs
    /// to enumerate per cube when computing may-edges. Defaults to 8
    /// (256 input combinations per cube). Over the cap, those inputs
    /// default to zero — sound under-approximation of input
    /// nondeterminism that may miss may-edges. Set to 0 to disable
    /// edge construction (recovers the legacy R.2.5 MVP behaviour
    /// where the Clts has cube states but no transitions).
    pub max_input_bits: usize,
    /// R.2.5b sub-item (2026-06-06) — must-edge inference policy.
    /// Defaults to [`MustEdgeInference::Off`], which preserves the
    /// pre-R.2.5b behaviour of emitting only `MayOnly` edges.
    /// See [`MustEdgeInference`] for the post-pass semantics.
    pub must_edge_inference: MustEdgeInference,
    /// MIG-3 (2026-06-13) — may-edge construction policy. Defaults to
    /// [`MayEdgeInference::Off`] (sampling, preserving pre-MIG-3
    /// behaviour). [`MayEdgeInference::SmtAllPairs`] replaces the
    /// sampling may-edges with the sound all-pairs SMT may relation.
    pub may_edge_inference: MayEdgeInference,
    /// R-Y7 (2026-06-07) — symbolic-init via predicate cubes.
    /// Map of `register_name → set of valid initial values` for
    /// under-constrained constants. When non-empty, the lifter
    /// expands the initial-state set to ALL cubes admissible
    /// under the R-S8 encoder's
    /// [`crate::adapter::btor2::r_s8_encoder::hyper_must_initial_cubes`]
    /// instead of the pre-R-Y7 single-initial-cube default.
    /// One mechanism (the predicate cube space) handles both
    /// init nondeterminism and state-space abstraction per Phase
    /// 8 §8.1 R-Y7 spec.
    ///
    /// Default empty preserves the pre-R-Y7 single-initial-cube
    /// behaviour exactly.
    pub config_values: std::collections::HashMap<String, Vec<u64>>,
    /// B.1 (increment 3b, 2026-06-25) — compound predicates, keyed by
    /// predicate **name** (the `PredicateSpec::name` of a predicate also
    /// present in the `predicates` list). When a predicate's name appears
    /// here, its cube-dimension truth is decided by the
    /// [`crate::adapter::btor2::predicate_expr::PredicateExpr`] (a boolean
    /// combination of register comparisons) rather than the simple
    /// `register == value` atom — see [`CubePredicate`].
    ///
    /// **Soundness gate:** because the cube→representative-registers
    /// *inverse* (the sampling may-edge path) is only sound for simple
    /// atoms, a non-empty `compound_exprs` REQUIRES
    /// `may_edge_inference == MayEdgeInference::SmtAllPairs` (the lift
    /// errors otherwise) — compounds are routed exclusively through the
    /// SMT may/must edge construction, which honours them via
    /// `PredicateLike::expr()`.
    ///
    /// Default empty preserves the pre-B.1 simple-atom behaviour exactly.
    pub compound_exprs:
        std::collections::HashMap<String, crate::adapter::btor2::predicate_expr::PredicateExpr>,
    /// H.E.2 (combinational outputs, 2026-06-28) — **derived combinational
    /// predicates**: `register == value` atoms whose `register` is a
    /// *combinational node* (an `eq`/`or`/… output of state, e.g. csrng's
    /// `main_sm_err_o`), NOT a cube dimension. Approach B
    /// (free-input-atoms.md §4): rather than make a determined signal a free
    /// dimension, the lift LABELS it per cube via
    /// [`crate::adapter::sts_ir::SmtEncode::combinational_labels`] — KleeneT/F
    /// where the cube pins it, KleeneBot where it doesn't — writing the label
    /// into `state_3valued_predicates` (keyed by the predicate `name`, so the
    /// evaluator binds the formula atom). These do NOT enlarge the cube space
    /// (`2^|predicates|` is unchanged). Default empty preserves pre-H.E
    /// behaviour.
    pub derived_predicates: Vec<PredicateSpec>,
    /// scalable-KMTS P1.4 (2026-07-12) — compute the `SmtAllPairs` may-edges via the compound-sound SMT
    /// **post-image** ([`compute_all_may_edges_smt_postimage`]) instead of the `O(2^{2|P|})` all-pairs
    /// loop (`BtorSts::may_edges`). Both encode the transition via `encode_design_for_lift` (identical,
    /// exact), so the may-relation — and thus the KMTS and every verdict — is IDENTICAL; only the cost
    /// differs (`O(2^|P| · #succ)` post-image vs `O(2^{2|P|})` all-pairs). Sound for compounds. Only
    /// consulted on the `SmtAllPairs` may path; `false` preserves the all-pairs behaviour exactly.
    pub may_postimage: bool,
}

impl Default for PredicateCubeLiftOptions {
    fn default() -> Self {
        Self {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: MayEdgeInference::Off,
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        }
    }
}

/// R.2.5b (2026-06-06) — Policy for inferring `must`-side
/// transitions (`Sharp`, `MustHyperOnly`) from the R.2.5 sampling-
/// based predicate-image MVP. Paired with the B.3.b/B.3.c phase-
/// boundary decision (2026-06-01): R.5.0 ships the soundness-warning
/// for alt-depth-≥-2 formulas on Sharp-only KMTSes (already in
/// main); R.2.5b's lifter post-pass opportunistically emits
/// `Sharp` and `MustHyperOnly` edges when the sampling-based
/// predicate-image converges on a single target cube (Sharp) or a
/// non-singleton target set (hyper-must) for every sampled (input,
/// UF-representative) pair.
///
/// **SOUNDNESS**: sampling-based inference. A single-target
/// convergence under sampling proves the must-edge ONLY when the
/// canonical representative is a sound proxy for every concrete
/// state in the cube. R.2.5b's follow-up session will replace the
/// sampling pass with an SMT-backed must-edge query (∀ concrete
/// state in source cube ⟹ ∃ input ⟹ next-state in target cube)
/// using Z3 array theory. Until the SMT swap lands, the inferred
/// must-edges carry a `// SOUNDNESS:` annotation + an
/// `AdapterWarning` on the lift result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MustEdgeInference {
    /// Pre-R.2.5b default. The post-pass is disabled; the lift
    /// emits only `MayOnly` edges. Use when callers require strict
    /// soundness (no sampling-derived must claims).
    Off,
    /// R.2.5b session-2 (2026-06-08). Post-pass: for each
    /// (source-cube, sampled-target) pair, run an SMT must-edge
    /// query via [`super::smt_must_edge::smt_per_target_must_check`].
    /// If Z3 proves the must-edge (UNSAT on the
    /// `src ∧ transition ∧ ¬tgt` formula), promote MayOnly → Sharp.
    /// Multi-target hyper-must inference is queued for a follow-up;
    /// the session-2 MVP only promotes per-target Sharp edges.
    ///
    /// **Stronger than the standard KMTS must-edge.** The MVP form
    /// is `∀ state ⊨ src. ∀ inputs. (transition ⟹ next ⊨ tgt)` —
    /// deterministic transition into tgt regardless of input. The
    /// standard form `∀ state ⊨ src. ∃ input. next ⊨ tgt` produces
    /// more must-edges; the MVP is sound but less precise. The
    /// standard form is shipped as [`MustEdgeInference::SmtPerTargetStandard`].
    ///
    /// SOUNDNESS: SMT-proved (no sampling). The session-2 AdapterWarning
    /// reads `[R.2.5b-smt-must]` instead of `[R.2.5b-sampling-must]`.
    SmtPerTarget,
    /// R.2.5b session-2 follow-up (2026-06-09). SMT-backed must-edge
    /// inference using the canonical KMTS ∀∃ form via Z3 quantifier
    /// alternation (`forall_const` over inputs + state_next). For
    /// each (source cube, sampled target) pair the query is:
    ///
    /// ```text
    /// ∀ state ⊨ src. ∃ inputs. ∃ state_next. (transition ∧ next ⊨ tgt)
    /// ```
    ///
    /// Strictly **more permissive** than [`MustEdgeInference::SmtPerTarget`]:
    /// every ∀∀-Must is also ∀∃-Must, but the ∀∃ form additionally
    /// promotes edges where SOME input combo (not necessarily all)
    /// reaches tgt per concrete source state. This is the canonical
    /// KMTS must-edge semantics from Bruns–Godefroid CONCUR 2000.
    ///
    /// Cost: per-query SMT time is higher than ∀∀ (quantifier
    /// alternation), but bounded by the same `timeout_ms`. AdapterWarning
    /// reads `[R.2.5b-smt-must-standard]`.
    SmtPerTargetStandard,
    /// R.2.5b session-2 follow-up (2026-06-09). SMT-backed
    /// **hyper-must** inference via Z3 ∀∃ with a disjunction over
    /// the candidate target set. Semantics:
    ///
    /// ```text
    /// ∀ state ⊨ src. ∃ inputs. ∃ t ∈ T. (transition ∧ next ⊨ t)
    /// ```
    ///
    /// Where `T` is the sampled-target set per source cube. The
    /// abstraction guarantees some t ∈ T is reached, but not
    /// necessarily the same t across concrete states (standard
    /// Shoham–Grumberg LMCS 2007 §4 hyper-must semantics).
    ///
    /// Lifter strategy: for each source cube,
    /// 1. First try the per-target ∀∃ check per sampled target. If
    ///    any singleton proves Must, promote MayOnly → Sharp.
    /// 2. Otherwise, run `smt_hyper_must_check` over the FULL
    ///    sampled target set. If Must, emit `MustHyperOnly` with
    ///    the full target set (MVP doesn't minimize T; sub-minimal
    ///    T inference is a follow-up).
    ///
    /// SOUNDNESS: same as `SmtPerTargetStandard`; warning reads
    /// `[R.2.5b-smt-must-hyper]`.
    SmtHyperMust,
}

/// MIG-3 (S-track migration, 2026-06-13) — may-edge construction policy
/// for [`predicate_cube_lift`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MayEdgeInference {
    /// Default — sampling-based may-edges (`max_input_bits` input
    /// enumeration per canonical-representative cube). Fast, but an
    /// **under-approximation of the may relation**: sampling only a
    /// subset of inputs (and a single canonical representative per
    /// cube) can MISS a real may-edge → unsound for safety. Preserves
    /// the pre-MIG-3 behaviour exactly.
    #[default]
    Off,
    /// MIG-3 — **sound** all-pairs SMT may-edges. For every
    /// (source-cube, target-cube) pair the lifter runs
    /// [`super::smt_must_edge::smt_per_target_may_check`] (`∃ s⊨src,
    /// inputs, s'⊨tgt. (s,s')∈R`) and emits a `MayOnly` edge iff a
    /// witness exists; an edge is excluded ONLY when Z3 proves it
    /// impossible (UNSAT). This is the authoritative may relation
    /// (`R_may(b,b') ⟺ ∃ s⊨b, s'⊨b'. (s,s')∈R`), a sound
    /// over-approximation that REPLACES the sampling may-edges.
    ///
    /// **Tractability.** All-pairs is O(cubes²) SMT calls — bounded for
    /// the small fixtures the SV migration targets (cubes ≤ 64), but it
    /// scales poorly at the default cube cap (1024² ≈ 10⁶). An all-SMT
    /// per-source enumeration (O(edges), reusing `all_smt`) is the
    /// queued perf follow-up; this MVP ships the sound all-pairs form.
    ///
    /// **Composing with must (P1 #3, IR-unification track).** Combining
    /// with a non-`Off` `must_edge_inference` now composes: the eager
    /// `predicate_cube_lift` computes the canonical ∀∃ KMTS must-relation
    /// via [`crate::adapter::sts_ir::SmtEncode::must_edges`] and promotes
    /// each must-edge `MayOnly` → `Sharp` (every must-edge ⊆ the
    /// SmtAllPairs may-edges by construction). This yields a KMTS with
    /// both may and must edges — the prerequisite for sound DEFINITE
    /// 3-valued verdicts (DR1 F5). The SmtAllPairs path uses the standard
    /// ∀∃ must for any non-Off inference; the per-variant ∀∀ / hyper-must
    /// distinctions remain on the sampling (`!SmtAllPairs`) path.
    SmtAllPairs,
}

/// R.5 lazy KMTS sub-item 2.1 — a single may-edge from
/// [`KmtsLiftLazy::expand_cube`]. The target cube is identified by
/// its 0-based index in the lifter's cube space; the label is the
/// transition's BTOR2-level label name (`"step"` for the R.2.5
/// MVP's single-label edges; future variants may carry more
/// distinguished labels per input combination).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LazyExpansionEdge {
    /// Label name on the may-edge. Matches the labels the eager
    /// `predicate_cube_lift` would emit for the same edge.
    pub label: String,
    /// 0-based cube index of the edge's target.
    pub target_cube: usize,
}

/// R.5 lazy KMTS sub-item 2.1 — trait for on-demand predicate-cube
/// expansion. The eager [`predicate_cube_lift`] materializes every
/// cube + every may-edge before returning; the lazy trait lets the
/// caller (typically R.5's CEGAR loop) drive expansion as the
/// failure-subgame walk explores cubes, so memory grows only with
/// the reachable cube set rather than `2^|P|`.
///
/// **Sub-item 2.1 scope.** This commit ships the trait + a
/// [`NullLazyLift`] stub impl that returns empty edges for every
/// cube (caller treats every cube as terminal). Sub-item 2.2 adds
/// an eager-wrapping impl that backs the lazy API with the
/// existing `predicate_cube_lift` results; sub-item 2.3 adds the
/// truly-lazy impl that computes cube successors on demand via
/// `simulate_one_step_with_uf_rep`. Sub-items 2.4 + 2.5 plumb the
/// trait into the CEGAR loop + add benchmark coverage.
///
/// **Per-method contracts**:
/// - [`Self::cube_count`] returns the total cube count
///   (`2^|predicates|` at the MVP).
/// - [`Self::expand_cube`] returns the may-edges out of the given
///   cube. Implementations may cache; repeat calls on the same
///   cube index must return the same set (sound under
///   determinism).
/// - [`Self::predicates`] returns the predicate set the handle was
///   built with; immutable across the handle's lifetime.
pub trait KmtsLiftLazy {
    /// Total number of cubes in the abstract state space.
    /// Equals `2^|predicates|` at the R.2.5 MVP.
    fn cube_count(&self) -> usize;

    /// Expand a single cube's may-edges. Caller drives exploration;
    /// implementations may memoize. Returns an empty `Vec` for
    /// cubes with no admissible successors (the cube is terminal).
    ///
    /// Out-of-range `cube_index` (>= `cube_count()`) returns an
    /// empty `Vec` — implementations should not panic.
    fn expand_cube(&mut self, cube_index: usize) -> Vec<LazyExpansionEdge>;

    /// The predicate set the handle was built with.
    fn predicates(&self) -> &[PredicateSpec];
}

/// R.5 lazy KMTS sub-item 2.1 — stub implementation of
/// [`KmtsLiftLazy`] that always returns empty edges. Useful for
/// testing the trait shape compiles + for callers that want a
/// cube-state-only abstraction with no dynamics (the legacy R.2.5
/// MVP behaviour pre-may-edge-construction).
#[derive(Debug, Clone)]
pub struct NullLazyLift {
    /// Predicates the handle was built with. Stored for
    /// [`KmtsLiftLazy::predicates`] return.
    pub predicates: Vec<PredicateSpec>,
}

impl NullLazyLift {
    /// Build a `NullLazyLift` with the given predicate set.
    pub fn new(predicates: Vec<PredicateSpec>) -> Self {
        Self { predicates }
    }
}

impl KmtsLiftLazy for NullLazyLift {
    fn cube_count(&self) -> usize {
        1usize << self.predicates.len()
    }

    fn expand_cube(&mut self, _cube_index: usize) -> Vec<LazyExpansionEdge> {
        Vec::new()
    }

    fn predicates(&self) -> &[PredicateSpec] {
        &self.predicates
    }
}

/// R.5 lazy KMTS sub-item 2.2 (2026-06-04) — eager wrapper that
/// satisfies [`KmtsLiftLazy`] by holding a fully-materialized
/// [`PredicateCubeLiftResult`]. All cube + edge work happens at
/// handle construction (via the eager [`predicate_cube_lift`]);
/// [`KmtsLiftLazy::expand_cube`] just walks the pre-computed
/// `Clts::outgoing` for the requested cube.
///
/// **Purpose**: drop-in adapter for callers wanting the lazy API
/// without the lazy performance characteristics yet. Sub-item
/// 2.3 ships the truly-lazy implementation; until then, this
/// wrapper lets the CEGAR loop's R.5 follow-up (sub-item 2.4)
/// migrate to the trait surface incrementally.
///
/// **Memory**: O(2^|P|) cubes × per-cube outgoing transitions —
/// exactly the eager lift's footprint. The lazy savings come
/// from sub-item 2.3.
#[derive(Debug, Clone)]
pub struct EagerLazyLift {
    result: PredicateCubeLiftResult,
}

impl EagerLazyLift {
    /// Wrap a pre-computed `PredicateCubeLiftResult`. Use when
    /// the caller has already invoked `predicate_cube_lift` (e.g.
    /// the CEGAR loop's iteration 0).
    pub fn from_result(result: PredicateCubeLiftResult) -> Self {
        Self { result }
    }

    /// Construct an `EagerLazyLift` directly from a BTOR2 source.
    /// Invokes `predicate_cube_lift` internally; propagates any
    /// adapter errors.
    pub fn from_btor2(
        predicates: Vec<PredicateSpec>,
        btor2_content: &str,
        adapter_options: &crate::adapter::AdapterOptions,
        lift_opts: &PredicateCubeLiftOptions,
    ) -> Result<Self, crate::adapter::AdapterError> {
        let result = predicate_cube_lift(predicates, btor2_content, adapter_options, lift_opts)?;
        Ok(Self::from_result(result))
    }

    /// Access the underlying lift result (Clts + metadata).
    /// Useful when consumers need both the lazy-trait shape AND
    /// the result's auxiliary fields (warnings, lift_time, etc.).
    pub fn result(&self) -> &PredicateCubeLiftResult {
        &self.result
    }
}

impl KmtsLiftLazy for EagerLazyLift {
    fn cube_count(&self) -> usize {
        self.result.cube_count
    }

    fn expand_cube(&mut self, cube_index: usize) -> Vec<LazyExpansionEdge> {
        // Out-of-range: per the trait contract, return empty
        // rather than panicking.
        if cube_index >= self.cube_count() {
            return Vec::new();
        }
        use crate::clts::StateId;
        let Some(src) = StateId::<DefaultStateIdx>::from_index(cube_index) else {
            return Vec::new();
        };
        self.result
            .clts
            .outgoing(src)
            .iter()
            .map(|t| {
                // Resolve each label-id to its first symbol via
                // `Clts::label_payload`. The R.2.5 lifter emits
                // single-symbol single-label transitions
                // (`["step"]`); future multi-symbol cases would
                // need a stable join convention here.
                let label_name = t
                    .labels()
                    .first()
                    .and_then(|lid| self.result.clts.label_payload(*lid))
                    .and_then(|symbols| symbols.first())
                    .cloned()
                    .unwrap_or_else(|| "?unknown_label".to_string());
                LazyExpansionEdge {
                    label: label_name,
                    target_cube: t.target().index(),
                }
            })
            .collect()
    }

    fn predicates(&self) -> &[PredicateSpec] {
        &self.result.predicates
    }
}

/// R.5 lazy KMTS sub-item 2.3 (2026-06-04) — context owned by a
/// `LazyLift` carrying everything needed to compute a single
/// cube's outgoing edges on demand. Built once at handle
/// construction (parses the BTOR2 + collects symbols, register
/// widths, boolean inputs, UF-wrapped Op NIDs); read-only
/// thereafter.
///
/// Currently DUPLICATES the per-cube logic of the eager
/// `predicate_cube_lift` (lines ~741–841). A 2.3-follow-up
/// (likely folded into sub-item 2.4) will refactor
/// `predicate_cube_lift` to use the same helper, removing the
/// duplication. The drift risk is mitigated by the
/// `r5_subitem_23_lazy_lift_edges_match_eager_lift` test which
/// asserts edge-set equality between the two paths.
#[derive(Debug, Clone)]
struct LazyLiftContext {
    file: crate::adapter::btor2::ast::Btor2File,
    pred_register_widths: std::collections::HashMap<String, u32>,
    boolean_inputs: Vec<String>,
    n_inputs: usize,
    n_combos: usize,
    uf_wrapped_nids: std::collections::HashSet<crate::adapter::btor2::ast::Nid>,
    label_name: String,
    predicates: Vec<PredicateSpec>,
    cube_count: usize,
}

/// R.5 lazy KMTS sub-item 2.3 (2026-06-04) — truly-lazy
/// implementation of `KmtsLiftLazy`. `expand_cube` computes
/// a single cube's outgoing edges on demand via
/// `simulate_one_step` / `simulate_one_step_with_uf_rep` and
/// caches the result. Subsequent `expand_cube(cube)` calls for
/// the same cube are O(1) lookups.
///
/// **Memory**: bounded by the number of cubes the caller
/// actually visits. For a `2^|P|`-cube space where only N cubes
/// are reachable from the initial state(s), memory is O(N)
/// rather than O(2^|P|) for `EagerLazyLift`.
///
/// **CPU**: per-cube cost is the same as the eager loop's
/// per-iteration cost. The savings come from never paying for
/// cubes that are never visited.
///
/// **MVP scope**: handles the R.2.5 may-edge construction path
/// (sampling-based, single canonical representative + boolean-
/// input enumeration). Does NOT handle must-edges (R.2.5b's SMT
/// work is required for those). Falls back to empty edge set
/// when the BTOR2 has no boolean inputs OR the predicate set is
/// empty, mirroring the eager lifter's `if lift_opts.max_input_bits
/// > 0 && !predicates.is_empty()` gate.
#[derive(Debug, Clone)]
pub struct LazyLift {
    ctx: LazyLiftContext,
    /// Per-cube cache. Key = cube index; value = the edge set
    /// computed on first `expand_cube` call. Bounded by the
    /// number of unique cubes the caller has visited.
    cache: std::collections::HashMap<usize, Vec<LazyExpansionEdge>>,
}

impl LazyLift {
    /// Build a `LazyLift` from a BTOR2 source + predicate set.
    /// Parses the BTOR2 + collects the context once; subsequent
    /// `expand_cube` calls are per-cube on-demand.
    pub fn from_btor2(
        mut predicates: Vec<PredicateSpec>,
        btor2_content: &str,
        adapter_options: &crate::adapter::AdapterOptions,
        lift_opts: &PredicateCubeLiftOptions,
    ) -> Result<Self, crate::adapter::AdapterError> {
        use crate::adapter::AdapterErrorKind;
        let file =
            crate::adapter::btor2::parser::parse(btor2_content).map_err(|e| AdapterError {
                kind: AdapterErrorKind::ParseError,
                location: None,
                message: format!("adapter/btor2/lazy_lift: parse failed: {}", e.message),
            })?;
        let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
        // P1 #1 (IR-unification track) — resolve predicate register
        // aliases to canonical state-cell names *before* the context
        // stores them, so the lazy per-cube `compute_cube_outgoing_edges`
        // path binds correctly (same fix as the eager `predicate_cube_lift`).
        resolve_predicate_registers(&file, &mut predicates)?;
        // U.4 — compound-predicate backstop. The lazy per-cube body samples
        // simple register==value atoms only and never consults
        // `compound_exprs`, so compounds on the Lazy strategy are rejected
        // here (the shared gate also runs at the `lift_predicate_cube` entry,
        // but this backstop covers any direct `LazyLift::from_btor2` caller).
        // No-op when `compound_exprs` is empty.
        ensure_compound_lift_supported(
            &predicates,
            lift_opts,
            crate::adapter::btor2::cegar::LiftStrategy::Lazy,
        )?;
        let cube_count: usize = 1usize << predicates.len();
        if cube_count > lift_opts.max_cube_count {
            return Err(AdapterError {
                kind: AdapterErrorKind::StateSpaceOverflow,
                location: None,
                message: format!(
                    "adapter/btor2/lazy_lift: cube count 2^{} = {cube_count} exceeds max_cube_count = {}",
                    predicates.len(),
                    lift_opts.max_cube_count
                ),
            });
        }

        // pred_register_widths: needed for the "predicate value == 0
        // → use non-zero representative" edge case (mirrors eager
        // lifter's logic at line ~728).
        let mut pred_register_widths: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for line in &file.lines {
            if let crate::adapter::btor2::ast::Node::State { sort, .. } = &line.node
                && let Some(width) = crate::adapter::btor2::parser::bv_width(&file, *sort)
                && let Some(name) = symbols.get(&line.nid)
            {
                pred_register_widths.insert(name.clone(), width);
            }
        }

        // Boolean inputs + UF-wrapped Op NIDs (mirrors eager
        // lifter's logic at lines ~716, ~494).
        let boolean_inputs: Vec<String> = collect_boolean_input_symbols(&file, &symbols);
        let n_inputs = boolean_inputs.len().min(lift_opts.max_input_bits);
        let n_combos: usize = 1usize << n_inputs;
        let uf_wrapped_nids =
            crate::adapter::btor2::bit_blast::collect_uf_wrapped_nids(&file, adapter_options);

        Ok(Self {
            ctx: LazyLiftContext {
                file,
                pred_register_widths,
                boolean_inputs,
                n_inputs,
                n_combos,
                uf_wrapped_nids,
                label_name: "step".to_string(),
                predicates,
                cube_count,
            },
            cache: std::collections::HashMap::new(),
        })
    }

    /// Number of cubes currently cached.
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    /// R.2.5b session-2 (2026-06-08) — borrow the parsed BTOR2
    /// file. Consumed by [`materialize_clts_from_lazy`]'s
    /// `SmtPerTarget` post-pass, which feeds the file into the
    /// shared [`super::smt_must_edge`] BV-theory encoder.
    pub fn file(&self) -> &crate::adapter::btor2::ast::Btor2File {
        &self.ctx.file
    }
}

impl KmtsLiftLazy for LazyLift {
    fn cube_count(&self) -> usize {
        self.ctx.cube_count
    }

    fn expand_cube(&mut self, cube_index: usize) -> Vec<LazyExpansionEdge> {
        if cube_index >= self.ctx.cube_count {
            return Vec::new();
        }
        if let Some(cached) = self.cache.get(&cube_index) {
            return cached.clone();
        }
        let edges = compute_cube_outgoing_edges(cube_index, &self.ctx);
        self.cache.insert(cube_index, edges.clone());
        edges
    }

    fn predicates(&self) -> &[PredicateSpec] {
        &self.ctx.predicates
    }
}

/// R.5 lazy KMTS sub-item 2.4 (2026-06-04) — materialize a
/// fully-formed `PredicateCubeLiftResult` from a `LazyLift` by
/// visiting every cube in 0..cube_count and emitting one
/// `Transition::MayOnly` per `LazyExpansionEdge`. Identity result
/// with `predicate_cube_lift` (sub-item 2.3's
/// `r5_subitem_23_lazy_lift_edges_match_eager_lift` test
/// asserts the per-cube edge sets agree).
///
/// **Purpose**: bridge for the CEGAR loop's `LiftStrategy::Lazy`
/// path (sub-item 2.4) — until the evaluator gains lazy-handle
/// support, the loop still needs a full `Clts` to call
/// `evaluate_3v_game_with_options`. This helper visits every
/// cube (defeating the laziness payoff for the CEGAR run) but
/// exercises the `LazyLift` machinery end-to-end, surfacing
/// any drift between the lazy + eager per-cube paths as a
/// verdict-equality test failure.
///
/// **Memory caveat**: visiting every cube populates the
/// `LazyLift`'s cache fully (cached_count() == cube_count()
/// after this call). The lazy memory savings only fire when
/// the caller visits a strict subset of cubes — which is what
/// sub-item 2.5's bench fixture will exercise standalone (not
/// via the CEGAR loop).
/// §Phase 10 stage 3.c.2a (2026-06-12) — encode a BTOR2 file for the
/// must-edge SMT post-passes, selecting the theory by whether the
/// design carries memory cells.
///
/// - No array-sorted state cells → `Theory::BvOnly` (pure QF_BV;
///   the pre-stage-3.c.2a behaviour, unchanged for every memory-
///   free fixture).
/// - One or more array-sorted state cells → `Theory::BvUfArray`
///   (QF_AUFBV), so the encoder's stage-3.c.1 `select`/`store`
///   handling produces an array-aware transition relation. This is
///   the **must-side** of the §Phase 10 havoc-may / array-must
///   composition: the must-edge query reasons about memory
///   precisely via Z3 array theory.
///
/// The cube predicates the must-edge query asserts are over BV
/// registers only (memory cells are not in `view.signals`), so the
/// query construction in `smt_must_edge.rs` is unchanged — it just
/// consumes a richer transition relation.
// P0 (IR-unification track) — `pub(crate)` so the STS-IR seam
// (`adapter::sts_ir::BtorSts`) encodes faithfully (memory-aware, array
// theory) rather than via the BvOnly `encode_design`, matching the
// production predicate-cube lift.
pub(crate) fn encode_design_for_lift(
    file: &crate::adapter::btor2::ast::Btor2File,
) -> Result<
    crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
    crate::adapter::sidecar::predicate_image::btor2_encode::EncodeError,
> {
    use crate::adapter::sidecar::predicate_image::btor2_encode::encode_design_with_theory;
    use crate::adapter::sidecar::predicate_image::theory::Theory;
    let theory = if crate::adapter::btor2::bit_blast::detect_btor2_memories(file).is_empty() {
        Theory::BvOnly
    } else {
        Theory::BvUfArray
    };
    encode_design_with_theory(file, theory)
}

/// U.2 (lift-unification, 2026-06-26) — outcome of the shared sampled-target
/// must-edge inference: how many `MayOnly` edges were promoted to `Sharp`, how
/// many `MustHyperOnly` edges were emitted, and the soundness/advisory warnings.
struct MustInferenceOutcome {
    sharp_promoted: usize,
    hyper_emitted: usize,
    warnings: Vec<crate::adapter::AdapterWarning>,
}

/// U.2 — the single shared implementation of the four sampled-target must-edge
/// post-passes (`SmtPerTarget`, `SmtPerTargetStandard`,
/// `SmtHyperMust`), previously duplicated verbatim between the eager
/// `predicate_cube_lift` and the lazy `materialize_clts_from_lazy`. Operates on
/// the already-collected `sampled_targets_per_source` (the may-edge sampling
/// pass's output) and promotes/emits must-edges on `builder`.
///
/// `source_tag` is substituted into the `[R.2.5b-*]`-tagged warning messages so
/// each caller's origin is identifiable (the tag prefixes, which tests assert,
/// are identical across callers).
///
/// `controllability_aware` gates the whole pass off (R.6.6): under
/// controllability-aware mode the promoted edges would carry mismatched
/// single-`step` labels, so the post-passes are skipped (R.6.6.b follow-up).
#[allow(clippy::too_many_arguments)]
fn apply_sampled_must_inference(
    builder: &mut crate::clts::CltsBuilder<DefaultStateIdx, DefaultLabelIdx>,
    state_ids: &[crate::clts::StateId<DefaultStateIdx>],
    sampled_targets_per_source: &[std::collections::BTreeSet<usize>],
    file: &crate::adapter::btor2::ast::Btor2File,
    predicates: &[PredicateSpec],
    label_id: crate::clts::LabelId<DefaultLabelIdx>,
    must_edge_inference: MustEdgeInference,
    controllability_aware: bool,
    source_tag: &str,
) -> MustInferenceOutcome {
    // Only the ∀∀ `SmtPerTarget` post-pass still uses a raw `smt_must_edge`
    // primitive; the ∀∃ (`SmtPerTargetStandard`) and hyper (`SmtHyperMust`)
    // passes route through the STS-IR seam (AR-S2).
    use crate::adapter::btor2::smt_must_edge::{
        SmtMustVerdict, build_register_nid_map, smt_per_target_must_check,
        smt_source_cube_proven_feasible,
    };
    let mut warnings: Vec<crate::adapter::AdapterWarning> = Vec::new();
    // R.6.6 gate — skip all post-passes under controllability-aware mode.
    if controllability_aware {
        return MustInferenceOutcome {
            sharp_promoted: 0,
            hyper_emitted: 0,
            warnings,
        };
    }
    let approx = crate::adapter::WarningKind::ApproximateTranslation;

    // SOUNDNESS (must ⊆ may fix, Tier 0) — a source cube whose predicate
    // conjunction is UNSATISFIABLE has no concrete state, yet every must-check
    // below fabricates a vacuous `Must` out of it (an empty `src` makes
    // `src ∧ trans ∧ ¬tgt` trivially UNSAT). That `Sharp` edge breaks the KMTS
    // invariant `R_must ⊆ R_may` and is the root of the false-`Violated` /
    // `TritBdd` panic on `$past` models (past-shadow-soundness.md §6) — a cube
    // like `din == V ∧ din__past == V'` with contradictory dimensions on one
    // register is empty yet promotes a must-edge. Prove each candidate source
    // cube feasible ONCE (one shared encode + z3 scope) and drop must-promotion
    // for the proven-empty ones. `Off` promotes nothing, so it needs no check.
    let infeasible_sources: std::collections::HashSet<usize> =
        if matches!(must_edge_inference, MustEdgeInference::Off) {
            std::collections::HashSet::new()
        } else {
            let cfg = z3::Config::new();
            z3::with_z3_config(&cfg, || -> std::collections::HashSet<usize> {
                let Ok(view) = encode_design_for_lift(file) else {
                    // Encoder failure — cannot prove any cube empty. The must-checks
                    // below re-encode and likewise promote nothing on failure, so an
                    // empty exclusion set is consistent (nothing to gate).
                    return std::collections::HashSet::new();
                };
                let nid_map = build_register_nid_map(&view);
                sampled_targets_per_source
                    .iter()
                    .enumerate()
                    .filter(|(_, targets)| !targets.is_empty())
                    .filter(|(src, _)| {
                        !smt_source_cube_proven_feasible(
                            &view,
                            *src as u64,
                            predicates,
                            &nid_map,
                            5_000,
                        )
                    })
                    .map(|(src, _)| src)
                    .collect()
            })
        };

    match must_edge_inference {
        MustEdgeInference::Off => MustInferenceOutcome {
            sharp_promoted: 0,
            hyper_emitted: 0,
            warnings,
        },
        // SmtPerTarget — ∀∀ form (stronger than canonical KMTS) per
        // (src, sampled-target). SOUNDNESS: SMT-proved.
        MustEdgeInference::SmtPerTarget => {
            let cfg = z3::Config::new();
            let sharp = z3::with_z3_config(&cfg, || -> usize {
                let Ok(view) = encode_design_for_lift(file) else {
                    return 0;
                };
                let nid_map = build_register_nid_map(&view);
                let mut promoted = 0usize;
                for (src_idx, targets) in sampled_targets_per_source.iter().enumerate() {
                    if targets.is_empty() {
                        continue;
                    }
                    // Fix B — no vacuous must-edge out of a proven-empty cube.
                    if infeasible_sources.contains(&src_idx) {
                        continue;
                    }
                    let src_id = state_ids[src_idx];
                    for &tgt_idx in targets {
                        if matches!(
                            smt_per_target_must_check(
                                &view,
                                src_idx as u64,
                                tgt_idx as u64,
                                predicates,
                                &nid_map,
                                5_000,
                            ),
                            SmtMustVerdict::Must
                        ) {
                            builder.transition_ids_with_modality(
                                src_id,
                                &[label_id],
                                state_ids[tgt_idx],
                                crate::clts::TransitionModality::Sharp,
                            );
                            promoted += 1;
                        }
                    }
                }
                promoted
            });
            if sharp > 0 {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: approx,
                    message: format!(
                        "[R.2.5b-smt-must] {source_tag}: SmtPerTarget post-pass promoted {sharp} edge(s) to Sharp via Z3 BV-theory must-check (stronger ∀∀ form). The standard ∀∃ form may promote additional edges; see `MustEdgeInference::SmtPerTargetStandard`. Hyper-must inference: see `MustEdgeInference::SmtHyperMust`."
                    ),
                    location: None,
                });
            }
            MustInferenceOutcome {
                sharp_promoted: sharp,
                hyper_emitted: 0,
                warnings,
            }
        }
        // SmtPerTargetStandard — ∀∃ form (canonical KMTS must, Bruns–Godefroid).
        // AR-S2 — routed through the STS-IR seam's single ∀∃ predicate-image
        // (`SmtEncode::must_edges_over`), applied to exactly the sampled
        // candidate pairs (so laziness is preserved). The seam builds the
        // encode + primed cache once and runs the uniform must-check —
        // behaviour-identical to the per-kind `smt_per_target_must_check_standard`
        // on state cube dimensions — collapsing the default path's ∀∃ must-image
        // into the seam's. Differential-guarded: `diff_corpus_cegar_vs_symbolic_
        // engine_parity` cross-checks every cube definite against the exact oracle.
        MustEdgeInference::SmtPerTargetStandard => {
            use crate::adapter::sts_ir::{BtorSts, SmtEncode};
            let candidates: Vec<(usize, usize)> = sampled_targets_per_source
                .iter()
                .enumerate()
                // Fix B — exclude proven-empty source cubes (vacuous ∀∃ must).
                .filter(|(src, _)| !infeasible_sources.contains(src))
                .flat_map(|(src, targets)| targets.iter().map(move |&t| (src, t)))
                .collect();
            let must = BtorSts::new(file).must_edges_over(predicates, &candidates, 5_000);
            let sharp = must.len();
            for &(src_idx, tgt_idx) in &must {
                builder.transition_ids_with_modality(
                    state_ids[src_idx],
                    &[label_id],
                    state_ids[tgt_idx],
                    crate::clts::TransitionModality::Sharp,
                );
            }
            if sharp > 0 {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: approx,
                    message: format!(
                        "[R.2.5b-smt-must-standard] {source_tag}: SmtPerTargetStandard post-pass promoted {sharp} edge(s) to Sharp via Z3 ∀∃ must-check (canonical KMTS form per Bruns–Godefroid CONCUR 2000). Hyper-must inference: see `MustEdgeInference::SmtHyperMust`."
                    ),
                    location: None,
                });
            }
            MustInferenceOutcome {
                sharp_promoted: sharp,
                hyper_emitted: 0,
                warnings,
            }
        }
        // SmtHyperMust — per-target ∀∃ singletons first; a full-set hyper-must
        // only for sources where no singleton promoted.
        //
        // AR-S2 follow-up — routed entirely through the STS-IR seam: the ∀∃
        // singletons via `SmtEncode::must_edges_over` (the same single ∀∃ image
        // `SmtPerTargetStandard` uses), the hyper-set via `SmtEncode::hyper_must_edges`
        // (the uniform hyper). `sampled_targets_per_source` is a `BTreeSet`, so its
        // iteration and the seam's sorted may-successor set agree on the target set
        // AND the primary target (`target_ids[0]`). This retires the last production
        // callers of `smt_per_target_must_check_standard` + `smt_hyper_must_check`.
        // Differential-guarded (exact-oracle cross-check on the corpus).
        MustEdgeInference::SmtHyperMust => {
            use crate::adapter::sts_ir::{BtorSts, SmtEncode};
            let sts = BtorSts::new(file);

            // 1. ∀∃ singletons over every sampled candidate pair.
            let singleton_candidates: Vec<(usize, usize)> = sampled_targets_per_source
                .iter()
                .enumerate()
                // Fix B — exclude proven-empty source cubes (vacuous ∀∃ must).
                .filter(|(src, _)| !infeasible_sources.contains(src))
                .flat_map(|(src, targets)| targets.iter().map(move |&t| (src, t)))
                .collect();
            let singleton_musts = sts.must_edges_over(predicates, &singleton_candidates, 5_000);
            let sharp = singleton_musts.len();
            let mut promoted_srcs: std::collections::HashSet<usize> =
                std::collections::HashSet::new();
            for &(src_idx, tgt_idx) in &singleton_musts {
                builder.transition_ids_with_modality(
                    state_ids[src_idx],
                    &[label_id],
                    state_ids[tgt_idx],
                    crate::clts::TransitionModality::Sharp,
                );
                promoted_srcs.insert(src_idx);
            }

            // 2. Full-set hyper-must only for un-promoted, multi-target sources —
            //    feed only their sampled edges as the may-relation.
            let hyper_may: Vec<(usize, usize)> = sampled_targets_per_source
                .iter()
                .enumerate()
                .filter(|(src, targets)| {
                    !promoted_srcs.contains(src)
                        && targets.len() > 1
                        // Fix B — exclude proven-empty source cubes (vacuous hyper-must).
                        && !infeasible_sources.contains(src)
                })
                .flat_map(|(src, targets)| targets.iter().map(move |&t| (src, t)))
                .collect();
            let hyper_edges = sts.hyper_must_edges(predicates, &hyper_may, 5_000);
            let hyper = hyper_edges.len();
            for (src_idx, targets) in &hyper_edges {
                let target_ids: smallvec::SmallVec<[crate::clts::StateId<DefaultStateIdx>; 4]> =
                    targets.iter().map(|&i| state_ids[i]).collect();
                builder.transition_ids_with_modality(
                    state_ids[*src_idx],
                    &[label_id],
                    target_ids[0],
                    crate::clts::TransitionModality::must_hyper(target_ids.clone()),
                );
            }

            if sharp > 0 || hyper > 0 {
                warnings.push(crate::adapter::AdapterWarning {
                    kind: approx,
                    message: format!(
                        "[R.2.5b-smt-must-hyper] {source_tag}: SmtHyperMust post-pass promoted {sharp} edge(s) to Sharp + emitted {hyper} MustHyperOnly edge(s) via Z3 ∀∃ checks. Hyper-must targets = full sampled set (MVP doesn't minimize T)."
                    ),
                    location: None,
                });
            }
            MustInferenceOutcome {
                sharp_promoted: sharp,
                hyper_emitted: hyper,
                warnings,
            }
        }
    }
}

/// SOUNDNESS (must ⊆ may fix, Tier 0) — validate the KMTS invariant
/// `R_must ⊆ R_may` on a freshly-lifted CLTS, ALWAYS (release included), not only
/// under the debug-`assert!` in [`crate::mu_calculus::symbolic::TritBdd::from_parts`].
///
/// The 3-valued modal operators are sound only when every must-edge is also a
/// may-edge (Bruns–Godefroid; see `symbolic.rs` §5 and past-shadow-soundness.md
/// §5). Both evaluators build `R_may` from EVERY outgoing edge and `R_must` from
/// `Sharp` ∪ `MustHyperOnly` edges (`symbolic.rs:357-377`, `evaluator.rs:20-56`),
/// so a `Sharp` edge is trivially a may-edge and cannot break containment on its
/// own. The one relation-level obligation that can fail is a
/// `MustHyperOnly(src → T)` whose target set `T` is not covered by `src`'s
/// may-successors — a hyper-must built over a candidate set that is not a subset
/// of the emitted may-relation. Left unchecked it panics `TritBdd::from_parts` in
/// debug and silently computes a wrong verdict in release. This runs once per lift
/// (O(edges), SMT-free) and turns that into an honest `IrConsistencyError`.
fn assert_must_subset_may(
    clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
) -> Result<(), AdapterError> {
    use crate::clts::TransitionModality;
    for sid in clts.states() {
        // R_may successors of `sid` = the targets of ALL its outgoing edges
        // (every edge contributes to R_may in both evaluators).
        let may_succ: std::collections::HashSet<usize> = clts
            .outgoing(sid)
            .iter()
            .map(|t| t.target().index())
            .collect();
        for t in clts.outgoing(sid) {
            if let TransitionModality::MustHyperOnly(targets) = t.modality() {
                for tgt in targets.iter() {
                    if !may_succ.contains(&tgt.index()) {
                        return Err(AdapterError {
                            kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                            location: None,
                            message: format!(
                                "adapter/btor2/kmts_lift: KMTS invariant R_must ⊆ R_may violated — \
                                 the MustHyperOnly edge from state {} names hyper-target {} which is \
                                 not a may-successor of it. A hyper-must target set must be a subset \
                                 of the emitted may-relation (docs/design/past-shadow-soundness.md §6).",
                                sid.index(),
                                tgt.index()
                            ),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn materialize_clts_from_lazy(
    lazy: &mut LazyLift,
    btor2_source_info_format: SourceFormat,
    must_edge_inference: MustEdgeInference,
) -> Result<PredicateCubeLiftResult, AdapterError> {
    use crate::adapter::AdapterErrorKind;
    use crate::clts::TransitionModality;

    let start = Instant::now();
    let cube_count = lazy.cube_count();
    let predicates = lazy.predicates().to_vec();
    let mut warnings: Vec<crate::adapter::AdapterWarning> = Vec::new();
    let mut sharp_edges_promoted: usize = 0;
    let mut hyper_must_edges_emitted: usize = 0;
    // R.2.5b session-1 follow-up — collect per-source target sets
    // (the sampled must-edge candidates) for the SMT must-edge
    // post-pass, same shape as the eager predicate_cube_lift path.
    let mut sampled_targets_per_source: Vec<std::collections::BTreeSet<usize>> =
        vec![std::collections::BTreeSet::new(); cube_count];

    // Build the Clts shape. Mirrors `predicate_cube_lift`'s
    // builder pattern (lines ~563–594 in this file).
    let mut builder = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
    let mut state_ids = Vec::with_capacity(cube_count);
    for i in 0..cube_count {
        let name = format!("cube_{i}");
        let id = builder
            .state_id_or_insert(&name)
            .ok_or_else(|| AdapterError {
                kind: AdapterErrorKind::StateSpaceOverflow,
                location: None,
                message: format!(
                    "adapter/btor2/materialize_clts_from_lazy: state id overflow at cube {i} / {cube_count}"
                ),
            })?;
        state_ids.push(id);
    }
    if let Some(initial) = state_ids.first() {
        builder.initial_state_id(*initial);
    }

    // Populate state_3valued_predicates per cube (same bit-pattern
    // convention as predicate_cube_lift line ~587).
    for (i, &state_id) in state_ids.iter().enumerate() {
        for (bit, pred) in predicates.iter().enumerate() {
            let verdict = if (i >> bit) & 1 == 1 {
                Tristate::KleeneT
            } else {
                Tristate::KleeneF
            };
            builder.with_3valued_predicate(state_id, &pred.name, verdict);
        }
    }

    // Intern the label store with the "step" label once, then
    // visit every cube + emit a MayOnly edge per
    // LazyExpansionEdge. The lazy helper dedupes target cubes
    // per source so we don't emit duplicate edges.
    let label_id = builder
        .labels()
        .intern(["step"])
        .map_err(|e| AdapterError {
            kind: AdapterErrorKind::IrConsistencyError,
            location: None,
            message: format!("adapter/btor2/materialize_clts_from_lazy: label intern failed: {e}"),
        })?;
    for i in 0..cube_count {
        let src_id = state_ids[i];
        for edge in lazy.expand_cube(i) {
            if edge.target_cube < state_ids.len() {
                let tgt_id = state_ids[edge.target_cube];
                builder.transition_ids_with_modality(
                    src_id,
                    &[label_id],
                    tgt_id,
                    TransitionModality::MayOnly,
                );
                sampled_targets_per_source[i].insert(edge.target_cube);
            }
        }
    }

    // U.2 — shared sampled-target must-edge inference (was duplicated here +
    // in predicate_cube_lift). Lazy is not controllability-aware (gate = false).
    let must_outcome = apply_sampled_must_inference(
        &mut builder,
        &state_ids,
        &sampled_targets_per_source,
        lazy.file(),
        &predicates,
        label_id,
        must_edge_inference,
        false,
        "materialize_clts_from_lazy",
    );
    sharp_edges_promoted += must_outcome.sharp_promoted;
    hyper_must_edges_emitted += must_outcome.hyper_emitted;
    warnings.extend(must_outcome.warnings);

    let clts = builder.build().map_err(|e| AdapterError {
        kind: AdapterErrorKind::IrConsistencyError,
        location: None,
        message: format!("adapter/btor2/materialize_clts_from_lazy: builder.build failed: {e}"),
    })?;

    // Fix C — enforce R_must ⊆ R_may before the KMTS reaches any evaluator.
    assert_must_subset_may(&clts)?;

    Ok(PredicateCubeLiftResult {
        clts,
        predicates,
        cube_count,
        source_info: SourceInfo {
            format: btor2_source_info_format,
            title: None,
            // Approximated; the precise `signal_count`
            // requires re-parsing the BTOR2 which we've
            // already done inside LazyLift. For sub-item 2.4
            // MVP, surface 0 (the caller doesn't use this
            // field for the Lazy path's CEGAR verdict).
            signal_count: 0,
            state_count: cube_count,
            property_count: 0,
        },
        lift_time: start.elapsed(),
        // Same caveat as predicate_cube_lift — may-edges are
        // populated; must-edges arrive via the SMT must-edge
        // post-pass (SmtPerTargetStandard / SmtHyperMust) when opted in.
        predicate_image_pending: false,
        // Warnings include UF-wrap (currently absent from lazy
        // path) + the R.2.5b SMT must-edge warning when the post-pass
        // fires.
        warnings,
        sharp_edges_promoted,
        hyper_must_edges_emitted,
    })
}

/// U.3 (lift-unification, 2026-06-26) — descriptor for the label-set a
/// sampled edge carries, returned by [`cube_sampling_edges`] so each
/// caller interns labels its own way.
///
/// - `Step` — the single canonical `step` label (the non-controllability
///   case + every lazy edge). The eager caller maps it to its interned
///   `step` `LabelId`; the lazy caller maps it to its `step`-named
///   [`LazyExpansionEdge`].
/// - `Controllability { env_combo, ctrl_combo }` — the dual-label
///   (env, ctrl) projection of the input combo (R.6.6). The eager caller
///   maps it to `[env_label_ids[env_combo], ctrl_label_ids[ctrl_combo]]`
///   (omitting an axis whose index slice is empty). Only the eager,
///   controllability-aware path produces this variant.
///
/// Derives `Hash`/`Eq`/`Copy` so [`cube_sampling_edges`] can dedup the
/// returned set by `(target, descriptor)` — the unifying insight that
/// reproduces BOTH the lazy (target-only) and eager (builder-merge-on-
/// `(src, labels, tgt)`) effective edge sets from one body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LabelDescriptor {
    Step,
    Controllability { env_combo: usize, ctrl_combo: usize },
}

/// U.3 (lift-unification, 2026-06-26) — borrowed context for the shared
/// per-cube sampling body [`cube_sampling_edges`]. Borrowed (not owned)
/// because the eager [`predicate_cube_lift`] CANNOT surrender its `file`:
/// it is also borrowed by the SmtAllPairs may-path and the U.2
/// [`apply_sampled_must_inference`] must-pass. The lazy [`LazyLift`]
/// builds it borrowing from its owned [`LazyLiftContext`]; the eager path
/// builds it borrowing from its locals.
///
/// `pred_register_widths` keys are `String` (not `&str`) so the eager and
/// lazy maps share one borrowed type — the eager path was migrated from
/// `&str` keys to `String` keys in U.3 for exactly this.
struct CubeSamplingCtx<'a> {
    file: &'a crate::adapter::btor2::ast::Btor2File,
    pred_register_widths: &'a std::collections::HashMap<String, u32>,
    boolean_inputs: &'a [String],
    n_inputs: usize,
    n_combos: usize,
    uf_wrapped_nids: &'a std::collections::HashSet<crate::adapter::btor2::ast::Nid>,
    predicates: &'a [PredicateSpec],
    cube_count: usize,
    /// Raw input-bit indices (into `boolean_inputs[..n_inputs]`) that are
    /// uncontrollable (env). Empty for the non-controllability case.
    env_input_indices: &'a [usize],
    /// Raw input-bit indices that are controllable (ctrl). Empty for the
    /// non-controllability case.
    ctrl_input_indices: &'a [usize],
    /// When true, each sampled edge's descriptor is
    /// `Controllability { env_combo, ctrl_combo }`; when false, every
    /// descriptor is `Step`. The lazy path always passes false.
    controllability_aware: bool,
}

/// U.3 (lift-unification, 2026-06-26) — the ONE per-cube sampling body,
/// shared by the eager [`predicate_cube_lift`] sampling loop and the lazy
/// [`compute_cube_outgoing_edges`]. Computes, for one `cube_index`: the
/// canonical representative register assignment, then for each boolean-
/// input combo (and each UF representative when Ops are UF-wrapped)
/// simulates one step and re-evaluates the predicates to a target cube.
///
/// Returns `(target_cube, LabelDescriptor)` pairs **deduped by
/// `(target, descriptor)`**. This dedup reproduces both effective edge
/// sets: the lazy path (descriptor always `Step` ⟹ dedup by target) and
/// the eager path (the builder's `(src, labels, tgt)` merge — combo↔
/// `(env_combo, ctrl_combo)` is a bijection, so distinct labels never
/// collide and the only collapse is the same target via UF snapshots,
/// which the builder also merges).
///
/// Returns empty Vec when the predicate set is empty (matches both
/// callers' prior gating) or when every `simulate_one_step` invocation
/// errors for this cube.
fn cube_sampling_edges(cube_index: usize, ctx: &CubeSamplingCtx) -> Vec<(usize, LabelDescriptor)> {
    // mununu#503 — the `ctx.predicates.is_empty()` early return is GONE. With zero dimensions
    // there is one cube holding every concrete state, and the canonical representative below
    // still simulates a step and maps back to cube 0, yielding the self-loop the KMTS must have.
    // Returning empty here produced an edgeless cell that `downgrade_unsatisfiable_cells` then
    // masked to ⊥. `n_inputs == 0` (n_combos == 1) was already allowed — a single iteration with
    // empty `input_values` — and that is exactly the shape this case needs.

    // Build canonical representative for cube_index.
    let mut registers: std::collections::HashMap<String, u128> = std::collections::HashMap::new();
    for (bit, pred) in ctx.predicates.iter().enumerate() {
        let truth = (cube_index >> bit) & 1 == 1;
        let entry = registers.entry(pred.register.clone()).or_insert(0);
        if truth {
            *entry = pred.value as u128;
        }
        // Predicate-false case leaves *entry at 0. If the predicate's
        // value is also 0 (i.e. predicate is `register == 0`), the false
        // case needs a non-zero representative; bump to 1 of the
        // appropriate width.
        if !truth && pred.value == 0 {
            let width = ctx
                .pred_register_widths
                .get(pred.register.as_str())
                .copied()
                .unwrap_or(1);
            if width >= 1 {
                *entry = 1;
            }
        }
    }

    let mut edges: Vec<(usize, LabelDescriptor)> = Vec::new();
    let mut seen: std::collections::HashSet<(usize, LabelDescriptor)> =
        std::collections::HashSet::new();

    // Enumerate input combinations + emit one (target, descriptor) per
    // newly-seen pair. Dedup is by (target, descriptor) so the eager
    // controllability dual-labels and the lazy/eager single-step labels
    // both reproduce their prior effective edge sets.
    for combo in 0..ctx.n_combos {
        let mut input_values: std::collections::HashMap<String, u128> =
            std::collections::HashMap::new();
        for (bit, name) in ctx.boolean_inputs.iter().take(ctx.n_inputs).enumerate() {
            let v = if (combo >> bit) & 1 == 1 { 1 } else { 0 };
            input_values.insert(name.clone(), v);
        }

        // R.5b multi-value UF enumeration — when at least one Op is
        // UF-wrapped, enumerate `UfRepresentative::{Zero, Ones}`; else
        // take the plain single step to keep the no-UF path on its
        // existing performance profile.
        //
        // AR-S2 — the no-UF concrete step routes through the STS-IR seam
        // (`StepEval::step` on `BtorSts`, which delegates to the shared
        // `simulate_one_step_observe` primitive; its `next_state` is
        // documented behaviour-identical to `simulate_one_step`). The seam
        // does not yet model the UF-representative step, so the UF branch
        // stays on `simulate_one_step_with_uf_rep` (a follow-up sub-item).
        let next_register_snapshots: Vec<std::collections::HashMap<String, u128>> =
            if ctx.uf_wrapped_nids.is_empty() {
                use crate::adapter::sts_ir::{BtorSts, StepEval};
                match BtorSts::new(ctx.file).step(&registers, &input_values) {
                    Ok(v) => vec![v],
                    Err(_) => continue,
                }
            } else {
                use crate::adapter::btor2::bit_blast::UfRepresentative;
                let mut snaps = Vec::with_capacity(2);
                for rep in [UfRepresentative::Zero, UfRepresentative::Ones] {
                    match crate::adapter::btor2::bit_blast::simulate_one_step_with_uf_rep(
                        ctx.file,
                        &registers,
                        &input_values,
                        ctx.uf_wrapped_nids,
                        rep,
                    ) {
                        Ok(v) => snaps.push(v),
                        Err(_) => continue,
                    }
                }
                snaps
            };

        // Descriptor for this combo. controllability_aware ⟹ project the
        // combo onto its (env_combo, ctrl_combo); else the single `step`.
        let descriptor = if ctx.controllability_aware {
            let mut env_c: usize = 0;
            for (slot, &raw_idx) in ctx.env_input_indices.iter().enumerate() {
                if (combo >> raw_idx) & 1 == 1 {
                    env_c |= 1 << slot;
                }
            }
            let mut ctrl_c: usize = 0;
            for (slot, &raw_idx) in ctx.ctrl_input_indices.iter().enumerate() {
                if (combo >> raw_idx) & 1 == 1 {
                    ctrl_c |= 1 << slot;
                }
            }
            LabelDescriptor::Controllability {
                env_combo: env_c,
                ctrl_combo: ctrl_c,
            }
        } else {
            LabelDescriptor::Step
        };

        for next_registers in &next_register_snapshots {
            let mut target_index: usize = 0;
            for (bit, pred) in ctx.predicates.iter().enumerate() {
                let next_v = next_registers.get(&pred.register).copied().unwrap_or(0);
                if next_v == pred.value as u128 {
                    target_index |= 1 << bit;
                }
            }
            if target_index < ctx.cube_count && seen.insert((target_index, descriptor)) {
                edges.push((target_index, descriptor));
            }
        }
    }

    edges
}

/// R.5 lazy KMTS sub-item 2.3 (2026-06-04) — compute one cube's outgoing
/// may-edges. U.3 (2026-06-26): now a thin wrapper over the shared
/// [`cube_sampling_edges`]. The lazy path is never controllability-aware
/// (empty env/ctrl index slices + `controllability_aware = false`), so
/// every descriptor comes back `Step` and maps onto a single-`step`-
/// labelled [`LazyExpansionEdge`]. The eager [`predicate_cube_lift`]
/// drives the identical body — U.1's differential test enforces they
/// can't diverge.
///
/// Returns empty Vec when:
/// - The predicate set is empty (matches the eager lifter's gating).
/// - All `simulate_one_step` invocations error for this cube
///   (e.g. BTOR2 references registers the lifter can't resolve).
fn compute_cube_outgoing_edges(cube_index: usize, ctx: &LazyLiftContext) -> Vec<LazyExpansionEdge> {
    let sampling_ctx = CubeSamplingCtx {
        file: &ctx.file,
        pred_register_widths: &ctx.pred_register_widths,
        boolean_inputs: &ctx.boolean_inputs,
        n_inputs: ctx.n_inputs,
        n_combos: ctx.n_combos,
        uf_wrapped_nids: &ctx.uf_wrapped_nids,
        predicates: &ctx.predicates,
        cube_count: ctx.cube_count,
        env_input_indices: &[],
        ctrl_input_indices: &[],
        controllability_aware: false,
    };
    cube_sampling_edges(cube_index, &sampling_ctx)
        .into_iter()
        .map(|(target_cube, _descriptor)| LazyExpansionEdge {
            label: ctx.label_name.clone(),
            target_cube,
        })
        .collect()
}

/// R.2.5 — Result of lifting one BTOR2 source through the predicate-
/// cube path. Carries a `Clts` whose state count is bounded by 2^|P|
/// (NOT 2^|Registers|), plus the predicate set and timing
/// information for the §10.1 R.2.5 binary capability check.
#[derive(Debug, Clone)]
pub struct PredicateCubeLiftResult {
    /// Abstract state space — `Clts` whose state count equals the
    /// number of satisfiable predicate cubes. At the R.2.5 MVP every
    /// cube is treated as satisfiable (no SMT check yet); the Clts
    /// has no transitions (R.5's `KmtsLiftLazy` populates them).
    pub clts: Clts<DefaultStateIdx, DefaultLabelIdx>,
    /// The predicate set the lifter consumed, in the order the cube
    /// bits index. `predicates[i]` controls bit i of each cube.
    pub predicates: Vec<PredicateSpec>,
    /// Total number of cubes the lifter materialized (`2^predicates.len()`
    /// at the MVP; future iterations may prune via SMT-satisfiability).
    pub cube_count: usize,
    /// Source-info metadata mirroring `AdapterOutput.source_info` for
    /// downstream consumers (e.g. CLI summary).
    pub source_info: SourceInfo,
    /// Wall-clock duration of the lift. The §10.1 R.2.5 done-criterion
    /// requires this to be < 10 s for the cap-exceeding fixture.
    pub lift_time: Duration,
    /// **Always `true` at R.2.5.** Flags that the must/may transition
    /// relation has not been populated — the Clts has the cube state
    /// set but no edges. R.5's `KmtsLiftLazy` shipping closes this
    /// debt; consumers that need verdicts on the cube space must wait
    /// for that integration.
    pub predicate_image_pending: bool,
    /// R.5b follow-up — Adapter warnings the lift emitted (e.g. UF
    /// wrapping naming). Replaces the prior tracing::warn!-only
    /// surface so callers (e.g. cegar.rs) can fold lift warnings
    /// into their own reporting (CegarTrace, CLI verdict-with-
    /// warnings, etc.). Empty for fixtures that triggered no
    /// warning-worthy abstraction decisions.
    pub warnings: Vec<crate::adapter::AdapterWarning>,
    /// R.2.5b (2026-06-06) — Number of `MayOnly` edges promoted to
    /// `Sharp` by the must-edge post-pass (the SMT ∀∃
    /// [`MustEdgeInference::SmtPerTargetStandard`] /
    /// [`MustEdgeInference::SmtHyperMust`] singleton passes). Zero
    /// when `must_edge_inference` is `Off` (the default) or when no
    /// must-edge was proved.
    pub sharp_edges_promoted: usize,
    /// R.2.5b (2026-06-06) — Number of `MustHyperOnly` edges
    /// emitted by the [`MustEdgeInference::SmtHyperMust`] post-pass.
    /// Zero when `must_edge_inference` is `Off` or when no
    /// hyper-must was proved.
    pub hyper_must_edges_emitted: usize,
}

/// P1 #1 (IR-unification track) — resolve every predicate's `register`
/// to the **canonical state-cell name** the SMT predicate-image and
/// `simulate_one_step` both bind against, via the STS-IR seam
/// ([`crate::adapter::sts_ir::BtorSts::resolve_register`]).
///
/// Three outcomes per predicate:
/// - **Direct hit** — the name is already a canonical state-cell symbol
///   (present in the BTOR2 symbol table). Kept verbatim.
/// - **Alias** — the name survives only on a `uext` / `Output` node
///   because Yosys' `flatten` stripped the symbol off the `state` line
///   (the uart_tx `bit_cnt_q` → `bit_cnt_d` case, the DR1 #1 blocker).
///   `resolve_register` BFS-walks to the nearest state cell and the name
///   is rewritten to that cell's canonical symbol, so every downstream
///   bind — the `registers` map fed to `simulate_one_step`, the
///   `next_registers` readback, `pred_register_widths`, and the SMT
///   `build_register_nid_map` — keys correctly.
/// - **Unresolvable** — resolves to no state cell. Errors, naming the
///   unknown register (matches the pre-P1 validation behaviour).
///
/// Both the eager [`predicate_cube_lift`] and the lazy
/// [`LazyLift::from_btor2`] call this so the resolution is shared across
/// the two lift strategies CEGAR routes between.
fn resolve_predicate_registers(
    file: &crate::adapter::btor2::ast::Btor2File,
    predicates: &mut [PredicateSpec],
) -> Result<(), AdapterError> {
    use crate::adapter::sts_ir::SymbolicTransitionSystem;
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let known: std::collections::HashSet<&String> = symbols.values().collect();
    let sts = crate::adapter::sts_ir::BtorSts::new(file);
    for pred in predicates.iter_mut() {
        if known.contains(&pred.register) {
            continue;
        }
        match sts.resolve_register(&pred.register) {
            Some(canonical) => {
                tracing::debug!(
                    predicate = %pred.name,
                    alias = %pred.register,
                    canonical = %canonical,
                    "predicate_cube_lift: resolved alias register name to canonical state cell"
                );
                pred.register = canonical;
            }
            None => {
                return Err(AdapterError {
                    kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                    location: None,
                    message: format!(
                        "adapter/btor2/predicate_cube_lift: predicate `{}` references unknown register `{}` (known: {:?})",
                        pred.name,
                        pred.register,
                        symbols.values().collect::<std::collections::BTreeSet<_>>()
                    ),
                });
            }
        }
    }
    Ok(())
}

/// U.4 (lift-unification, 2026-06-26) — the compound-predicate soundness
/// gate, shared by the [`lift_predicate_cube`] entry and the defensive
/// copies in [`predicate_cube_lift`] + [`LazyLift::from_btor2`] (for the
/// ~39 direct callers that bypass the entry).
///
/// Compound predicates (a `compound_exprs` entry keyed by predicate name)
/// can ONLY be honoured by the eager SmtAllPairs may-relation: the
/// sampling representative inverse can't realise e.g. `a==0 && b==0`, and
/// the lazy per-cube body ([`cube_sampling_edges`]) samples simple
/// `register==value` atoms only — it never consults `compound_exprs`. So
/// when `compound_exprs` is non-empty this gate enforces:
///
/// 1. every key names a predicate in the set (else the expr is orphaned);
/// 2. `strategy == Eager` (the lazy path can't honour compounds at all);
/// 3. `may_edge_inference == SmtAllPairs` (sampling is simple-atom-only;
///    the SMT seam honours compounds via `PredicateLike::expr`).
///
/// No-op (`Ok`) when `compound_exprs` is empty — the simple-atom path is
/// unaffected.
fn ensure_compound_lift_supported(
    predicates: &[PredicateSpec],
    lift_opts: &PredicateCubeLiftOptions,
    strategy: crate::adapter::btor2::cegar::LiftStrategy,
) -> Result<(), AdapterError> {
    use crate::adapter::btor2::cegar::LiftStrategy;
    if lift_opts.compound_exprs.is_empty() {
        return Ok(());
    }
    // A compound_exprs key may name either a cube DIMENSION (`predicates`) or —
    // H.F — a relational DERIVED label (`derived_predicates`, labelled per cube,
    // not a cube index bit). Both legitimately carry a `PredicateExpr`.
    let names: std::collections::HashSet<&str> = predicates
        .iter()
        .chain(lift_opts.derived_predicates.iter())
        .map(|p| p.name.as_str())
        .collect();
    for key in lift_opts.compound_exprs.keys() {
        if !names.contains(key.as_str()) {
            return Err(AdapterError {
                kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                location: None,
                message: format!(
                    "adapter/btor2/lift_predicate_cube: compound_exprs key '{key}' names no predicate in the predicate set"
                ),
            });
        }
    }
    if matches!(strategy, LiftStrategy::Lazy) {
        return Err(AdapterError {
            kind: crate::adapter::AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: "adapter/btor2/lift_predicate_cube: compound predicates are not supported on \
                      the Lazy lift strategy (the lazy per-cube body samples simple \
                      register==value atoms only); use LiftStrategy::Eager with \
                      may_edge_inference = SmtAllPairs"
                .to_string(),
        });
    }
    if !matches!(lift_opts.may_edge_inference, MayEdgeInference::SmtAllPairs) {
        return Err(AdapterError {
            kind: crate::adapter::AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: "adapter/btor2/lift_predicate_cube: compound predicates require \
                      may_edge_inference = SmtAllPairs (the sampling representative is sound \
                      only for simple register==value atoms)"
                .to_string(),
        });
    }
    Ok(())
}

/// U.4 (lift-unification, 2026-06-26) — the single gated entry the CEGAR
/// loop + verify orchestrator call to lift a BTOR2 source through the
/// predicate-cube path. Owns the eager-vs-lazy dispatch (moved out of
/// `cegar_refine_loop`) and runs the compound-predicate soundness gate
/// once via [`ensure_compound_lift_supported`] before dispatch.
///
/// - [`LiftStrategy::Eager`] → [`predicate_cube_lift`] (materializes
///   2^|P| cubes; the default full-fidelity path; the only path that
///   honours compound predicates via the SmtAllPairs seam).
/// - [`LiftStrategy::Lazy`] → [`LazyLift`] + [`materialize_clts_from_lazy`]
///   (per-cube on-demand; produces an identical `Clts` to Eager by U.1's
///   differential test; does NOT support compounds — the gate rejects
///   them before dispatch).
///
/// The lazy path's must-edge policy is taken from
/// `lift_opts.must_edge_inference` (the CEGAR loop sets it equal to
/// `cegar_opts.must_edge_inference`, so no separate parameter is needed —
/// the two were already kept in lock-step at the cegar call site).
///
/// [`LiftStrategy::Eager`]: crate::adapter::btor2::cegar::LiftStrategy::Eager
/// [`LiftStrategy::Lazy`]: crate::adapter::btor2::cegar::LiftStrategy::Lazy
pub fn lift_predicate_cube(
    predicates: Vec<PredicateSpec>,
    btor2_content: &str,
    adapter_options: &AdapterOptions,
    lift_opts: &PredicateCubeLiftOptions,
    strategy: crate::adapter::btor2::cegar::LiftStrategy,
) -> Result<PredicateCubeLiftResult, AdapterError> {
    use crate::adapter::btor2::cegar::LiftStrategy;
    // U.4 — compound gate runs ONCE at the entry. Each impl keeps a thin
    // defensive copy for its direct callers; the double-call on this path
    // is idempotent + cheap.
    ensure_compound_lift_supported(&predicates, lift_opts, strategy)?;
    match strategy {
        LiftStrategy::Eager => {
            predicate_cube_lift(predicates, btor2_content, adapter_options, lift_opts)
        }
        LiftStrategy::Lazy => {
            let mut lazy =
                LazyLift::from_btor2(predicates, btor2_content, adapter_options, lift_opts)?;
            materialize_clts_from_lazy(
                &mut lazy,
                crate::adapter::SourceFormat::Btor2,
                lift_opts.must_edge_inference,
            )
        }
    }
}

/// R.2.5 — Lift one BTOR2 source through the predicate-cube path.
///
/// **Does NOT call `Btor2Adapter::translate`** — the bit-blaster is
/// the cap source we are bypassing. Parses BTOR2 directly to validate
/// each predicate's register name, then enumerates 2^|P| cubes as
/// `Clts` states with matching `state_3valued_predicates` labellings.
///
/// **Binary capability test** (§10.1 R.2.5 done-criterion): the
/// returned `PredicateCubeLiftResult.clts` exists and the cube count
/// matches `2^predicates.len()`. The fixture lifting is what we are
/// validating; the must/may transition relation is left to R.5's
/// `KmtsLiftLazy` integration (the `predicate_image_pending` flag
/// flags this explicitly).
///
/// Errors when:
/// - The BTOR2 content fails to parse (delegated to the existing
///   `parser::parse`).
/// - A predicate names a register that does not exist in the BTOR2
///   source's symbol table.
/// - The cube count exceeds `lift_opts.max_cube_count`.
pub fn predicate_cube_lift(
    mut predicates: Vec<PredicateSpec>,
    btor2_content: &str,
    options: &AdapterOptions,
    lift_opts: &PredicateCubeLiftOptions,
) -> Result<PredicateCubeLiftResult, AdapterError> {
    let start = Instant::now();

    // 1. Parse the BTOR2 source to validate predicate register names
    //    against the symbol table. Bypasses bit_blast.
    let file = crate::adapter::btor2::parser::parse(btor2_content).map_err(|mut e| {
        e.message = format!("adapter/btor2/predicate_cube_lift: {}", e.message);
        e
    })?;
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    let known_registers: std::collections::HashSet<String> = symbols.values().cloned().collect();

    // R.5b lifter consumer wiring — resolve sidecar `uf_wrap` /
    // `uf_unwrap` declarations + the default policy (Op::Mul always;
    // Op::Add/Sub when width > 32) to the set of BTOR2 Op NIDs the
    // simulate-one-step pass should treat as uninterpreted (returns
    // BvValue::zero on evaluation). Empty set ⇒ no UF wrapping;
    // simulate_one_step is used directly with no behaviour change.
    let uf_wrapped_nids = crate::adapter::btor2::bit_blast::collect_uf_wrapped_nids(&file, options);
    let mut warnings: Vec<crate::adapter::AdapterWarning> = Vec::new();
    if !uf_wrapped_nids.is_empty() {
        // R.5b AdapterWarning channel + multi-value UF enumeration —
        // surface the UF wrapping via both tracing (log-stream
        // visibility) and the structured warnings vec (for callers
        // like cegar.rs to fold into their own reporting). The
        // wrapped Ops' outputs are enumerated under both
        // UfRepresentative::Zero and UfRepresentative::Ones per
        // (cube, input combo) → 2× may-edges in the lifted Clts.
        let message = format!(
            "R.5b predicate-image: {} Op(s) UF-wrapped (substituted to zero AND ones via the \
             multi-value enumeration in simulate-one-step). SOUND for safety (may-side \
             over-approximation per §6.10); the wrapped cells' downstream cube successors may \
             shift toward KleeneBot. To remove a wrapping, list the cell in the sidecar's \
             `uf_unwrap` field.",
            uf_wrapped_nids.len()
        );
        tracing::warn!(uf_wrapped_count = uf_wrapped_nids.len(), "{}", message);
        warnings.push(crate::adapter::AdapterWarning {
            kind: crate::adapter::WarningKind::ApproximateTranslation,
            message,
            location: None,
        });
    }

    // P1 #1 (IR-unification track) — resolve each predicate's register
    // name to the canonical state-cell symbol via the STS-IR seam, so a
    // predicate over a symbol-stripped alias (the uart_tx `bit_cnt_q` →
    // `bit_cnt_d` case, the DR1 #1 blocker) binds to the real register
    // everywhere downstream. Direct hits are kept; aliases are rewritten;
    // unresolvable names still error.
    resolve_predicate_registers(&file, &mut predicates)?;

    // B.1 (increment 3b) / U.4 — compound-predicate soundness gate.
    // Defensive call to the shared [`ensure_compound_lift_supported`] for
    // the ~39 direct callers of `predicate_cube_lift` that bypass the
    // [`lift_predicate_cube`] entry. No-op when `compound_exprs` is empty;
    // when present, enforces every key names a predicate + (this eager
    // path) requires `may_edge_inference = SmtAllPairs` (the sampling
    // representative is sound only for simple register==value atoms — the
    // SMT may/must seam honours compounds via `PredicateLike::expr`).
    ensure_compound_lift_supported(
        &predicates,
        lift_opts,
        crate::adapter::btor2::cegar::LiftStrategy::Eager,
    )?;

    // 2. Cube count check — `2^|P|` must fit `max_cube_count`. For
    //    |P| > 63 the shift overflows so we cap at 63 explicitly.
    let p = predicates.len();
    if p > 63 {
        return Err(AdapterError {
            kind: crate::adapter::AdapterErrorKind::StateSpaceOverflow,
            location: None,
            message: format!(
                "adapter/btor2/predicate_cube_lift: |P| = {p} exceeds 63 (cube count would overflow usize)"
            ),
        });
    }
    let cube_count: usize = 1usize << p;
    if cube_count > lift_opts.max_cube_count {
        return Err(AdapterError {
            kind: crate::adapter::AdapterErrorKind::StateSpaceOverflow,
            location: None,
            message: format!(
                "adapter/btor2/predicate_cube_lift: cube count 2^{p} = {cube_count} exceeds max_cube_count = {}",
                lift_opts.max_cube_count
            ),
        });
    }

    // 3. Build the Clts: one state per cube. State name `cube_<i>`
    //    where i is the bit pattern. State_3valued_predicate at each
    //    state labels each predicate KleeneT if the corresponding bit
    //    is set in i, KleeneF otherwise. Predicates are populated on
    //    the builder *before* `build()` because `with_3valued_predicate`
    //    is a builder-side mutator.
    let mut builder = Clts::builder();
    let mut state_ids = Vec::with_capacity(cube_count);
    for i in 0..cube_count {
        let name = format!("cube_{i}");
        let id = builder
            .state_id_or_insert(&name)
            .ok_or_else(|| AdapterError {
                kind: crate::adapter::AdapterErrorKind::StateSpaceOverflow,
                location: None,
                message: format!(
                    "adapter/btor2/predicate_cube_lift: state id overflow at cube {i} / {cube_count}"
                ),
            })?;
        state_ids.push(id);
    }
    // R-Y7 (2026-06-07) — initial-state set selection:
    // - When `lift_opts.config_values` is non-empty, expand the
    //   initial-state set to all cubes admissible under the
    //   R-S8 encoder. Each cube whose predicate evaluation is
    //   consistent with some valid value for every constrained
    //   register becomes an initial state.
    // - Otherwise (pre-R-Y7 default): single initial cube
    //   (cube_0, all-predicates-false). A future iteration can
    //   pick the cube matching the BTOR2 `init` values.
    if !lift_opts.config_values.is_empty() {
        let admissible_cubes = crate::adapter::btor2::r_s8_encoder::hyper_must_initial_cubes(
            &predicates,
            &lift_opts.config_values,
        );
        for cube_idx in &admissible_cubes {
            if let Some(state_id) = state_ids.get(*cube_idx) {
                builder.initial_state_id(*state_id);
            }
        }
        if admissible_cubes.is_empty()
            && let Some(initial) = state_ids.first()
        {
            // Defensive: if no cube is admissible, fall back to
            // cube_0 to avoid producing a Clts with no initial
            // states (which would error at evaluator time).
            builder.initial_state_id(*initial);
        }
    } else if let Some(initial) = state_ids.first() {
        builder.initial_state_id(*initial);
    }

    // Populate state_3valued_predicates per cube *on the builder*.
    for (i, &state_id) in state_ids.iter().enumerate() {
        for (bit, pred) in predicates.iter().enumerate() {
            let verdict = if (i >> bit) & 1 == 1 {
                Tristate::KleeneT
            } else {
                Tristate::KleeneF
            };
            builder.with_3valued_predicate(state_id, &pred.name, verdict);
        }
    }

    // H.E.2 — derived combinational predicate labels (Approach B). A
    // combinational atom is NOT a cube dimension; its per-cube KleeneT/F/Bot
    // label is decided by SMT (`combinational_labels`) over the cube's dimension
    // predicates + the signal's encoded BV, then written into
    // `state_3valued_predicates` keyed by the predicate name (the evaluator binds
    // the formula atom by name). Independent of the may/must edge policy; the
    // cube space is unchanged (these are labels, not dimensions).
    if !lift_opts.derived_predicates.is_empty() {
        use crate::adapter::sts_ir::{BtorSts, SmtEncode};
        let cube_preds = cube_predicates(&predicates, &lift_opts.compound_exprs);
        // H.F — a derived predicate may be relational (`cnt_q >= cfg_*`); its
        // `PredicateExpr` lives in `compound_exprs` (keyed by name, disjoint from
        // the dimension keys), so the labeller resolves it. Simple
        // combinational-of-input atoms have no entry → `expr() == None` (the
        // signal⋈value labeller branch).
        let derived_cube =
            cube_predicates(&lift_opts.derived_predicates, &lift_opts.compound_exprs);
        let labels = BtorSts::new(&file).combinational_labels(&cube_preds, &derived_cube, 5_000);
        for (cube_idx, d_idx, label) in labels {
            if let (Some(&sid), Some(d)) = (
                state_ids.get(cube_idx),
                lift_opts.derived_predicates.get(d_idx),
            ) {
                builder.with_3valued_predicate(sid, &d.name, label);
            }
        }
    }

    // R.2.5 predicate-image MVP — emit MayOnly edges between cubes.
    //
    // For each cube_i, build a "canonical representative" concrete
    // register assignment (each predicate's source register set to
    // the value the predicate checks against when the cube has the
    // predicate true; left at 0 otherwise — which is a valid
    // representative for the predicate-false case under the
    // simplifying assumption that 0 ≠ predicate.value, which holds
    // for the typical equality predicate `register == nonzero_constant`).
    //
    // For each boolean input combination (up to `max_input_bits` 1-bit
    // BTOR2 inputs), simulate one clock step via
    // `bit_blast::simulate_one_step`. The resulting register values
    // map to a target cube `cube_j` via predicate re-evaluation;
    // emit a MayOnly edge `cube_i → cube_j` with a "step" label.
    //
    // Soundness: emitting MayOnly (no must-witness) is the safe
    // under-approximation per the standard KMTS preservation theorem.
    // Sampling a single canonical representative per cube is an
    // under-approximation of the may-set — every concrete state in
    // the cube might reach OTHER cubes via inputs we don't sample.
    // R.5 / R.5b's SMT-driven must-edges + lazy `KmtsLiftLazy` close
    // these gaps.
    // R.2.5b (2026-06-06) — collect target-cube sets per source cube
    // (the sampled must-edge candidates for the SMT must post-pass).
    // Indexed by source cube index; each entry is the set of target
    // cube indices reached across all (input, UF-rep) samples for that source.
    let mut sharp_edges_promoted: usize = 0;
    let mut hyper_must_edges_emitted: usize = 0;

    let mut predicate_image_pending = true;

    // MIG-3.2 (S-track migration, 2026-06-13) — sound all-pairs SMT
    // may-edges. When opted in, this REPLACES the sampling may-edge
    // pass below (the two are mutually exclusive). For every
    // (source-cube, target-cube) pair, Z3 decides whether a concrete
    // witness exists (`smt_per_target_may_check`); a `MayOnly` edge is
    // emitted iff so. Unlike sampling — which can miss may-edges and
    // thus under-approximate the may relation (unsound for safety) —
    // this excludes an edge ONLY when Z3 proves it impossible. Edges
    // are collected inside the Z3 scope and emitted after it (the
    // builder is not borrowed into the closure).
    // mununu#503 — NO `!predicates.is_empty()` guard. A property whose atoms are all DERIVED
    // labels (relational-with-input / combinational-of-input) contributes ZERO cube dimensions,
    // and this block used to be skipped for it — emitting a 1-cube, 0-edge KMTS. An edgeless
    // cell is then masked to ⊥ by `downgrade_unsatisfiable_cells` (cegar.rs), whose proxy reads
    // "no outgoing edges" as "unsatisfiable cube". The result was that EVERY such property
    // returned Unknown regardless of what it asserted — even `nu X. ([] X)`, which is true at
    // every state of every KMTS. At |P| = 0 this block does exactly one 0 -> 0 check, which is
    // cheap and EXACT: a total design yields the self-loop, and a design whose `constraint`
    // admits no transition correctly yields none (⊥ there is right). The `"step"` label is also
    // interned here, so skipping the block left the KMTS with no labels at all.
    if matches!(lift_opts.may_edge_inference, MayEdgeInference::SmtAllPairs) {
        let step_label = builder
            .labels()
            .intern(["step"])
            .map_err(|e| AdapterError {
                kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                location: None,
                message: format!(
                    "adapter/btor2/predicate_cube_lift MIG-3.2: label intern failed: {e}"
                ),
            })?;
        // P1 #2 (IR-unification track) — consume the STS-IR seam's
        // batched `SmtEncode::may_edges` instead of re-inlining the
        // `encode_design_for_lift` → `build_register_nid_map` →
        // all-pairs `smt_per_target_may_check` Z3 loop here. The seam
        // wraps the identical memory-aware encode, the same cube
        // encoding (indices `0..2^|P|`), the same per-pair may-check, and
        // the same "no edges on encoder error" fallback — so this is
        // behaviour-preserving and de-dups the inlined loop onto the
        // single seam implementation `predicate_cube_lift` now shares
        // with `BtorSts::may_edges` and the lazy must-edge passes.
        // B.1 (increment 3b) — pair each predicate with its compound expr (if
        // any) so the seam encodes compounds via `PredicateLike::expr`. With no
        // compounds every expr is None → identical to passing `&predicates`
        // (behaviour-preserving for the simple SmtAllPairs path).
        let cube_preds = cube_predicates(&predicates, &lift_opts.compound_exprs);
        // Phase 0.a — ONE `BtorSts` (hence one `ModelFacts` memo) shared across the may /
        // hyper-must / must edge relations below, so the theory selection
        // (`detect_btor2_memories`) runs once per lift instead of once per edge relation.
        use crate::adapter::sts_ir::{BtorSts, SmtEncode};
        let sts = BtorSts::new(&file);
        // scalable-KMTS P1.4 — the may-relation via post-image (O(2^|P|·#succ)) instead of all-pairs
        // (O(2^{2|P|})). Both use `encode_design_for_lift`, so the edge set is IDENTICAL; a `None` (an
        // unresolvable register / cube-space over the bound) falls back to the all-pairs seam.
        // GATE (follow-up A): only for |P| >= 2. At |P| == 1 all-pairs is 2 cubes / ~4 checks (trivial),
        // and the post-image's fixed all-SAT overhead loses (measured 1.6s vs 0.13s); from |P| == 2 the
        // post-image wins (and grows to 33-256x by |P| = 3-7).
        // Phase 0.c (session cache) — when the may-relation is computed via the STS-IR
        // seam (`BtorSts`, i.e. NOT the post-image path) AND a hyper-must is requested,
        // compute may + hyper-must in ONE z3 scope sharing a single encoded view (else
        // each re-encodes the transition relation). Guarded to the cases where the
        // post-image path is provably not taken (`!may_postimage || |P| < 2`), so the
        // may-relation is byte-identical to the split path; `precomputed_hyper` carries
        // the shared-scope hyper result to the SmtHyperMust arm below.
        let use_session_may_hyper = matches!(
            lift_opts.must_edge_inference,
            MustEdgeInference::SmtHyperMust
        ) && (!lift_opts.may_postimage || predicates.len() < 2);
        let (may_edges, precomputed_hyper): (
            Vec<(usize, usize)>,
            Option<crate::adapter::sts_ir::HyperMustEdges>,
        ) = if use_session_may_hyper {
            let (may, hyper) = sts.may_and_hyper_must_edges(&cube_preds, 5_000);
            (may, Some(hyper))
        } else if lift_opts.may_postimage && predicates.len() >= 2 {
            // mununu#504 — THREE outcomes, and conflating two of them is the trap `MayPostimage`
            // exists to prevent. `NotApplicable` falls back to the all-pairs seam;
            // `BudgetExceeded` must NOT, because that seam is O(2^2|P|) — strictly slower than
            // the post-image we just abandoned for lack of time.
            match compute_all_may_edges_smt_postimage(
                &file,
                &predicates,
                &lift_opts.compound_exprs,
                crate::adapter::btor2::smt_must_edge::cube_smt_rlimit(),
            ) {
                MayPostimage::Complete(map) => {
                    let mut pairs: Vec<(usize, usize)> = map
                        .into_iter()
                        .flat_map(|(src, tgts)| tgts.into_iter().map(move |t| (src, t)))
                        .collect();
                    pairs.sort_unstable();
                    (pairs, None)
                }
                MayPostimage::NotApplicable => (sts.may_edges(&cube_preds, 5_000), None),
                MayPostimage::BudgetExceeded => {
                    return Err(AdapterError {
                        kind: crate::adapter::AdapterErrorKind::ResourceBudgetExceeded,
                        location: None,
                        message: format!(
                            "adapter/btor2/predicate_cube_lift: the wall-clock budget expired \
                             while computing the may-relation over {} cubes. Raise or clear {} / \
                             {}, or narrow the property's cone.",
                            1usize << predicates.len(),
                            crate::adapter::run_budget::PROPERTY_BUDGET_ENV,
                            crate::adapter::run_budget::RUN_BUDGET_ENV,
                        ),
                    });
                }
            }
        } else {
            (sts.may_edges(&cube_preds, 5_000), None)
        };
        // Emit MayOnly edges. Keep `may_edges` (borrow) — the SmtHyperMust
        // branch below reuses it as the per-source candidate target set.
        for &(i, j) in &may_edges {
            builder.transition_ids_with_modality(
                state_ids[i],
                &[step_label],
                state_ids[j],
                crate::clts::TransitionModality::MayOnly,
            );
        }

        // P1 #3 (IR-unification track) — may+must composition. Closes
        // the DR1 F5 gap: a sound DEFINITE verdict needs must-witnesses
        // (diamonds need a must-successor), but SmtAllPairs may + a
        // non-Off must inference previously did not compose (the must
        // post-pass consumed the sampling pass's candidate set, which
        // SmtAllPairs bypasses). When a non-Off must inference is
        // requested alongside SmtAllPairs may, compute the canonical ∀∃
        // KMTS must-relation via the same STS-IR seam and promote each
        // must-edge MayOnly → Sharp. `R_must ⊆ R_may` by construction
        // (∀∃ ⟹ ∃), so every promoted edge already exists as a may-edge
        // above; the builder's edge-modality merge (Sharp dominates
        // MayOnly to the same target) upgrades it in place.
        //
        // The SmtAllPairs composition uses the standard ∀∃ must for ANY
        // non-Off inference — it is the canonical, sound KMTS must
        // (Bruns–Godefroid CONCUR 2000) and supersedes the removed
        // sampling-derived confluence heuristic, which had no
        // meaning without a sampling pass. The per-variant ∀∀ /
        // hyper-must distinctions remain selectable on the sampling
        // (`!SmtAllPairs`) path below.
        if !matches!(lift_opts.must_edge_inference, MustEdgeInference::Off) {
            // BtorSts / SmtEncode already in scope from the shared `sts` above (Phase 0.a).
            match lift_opts.must_edge_inference {
                MustEdgeInference::SmtHyperMust => {
                    // B.2 (2026-06-26) — GKMTS hyper-must on the SmtAllPairs /
                    // compound path. The ∀∃ Sharp promotion (the `_` arm
                    // below) is the *standard* KMTS must, which is non-monotone
                    // under refinement on alternating fixpoints (νμ) — so a
                    // refined recoverability verdict over it is only
                    // soundness-tagged (the B.3.b warning fires when no
                    // `MustHyperOnly` edges exist). The hyper-must form is
                    // monotone (Shoham–Grumberg LMCS 2007), so emitting it
                    // makes compound νμ verdicts clean-sound. Each source's
                    // candidate target set is its may-successor set; the seam
                    // proves `∀ s ⊨ src. ∃ input ∃ t ∈ T. reach(t)` and emits
                    // a `MustHyperOnly(T)` edge only on a definite Must.
                    // Phase 0.c — reuse the hyper-must computed in the same z3 scope as
                    // `may` when the session path was taken; else compute it standalone.
                    let hyper = precomputed_hyper
                        .unwrap_or_else(|| sts.hyper_must_edges(&cube_preds, &may_edges, 5_000));
                    let emitted = hyper.len();
                    for (src, targets) in hyper {
                        let target_ids: smallvec::SmallVec<
                            [crate::clts::StateId<DefaultStateIdx>; 4],
                        > = targets.iter().map(|&t| state_ids[t]).collect();
                        if let Some(&first) = target_ids.first() {
                            builder.transition_ids_with_modality(
                                state_ids[src],
                                &[step_label],
                                first,
                                crate::clts::TransitionModality::must_hyper(target_ids.clone()),
                            );
                        }
                    }
                    hyper_must_edges_emitted += emitted;
                    if emitted > 0 {
                        warnings.push(crate::adapter::AdapterWarning {
                            kind: crate::adapter::WarningKind::ApproximateTranslation,
                            message: format!(
                                "[B.2 may+hyper-must] predicate_cube_lift: SmtAllPairs may + SmtHyperMust composition emitted {emitted} MustHyperOnly edge(s) over per-source may-successor sets (Z3-proved ∀∃ hyper-must; monotone under refinement per Shoham–Grumberg LMCS 2007). Compound/νμ verdicts over this KMTS are clean-sound (no standard-KMTS non-monotonicity tag). Hyper-must targets = full may-successor set (MVP doesn't minimize T)."
                            ),
                            location: None,
                        });
                    }
                }
                _ => {
                    // P1 #3 — canonical ∀∃ standard per-target must → Sharp.
                    //
                    // SOUNDNESS (must ⊆ may fix, Tier 0) — restrict the must-image to
                    // the `may_edges` already emitted above (`must_edges_over`) instead
                    // of the full `2^|P|×2^|P|` grid (`must_edges`). The ∀∃ must is
                    // vacuously true out of an UNSATISFIABLE source cube (∀ over ∅), so
                    // the all-pairs form fabricates a `Sharp` edge to a target that has
                    // NO may-edge (the ∃-witness may-check correctly excludes an empty
                    // src) — breaking the KMTS invariant `R_must ⊆ R_may`, which trips
                    // `TritBdd::from_parts` (symbolic engine) and yields a spurious
                    // `Violated` in the explicit engine (past-shadow-soundness.md §6).
                    // `may_edges` is the emitted may-relation, so every promoted edge is
                    // a may-edge BY CONSTRUCTION (candidates ⊆ may), and an empty src —
                    // absent from `may_edges` — is never a candidate. Genuine ∀∃ musts
                    // out of a satisfiable src are unaffected (∀∃ ⟹ ∃ ⟹ may).
                    let must_edges = sts.must_edges_over(&cube_preds, &may_edges, 5_000);
                    let promoted = must_edges.len();
                    for (i, j) in must_edges {
                        builder.transition_ids_with_modality(
                            state_ids[i],
                            &[step_label],
                            state_ids[j],
                            crate::clts::TransitionModality::Sharp,
                        );
                    }
                    sharp_edges_promoted += promoted;
                    if promoted > 0 {
                        warnings.push(crate::adapter::AdapterWarning {
                            kind: crate::adapter::WarningKind::ApproximateTranslation,
                            message: format!(
                                "[P1 #3 may+must] predicate_cube_lift: SmtAllPairs may + SMT must composition promoted {promoted} edge(s) MayOnly → Sharp via the canonical ∀∃ must-relation restricted to the emitted may-edges (Z3-proved; sound under-approximation, R_must ⊆ R_may by construction). The resulting KMTS carries both may (over-approx) and must (under-approx) edges, enabling sound DEFINITE 3-valued verdicts."
                            ),
                            location: None,
                        });
                    }
                }
            }
        }
        predicate_image_pending = false;
    }

    // mununu#503 — `!predicates.is_empty()` removed here too; see the note on the SmtAllPairs
    // block above. `max_input_bits > 0` stays: that one is a real opt-out, not a shape guard.
    if !matches!(lift_opts.may_edge_inference, MayEdgeInference::SmtAllPairs)
        && lift_opts.max_input_bits > 0
    {
        let mut sampled_targets_per_source: Vec<std::collections::BTreeSet<usize>> =
            vec![std::collections::BTreeSet::new(); state_ids.len()];
        let boolean_inputs: Vec<String> = collect_boolean_input_symbols(&file, &symbols);
        let n_inputs = boolean_inputs.len().min(lift_opts.max_input_bits);
        let n_combos: usize = 1usize << n_inputs;

        // R.6.6 (2026-06-08) — controllability-aware label emission.
        // When `AdapterOptions.controllable_inputs` is non-empty, the
        // lifter partitions boolean inputs into env (uncontrollable) +
        // ctrl (controllable) classes per-symbol-name, then emits each
        // transition with a dual-label set `[env_combo_label,
        // ctrl_combo_label]`. The env labels are tagged
        // `LabelControllability::Uncontrollable`; ctrl labels are
        // `Controllable`. The Skolem grouping in modal_exists /
        // modal_forall (group_transitions_by_uncontrollable_labels)
        // then partitions correctly along the controllability axis:
        // ∀ env-combo, ∃ ctrl-combo for the synthesis idiom.
        //
        // When `controllable_inputs` is empty: legacy single-`step`
        // label behavior (controllability axis disabled). This keeps
        // every pre-R.6.6 fixture verdict-equivalent — the new path
        // is strictly opt-in.
        //
        // Per the R.6.6 done-criterion (kmts-theory.md §7 / R.6 plan §1):
        // this is the first adapter that emits a CLTS carrying BOTH
        // controllable labels AND MayOnly/MustHyperOnly edges from the
        // same source — the model R.6.3/4/5 evaluators are designed to
        // consume.
        let controllable_inputs_set: std::collections::HashSet<&str> = options
            .controllable_inputs
            .iter()
            .map(|s| s.as_str())
            .collect();
        let env_input_indices: Vec<usize> = boolean_inputs
            .iter()
            .take(n_inputs)
            .enumerate()
            .filter(|(_, name)| !controllable_inputs_set.contains(name.as_str()))
            .map(|(i, _)| i)
            .collect();
        let ctrl_input_indices: Vec<usize> = boolean_inputs
            .iter()
            .take(n_inputs)
            .enumerate()
            .filter(|(_, name)| controllable_inputs_set.contains(name.as_str()))
            .map(|(i, _)| i)
            .collect();
        let controllability_aware = !controllable_inputs_set.is_empty()
            && (!env_input_indices.is_empty() || !ctrl_input_indices.is_empty());

        let label_id = builder
            .labels()
            .intern(["step"])
            .map_err(|e| AdapterError {
                kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                location: None,
                message: format!("adapter/btor2/predicate_cube_lift: label intern failed: {e}"),
            })?;

        // R.6.6 — pre-intern the per-combo controllability-aware
        // labels when opted in. We intern up-front (rather than
        // lazily per-transition) so the controllability map is set
        // exactly once per label. The label names encode the input-
        // combo bit-pattern for unique identification.
        let n_env = env_input_indices.len();
        let n_ctrl = ctrl_input_indices.len();
        let n_env_combos: usize = 1usize << n_env;
        let n_ctrl_combos: usize = 1usize << n_ctrl;
        let mut env_label_ids: Vec<crate::clts::LabelId<DefaultLabelIdx>> =
            Vec::with_capacity(n_env_combos);
        let mut ctrl_label_ids: Vec<crate::clts::LabelId<DefaultLabelIdx>> =
            Vec::with_capacity(n_ctrl_combos);
        if controllability_aware {
            for env_c in 0..n_env_combos {
                let name = format!("env_c{env_c}");
                let lid = builder
                    .labels()
                    .intern([name.as_str()])
                    .map_err(|e| AdapterError {
                        kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                        location: None,
                        message: format!(
                            "adapter/btor2/predicate_cube_lift R.6.6: env label intern failed: {e}"
                        ),
                    })?;
                builder.set_label_controllability(
                    lid,
                    crate::clts::LabelControllability::Uncontrollable,
                );
                env_label_ids.push(lid);
            }
            for ctrl_c in 0..n_ctrl_combos {
                let name = format!("ctrl_c{ctrl_c}");
                let lid = builder
                    .labels()
                    .intern([name.as_str()])
                    .map_err(|e| AdapterError {
                        kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
                        location: None,
                        message: format!(
                            "adapter/btor2/predicate_cube_lift R.6.6: ctrl label intern failed: {e}"
                        ),
                    })?;
                builder.set_label_controllability(
                    lid,
                    crate::clts::LabelControllability::Controllable,
                );
                ctrl_label_ids.push(lid);
            }
        }

        // Collect register widths so the representative builder in
        // `cube_sampling_edges` can mask predicate-false values to the
        // cell's bit-width. U.3 — `String` keys (was `&str`) so the
        // borrowed `CubeSamplingCtx` shares one map type with the lazy
        // path's `LazyLiftContext`.
        let mut pred_register_widths: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for line in &file.lines {
            if let crate::adapter::btor2::ast::Node::State { sort, .. } = &line.node
                && let Some(width) = crate::adapter::btor2::parser::bv_width(&file, *sort)
                && let Some(name) = symbols.get(&line.nid)
            {
                pred_register_widths.insert(name.clone(), width);
            }
        }

        // U.3 — drive the shared per-cube sampling body. The eager path
        // borrows its locals into a `CubeSamplingCtx`; `cube_sampling_edges`
        // returns `(target, LabelDescriptor)` pairs deduped by
        // `(target, descriptor)`. The eager caller interns each descriptor
        // into the controllability-aware dual-label set (or the single
        // `step` label); the lazy caller (`compute_cube_outgoing_edges`)
        // drives the identical body onto its `LazyExpansionEdge` shape —
        // U.1's differential test enforces the two can never diverge.
        //
        // R.6.6 — the `Controllability { env_combo, ctrl_combo }` descriptor
        // carries the projection of the input combo; the env labels are
        // tagged `Uncontrollable`, the ctrl labels `Controllable`, so the
        // Skolem grouping (group_transitions_by_uncontrollable_labels)
        // partitions transitions along the controllability axis correctly.
        let sampling_ctx = CubeSamplingCtx {
            file: &file,
            pred_register_widths: &pred_register_widths,
            boolean_inputs: &boolean_inputs,
            n_inputs,
            n_combos,
            uf_wrapped_nids: &uf_wrapped_nids,
            predicates: &predicates,
            cube_count,
            env_input_indices: &env_input_indices,
            ctrl_input_indices: &ctrl_input_indices,
            controllability_aware,
        };

        for (i, &src_id) in state_ids.iter().enumerate() {
            for (target_index, descriptor) in cube_sampling_edges(i, &sampling_ctx) {
                let tgt_id = state_ids[target_index];
                let labels: smallvec::SmallVec<[crate::clts::LabelId<DefaultLabelIdx>; 4]> =
                    match descriptor {
                        LabelDescriptor::Step => smallvec::smallvec![label_id],
                        LabelDescriptor::Controllability {
                            env_combo,
                            ctrl_combo,
                        } => {
                            let mut lbls = smallvec::SmallVec::new();
                            if !env_input_indices.is_empty() {
                                lbls.push(env_label_ids[env_combo]);
                            }
                            if !ctrl_input_indices.is_empty() {
                                lbls.push(ctrl_label_ids[ctrl_combo]);
                            }
                            lbls
                        }
                    };
                builder.transition_ids_with_modality(
                    src_id,
                    &labels,
                    tgt_id,
                    crate::clts::TransitionModality::MayOnly,
                );
                // R.2.5b — record sampled target for the must post-pass.
                sampled_targets_per_source[i].insert(target_index);
            }
        }
        predicate_image_pending = false;

        // U.2 — shared sampled-target must-edge inference (was duplicated here +
        // in materialize_clts_from_lazy). The R.6.6 controllability gate lives
        // inside the shared fn (skips the post-passes under controllability-aware).
        let must_outcome = apply_sampled_must_inference(
            &mut builder,
            &state_ids,
            &sampled_targets_per_source,
            &file,
            &predicates,
            label_id,
            lift_opts.must_edge_inference,
            controllability_aware,
            "predicate_cube_lift",
        );
        sharp_edges_promoted += must_outcome.sharp_promoted;
        hyper_must_edges_emitted += must_outcome.hyper_emitted;
        warnings.extend(must_outcome.warnings);
    }

    let clts = builder.build().map_err(|e| AdapterError {
        kind: crate::adapter::AdapterErrorKind::IrConsistencyError,
        location: None,
        message: format!("adapter/btor2/predicate_cube_lift: builder.build failed: {e}"),
    })?;

    // Fix C — enforce R_must ⊆ R_may before the KMTS reaches any evaluator.
    assert_must_subset_may(&clts)?;

    let elapsed = start.elapsed();

    Ok(PredicateCubeLiftResult {
        clts,
        predicates,
        cube_count,
        source_info: SourceInfo {
            format: SourceFormat::Btor2,
            title: None,
            signal_count: known_registers.len(),
            state_count: cube_count,
            property_count: 0,
        },
        lift_time: elapsed,
        // R.2.5 predicate-image MVP populates may-edges when
        // `max_input_bits > 0` and at least one predicate exists;
        // R.5 / R.5b will additionally populate must-edges via SMT.
        predicate_image_pending,
        // R.5b AdapterWarning channel — UF wrapping naming + other
        // lift-time abstraction decisions surface here for caller
        // reporting.
        warnings,
        sharp_edges_promoted,
        hyper_must_edges_emitted,
    })
}

/// R.2.5 predicate-image MVP helper — collect the symbols of every
/// 1-bit BTOR2 input that is NOT a clock signal. Clock inputs (per
/// `looks_like_clock` in `bit_blast`) are excluded because each CLTS
/// step already represents one clock edge.
///
/// Returns symbols in BTOR2 NID order (matches the order
/// `simulate_one_step` walks the file when building its Env). Inputs
/// without a symbol are skipped (they're typically Yosys-generated
/// phi inputs the property doesn't reference anyway).
fn collect_boolean_input_symbols(
    file: &crate::adapter::btor2::ast::Btor2File,
    symbols: &std::collections::HashMap<crate::adapter::btor2::ast::Nid, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for line in &file.lines {
        if let crate::adapter::btor2::ast::Node::Input { sort, .. } = &line.node {
            let width = crate::adapter::btor2::parser::bv_width(file, *sort).unwrap_or(0);
            if width != 1 {
                continue;
            }
            let symbol = match symbols.get(&line.nid) {
                Some(s) => s.clone(),
                None => continue,
            };
            // Skip clock inputs; reuse the same heuristic as
            // bit_blast's `looks_like_clock`. Conservative: a name
            // containing "clk" or "clock" is treated as a clock.
            let lower = symbol.to_lowercase();
            if lower.contains("clk") || lower.contains("clock") {
                continue;
            }
            out.push(symbol);
        }
    }
    out
}

/// P1 (scalable-KMTS, 2026-07-12) — compound-sound SMT POST-IMAGE of the predicate cube's may-edges.
/// The eager `SmtAllPairs` may-relation checks all `O(cubes²)` (source, target) pairs; this instead
/// computes, for each source cube, its may-successors by projecting `cube_curr ∧ T` onto the next-state
/// predicate valuations (all-SAT enumeration). A reachability-guided BFS over these edges pays only
/// `O(reachable × #successors)` — the P1 scalability lever the P0 measurement identified.
///
/// **Sound for compound/relational predicates** (unlike the sampling `LazyLift`): each predicate's
/// constraint is built via [`PredicateExpr::build_constraint`] straight into the SMT — over
/// `state_curr` for the source cube, `state_next` for the projection — so a relation like `a == b` is
/// honoured with no sampling representative. The transition is the EXACT `view.transition`
/// (`bvadd`/`bvmul`), matching the eager `SmtAllPairs` on designs the cube does not UF-wrap.
///
/// Returns `cube_index → sorted distinct may-successor cube indices`, or `None` if a predicate register
/// does not resolve to a state cell / the cube space exceeds the bound (the caller falls back to eager).
///
/// P1 increment 1: validated by `p1_smt_postimage_may_edges_match_concrete_oracle`. P1.4: wired into
/// `predicate_cube_lift` behind `PredicateCubeLiftOptions::may_postimage`.
/// mununu#504 — the post-image outcome. THREE cases, not two.
///
/// The trap this exists to avoid: `None` already meant *"not applicable, fall back"*, and the
/// fallback is the O(2^2|P|) all-pairs seam — **slower** than the post-image. So returning `None`
/// on a budget expiry would make a timeout strictly WORSE than doing nothing, by dropping a
/// 2^|P| loop into a 2^2|P| one at exactly the moment we are out of time.
#[derive(Debug)]
enum MayPostimage {
    /// The complete post-image may-relation.
    Complete(std::collections::HashMap<usize, Vec<usize>>),
    /// Not applicable to this input (an unresolvable predicate, or |P| out of range). The caller
    /// falls back to the all-pairs seam — the historical `None`.
    NotApplicable,
    /// The wall-clock budget expired mid-enumeration. NOT a partial map: the caller must abort
    /// the lift, never fall back.
    BudgetExceeded,
}

#[cfg(test)]
impl MayPostimage {
    /// Test-only unwrap to the completed map.
    fn expect(self, msg: &str) -> std::collections::HashMap<usize, Vec<usize>> {
        match self {
            MayPostimage::Complete(m) => m,
            other => panic!("{msg}: expected Complete, got {other:?}"),
        }
    }
}

/// `rlimit`: the deterministic z3 resource bound, passed in rather than read from the
/// environment so the `Unknown`-saturation path (mununu#504) is testable without mutating
/// process-global state — the same pure-helper idiom `memory_budget.rs` uses. Production passes
/// `smt_must_edge::cube_smt_rlimit()`; a test passes `Some(1)` to force every query `Unknown`.
fn compute_all_may_edges_smt_postimage(
    file: &crate::adapter::btor2::ast::Btor2File,
    predicates: &[PredicateSpec],
    compound_exprs: &std::collections::HashMap<
        String,
        crate::adapter::btor2::predicate_expr::PredicateExpr,
    >,
    rlimit: Option<u32>,
) -> MayPostimage {
    use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
    let n = predicates.len();
    if n == 0 || n > 20 {
        return MayPostimage::NotApplicable; // outside the cube-space bound; eager path handles it
    }
    let cfg = z3::Config::new();
    let res = z3::with_z3_config(&cfg, || {
        let view = encode_design_for_lift(file).ok()?;
        let nid_map = crate::adapter::btor2::smt_must_edge::build_register_nid_map(&view);
        // Effective constraint per predicate: a compound (by name) or the simple `reg == value`.
        let exprs: Vec<PredicateExpr> = predicates
            .iter()
            .map(|s| {
                compound_exprs
                    .get(&s.name)
                    .cloned()
                    .unwrap_or(PredicateExpr::Cmp {
                        register: s.register.clone(),
                        op: CmpOp::Eq,
                        value: s.value,
                    })
            })
            .collect();
        let curr = |name: &str| {
            nid_map
                .get(name)
                .and_then(|nid| view.curr_state(*nid))
                .cloned()
        };
        let next = |name: &str| {
            nid_map
                .get(name)
                .and_then(|nid| view.next_state(*nid))
                .cloned()
        };
        let curr_c: Vec<z3::ast::Bool> = exprs
            .iter()
            .map(|e| e.build_constraint(&curr))
            .collect::<Option<Vec<_>>>()?;
        let next_c: Vec<z3::ast::Bool> = exprs
            .iter()
            .map(|e| e.build_constraint(&next))
            .collect::<Option<Vec<_>>>()?;

        let mut budget_expired = false;
        let mut params = z3::Params::new();
        // mununu#504 — clamp the per-query timeout to the time actually left, so the LAST query
        // cannot overshoot the deadline by its own full 5 s.
        params.set_u32("timeout", crate::adapter::run_budget::clamp_query_ms(5000));
        // mununu#504 — the deterministic resource bound, the same lever every check in
        // `smt_must_edge` already applies. It was missing HERE, on the post-image path, which is
        // the measured hang suspect (`for cube in 0..2^n` around an all-SAT loop, x17 CEGAR
        // rounds x N properties). Unset by default ⇒ no behaviour change; set, it turns a grind
        // into a fast, deterministic result. Safe to apply only because the `Unknown` arm below
        // now saturates instead of silently truncating — see that comment.
        if let Some(rl) = rlimit {
            params.set_u32("rlimit", rl);
        }
        let mut out: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();
        for cube in 0..(1usize << n) {
            // mununu#504 — the outer poll. This loop is the measured hang: up to 2^|P| cubes,
            // x17 CEGAR rounds, x N properties, previously with only a per-QUERY timeout and no
            // aggregate bound. Bailing here is signalled to the caller as `BudgetExceeded`, NOT
            // as a partial map: a truncated may-relation is an under-approximation (see the
            // saturation note in the all-SAT loop below).
            if crate::adapter::run_budget::expired() {
                budget_expired = true;
                break;
            }
            let solver = z3::Solver::new();
            solver.set_params(&params);
            solver.assert(&view.transition);
            for (i, cc) in curr_c.iter().enumerate() {
                if (cube >> i) & 1 == 1 {
                    solver.assert(cc);
                } else {
                    let neg = cc.not();
                    solver.assert(&neg);
                }
            }
            // All-SAT: project `cube_curr ∧ T` onto the next-state predicate valuation.
            //
            // SOUNDNESS (mununu#504, 2026-09-07) — `may` OVER-approximates: soundness requires
            // may ⊇ concrete. This loop used to be `while matches!(solver.check(), Sat)`, so an
            // `Unknown` (a z3 timeout, or an rlimit hit) exited the loop and stored the PARTIAL
            // enumeration as if it were complete — a TRUNCATED may-relation, i.e. an
            // UNDER-approximation. That is unsound in the definite direction: `[]φ` is True when
            // all may-successors satisfy φ, so dropping may-edges can manufacture a spurious
            // definite True. It was already reachable with the 5 s per-query timeout on a wide
            // cone; adding an rlimit (which exists precisely to make queries return `Unknown`)
            // would have made it routine.
            //
            // On a non-`Sat` INCONCLUSIVE result we therefore SATURATE this cube — every target
            // is a may-successor — which is the sound direction (denser may ⇒ more ⊥, never a
            // wrong verdict). `Unsat` is different: it is a real proof that no further next-state
            // valuation exists, so it ends the enumeration normally.
            let mut targets: Vec<usize> = Vec::new();
            let mut inconclusive = false;
            loop {
                // mununu#504 — the inner poll. One z3 query is the interruptible unit, so the
                // worst-case overshoot past the deadline is a single query (and `clamp_query_ms`
                // below bounds even that).
                if crate::adapter::run_budget::expired() {
                    budget_expired = true;
                    inconclusive = true; // do not trust a partial enumeration
                    break;
                }
                match solver.check() {
                    z3::SatResult::Sat => {}
                    z3::SatResult::Unsat => break, // enumeration provably complete
                    z3::SatResult::Unknown => {
                        inconclusive = true;
                        break;
                    }
                }
                let Some(model) = solver.get_model() else {
                    // `Sat` with no retrievable model: we cannot block this valuation, so we
                    // cannot trust the enumeration to terminate correctly either.
                    inconclusive = true;
                    break;
                };
                let mut tgt = 0usize;
                let mut block: Vec<z3::ast::Bool> = Vec::with_capacity(n);
                for (i, nc) in next_c.iter().enumerate() {
                    let holds = model
                        .eval(nc, true)
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false);
                    if holds {
                        tgt |= 1 << i;
                        block.push(nc.clone());
                    } else {
                        block.push(nc.not());
                    }
                }
                targets.push(tgt);
                // Block this exact next valuation: ¬(⋀ block).
                let refs: Vec<&z3::ast::Bool> = block.iter().collect();
                let bar = z3::ast::Bool::and(&refs).not();
                solver.assert(&bar);
                if targets.len() > (1usize << n) {
                    break; // safety: at most 2^n distinct next cubes
                }
            }
            if inconclusive {
                // Sound fallback for this cube only: every cube is a may-successor.
                targets = (0..(1usize << n)).collect();
            }
            targets.sort_unstable();
            targets.dedup();
            out.insert(cube, targets);
        }
        if budget_expired {
            return Some(Err(())); // budget expiry, distinct from "not applicable"
        }
        Some(Ok(out))
    });
    // Three-way at the boundary: `None` = not applicable (fall back), `Some(Err)` = budget
    // expiry (abort, never fall back — the fallback is SLOWER), `Some(Ok)` = complete.
    match res {
        None => MayPostimage::NotApplicable,
        Some(Err(())) => MayPostimage::BudgetExceeded,
        Some(Ok(map)) => MayPostimage::Complete(map),
    }
}

/// P1 (scalable-KMTS) increment 2 — the compound-aware MUST-edge pass for the post-image lazy path.
/// For each post-image may-edge `src → tgt`, run the per-target ∀∃ must check
/// ([`crate::adapter::btor2::smt_must_edge::smt_per_target_must_check`]) over the compound-aware
/// [`CubePredicate`]s (spec + `compound_exprs`) — the same machinery the eager path uses, so the
/// resulting `Sharp` (must) / `MayOnly` modalities match. Returns `cube → sorted Sharp target cubes`.
///
/// Together with [`compute_all_may_edges_smt_postimage`] this reproduces the eager
/// `SmtAllPairs` + `SmtPerTargetStandard` KMTS (may + per-target must) — validated verdict-for-verdict
/// by `p1_postimage_may_and_must_match_eager_lift` below — but via reachability-friendly post-image
/// (`O(2^|P| · #succ)`) rather than all-pairs (`O(2^{2|P|})`), and soundly for compound predicates.
#[allow(dead_code)]
fn postimage_sharp_edges(
    file: &crate::adapter::btor2::ast::Btor2File,
    predicates: &[PredicateSpec],
    compound_exprs: &std::collections::HashMap<
        String,
        crate::adapter::btor2::predicate_expr::PredicateExpr,
    >,
    may_edges: &std::collections::HashMap<usize, Vec<usize>>,
) -> Option<std::collections::HashMap<usize, Vec<usize>>> {
    use crate::adapter::btor2::smt_must_edge::{SmtMustVerdict, smt_per_target_must_check};
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = encode_design_for_lift(file).ok()?;
        let nid_map = crate::adapter::btor2::smt_must_edge::build_register_nid_map(&view);
        let cube_preds = cube_predicates(predicates, compound_exprs);
        let mut sharp: std::collections::HashMap<usize, Vec<usize>> =
            std::collections::HashMap::new();
        for (&src, tgts) in may_edges {
            let mut sharp_tgts: Vec<usize> = Vec::new();
            for &tgt in tgts {
                let verdict = smt_per_target_must_check(
                    &view,
                    src as u64,
                    tgt as u64,
                    &cube_preds,
                    &nid_map,
                    5000,
                );
                if matches!(verdict, SmtMustVerdict::Must) {
                    sharp_tgts.push(tgt);
                }
            }
            sharp_tgts.sort_unstable();
            sharp.insert(src, sharp_tgts);
        }
        Some(sharp)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared 2-bit relational design for the P1 post-image tests: x'=x+1, y'=y+1 (x==y preserved),
    // predicates P0=(x==0) and the RELATIONAL P1=(x==y).
    fn p1_rel_design() -> (
        &'static str,
        Vec<PredicateSpec>,
        std::collections::HashMap<String, crate::adapter::btor2::predicate_expr::PredicateExpr>,
    ) {
        let btor2 = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 x
4 state 2 y
5 zero 2
6 one 2
7 init 2 3 5
8 init 2 4 5
9 add 2 3 6
10 next 2 3 9
11 add 2 4 6
12 next 2 4 11
";
        let preds = vec![
            PredicateSpec {
                name: "p0".into(),
                register: "x".into(),
                value: 0,
            },
            PredicateSpec {
                name: "rel".into(),
                register: "x".into(),
                value: 0,
            },
        ];
        let mut compound = std::collections::HashMap::new();
        compound.insert(
            "rel".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::CmpReg {
                lhs: "x".into(),
                op: crate::adapter::btor2::predicate_expr::CmpOp::Eq,
                rhs: "y".into(),
            },
        );
        (btor2, preds, compound)
    }

    #[test]
    /// mununu#504 — an expired budget must report `BudgetExceeded`, NOT `NotApplicable`.
    ///
    /// This is the trap `MayPostimage` exists for. `NotApplicable` means "fall back to the
    /// all-pairs seam", and that seam is O(2^2|P|) — **slower** than the post-image loop we just
    /// abandoned for lack of time. Conflating the two would make a timeout strictly worse than
    /// having no budget at all.
    fn postimage_reports_budget_expiry_distinctly_from_not_applicable() {
        use crate::adapter::run_budget::{Budget, enter};
        const B: &str = "1 sort bitvec 1\n\
                         2 state 1 q\n\
                         3 zero 1\n\
                         4 init 1 2 3\n\
                         5 next 1 2 2\n";
        let file = crate::adapter::btor2::parser::parse(B).expect("parse");
        let preds = vec![PredicateSpec {
            name: "q == 1".into(),
            register: "q".into(),
            value: 1,
        }];
        let compound = std::collections::HashMap::new();

        // Expired budget ⇒ BudgetExceeded, promptly.
        let t0 = std::time::Instant::now();
        let out = {
            let _g = enter(&Budget::with_ms(0));
            compute_all_may_edges_smt_postimage(&file, &preds, &compound, None)
        };
        assert!(
            matches!(out, MayPostimage::BudgetExceeded),
            "an expired budget must be distinguishable from `not applicable`; got {out:?}"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "must bail promptly, took {:?}",
            t0.elapsed()
        );

        // Control: unbounded ⇒ the real relation, so the assertion above is about the BUDGET and
        // not about this fixture being unusable.
        let ok = compute_all_may_edges_smt_postimage(&file, &preds, &compound, None);
        assert!(
            matches!(ok, MayPostimage::Complete(_)),
            "with no budget the same call completes; got {ok:?}"
        );

        // And `NotApplicable` remains reachable for its own reason (|P| = 0), unchanged.
        let na = compute_all_may_edges_smt_postimage(&file, &[], &compound, None);
        assert!(
            matches!(na, MayPostimage::NotApplicable),
            "the out-of-range case still says NotApplicable; got {na:?}"
        );
    }

    #[test]
    /// mununu#504 — the post-image all-SAT loop must SATURATE, never truncate, when a query is
    /// inconclusive.
    ///
    /// `may` OVER-approximates, so soundness needs may ⊇ concrete. The loop used to be
    /// `while matches!(solver.check(), Sat)`, so an `Unknown` exited it and stored the PARTIAL
    /// enumeration as complete — an under-approximation, which can manufacture a spurious
    /// definite `True` for a box property (`[]φ` is True when all may-successors satisfy φ, so
    /// dropping successors makes it easier to hold).
    ///
    /// Forced deterministically with `rlimit = 1`: every z3 query returns `Unknown` immediately,
    /// on any machine, in milliseconds. The result must be the FULL target grid, not a partial
    /// or empty one.
    fn postimage_saturates_rather_than_truncating_on_an_inconclusive_query() {
        // `q' = q` — a HELD register. Its true may-relation is strictly sparser than the full
        // grid (cube 0 reaches only cube 0, cube 1 only cube 1), which is what makes saturation
        // distinguishable from the exact answer. A design with a free input driving `q` would
        // have the full grid as its TRUE relation, and the test would prove nothing — the
        // control assertion at the end of this test exists to catch exactly that mistake.
        const B: &str = "1 sort bitvec 1\n\
                         2 state 1 q\n\
                         3 zero 1\n\
                         4 init 1 2 3\n\
                         5 next 1 2 2\n";
        let file = crate::adapter::btor2::parser::parse(B).expect("parse");
        let preds = vec![PredicateSpec {
            name: "q == 1".into(),
            register: "q".into(),
            value: 1,
        }];
        let compound = std::collections::HashMap::new();

        let t0 = std::time::Instant::now();
        let starved = compute_all_may_edges_smt_postimage(&file, &preds, &compound, Some(1))
            .expect("post-image still returns a map");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "an rlimit-starved run must fail FAST, not grind"
        );

        // |P| = 1 ⇒ 2 cubes. Every cube must reach every cube: the sound over-approximation.
        for cube in 0..2usize {
            let targets = starved
                .get(&cube)
                .unwrap_or_else(|| panic!("cube {cube} present"));
            assert_eq!(
                targets.as_slice(),
                &[0usize, 1],
                "cube {cube}: an inconclusive enumeration must SATURATE to every target; a \
                 partial set here is an under-approximated may relation, which is unsound"
            );
        }

        // Control: with no rlimit the same call computes the REAL (sparser) relation, so the
        // assertion above is testing saturation and not merely a degenerate design.
        let exact = compute_all_may_edges_smt_postimage(&file, &preds, &compound, None)
            .expect("post-image");
        assert!(
            exact.values().any(|t| t.len() < 2),
            "the unstarved relation must be strictly sparser somewhere, else this test proves \
             nothing about saturation; got {exact:?}"
        );
    }

    #[test]
    /// mununu#503 — the LIFT-LEVEL pin for the zero-dimension defect. With no cube dimensions
    /// there is exactly one cube (the universal set), and a lift from a TOTAL transition
    /// relation must give it a self-loop. Every may-edge path used to be gated on
    /// `!predicates.is_empty()`, so this lift produced a 1-state, 0-edge, 0-LABEL KMTS —
    /// which `downgrade_unsatisfiable_cells` then masks to ⊥ (its proxy reads "no outgoing
    /// edges" as "unsatisfiable cube"). Asserting the edge HERE localises a future regression
    /// to the lift rather than to a verdict several layers up.
    fn zero_dimension_lift_still_emits_the_universal_cube_self_loop() {
        const B: &str = "1 sort bitvec 1\n\
                         2 state 1 q\n\
                         3 zero 1\n\
                         4 init 1 2 3\n\
                         5 input 1 en\n\
                         6 next 1 2 5\n";
        let out = lift_predicate_cube(
            Vec::new(), // zero dimensions — the degenerate cube space
            B,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions {
                may_edge_inference: MayEdgeInference::SmtAllPairs,
                ..Default::default()
            },
            crate::adapter::btor2::cegar::LiftStrategy::Eager,
        )
        .expect("zero-dimension lift succeeds");

        assert_eq!(out.cube_count, 1, "|P| = 0 ⇒ exactly one universal cube");
        assert_eq!(out.clts.state_count(), 1, "one cube ⇒ one state");
        let s0 = crate::clts::StateId::<DefaultStateIdx>::from_index(0).expect("state 0");
        assert!(
            !out.clts.outgoing(s0).is_empty(),
            "the universal cube of a TOTAL design must have a self-loop; an edgeless cell is \
             indistinguishable from an unsatisfiable one and gets masked to ⊥"
        );
    }

    #[test]
    fn p1_postimage_lift_matches_all_pairs_lift() {
        // P1.4 — lifting with `may_postimage = true` produces the IDENTICAL KMTS (may-edges + must
        // modalities, incl. SmtHyperMust) as the all-pairs path, because both compute the may-relation
        // via `encode_design_for_lift`. The production swap is verdict-equivalent, just cheaper.
        use crate::clts::{StateId, TransitionModality};
        let (btor2, preds, compound) = p1_rel_design();
        let base = PredicateCubeLiftOptions {
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            compound_exprs: compound.clone(),
            ..Default::default()
        };
        let all_pairs =
            predicate_cube_lift(preds.clone(), btor2, &AdapterOptions::default(), &base)
                .expect("all-pairs lift");
        let mut pi_opts = base.clone();
        pi_opts.may_postimage = true;
        let post_image =
            predicate_cube_lift(preds.clone(), btor2, &AdapterOptions::default(), &pi_opts)
                .expect("post-image lift");

        let edges =
            |r: &PredicateCubeLiftResult| -> std::collections::BTreeSet<(usize, usize, u8)> {
                let mut s = std::collections::BTreeSet::new();
                for i in 0..r.cube_count {
                    let sid = StateId::<DefaultStateIdx>::from_index(i).expect("state id");
                    for t in r.clts.outgoing(sid) {
                        let tag = match t.modality() {
                            TransitionModality::MayOnly => 0u8,
                            TransitionModality::Sharp => 1,
                            TransitionModality::MustHyperOnly(_) => 2,
                        };
                        s.insert((i, t.target().index(), tag));
                    }
                }
                s
            };
        assert_eq!(all_pairs.cube_count, post_image.cube_count);
        assert_eq!(
            edges(&all_pairs),
            edges(&post_image),
            "post-image lift KMTS (may + must modalities) differs from the all-pairs lift"
        );
    }

    #[test]
    fn p1_postimage_may_and_must_match_eager_lift() {
        // P1 increment 2 — the post-image path (may via `compute_all_may_edges_smt_postimage`, Sharp
        // must via `postimage_sharp_edges`) must reproduce the EAGER `SmtAllPairs` +
        // `SmtPerTargetStandard` compound lift's (may-edges, Sharp must-edges), VERDICT-FOR-VERDICT —
        // the P1 soundness gate. A discriminating mix: some cubes have split successors (no Sharp),
        // others a single-successor Sharp.
        use crate::clts::{StateId, TransitionModality};
        let (btor2, preds, compound) = p1_rel_design();
        let file = crate::adapter::btor2::parser::parse(btor2).expect("parse");

        // Eager reference.
        let lift_opts = PredicateCubeLiftOptions {
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            must_edge_inference: MustEdgeInference::SmtPerTargetStandard,
            compound_exprs: compound.clone(),
            ..Default::default()
        };
        let eager =
            predicate_cube_lift(preds.clone(), btor2, &AdapterOptions::default(), &lift_opts)
                .expect("eager lift");
        assert_eq!(eager.cube_count, 4);
        let mut e_may: std::collections::HashMap<usize, std::collections::BTreeSet<usize>> =
            std::collections::HashMap::new();
        let mut e_sharp: std::collections::HashMap<usize, std::collections::BTreeSet<usize>> =
            std::collections::HashMap::new();
        for i in 0..4usize {
            let sid = StateId::<DefaultStateIdx>::from_index(i).expect("state id");
            for t in eager.clts.outgoing(sid) {
                let tgt = t.target().index();
                e_may.entry(i).or_default().insert(tgt);
                if matches!(t.modality(), TransitionModality::Sharp) {
                    e_sharp.entry(i).or_default().insert(tgt);
                }
            }
        }

        // Post-image path.
        let may = compute_all_may_edges_smt_postimage(&file, &preds, &compound, None).expect("may");
        let sharp = postimage_sharp_edges(&file, &preds, &compound, &may).expect("sharp");

        for i in 0..4usize {
            let my_may: std::collections::BTreeSet<usize> = may
                .get(&i)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            let my_sharp: std::collections::BTreeSet<usize> = sharp
                .get(&i)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            assert_eq!(
                my_may,
                e_may.get(&i).cloned().unwrap_or_default(),
                "post-image MAY-edges of cube {i} differ from the eager lift"
            );
            assert_eq!(
                my_sharp,
                e_sharp.get(&i).cloned().unwrap_or_default(),
                "post-image SHARP must-edges of cube {i} differ from the eager lift"
            );
        }
    }

    #[test]
    fn p1_smt_postimage_may_edges_match_concrete_oracle() {
        // P1 increment 1 — no-input 2-bit design: x'=x+1, y'=y+1 (so `x==y` is PRESERVED). Predicates
        // P0=(x==0) and P1=(x==y); P1 is a RELATIONAL compound the sampling lazy lift cannot realise.
        // The SMT post-image must reproduce the exact may-edge relation an independent concrete-
        // simulation oracle induces (the P1 soundness gate, at a width small enough that no UF-wrap
        // diverges the transition abstraction).
        let btor2 = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 x
4 state 2 y
5 zero 2
6 one 2
7 init 2 3 5
8 init 2 4 5
9 add 2 3 6
10 next 2 3 9
11 add 2 4 6
12 next 2 4 11
";
        let file = crate::adapter::btor2::parser::parse(btor2).expect("parse");
        let preds = vec![
            PredicateSpec {
                name: "p0".into(),
                register: "x".into(),
                value: 0,
            },
            PredicateSpec {
                name: "rel".into(),
                register: "x".into(),
                value: 0,
            },
        ];
        let mut compound = std::collections::HashMap::new();
        compound.insert(
            "rel".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::CmpReg {
                lhs: "x".into(),
                op: crate::adapter::btor2::predicate_expr::CmpOp::Eq,
                rhs: "y".into(),
            },
        );
        // Bit 0 = P0 (x==0); bit 1 = P1 (x==y).
        let cube_of = |x: u128, y: u128| -> usize {
            (if x == 0 { 1usize } else { 0 }) | (if x == y { 2usize } else { 0 })
        };
        // Independent concrete oracle over all 2-bit (x,y).
        let mut oracle: std::collections::HashMap<usize, std::collections::BTreeSet<usize>> =
            std::collections::HashMap::new();
        let no_inputs = std::collections::HashMap::<String, u128>::new();
        for x in 0u128..4 {
            for y in 0u128..4 {
                let regs =
                    std::collections::HashMap::from([("x".to_string(), x), ("y".to_string(), y)]);
                let nxt =
                    crate::adapter::btor2::bit_blast::simulate_one_step(&file, &regs, &no_inputs)
                        .expect("simulate");
                oracle
                    .entry(cube_of(x, y))
                    .or_default()
                    .insert(cube_of(nxt["x"], nxt["y"]));
            }
        }
        let got = compute_all_may_edges_smt_postimage(&file, &preds, &compound, None)
            .expect("post-image");
        for cube in 0..4usize {
            let mine: std::collections::BTreeSet<usize> = got
                .get(&cube)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            let want = oracle.get(&cube).cloned().unwrap_or_default();
            assert_eq!(
                mine, want,
                "may-successors of cube {cube} disagree with the concrete oracle"
            );
        }
    }

    // Test fixture: (automaton, [(state, [(var, value)])]).
    type StateVarPair<'a> = (&'a str, &'a str);
    type StateVals<'a> = (&'a str, Vec<StateVarPair<'a>>);
    type AutomatonVals<'a> = (&'a str, Vec<StateVals<'a>>);

    fn make_output_with_valuations(valuations: Vec<AutomatonVals<'_>>) -> AdapterOutput {
        use crate::adapter::{SourceFormat, SourceInfo};
        let mut state_valuations = HashMap::new();
        for (aut, states) in valuations {
            let mut per_state = HashMap::new();
            for (state, vars) in states {
                let bmap: BTreeMap<String, String> = vars
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                per_state.insert(state.to_string(), bmap);
            }
            state_valuations.insert(aut.to_string(), per_state);
        }
        AdapterOutput {
            ctxdsl: String::new(),
            warnings: Vec::new(),
            source_info: SourceInfo {
                format: SourceFormat::Btor2,
                title: None,
                signal_count: 0,
                state_count: 0,
                property_count: 0,
            },
            sidecars: Vec::new(),
            state_valuations,
            transition_observations: Default::default(),
            partition_summary: Default::default(),
        }
    }

    #[test]
    fn synthesise_predicates_sorted_and_deduplicated() {
        let out = make_output_with_valuations(vec![(
            "M",
            vec![
                ("s0", vec![("cnt", "0"), ("flag", "true")]),
                ("s1", vec![("cnt", "1"), ("flag", "true")]),
                ("s2", vec![("cnt", "1"), ("flag", "false")]),
            ],
        )]);
        let (preds, _) = synthesise_predicates_and_labellings(&out, &KmtsLiftOptions::default());
        let names: Vec<&str> = preds.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["cnt=0", "cnt=1", "flag=false", "flag=true"],
            "predicates must be deterministic + deduplicated"
        );
    }

    #[test]
    fn labelling_assigns_kleenet_at_matching_state() {
        let out = make_output_with_valuations(vec![(
            "M",
            vec![("s0", vec![("cnt", "0")]), ("s1", vec![("cnt", "1")])],
        )]);
        let (_, lab) = synthesise_predicates_and_labellings(&out, &KmtsLiftOptions::default());
        let s0 = &lab["M"]["s0"];
        assert_eq!(s0["cnt=0"], Tristate::KleeneT);
        assert_eq!(s0["cnt=1"], Tristate::KleeneF);
        let s1 = &lab["M"]["s1"];
        assert_eq!(s1["cnt=0"], Tristate::KleeneF);
        assert_eq!(s1["cnt=1"], Tristate::KleeneT);
    }

    #[test]
    fn labelling_no_kleenebot_at_r2() {
        // R.2 invariant: every predicate has a definite verdict at
        // every enumerated state. KleeneBot would only arise from
        // CEGAR refinement (R.5) or UF-abstracted predicate-image
        // queries (R.5b), neither of which are wired yet.
        let out =
            make_output_with_valuations(vec![("M", vec![("s0", vec![("a", "0"), ("b", "x")])])]);
        let (_, lab) = synthesise_predicates_and_labellings(&out, &KmtsLiftOptions::default());
        for state_lab in lab["M"].values() {
            for verdict in state_lab.values() {
                assert!(
                    *verdict != Tristate::KleeneBot,
                    "R.2 must not produce KleeneBot"
                );
            }
        }
    }

    #[test]
    fn empty_valuations_yields_empty_labellings() {
        // Bit-blaster did not populate state_valuations (e.g. a
        // degenerate single-state fixture). The lifter must
        // gracefully produce zero predicates + zero labellings
        // rather than erroring.
        let out = make_output_with_valuations(vec![]);
        let (preds, lab) = synthesise_predicates_and_labellings(&out, &KmtsLiftOptions::default());
        assert!(preds.is_empty());
        assert!(lab.is_empty());
    }

    #[test]
    fn max_predicates_cap_truncates() {
        let out = make_output_with_valuations(vec![(
            "M",
            vec![
                ("s0", vec![("a", "0"), ("b", "0"), ("c", "0")]),
                ("s1", vec![("a", "1"), ("b", "1"), ("c", "1")]),
            ],
        )]);
        let lift_opts = KmtsLiftOptions {
            max_predicates: Some(2),
            ..Default::default()
        };
        let (preds, _) = synthesise_predicates_and_labellings(&out, &lift_opts);
        assert_eq!(preds.len(), 2);
    }

    #[test]
    fn lifted_result_labelling_count_sums_correctly() {
        let out = make_output_with_valuations(vec![(
            "M",
            vec![("s0", vec![("x", "0")]), ("s1", vec![("x", "1")])],
        )]);
        let (preds, lab) = synthesise_predicates_and_labellings(&out, &KmtsLiftOptions::default());
        let result = KmtsLiftResult {
            adapter_output: AdapterOutput {
                ctxdsl: String::new(),
                warnings: Vec::new(),
                source_info: crate::adapter::SourceInfo {
                    format: crate::adapter::SourceFormat::Btor2,
                    title: None,
                    signal_count: 0,
                    state_count: 0,
                    property_count: 0,
                },
                sidecars: Vec::new(),
                state_valuations: HashMap::new(),
                transition_observations: Default::default(),
                partition_summary: Default::default(),
            },
            predicates: preds,
            predicate_labellings: lab,
        };
        // 2 states × 2 predicates = 4 (state, predicate) entries.
        assert_eq!(result.labelling_count(), 4);
    }

    // ---- R.2.5 — predicate-cube lift tests ----

    /// Small BTOR2 fixture with 2 state registers (1 bit each) for
    /// API-shape testing. Below the bit-blaster cap; serves as the
    /// "happy path" sanity check.
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

    /// R.2.5 binary capability test — synthetic BTOR2 fixture with
    /// more than MAX_STATE_BITS total state bits (6 registers × 4
    /// bits = 24, vs `MAX_STATE_BITS = 20`). The R.2 lifter errors
    /// on this fixture with "BTOR2 design has 24 state bits"; the
    /// R.2.5 lifter must succeed with a small predicate set.
    const CAP_EXCEEDING_BTOR2: &str = "\
1 sort bitvec 4
2 state 1 reg_0
3 state 1 reg_1
4 state 1 reg_2
5 state 1 reg_3
6 state 1 reg_4
7 state 1 reg_5
8 zero 1
9 init 1 2 8
10 init 1 3 8
11 init 1 4 8
12 init 1 5 8
13 init 1 6 8
14 init 1 7 8
15 next 1 2 8
16 next 1 3 8
17 next 1 4 8
18 next 1 5 8
19 next 1 6 8
20 next 1 7 8
";

    #[test]
    fn predicate_cube_lift_validates_predicate_register_names() {
        // Predicate references a register that does not exist.
        let preds = vec![PredicateSpec {
            name: "bogus".into(),
            register: "nonexistent_reg".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(
            preds,
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().message;
        assert!(
            msg.contains("nonexistent_reg"),
            "error message should name the unknown register; got: {msg}"
        );
    }

    #[test]
    fn predicate_cube_lift_respects_max_cube_count() {
        // |P| = 4 → cube_count = 16; max set to 8 → should error.
        let preds = (0..4)
            .map(|i| PredicateSpec {
                name: format!("p_{i}"),
                register: "reg_a".into(),
                value: i,
            })
            .collect();
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 8,
            max_input_bits: 0,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts);
        assert!(result.is_err());
        let msg = result.unwrap_err().message;
        assert!(
            msg.contains("cube count") && msg.contains("16"),
            "error should mention cube count overflow; got: {msg}"
        );
    }

    #[test]
    fn predicate_cube_lift_emits_2_to_p_states() {
        // |P| = 3 → cube_count = 8 → 8 states in the resulting Clts.
        let preds = vec![
            PredicateSpec {
                name: "p0".into(),
                register: "reg_a".into(),
                value: 0,
            },
            PredicateSpec {
                name: "p1".into(),
                register: "reg_a".into(),
                value: 1,
            },
            PredicateSpec {
                name: "p2".into(),
                register: "reg_b".into(),
                value: 0,
            },
        ];
        let result = predicate_cube_lift(
            preds,
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("predicate_cube_lift succeeds on valid input");
        assert_eq!(result.cube_count, 8);
        assert_eq!(result.clts.state_count(), 8);
        assert_eq!(result.predicates.len(), 3);
        // R.2.5 predicate-image MVP populates may-edges by default
        // (when `max_input_bits > 0` AND at least one predicate exists).
        // The flag flips to `false` to advertise edges are present.
        assert!(
            !result.predicate_image_pending,
            "R.2.5 predicate-image MVP should populate may-edges under default opts"
        );
        // Each cube state must have all 3 predicate verdicts populated.
        for state in result.clts.states() {
            for pred in &result.predicates {
                let verdict = result.clts.state_3valued_predicate(state, &pred.name);
                assert!(
                    verdict.is_some(),
                    "cube state {state:?} missing predicate `{}`",
                    pred.name
                );
                let v = verdict.unwrap();
                assert!(
                    matches!(v, Tristate::KleeneT | Tristate::KleeneF),
                    "R.2.5 MVP cubes carry definite verdicts only (no KleeneBot)"
                );
            }
        }
    }

    #[test]
    fn predicate_cube_lift_state_3valued_predicates_match_cube_bit_pattern() {
        // |P| = 2 → 4 cubes. cube_i's bit pattern matches predicate
        // verdicts: cube 0 = both F; cube 1 = p0 T; cube 2 = p1 T;
        // cube 3 = both T.
        let preds = vec![
            PredicateSpec {
                name: "p0".into(),
                register: "reg_a".into(),
                value: 0,
            },
            PredicateSpec {
                name: "p1".into(),
                register: "reg_b".into(),
                value: 0,
            },
        ];
        let result = predicate_cube_lift(
            preds,
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");
        assert_eq!(result.cube_count, 4);

        // cube_0: bit 0 = 0 → p0 KleeneF; bit 1 = 0 → p1 KleeneF
        let s0 = result.clts.state_id("cube_0").expect("cube_0 exists");
        assert_eq!(
            result.clts.state_3valued_predicate(s0, "p0"),
            Some(Tristate::KleeneF)
        );
        assert_eq!(
            result.clts.state_3valued_predicate(s0, "p1"),
            Some(Tristate::KleeneF)
        );
        // cube_3: bit 0 = 1 → p0 KleeneT; bit 1 = 1 → p1 KleeneT
        let s3 = result.clts.state_id("cube_3").expect("cube_3 exists");
        assert_eq!(
            result.clts.state_3valued_predicate(s3, "p0"),
            Some(Tristate::KleeneT)
        );
        assert_eq!(
            result.clts.state_3valued_predicate(s3, "p1"),
            Some(Tristate::KleeneT)
        );
    }

    #[test]
    fn r2_5_binary_capability_test_cap_exceeding_fixture_lifts() {
        // R.2 errors on CAP_EXCEEDING_BTOR2 (24 state bits >
        // MAX_STATE_BITS = 20). R.2.5 must succeed with a small
        // predicate set.
        let r2_result = lift_btor2_to_kmts(
            CAP_EXCEEDING_BTOR2,
            &AdapterOptions::default(),
            &KmtsLiftOptions::default(),
        );
        assert!(
            r2_result.is_err(),
            "R.2 lifter must error on the cap-exceeding fixture (it inherits MAX_STATE_BITS)"
        );

        // R.2.5 with |P| = 4 cubes should lift in < 10s and < 256MB
        // (the §10.1 R.2.5 done-criterion bounds). Wall-clock here
        // measured by the lift itself; memory is not instrumented at
        // the MVP but the trivial state-construction cost dominates.
        let preds = vec![
            PredicateSpec {
                name: "reg_0_eq_0".into(),
                register: "reg_0".into(),
                value: 0,
            },
            PredicateSpec {
                name: "reg_1_eq_0".into(),
                register: "reg_1".into(),
                value: 0,
            },
            PredicateSpec {
                name: "reg_2_eq_0".into(),
                register: "reg_2".into(),
                value: 0,
            },
            PredicateSpec {
                name: "reg_3_eq_0".into(),
                register: "reg_3".into(),
                value: 0,
            },
        ];
        let r2_5_result = predicate_cube_lift(
            preds,
            CAP_EXCEEDING_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        );
        assert!(
            r2_5_result.is_ok(),
            "R.2.5 lifter must succeed on the cap-exceeding fixture; got {:?}",
            r2_5_result.err()
        );
        let result = r2_5_result.unwrap();
        assert_eq!(result.cube_count, 16);
        assert_eq!(result.clts.state_count(), 16);
        // Wall-clock bound (§10.1 R.2.5 done-criterion: < 10 s). The
        // MVP enumeration is O(|cubes| × |predicates|) bit operations
        // + |cubes| HashMap inserts — should complete in milliseconds.
        assert!(
            result.lift_time < std::time::Duration::from_secs(10),
            "R.2.5 done-criterion wall-clock bound exceeded: {:?}",
            result.lift_time
        );
        // R.2.5 predicate-image MVP populates may-edges by default;
        // the flag flips to `false`. The cap-exceeding fixture's
        // edges are sound under the canonical-representative +
        // boolean-input sampling discipline (§Phase 11 R.2.5
        // predicate-image MVP design doc).
        assert!(
            !result.predicate_image_pending,
            "R.2.5 predicate-image MVP should populate may-edges under default opts"
        );
    }

    // ---- R.2.5 predicate-image MVP — may-edge construction tests ----

    /// BTOR2 fixture: a single 2-bit counter `cnt` that increments
    /// every cycle. One state cell + one 1-bit input `clr` that
    /// resets cnt to 0 when high. Used by R.2.5 predicate-image
    /// tests to verify may-edges are emitted between cubes.
    const COUNTER_BTOR2: &str = r#"
1 sort bitvec 1
2 sort bitvec 2
3 zero 2
4 const 2 11
5 input 1 clr
6 state 2 cnt
7 add 2 6 4
8 ite 2 5 3 7
9 next 2 6 8
"#;

    #[test]
    fn predicate_image_mvp_emits_may_edges_between_cubes() {
        // 2 predicates over the counter: cnt == 0 and cnt == 1.
        // Default opts enable boolean-input enumeration; the lifter
        // should emit MayOnly edges and clear predicate_image_pending.
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("predicate_cube_lift succeeds");

        // 4 cubes (2^2 predicates) — same as the legacy MVP.
        assert_eq!(result.cube_count, 4);
        assert_eq!(result.clts.state_count(), 4);

        // Must/may edges populated (vs the legacy MVP's empty Clts).
        assert!(
            !result.predicate_image_pending,
            "R.2.5 predicate-image MVP must clear predicate_image_pending when edges are populated"
        );

        // Sanity: every cube has at least one outgoing transition
        // (either to itself or another cube). With 1 boolean input
        // `clr` and 4 cube source states, we expect 4 × 2 = 8 edges
        // total (the MVP enumerates each input combo per cube).
        let total_transitions: usize = result
            .clts
            .states()
            .map(|s| result.clts.outgoing(s).len())
            .sum();
        assert!(
            total_transitions > 0,
            "R.2.5 predicate-image MVP must emit at least one may-edge across the cube space"
        );

        // Every transition must be MayOnly (R.2.5 MVP under-
        // approximation discipline — must-witnesses wait for R.5).
        for state in result.clts.states() {
            for transition in result.clts.outgoing(state) {
                assert!(
                    matches!(
                        transition.modality(),
                        crate::clts::TransitionModality::MayOnly
                    ),
                    "R.2.5 predicate-image MVP must emit only MayOnly edges; got {:?}",
                    transition.modality()
                );
            }
        }
    }

    // P1 #1 (IR-unification track) — uart_tx alias pattern: a 1-bit
    // toggle register whose `state` line is labelled `cnt_d` (the `_d`
    // flavour Yosys' flatten leaves), with the `_q` flavour surviving
    // only on a `uext` alias node. A predicate over `cnt_q` must resolve
    // to the real register `cnt_d` (the DR1 #1 blocker) so the toggle
    // transition relation binds correctly.
    const ALIASED_TOGGLE_BTOR2: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 cnt_d\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n7 uext 1 3 0 cnt_q\n";

    /// Collect every `(src_cube, tgt_cube)` edge of a lifted Clts.
    fn collect_cube_edges(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> std::collections::HashSet<(usize, usize)> {
        let mut edges = std::collections::HashSet::new();
        for s in clts.states() {
            for t in clts.outgoing(s) {
                edges.insert((s.index(), t.target().index()));
            }
        }
        edges
    }

    /// A set of `(src_cube, tgt_cube)` edges, indexed by cube state index.
    type CubeEdgeSet = std::collections::HashSet<(usize, usize)>;

    /// PO-1 helper (cube-modal soundness audit, 2026-06-23). Lifts `btor2`
    /// over `predicates` with the SOUND may (`SmtAllPairs`, ∃) + must
    /// (`SmtPerTargetStandard`, ∀∃) relations, then builds the concrete
    /// cube→cube transition relation from an INDEPENDENT oracle
    /// (`simulate_one_step` over every state value × input combo). Returns
    /// `(may_edges, must_edges, concrete_edges)`. Predicates are equality
    /// checks on a single state register, so `cube_of(v)` = the bit-pattern
    /// of which predicate values equal `v`, indexed by `result.predicates`
    /// (the lifter's own cube-bit order).
    fn may_must_concrete_brackets(
        btor2: &str,
        predicates: Vec<PredicateSpec>,
        state_sym: &str,
        state_values: &[u128],
        inputs: &[(&str, &[u128])],
    ) -> (CubeEdgeSet, CubeEdgeSet, CubeEdgeSet) {
        use crate::clts::TransitionModality;
        let lift_opts = PredicateCubeLiftOptions {
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            must_edge_inference: MustEdgeInference::SmtPerTargetStandard,
            ..Default::default()
        };
        let result = predicate_cube_lift(predicates, btor2, &AdapterOptions::default(), &lift_opts)
            .expect("predicate_cube_lift");
        let preds = result.predicates.clone();
        let cube_of = |v: u128| -> usize {
            let mut idx = 0usize;
            for (i, p) in preds.iter().enumerate() {
                if p.value as u128 == v {
                    idx |= 1 << i;
                }
            }
            idx
        };

        // Independent concrete oracle: enumerate every (state, input combo)
        // and record the cube→cube edge it induces.
        let file = crate::adapter::btor2::parser::parse(btor2).expect("parse btor2");
        let mut combos: Vec<std::collections::HashMap<String, u128>> =
            vec![std::collections::HashMap::new()];
        for (name, vals) in inputs {
            let mut next = Vec::new();
            for base in &combos {
                for v in *vals {
                    let mut m = base.clone();
                    m.insert((*name).to_string(), *v);
                    next.push(m);
                }
            }
            combos = next;
        }
        let mut concrete = std::collections::HashSet::new();
        for &v in state_values {
            let regs = std::collections::HashMap::from([(state_sym.to_string(), v)]);
            for combo in &combos {
                let nxt = crate::adapter::btor2::bit_blast::simulate_one_step(&file, &regs, combo)
                    .expect("simulate_one_step");
                let nv = *nxt.get(state_sym).expect("next-state value present");
                concrete.insert((cube_of(v), cube_of(nv)));
            }
        }

        let may = collect_cube_edges(&result.clts);
        let mut must = std::collections::HashSet::new();
        for s in result.clts.states() {
            for t in result.clts.outgoing(s) {
                if matches!(
                    t.modality(),
                    TransitionModality::Sharp | TransitionModality::MustHyperOnly(_)
                ) {
                    must.insert((s.index(), t.target().index()));
                }
            }
        }
        (may, must, concrete)
    }

    /// PO-1 (cube-modal soundness audit, 2026-06-23) — the predicate-cube
    /// lift produces a SOUND KMTS: `may` OVER-approximates and `must`
    /// UNDER-approximates the concrete BTOR2 relation. These are exactly
    /// the two premises the §4.5 preservation theorem needs (may-step
    /// accommodation `R ⊆ γ(R_may)` + must-step preservation
    /// `R_must ⊆ ᾱ(R)`). With PO-5 (the evaluator computes §4.3 over
    /// whatever KMTS it is handed) this closes the end-to-end soundness
    /// chain for the audited-sound fragment: a sound KMTS in, §4.3 out,
    /// preservation transfers the definite verdict to the concrete design.
    ///
    /// Deterministic fixture (1-bit toggle): `may == must == concrete` —
    /// exact, no over/under gap.
    #[test]
    fn po1_cube_brackets_concrete_deterministic_toggle() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_1".into(),
            register: "cnt_q".into(),
            value: 1,
        }];
        let (may, must, concrete) =
            may_must_concrete_brackets(ALIASED_TOGGLE_BTOR2, preds, "cnt_d", &[0, 1], &[]);

        assert!(
            concrete.is_subset(&may),
            "may must OVER-approximate concrete: may={may:?} concrete={concrete:?}"
        );
        assert!(
            must.is_subset(&concrete),
            "must must UNDER-approximate concrete: must={must:?} concrete={concrete:?}"
        );
        // cube_0 = {cnt=0}, cube_1 = {cnt=1}; toggle ⇒ {0→1, 1→0}, tight.
        let expected: std::collections::HashSet<(usize, usize)> =
            [(0, 1), (1, 0)].into_iter().collect();
        assert_eq!(concrete, expected, "toggle concrete relation");
        assert_eq!(may, expected, "deterministic ⇒ may has no spurious edges");
        assert_eq!(
            must, expected,
            "deterministic ⇒ every concrete edge is a must"
        );
    }

    /// PO-1 — nondeterministic fixture (2-bit down-counter with a free
    /// `clr` input) under a COARSE predicate `cnt=0`, so `cube_0 =
    /// {cnt∈1,2,3}` spans three concrete states and the over/under gap is
    /// real. `may ⊇ concrete`, `must ⊆ concrete`, AND `must ⊊ may`:
    /// `cube_0→cube_0` is may-only — `cnt=1` decrements only to `0` (it
    /// cannot stay in {1,2,3}), so the move is possible but not forced.
    #[test]
    fn po1_cube_brackets_concrete_nondeterministic_gap() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let (may, must, concrete) = may_must_concrete_brackets(
            COUNTER_BTOR2,
            preds,
            "cnt",
            &[0, 1, 2, 3],
            &[("clr", &[0, 1])],
        );

        assert!(
            concrete.is_subset(&may),
            "may must OVER-approximate concrete: may={may:?} concrete={concrete:?}"
        );
        assert!(
            must.is_subset(&concrete),
            "must must UNDER-approximate concrete: must={must:?} concrete={concrete:?}"
        );
        // The over/under gap: cube_0→cube_0 (cube_0 = {cnt∈1,2,3}).
        let self_loop_0 = (0usize, 0usize);
        assert!(
            may.contains(&self_loop_0),
            "cube_0→cube_0 IS concretely possible (cnt=2→1): may={may:?}"
        );
        assert!(
            !must.contains(&self_loop_0),
            "cube_0→cube_0 is NOT forced (cnt=1 decrements only to 0): must={must:?}"
        );
        assert!(
            must.len() < may.len(),
            "the may/must gap must be non-empty: may={may:?} must={must:?}"
        );
    }

    #[test]
    fn p1_predicate_cube_lift_resolves_alias_register_eager() {
        // Predicate names the alias `cnt_q`. Pre-P1 this errored with
        // "unknown register `cnt_q`"; with the seam resolution it binds
        // to the canonical `cnt_d` and the toggle relation (0↔1) emerges.
        let preds = vec![PredicateSpec {
            name: "cnt_is_1".into(),
            register: "cnt_q".into(),
            value: 1,
        }];
        let result = predicate_cube_lift(
            preds,
            ALIASED_TOGGLE_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("alias `cnt_q` must resolve to canonical `cnt_d`, not error");

        // The predicate was rewritten to the canonical state-cell name.
        assert_eq!(
            result.predicates[0].register, "cnt_d",
            "P1 #1: predicate register should be rewritten to the canonical state cell"
        );

        // Toggle binding: cube_0 (cnt_q==1 false ⇒ cnt_d=0) → cube_1,
        // cube_1 (cnt_d=1) → cube_0. The self-loop (1,1) would only
        // appear if the alias never bound (cnt_d stuck at its default 0).
        let edges = collect_cube_edges(&result.clts);
        assert!(edges.contains(&(0, 1)), "0→1 expected: {edges:?}");
        assert!(edges.contains(&(1, 0)), "1→0 expected (toggle): {edges:?}");
        assert!(
            !edges.contains(&(1, 1)),
            "no self-loop on cube_1 — its presence means `cnt_q` never bound: {edges:?}"
        );
    }

    #[test]
    fn p1_lazy_lift_resolves_alias_register() {
        // Same alias fixture through the lazy path CEGAR also routes
        // through. Pre-P1 the lazy path did no resolution at all, so the
        // unbound `cnt_q` left `cnt_d` stuck at 0 and cube_1 self-looped.
        let preds = vec![PredicateSpec {
            name: "cnt_is_1".into(),
            register: "cnt_q".into(),
            value: 1,
        }];
        let mut lazy = LazyLift::from_btor2(
            preds,
            ALIASED_TOGGLE_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("lazy from_btor2 must resolve the alias, not error");

        let t0: Vec<usize> = lazy.expand_cube(0).iter().map(|e| e.target_cube).collect();
        let t1: Vec<usize> = lazy.expand_cube(1).iter().map(|e| e.target_cube).collect();
        assert_eq!(t0, vec![1], "cube_0 toggles to cube_1: {t0:?}");
        assert_eq!(
            t1,
            vec![0],
            "cube_1 toggles to cube_0 — a [1] here means the alias never bound: {t1:?}"
        );
    }

    #[test]
    fn p1_predicate_cube_lift_unresolvable_register_still_errors() {
        // A predicate over a name that matches no state cell (directly
        // or via any alias) must still error, naming the register.
        let preds = vec![PredicateSpec {
            name: "bogus".into(),
            register: "no_such_signal".into(),
            value: 1,
        }];
        let err = predicate_cube_lift(
            preds,
            ALIASED_TOGGLE_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect_err("an unresolvable register must error");
        assert!(
            err.message.contains("no_such_signal"),
            "error should name the unknown register; got: {}",
            err.message
        );
    }

    #[test]
    fn p1_3_smt_all_pairs_composes_with_must_promotes_sharp() {
        // P1 #3 (IR-unification track) — SmtAllPairs may + a non-Off must
        // inference now compose (DR1 F5). On the deterministic toggle
        // q'=!q the canonical ∀∃ must-relation equals the may-relation, so
        // both 0→1 and 1→0 promote MayOnly → Sharp. Baseline (must=Off)
        // composes nothing; the composed run promotes two edges.
        let toggle =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let preds = || {
            vec![PredicateSpec {
                name: "q_is_1".into(),
                register: "q".into(),
                value: 1,
            }]
        };
        let opts = |must| PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: must,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };

        // Baseline: SmtAllPairs may, no must → MayOnly only, zero promotions.
        let no_must = predicate_cube_lift(
            preds(),
            toggle,
            &AdapterOptions::default(),
            &opts(MustEdgeInference::Off),
        )
        .expect("lift (may only)");
        assert_eq!(
            no_must.sharp_edges_promoted, 0,
            "must=Off composes no must-edges"
        );

        // Composed: SmtAllPairs may + ∀∃ must → 2 Sharp promotions.
        let composed = predicate_cube_lift(
            preds(),
            toggle,
            &AdapterOptions::default(),
            &opts(MustEdgeInference::SmtPerTargetStandard),
        )
        .expect("lift (may + must)");
        assert_eq!(
            composed.sharp_edges_promoted, 2,
            "both toggle edges (0→1, 1→0) must promote MayOnly → Sharp"
        );
        let mut sharp = 0usize;
        for s in composed.clts.states() {
            for t in composed.clts.outgoing(s) {
                if matches!(t.modality(), crate::clts::TransitionModality::Sharp) {
                    sharp += 1;
                }
            }
        }
        assert!(
            sharp >= 2,
            "composed CLTS must carry the promoted Sharp must-edges; got {sharp}"
        );
    }

    #[test]
    fn b2_smt_all_pairs_smt_hyper_must_emits_must_hyper_only() {
        // B.2 (2026-06-26) — on the SmtAllPairs path, `SmtHyperMust` emits
        // GKMTS `MustHyperOnly` edges (over each source's may-successor set)
        // instead of the ∀∃ `Sharp` promotion. This is the monotone must
        // form that makes compound/νμ verdicts clean-sound (no B.3.b
        // soundness-tag). On the deterministic toggle q'=!q each source has a
        // singleton may-successor set, so both states get a hyper-must edge.
        let toggle =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let preds = vec![PredicateSpec {
            name: "q_is_1".into(),
            register: "q".into(),
            value: 1,
        }];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let lifted = predicate_cube_lift(preds, toggle, &AdapterOptions::default(), &opts)
            .expect("SmtAllPairs + SmtHyperMust lift");
        assert!(
            lifted.hyper_must_edges_emitted > 0,
            "SmtAllPairs + SmtHyperMust must emit MustHyperOnly edges; got {}",
            lifted.hyper_must_edges_emitted
        );
        // The SmtHyperMust branch emits hyper-must, NOT the ∀∃ Sharp form.
        assert_eq!(
            lifted.sharp_edges_promoted, 0,
            "the SmtHyperMust branch emits MustHyperOnly, not Sharp promotions"
        );
        let hyper = lifted
            .clts
            .states()
            .flat_map(|s| lifted.clts.outgoing(s))
            .filter(|t| {
                matches!(
                    t.modality(),
                    crate::clts::TransitionModality::MustHyperOnly(_)
                )
            })
            .count();
        assert!(
            hyper > 0,
            "the lifted CLTS must carry MustHyperOnly transitions; got {hyper}"
        );
    }

    #[test]
    fn b1_compound_predicate_lifts_via_smt_all_pairs() {
        // B.1 (3b) — compound predicate flows end-to-end through the lift.
        // a:=0 (always), b:=in_b. Compound idle=(a==0 && b==0). cube_0 =
        // {idle false}, cube_1 = {idle true}. Because b can be 1, a non-idle
        // state can STAY non-idle (in_b=1), so the may-edge cube_0→cube_0
        // exists ONLY because of the `&& b==0` conjunct — the a==0-only atom
        // would lack it (a:=0 always ⇒ next a==0 ⇒ never stays non-idle).
        let btor2 = "1 sort bitvec 1\n2 input 1 in_b\n3 state 1 a\n4 state 1 b\n5 zero 1\n6 init 1 3 5\n7 init 1 4 5\n8 next 1 3 5\n9 next 1 4 2\n";
        let mut compound_exprs = std::collections::HashMap::new();
        compound_exprs.insert(
            "idle".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::And(
                Box::new(crate::adapter::btor2::predicate_expr::PredicateExpr::eq(
                    "a", 0,
                )),
                Box::new(crate::adapter::btor2::predicate_expr::PredicateExpr::eq(
                    "b", 0,
                )),
            ),
        );
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            config_values: std::collections::HashMap::new(),
            compound_exprs,
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let preds = vec![PredicateSpec {
            name: "idle".into(),
            // Placeholder register; the compound expr drives the cube truth.
            register: "a".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(preds, btor2, &AdapterOptions::default(), &opts)
            .expect("compound lift via SmtAllPairs");
        let cube0 = result.clts.state_id("cube_0").expect("cube_0 exists");
        let targets: Vec<usize> = result
            .clts
            .outgoing(cube0)
            .iter()
            .map(|t| t.target().index())
            .collect();
        assert!(
            targets.contains(&0),
            "compound idle=(a==0 && b==0) with b:=in_b must have may-edge cube_0→cube_0 \
             (in_b=1 keeps it non-idle) — the conjunct the a==0-only atom would miss; got {targets:?}"
        );
    }

    #[test]
    fn b1_compound_predicate_requires_smt_all_pairs() {
        // The 3b gate: compounds + a non-SmtAllPairs may-source must error,
        // because the cube→representative-registers inverse (sampling) is sound
        // only for simple register==value atoms.
        let btor2 = "1 sort bitvec 1\n2 state 1 a\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let mut compound_exprs = std::collections::HashMap::new();
        compound_exprs.insert(
            "p".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::eq("a", 0),
        );
        let opts = PredicateCubeLiftOptions {
            // Sampling (Off) — disallowed when compounds are present.
            may_edge_inference: MayEdgeInference::Off,
            compound_exprs,
            derived_predicates: Vec::new(),
            may_postimage: false,
            ..Default::default()
        };
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "a".into(),
            value: 0,
        }];
        let err = predicate_cube_lift(preds, btor2, &AdapterOptions::default(), &opts)
            .expect_err("compound + sampling must error");
        assert!(
            err.message.contains("compound predicates require"),
            "expected the SmtAllPairs gate error; got: {}",
            err.message
        );
    }

    #[test]
    fn rel_relational_predicate_lifts_via_smt_all_pairs() {
        // REL — a relational predicate `stable = (s == s_past)` (the `$stable`
        // shape) flows end-to-end through predicate_cube_lift. s := in_s (free),
        // s_past := s (the XL.3b shadow flop). From a stable cube, in_s=0 keeps
        // it stable and in_s=1 breaks it, so cube_1 has may-edges to BOTH cubes
        // — a distinction a `register==literal` predicate cannot express. This
        // exercises the full wiring: resolve, the SmtAllPairs gate, and the SMT
        // predicate-image honouring a CmpReg leaf.
        let btor2 = "1 sort bitvec 1\n2 input 1 in_s\n3 state 1 s\n4 state 1 s_past\n5 zero 1\n6 init 1 3 5\n7 init 1 4 5\n8 next 1 3 2\n9 next 1 4 3\n";
        let mut compound_exprs = std::collections::HashMap::new();
        compound_exprs.insert(
            "stable".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::eq_reg("s", "s_past"),
        );
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            config_values: std::collections::HashMap::new(),
            compound_exprs,
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let preds = vec![PredicateSpec {
            name: "stable".into(),
            // Placeholder register; the relational CmpReg expr drives the cube bit.
            register: "s".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(preds, btor2, &AdapterOptions::default(), &opts)
            .expect("relational lift via SmtAllPairs");
        assert_eq!(result.clts.state_count(), 2, "|P| = 1 → 2 cubes");
        let stable_cube = result
            .clts
            .state_id("cube_1")
            .expect("cube_1 (stable=true)");
        let targets: Vec<usize> = result
            .clts
            .outgoing(stable_cube)
            .iter()
            .map(|t| t.target().index())
            .collect();
        assert!(
            targets.contains(&1),
            "stable can stay stable (in_s=0): cube_1→cube_1; got {targets:?}"
        );
        assert!(
            targets.contains(&0),
            "stable can become unstable (in_s=1): cube_1→cube_0 — the relational \
             distinction a literal predicate can't make; got {targets:?}"
        );
    }

    // U.1 (lift-unification, 2026-06-26) — the eager≡lazy differential guard.
    // The eager sampling lift (`predicate_cube_lift` with may = sampling) and the
    // lazy lift (`LazyLift` + `materialize_clts_from_lazy`) MUST produce identical
    // CLTSes — edge-for-edge, modality-for-modality, 3-valued-label-for-label — for
    // the same fixture + options, so the two duplicated paths can never silently
    // diverge. This is the regression net the lift-unification refactor (U.2–U.4)
    // is built on; it must stay green through every extraction phase. (SmtAllPairs
    // may is eager-only-global, outside this equivalence; the corpus is
    // non-controllability — lazy gains controllability parity in U.3.)
    // (src_index, sorted label payloads, target_index, modality string).
    type CanonEdge = (usize, Vec<String>, usize, String);
    // (state_index, predicate name, tristate string).
    type CanonPred = (usize, String, String);
    fn canonical_clts(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
    ) -> (Vec<CanonEdge>, Vec<CanonPred>) {
        let modality_str = |m: &crate::clts::TransitionModality<DefaultStateIdx>| -> String {
            match m {
                crate::clts::TransitionModality::Sharp => "Sharp".to_string(),
                crate::clts::TransitionModality::MayOnly => "MayOnly".to_string(),
                crate::clts::TransitionModality::MustHyperOnly(targets) => {
                    let mut idx: Vec<usize> = targets.iter().map(|t| t.index()).collect();
                    idx.sort_unstable();
                    format!("MustHyper{idx:?}")
                }
            }
        };
        let mut edges: Vec<CanonEdge> = Vec::new();
        for s in clts.states() {
            for t in clts.outgoing(s) {
                let mut labels: Vec<String> = t
                    .labels()
                    .iter()
                    .filter_map(|l| clts.label_payload(*l).map(|p| p.join("+")))
                    .collect();
                labels.sort();
                edges.push((
                    s.index(),
                    labels,
                    t.target().index(),
                    modality_str(t.modality()),
                ));
            }
        }
        edges.sort();
        edges.dedup();
        let mut preds: Vec<CanonPred> = Vec::new();
        for s in clts.states() {
            for (name, tri) in clts.state_3valued_predicate_entries(s) {
                preds.push((s.index(), name.to_string(), format!("{tri:?}")));
            }
        }
        preds.sort();
        (edges, preds)
    }

    #[test]
    fn u1_eager_lazy_differential_equivalence() {
        use crate::adapter::AdapterOptions;
        // toggle (1 pred), 2-bit wrapping counter (2 preds), input-driven (1 pred,
        // nondeterministic). All non-controllability, may = sampling.
        let toggle =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let counter = "1 sort bitvec 2\n2 state 2 r\n3 one 2\n4 add 2 2 3\n5 zero 2\n6 init 2 2 5\n7 next 2 2 4\n";
        let input_driven = "1 sort bitvec 1\n2 input 1 in_a\n3 state 1 reg_a\n4 zero 1\n5 init 1 3 4\n6 next 1 3 2\n";
        let fixtures: Vec<(&str, &str, Vec<PredicateSpec>)> = vec![
            (
                "toggle",
                toggle,
                vec![PredicateSpec {
                    name: "q1".into(),
                    register: "q".into(),
                    value: 1,
                }],
            ),
            (
                "counter",
                counter,
                vec![
                    PredicateSpec {
                        name: "r0".into(),
                        register: "r".into(),
                        value: 0,
                    },
                    PredicateSpec {
                        name: "r3".into(),
                        register: "r".into(),
                        value: 3,
                    },
                ],
            ),
            (
                "input_driven",
                input_driven,
                vec![PredicateSpec {
                    name: "a0".into(),
                    register: "reg_a".into(),
                    value: 0,
                }],
            ),
        ];

        for must in [
            MustEdgeInference::Off,
            MustEdgeInference::SmtPerTarget,
            MustEdgeInference::SmtPerTargetStandard,
            MustEdgeInference::SmtHyperMust,
        ] {
            for (label, btor2, preds) in &fixtures {
                let lift_opts = PredicateCubeLiftOptions {
                    max_cube_count: 1024,
                    max_input_bits: 8,
                    must_edge_inference: must,
                    may_edge_inference: MayEdgeInference::Off,
                    config_values: std::collections::HashMap::new(),
                    compound_exprs: std::collections::HashMap::new(),
                    derived_predicates: Vec::new(),
                    may_postimage: false,
                };
                let eager = predicate_cube_lift(
                    preds.clone(),
                    btor2,
                    &AdapterOptions::default(),
                    &lift_opts,
                )
                .unwrap_or_else(|e| panic!("eager lift {label}/{must:?}: {}", e.message));
                let mut lazy = LazyLift::from_btor2(
                    preds.clone(),
                    btor2,
                    &AdapterOptions::default(),
                    &lift_opts,
                )
                .unwrap_or_else(|e| panic!("lazy build {label}/{must:?}: {}", e.message));
                let lazy_result = materialize_clts_from_lazy(
                    &mut lazy,
                    crate::adapter::SourceFormat::Btor2,
                    must,
                )
                .unwrap_or_else(|e| panic!("lazy materialize {label}/{must:?}: {}", e.message));
                assert_eq!(
                    canonical_clts(&eager.clts),
                    canonical_clts(&lazy_result.clts),
                    "eager ≢ lazy for fixture {label}, must = {must:?}"
                );
            }
        }
    }

    // U.4 (lift-unification, 2026-06-26) — the gated `lift_predicate_cube`
    // entry must dispatch Eager vs Lazy to identical CLTSes (the entry-level
    // analogue of U.1, proving the dispatch itself is correct).
    #[test]
    fn u4_lift_predicate_cube_entry_eager_lazy_equivalence() {
        use crate::adapter::AdapterOptions;
        use crate::adapter::btor2::cegar::LiftStrategy;
        let counter = "1 sort bitvec 2\n2 state 2 r\n3 one 2\n4 add 2 2 3\n5 zero 2\n6 init 2 2 5\n7 next 2 2 4\n";
        let preds = vec![
            PredicateSpec {
                name: "r0".into(),
                register: "r".into(),
                value: 0,
            },
            PredicateSpec {
                name: "r3".into(),
                register: "r".into(),
                value: 3,
            },
        ];
        let lift_opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtPerTarget,
            may_edge_inference: MayEdgeInference::Off,
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let eager = lift_predicate_cube(
            preds.clone(),
            counter,
            &AdapterOptions::default(),
            &lift_opts,
            LiftStrategy::Eager,
        )
        .expect("eager entry");
        let lazy = lift_predicate_cube(
            preds,
            counter,
            &AdapterOptions::default(),
            &lift_opts,
            LiftStrategy::Lazy,
        )
        .expect("lazy entry");
        assert_eq!(
            canonical_clts(&eager.clts),
            canonical_clts(&lazy.clts),
            "lift_predicate_cube entry: Eager ≢ Lazy"
        );
    }

    // U.4 — the entry's compound gate rejects Lazy + compounds (the lazy
    // per-cube body never consults `compound_exprs`).
    #[test]
    fn u4_lift_predicate_cube_lazy_rejects_compounds() {
        use crate::adapter::AdapterOptions;
        use crate::adapter::btor2::cegar::LiftStrategy;
        let btor2 = "1 sort bitvec 1\n2 state 1 a\n3 zero 1\n4 init 1 2 3\n5 next 1 2 3\n";
        let mut compound_exprs = std::collections::HashMap::new();
        compound_exprs.insert(
            "p".to_string(),
            crate::adapter::btor2::predicate_expr::PredicateExpr::eq("a", 0),
        );
        let opts = PredicateCubeLiftOptions {
            // Even with SmtAllPairs, Lazy can't honour compounds.
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            compound_exprs,
            derived_predicates: Vec::new(),
            may_postimage: false,
            ..Default::default()
        };
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "a".into(),
            value: 0,
        }];
        let err = lift_predicate_cube(
            preds,
            btor2,
            &AdapterOptions::default(),
            &opts,
            LiftStrategy::Lazy,
        )
        .expect_err("Lazy + compounds must error");
        assert!(
            err.message
                .contains("not supported on the Lazy lift strategy"),
            "expected the Lazy compound-gate error; got: {}",
            err.message
        );
    }

    #[test]
    fn predicate_image_mvp_max_input_bits_zero_disables_edges() {
        // Setting max_input_bits = 0 recovers the legacy R.2.5 MVP
        // behaviour (cube states but no edges; predicate_image_pending
        // stays true). Useful for the binary-capability test that
        // doesn't need edges.
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 0,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("ok");
        assert!(
            result.predicate_image_pending,
            "max_input_bits=0 must preserve the legacy 'no edges' behaviour"
        );
        let total_transitions: usize = result
            .clts
            .states()
            .map(|s| result.clts.outgoing(s).len())
            .sum();
        assert_eq!(
            total_transitions, 0,
            "max_input_bits=0 must emit zero edges (legacy MVP behaviour)"
        );
    }

    // ---- R.2.5b — must-edge post-pass tests ----

    /// R.2.5b — Default `MustEdgeInference::Off` preserves the
    /// pre-R.2.5b behaviour: only MayOnly edges, zero promotions.
    #[test]
    fn r5_2_5b_default_inference_off_preserves_legacy_behaviour() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("predicate_cube_lift succeeds");
        assert_eq!(result.sharp_edges_promoted, 0, "no promotions when Off");
        assert_eq!(result.hyper_must_edges_emitted, 0, "no hyper-must when Off");
    }

    // ---- R.2.5b session-2 — SmtPerTarget post-pass tests ----

    /// R.2.5b session 2 — `SmtPerTarget` post-pass on the counter
    /// fixture. The Z3 BV-theory check should prove at least one
    /// Sharp edge (the `clr=1 ⟹ cnt:=0` branch is deterministic
    /// for cube `{cnt==0}` self-loop under all inputs that satisfy
    /// the source predicate IF the source predicate already pins
    /// the relevant register). The exact count is fixture-dependent;
    /// the test asserts non-zero + the SMT soundness warning.
    #[test]
    fn r5_2_5b_smt_per_target_emits_sharp_promotions_on_counter() {
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtPerTarget,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("predicate_cube_lift succeeds");

        // SmtPerTarget only emits per-target Sharp; never hyper-must
        // (queued for session-2 follow-up).
        assert_eq!(
            result.hyper_must_edges_emitted, 0,
            "SmtPerTarget MVP must NOT emit hyper-must edges; got {}",
            result.hyper_must_edges_emitted
        );

        // The Z3 SMT check should prove at least one Sharp on this
        // fixture — the cube `{cnt_is_0, cnt_is_1}` (an unreachable
        // combination, both predicates true) is self-loop-Sharp
        // under SMT because the cube's UNSAT precondition (no
        // register value can satisfy both) makes the must-edge
        // formula vacuously true.
        //
        // The MVP's strong ∀∀ form succeeds on at least one
        // (src, tgt) pair where src's predicates UNSAT-constrain
        // the state space; on those pairs ¬tgt is irrelevant.
        // Soundness warning fires when any promotion happens.
        if result.sharp_edges_promoted > 0 {
            let any_smt_warning = result.warnings.iter().any(|w| {
                w.message.contains("R.2.5b-smt-must")
                    && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
            });
            assert!(
                any_smt_warning,
                "SmtPerTarget must emit the [R.2.5b-smt-must] warning when it promotes edges; got warnings: {:?}",
                result.warnings
            );
        }
        // The MVP may legitimately produce 0 Sharp on this fixture
        // (the ∀∀ form is strictly stronger than ∀∃; only vacuous
        // or input-independent edges qualify). The non-strict
        // assertion above guards the warning's presence iff
        // promotion happened — which is the invariant we test.
    }

    /// R.2.5b session 2 — `SmtPerTarget` skips the post-pass when
    /// `max_input_bits = 0` (no may-edges sampled → no target sets
    /// to SMT-check → no promotions).
    #[test]
    fn r5_2_5b_smt_per_target_noop_when_max_input_bits_zero() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 0,
            must_edge_inference: MustEdgeInference::SmtPerTarget,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("predicate_cube_lift succeeds");
        assert_eq!(
            result.sharp_edges_promoted, 0,
            "no SMT promotions when max_input_bits=0"
        );
        assert_eq!(
            result.hyper_must_edges_emitted, 0,
            "no hyper-must when SmtPerTarget + max_input_bits=0"
        );
    }

    /// R.2.5b session 2 — Lazy materialiser threads
    /// `MustEdgeInference::SmtPerTarget` end-to-end. Same shape as
    /// the `SmtHyperMust` lazy test; ensures both eager + lazy
    /// paths share the dispatch.
    #[test]
    fn r5_2_5b_lazy_materialiser_honors_smt_per_target() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtPerTarget,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let mut lazy =
            LazyLift::from_btor2(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
                .expect("lazy lift constructs");
        let result = materialize_clts_from_lazy(
            &mut lazy,
            crate::adapter::SourceFormat::Btor2,
            MustEdgeInference::SmtPerTarget,
        )
        .expect("materialize succeeds");

        // Same invariants as the eager path: no hyper-must in MVP;
        // if any Sharp promoted, the [R.2.5b-smt-must] warning
        // must be present.
        assert_eq!(
            result.hyper_must_edges_emitted, 0,
            "Lazy SmtPerTarget MVP must NOT emit hyper-must edges"
        );
        if result.sharp_edges_promoted > 0 {
            let any_smt_warning = result.warnings.iter().any(|w| {
                w.message.contains("R.2.5b-smt-must")
                    && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
            });
            assert!(
                any_smt_warning,
                "Lazy SmtPerTarget must emit [R.2.5b-smt-must] when promoting; got warnings: {:?}",
                result.warnings
            );
        }
    }

    // ---- R.2.5b session-2 follow-up (2026-06-09) — SmtPerTargetStandard + SmtHyperMust ----

    /// R.2.5b session-2 follow-up — `SmtPerTargetStandard` ∀∃ form
    /// on the counter fixture. Strict-supremacy invariant: this form
    /// promotes a SUPERSET of the SmtPerTarget MVP's edges (every
    /// ∀∀-Must is also ∀∃-Must, plus additional edges where SOME
    /// input per state reaches tgt).
    #[test]
    fn r5_2_5b_smt_per_target_standard_emits_sharp_promotions_on_counter() {
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtPerTargetStandard,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(
            preds.clone(),
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lift succeeds");
        // SmtPerTargetStandard never emits hyper-must edges (those
        // are SmtHyperMust territory).
        assert_eq!(
            result.hyper_must_edges_emitted, 0,
            "SmtPerTargetStandard never emits hyper-must edges"
        );
        if result.sharp_edges_promoted > 0 {
            let any_warning = result.warnings.iter().any(|w| {
                w.message.contains("R.2.5b-smt-must-standard")
                    && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
            });
            assert!(
                any_warning,
                "SmtPerTargetStandard must emit [R.2.5b-smt-must-standard] when promoting; \
                 got warnings: {:?}",
                result.warnings
            );
        }
        // Strict-supremacy: the ∀∃ standard form should promote
        // AT LEAST as many edges as the ∀∀ MVP form on the same
        // fixture. Re-run with MVP for comparison.
        let mvp_opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtPerTarget,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let mvp_result =
            predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &mvp_opts)
                .expect("MVP lift succeeds");
        assert!(
            result.sharp_edges_promoted >= mvp_result.sharp_edges_promoted,
            "Strict-supremacy: ∀∃ standard form ({}) must promote ≥ ∀∀ MVP ({}); \
             every ∀∀-Must is also ∀∃-Must.",
            result.sharp_edges_promoted,
            mvp_result.sharp_edges_promoted
        );
    }

    /// R.2.5b session-2 follow-up — `SmtHyperMust` on the counter
    /// fixture. The MVP strategy: per-target ∀∃ singletons first, then
    /// full-target-set hyper-must fallback. Counter fixture has small
    /// |T| per source (typically 1–2), so the singleton path
    /// dominates. Hyper-must emits only when no singleton proves
    /// AND |T| > 1.
    #[test]
    fn r5_2_5b_smt_hyper_must_emits_promotions_on_counter() {
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("lift succeeds");
        if result.sharp_edges_promoted > 0 || result.hyper_must_edges_emitted > 0 {
            let any_warning = result.warnings.iter().any(|w| {
                w.message.contains("R.2.5b-smt-must-hyper")
                    && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
            });
            assert!(
                any_warning,
                "SmtHyperMust must emit [R.2.5b-smt-must-hyper] when promoting; \
                 got warnings: {:?}",
                result.warnings
            );
        }
    }

    /// R.2.5b session-2 follow-up — both new variants noop when
    /// `max_input_bits = 0` (no may-edges sampled → no targets to
    /// SMT-check).
    #[test]
    fn r5_2_5b_smt_standard_and_hyper_noop_when_max_input_bits_zero() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        for variant in [
            MustEdgeInference::SmtPerTargetStandard,
            MustEdgeInference::SmtHyperMust,
        ] {
            let opts = PredicateCubeLiftOptions {
                max_cube_count: 1024,
                max_input_bits: 0,
                must_edge_inference: variant,
                may_edge_inference: Default::default(),
                config_values: std::collections::HashMap::new(),
                compound_exprs: std::collections::HashMap::new(),
                derived_predicates: Vec::new(),
                may_postimage: false,
            };
            let result = predicate_cube_lift(
                preds.clone(),
                COUNTER_BTOR2,
                &AdapterOptions::default(),
                &opts,
            )
            .expect("lift succeeds");
            assert_eq!(
                result.sharp_edges_promoted, 0,
                "{variant:?}: no promotions when max_input_bits=0"
            );
            assert_eq!(
                result.hyper_must_edges_emitted, 0,
                "{variant:?}: no hyper-must when max_input_bits=0"
            );
        }
    }

    /// R.2.5b session-2 follow-up — lazy materialiser threads both
    /// new variants end-to-end. Ensures dispatch symmetry between
    /// eager + lazy paths.
    #[test]
    fn r5_2_5b_lazy_materialiser_honors_smt_standard_and_hyper() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        for variant in [
            MustEdgeInference::SmtPerTargetStandard,
            MustEdgeInference::SmtHyperMust,
        ] {
            let opts = PredicateCubeLiftOptions {
                max_cube_count: 1024,
                max_input_bits: 8,
                must_edge_inference: variant,
                may_edge_inference: Default::default(),
                config_values: std::collections::HashMap::new(),
                compound_exprs: std::collections::HashMap::new(),
                derived_predicates: Vec::new(),
                may_postimage: false,
            };
            let mut lazy = LazyLift::from_btor2(
                preds.clone(),
                COUNTER_BTOR2,
                &AdapterOptions::default(),
                &opts,
            )
            .expect("lazy lift constructs");
            let result =
                materialize_clts_from_lazy(&mut lazy, crate::adapter::SourceFormat::Btor2, variant)
                    .expect("materialize succeeds");
            // SmtPerTargetStandard never emits hyper-must; SmtHyperMust may.
            if matches!(variant, MustEdgeInference::SmtPerTargetStandard) {
                assert_eq!(
                    result.hyper_must_edges_emitted, 0,
                    "Lazy SmtPerTargetStandard MVP must NOT emit hyper-must edges"
                );
            }
            if result.sharp_edges_promoted > 0 || result.hyper_must_edges_emitted > 0 {
                let pattern = match variant {
                    MustEdgeInference::SmtPerTargetStandard => "R.2.5b-smt-must-standard",
                    MustEdgeInference::SmtHyperMust => "R.2.5b-smt-must-hyper",
                    _ => unreachable!(),
                };
                let any_warning = result.warnings.iter().any(|w| {
                    w.message.contains(pattern)
                        && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
                });
                assert!(
                    any_warning,
                    "Lazy {variant:?} must emit [{pattern}] when promoting; \
                     got warnings: {:?}",
                    result.warnings
                );
            }
        }
    }

    // ---- R.6.6 (2026-06-08) — controllability-aware lifter tests ----

    /// R.6.6 — when `AdapterOptions::controllable_inputs` is empty
    /// (the default), the lifter preserves pre-R.6.6 behavior
    /// bit-for-bit: a single `step` label per transition, no
    /// controllability tags on labels. Verdict-equivalence invariant
    /// for the R-track's existing fixtures.
    #[test]
    fn r6_6_no_controllable_inputs_preserves_legacy_single_label() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("lift succeeds");
        let labels = result.clts.alphabet();
        assert!(
            labels.iter().any(|l| l == "step"),
            "legacy `step` label must be present; got labels: {labels:?}"
        );
        // No env_/ctrl_ labels emitted in legacy mode.
        assert!(
            !labels
                .iter()
                .any(|l| l.starts_with("env_c") || l.starts_with("ctrl_c")),
            "no R.6.6 env_/ctrl_ labels in legacy mode; got: {labels:?}"
        );
    }

    /// R.6.6 — when `controllable_inputs = ["clr"]` on the counter
    /// fixture (whose single boolean input is `clr`), the lifter
    /// emits per-combo labels with controllability tags. The counter
    /// has 1 input, so n_env=0 + n_ctrl=1 ⇒ 2 ctrl labels
    /// (`ctrl_c0`, `ctrl_c1`), no env labels.
    #[test]
    fn r6_6_single_controllable_input_emits_ctrl_labels() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let mut opts = AdapterOptions::default();
        opts.controllable_inputs.push("clr".into());
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &opts,
            &PredicateCubeLiftOptions::default(),
        )
        .expect("lift succeeds");
        let labels = result.clts.alphabet();
        assert!(
            labels.iter().any(|l| l == "ctrl_c0"),
            "expected ctrl_c0 label; got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "ctrl_c1"),
            "expected ctrl_c1 label; got: {labels:?}"
        );
        // No env labels (0 env inputs).
        assert!(
            !labels.iter().any(|l| l.starts_with("env_c")),
            "no env labels expected with 0 env inputs; got: {labels:?}"
        );
        // Every ctrl_c* label must land in the controllable alphabet.
        for &label_id in result.clts.controllable_alphabet() {
            if let Some(payload) = result.clts.label_payload(label_id)
                && let Some(name) = payload.first()
            {
                assert!(
                    name.starts_with("ctrl_c") || name == "step",
                    "controllable alphabet must contain only ctrl_c* or legacy step; got {name:?}"
                );
            }
        }
        // ctrl_c0 + ctrl_c1 must NOT appear in the uncontrollable alphabet.
        for &label_id in result.clts.uncontrollable_alphabet() {
            if let Some(payload) = result.clts.label_payload(label_id)
                && let Some(name) = payload.first()
            {
                assert!(
                    !name.starts_with("ctrl_c"),
                    "ctrl_c* label {name:?} must not appear in uncontrollable alphabet"
                );
            }
        }
    }

    /// R.6.6 — when controllability-aware, the post-pass Sharp /
    /// MustHyperOnly promotions are SKIPPED (the gate flags
    /// `!controllability_aware` before firing). Even with a must mode
    /// (`MustEdgeInference::SmtHyperMust`) requested, no promotions
    /// happen. R.6.6.b will lift this gate by adding the per-combo
    /// promotion shape.
    #[test]
    fn r6_6_controllability_aware_skips_post_pass_promotions() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let mut adapter_opts = AdapterOptions::default();
        adapter_opts.controllable_inputs.push("clr".into());
        let lift_opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: Default::default(),
            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &adapter_opts, &lift_opts)
            .expect("lift succeeds");
        assert_eq!(
            result.sharp_edges_promoted, 0,
            "R.6.6 gate: controllability-aware mode skips Sharp promotion (R.6.6.b follow-up); got {} promotions",
            result.sharp_edges_promoted
        );
        assert_eq!(
            result.hyper_must_edges_emitted, 0,
            "R.6.6 gate: controllability-aware mode skips MustHyperOnly emission; got {}",
            result.hyper_must_edges_emitted
        );
    }

    /// MIG-3.2 — `MayEdgeInference::SmtAllPairs` emits the SOUND
    /// may-edge set and excludes proven-impossible edges. Fixture: a
    /// 1-bit register that toggles (`next q = ~q`), predicate {q==1} →
    /// two cubes. The sound may-edges are exactly the toggle cycle
    /// (cube_0 ⇄ cube_1); the self-loops (cube_i → cube_i) are
    /// impossible (the toggle never stays) and MUST be excluded — the
    /// soundness property the SMT query gives that sampling cannot.
    #[test]
    fn mig3_smt_all_pairs_emits_sound_may_edges() {
        let src = "1 sort bitvec 1\n2 state 1 q\n3 not 1 2\n4 next 1 2 3\n";
        let preds = vec![PredicateSpec {
            name: "q1".into(),
            register: "q".into(),
            value: 1,
        }];
        let lift_opts = PredicateCubeLiftOptions {
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            ..Default::default()
        };
        let result = predicate_cube_lift(preds, src, &AdapterOptions::default(), &lift_opts)
            .expect("predicate_cube_lift (SmtAllPairs)");
        assert!(
            !result.predicate_image_pending,
            "SmtAllPairs populates the may relation"
        );
        let clts = &result.clts;
        let c0 = clts.state_id("cube_0").expect("cube_0 exists");
        let c1 = clts.state_id("cube_1").expect("cube_1 exists");
        let has_may = |from, to| {
            clts.outgoing(from).iter().any(|t| {
                t.target() == to && matches!(t.modality(), crate::clts::TransitionModality::MayOnly)
            })
        };
        // Toggle cycle — both directions are sound may-edges.
        assert!(has_may(c0, c1), "cube_0 → cube_1 (q: 0→1) is a may-edge");
        assert!(has_may(c1, c0), "cube_1 → cube_0 (q: 1→0) is a may-edge");
        // Impossible self-loops — excluded (UNSAT-proved), the soundness
        // property sampling cannot guarantee.
        assert!(
            !has_may(c0, c0),
            "cube_0 → cube_0 must be excluded (toggle never stays at q==0)"
        );
        assert!(
            !has_may(c1, c1),
            "cube_1 → cube_1 must be excluded (toggle never stays at q==1)"
        );
    }

    /// R.2.5b session-1 follow-up — Lazy materialiser threads a must
    /// mode (`MustEdgeInference::SmtHyperMust`) end-to-end. Same
    /// fixture as the eager path's test; produces at least one
    /// Sharp / MustHyperOnly edge + the `[R.2.5b-smt-must-hyper]`
    /// warning. Exercises the seam-routed must inference (AR-S2) on
    /// the lazy path.
    #[test]
    fn r5_2_5b_lazy_materialiser_honors_smt_hyper_must() {
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 8,
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            may_edge_inference: Default::default(),

            config_values: std::collections::HashMap::new(),
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let mut lazy =
            LazyLift::from_btor2(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
                .expect("LazyLift::from_btor2 succeeds");
        let result = materialize_clts_from_lazy(
            &mut lazy,
            crate::adapter::SourceFormat::Btor2,
            MustEdgeInference::SmtHyperMust,
        )
        .expect("materialize_clts_from_lazy succeeds");

        let total_promoted = result.sharp_edges_promoted + result.hyper_must_edges_emitted;
        assert!(
            total_promoted > 0,
            "lazy materialiser SmtHyperMust must emit at least one Sharp or MustHyperOnly edge; got sharp={}, hyper={}",
            result.sharp_edges_promoted,
            result.hyper_must_edges_emitted
        );
        let any_warning = result.warnings.iter().any(|w| {
            w.message.contains("R.2.5b-smt-must-hyper")
                && matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
        });
        assert!(
            any_warning,
            "lazy materialiser SmtHyperMust must emit the soundness warning"
        );
    }

    /// R-Y7 (2026-06-07) — Default `config_values` (empty) → the
    /// lifted Clts has a single initial state (cube_0), matching
    /// pre-R-Y7 behaviour exactly.
    #[test]
    fn r_y7_empty_config_values_preserves_legacy_single_initial_state() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("predicate_cube_lift succeeds");
        assert_eq!(
            result.clts.initial_states().len(),
            1,
            "pre-R-Y7 default: single initial state"
        );
    }

    /// R-Y7 — Non-empty `config_values` expands the initial-state
    /// set per the R-S8 encoder. For a register with multiple
    /// valid values, multiple cubes become admissible initial
    /// states.
    #[test]
    fn r_y7_config_values_expands_initial_state_set() {
        // 2 predicates over the counter: cnt == 0 and cnt == 1.
        // config_values: cnt is in {0, 1} → both predicate-true
        // cubes are admissible (cube 1 = pred_0 true; cube 2 =
        // pred_1 true). The over-approximation in R-S8 also
        // admits cube 0 (both predicates false; could be other
        // valid value) and cube 3 (both predicates true;
        // inconsistent but not filtered).
        let preds = vec![
            PredicateSpec {
                name: "cnt_is_0".into(),
                register: "cnt".into(),
                value: 0,
            },
            PredicateSpec {
                name: "cnt_is_1".into(),
                register: "cnt".into(),
                value: 1,
            },
        ];
        let mut config_values: std::collections::HashMap<String, Vec<u64>> =
            std::collections::HashMap::new();
        config_values.insert("cnt".to_string(), vec![0, 1]);
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 0,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: Default::default(),
            config_values,
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("predicate_cube_lift succeeds");
        let initial_count = result.clts.initial_states().len();
        assert!(
            initial_count > 1,
            "R-Y7: config_values must expand initial-state set beyond singleton; got {initial_count}"
        );
        assert_eq!(
            initial_count, 4,
            "R-Y7: with both predicate values valid + over-approximation, all 4 cubes admissible"
        );
    }

    /// R-Y7 — When `config_values` excludes the predicate's
    /// value, the cube where that predicate is TRUE is NOT
    /// admissible (per R-S8's load-bearing rule).
    #[test]
    fn r_y7_invalid_predicate_value_excludes_initial_cube() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_7".into(),
            register: "cnt".into(),
            value: 7,
        }];
        let mut config_values: std::collections::HashMap<String, Vec<u64>> =
            std::collections::HashMap::new();
        // Valid values are 0, 1, 2 — predicate's value 7 is NOT valid.
        config_values.insert("cnt".to_string(), vec![0, 1, 2]);
        let opts = PredicateCubeLiftOptions {
            max_cube_count: 1024,
            max_input_bits: 0,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: Default::default(),
            config_values,
            compound_exprs: std::collections::HashMap::new(),
            derived_predicates: Vec::new(),
            may_postimage: false,
        };
        let result = predicate_cube_lift(preds, COUNTER_BTOR2, &AdapterOptions::default(), &opts)
            .expect("predicate_cube_lift succeeds");
        let initial_count = result.clts.initial_states().len();
        // Cube 0 (predicate false): admissible (cnt holds 0, 1,
        // or 2). Cube 1 (predicate true → cnt == 7): NOT
        // admissible. → 1 initial state.
        assert_eq!(
            initial_count, 1,
            "R-Y7: predicate-value-not-valid cube excluded from initial set"
        );
    }

    /// R.2.5b session-1 follow-up — Lazy materialiser with
    /// `MustEdgeInference::Off` (the default) produces zero
    /// promotions, matching the pre-R.2.5b behaviour.
    #[test]
    fn r5_2_5b_lazy_materialiser_off_preserves_legacy_behaviour() {
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let mut lazy = LazyLift::from_btor2(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("LazyLift::from_btor2 succeeds");
        let result = materialize_clts_from_lazy(
            &mut lazy,
            crate::adapter::SourceFormat::Btor2,
            MustEdgeInference::Off,
        )
        .expect("materialize_clts_from_lazy succeeds");
        assert_eq!(
            result.sharp_edges_promoted, 0,
            "Off must yield zero promotions"
        );
        assert_eq!(result.hyper_must_edges_emitted, 0);
    }

    // ---- R.5b consumer wiring — predicate_cube_lift honors uf_wrap ----

    /// BTOR2 fixture for R.5b consumer wiring: 2-bit `cnt` register
    /// driven by `add` aliased via a uext line carrying the symbol
    /// `wide_add`. With `uf_wrap = ["wide_add"]` in the sidecar,
    /// predicate_cube_lift's simulate_one_step pass should substitute
    /// the add's result with zero — `cnt` stays at 0 across cube
    /// transitions instead of advancing.
    const R5B_CONSUMER_FIXTURE: &str = r#"
1 sort bitvec 2
2 zero 1
3 const 1 11
4 state 1 cnt
5 init 1 4 2
6 add 1 4 3
7 uext 1 6 0 wide_add
8 next 1 4 7
"#;

    #[test]
    fn r5b_predicate_cube_lift_consumes_uf_wrap_sidecar() {
        // Predicate: cnt == 1. Two cubes (cnt=1 true; cnt=1 false).
        let preds = vec![PredicateSpec {
            name: "cnt_is_1".into(),
            register: "cnt".into(),
            value: 1,
        }];

        // WITHOUT UF wrap: `add` evaluates normally. From cnt=0 (cube 0)
        // the next-value is add(0, 3) = 3 → cnt=3 (mask to 2 bits = 3).
        // 3 != 1 → target cube is cube 0 (cnt_is_1 false). From cnt=1
        // (cube 1) next is add(1, 3) = 4 mask = 0 → target cube 0.
        // Either way: every cube transitions to cube 0 → at least one
        // edge goes to cube 0.
        let no_uf_opts = AdapterOptions::default();
        let no_uf_result = predicate_cube_lift(
            preds.clone(),
            R5B_CONSUMER_FIXTURE,
            &no_uf_opts,
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");
        assert!(
            !no_uf_result.predicate_image_pending,
            "predicate-image MVP populates may-edges under default opts"
        );

        // WITH UF wrap on wide_add: `add` is substituted to zero, so
        // next cnt = 0 from every starting cube. Both cubes transition
        // to cube 0 (cnt=0, cnt_is_1 false).
        let uf_sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["wide_add"]
        });
        let uf_opts = AdapterOptions {
            sidecar_json: Some(uf_sidecar.to_string()),
            ..Default::default()
        };
        let uf_result = predicate_cube_lift(
            preds,
            R5B_CONSUMER_FIXTURE,
            &uf_opts,
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");
        assert!(
            !uf_result.predicate_image_pending,
            "predicate-image MVP populates may-edges under UF wrapping too"
        );

        // Both runs produce non-zero transitions; the consumer wiring
        // is exercised in the UF case via `simulate_one_step_with_uf_rep`.
        // The observable invariant (post-R.5b multi-value enumeration):
        // under UF wrap, each (cube, input combo) emits TWO may-edges
        // — one per UfRepresentative variant ({Zero, Ones}) — so the
        // UF run produces ~2× the edge count of the no-UF run. The
        // exact 2× factor depends on whether Zero and Ones land in
        // the SAME target cube (deduped by the builder) or DIFFERENT
        // cubes (both edges survive). For this fixture (2-bit add
        // with target cube always being cube 0), both representatives
        // land in cube 0 → edges deduplicate.
        let uf_edges: usize = uf_result
            .clts
            .states()
            .map(|s| uf_result.clts.outgoing(s).len())
            .sum();
        assert!(
            uf_edges > 0,
            "predicate_cube_lift under UF wrap must still produce may-edges"
        );
        let no_uf_edges: usize = no_uf_result
            .clts
            .states()
            .map(|s| no_uf_result.clts.outgoing(s).len())
            .sum();
        assert!(
            uf_edges >= no_uf_edges,
            "multi-value UF enumeration must produce at least as many edges as no-UF; \
             got uf={uf_edges}, no_uf={no_uf_edges}"
        );
        assert!(
            uf_edges <= no_uf_edges * 2,
            "multi-value UF enumeration capped at 2× no-UF edges (Zero + Ones representatives); \
             got uf={uf_edges}, no_uf={no_uf_edges}"
        );
    }

    #[test]
    fn r5b_predicate_cube_lift_surfaces_uf_wrap_warning_to_caller() {
        // R.5b AdapterWarning channel test — when UF wrapping fires,
        // the result.warnings vec must carry an entry naming the
        // wrapping. Mirror of the tracing::warn! surface but
        // structured for caller consumption (e.g. cegar.rs folding
        // into CegarTrace).
        let preds = vec![PredicateSpec {
            name: "cnt_is_1".into(),
            register: "cnt".into(),
            value: 1,
        }];
        let uf_sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["wide_add"]
        });
        let uf_opts = AdapterOptions {
            sidecar_json: Some(uf_sidecar.to_string()),
            ..Default::default()
        };
        let result = predicate_cube_lift(
            preds,
            R5B_CONSUMER_FIXTURE,
            &uf_opts,
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");
        assert!(
            !result.warnings.is_empty(),
            "UF wrapping must surface as at least one AdapterWarning on the result"
        );
        let has_uf_warning = result.warnings.iter().any(|w| {
            matches!(w.kind, crate::adapter::WarningKind::ApproximateTranslation)
                && w.message.contains("UF-wrapped")
        });
        assert!(
            has_uf_warning,
            "warnings must include an UF-wrapping naming entry; got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn r5b_predicate_cube_lift_no_warning_when_no_wrapping() {
        // Fixture with NO Op::Mul + no wide add → no UF wrapping →
        // no warning surfaces.
        let preds = vec![PredicateSpec {
            name: "cnt_is_0".into(),
            register: "cnt".into(),
            value: 0,
        }];
        let result = predicate_cube_lift(
            preds,
            COUNTER_BTOR2,
            &AdapterOptions::default(),
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");
        assert!(
            result.warnings.is_empty(),
            "warnings must be empty when no UF wrapping fires; got: {:?}",
            result.warnings
        );
    }

    /// Multi-value UF enumeration produces distinct target cubes when
    /// Zero and Ones representatives diverge. Fixture is a 1-bit
    /// register `bit_q` whose next-value = `wide_op` (an add aliased
    /// via uext). With UF wrap, Zero → bit_q=0; Ones → bit_q=1. So
    /// the cube cnt_is_1=true and cnt_is_1=false BOTH show as
    /// targets — 2 distinct may-edges per source cube.
    const R5B_MULTIVALUE_FIXTURE: &str = r#"
1 sort bitvec 1
2 zero 1
3 const 1 1
4 state 1 bit_q
5 init 1 4 2
6 add 1 4 3
7 uext 1 6 0 wide_op
8 next 1 4 7
"#;

    #[test]
    fn r5b_multi_value_uf_enumeration_emits_distinct_target_cubes() {
        let preds = vec![PredicateSpec {
            name: "bit_is_1".into(),
            register: "bit_q".into(),
            value: 1,
        }];
        let uf_sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["wide_op"]
        });
        let uf_opts = AdapterOptions {
            sidecar_json: Some(uf_sidecar.to_string()),
            ..Default::default()
        };
        let result = predicate_cube_lift(
            preds,
            R5B_MULTIVALUE_FIXTURE,
            &uf_opts,
            &PredicateCubeLiftOptions::default(),
        )
        .expect("ok");

        // 2 cubes total. Each cube under UF wrap produces 2 may-edges
        // per input combo (Zero → cube_0, Ones → cube_1). The edge
        // count should reflect the multi-value enumeration. Sanity
        // check: per-cube outgoing-edge count is >= 2 (one per
        // representative when reps diverge).
        for state in result.clts.states() {
            let trans = result.clts.outgoing(state);
            // Count distinct (label, target) pairs — the builder
            // dedups identical edges. Under multi-value with diverging
            // reps, we expect both cube_0 and cube_1 as targets.
            let distinct_targets: std::collections::HashSet<_> =
                trans.iter().map(|t| t.target()).collect();
            assert!(
                distinct_targets.len() >= 2,
                "multi-value UF enumeration must reach distinct target cubes from state {:?}; \
                 got distinct targets: {:?}",
                state,
                distinct_targets.len()
            );
        }
    }

    #[test]
    fn r5_lazy_kmts_null_lift_cube_count_matches_two_to_p() {
        let preds = vec![
            PredicateSpec {
                name: "p0".to_string(),
                register: "reg_a".to_string(),
                value: 0,
            },
            PredicateSpec {
                name: "p1".to_string(),
                register: "reg_a".to_string(),
                value: 1,
            },
            PredicateSpec {
                name: "p2".to_string(),
                register: "reg_b".to_string(),
                value: 0,
            },
        ];
        let null_lift = NullLazyLift::new(preds.clone());
        assert_eq!(
            null_lift.cube_count(),
            1usize << preds.len(),
            "NullLazyLift::cube_count must equal 2^|P|"
        );
    }

    #[test]
    fn r5_lazy_kmts_null_lift_expand_cube_returns_empty() {
        let preds = vec![PredicateSpec {
            name: "p0".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let mut null_lift = NullLazyLift::new(preds);
        for cube in 0..null_lift.cube_count() {
            let edges = null_lift.expand_cube(cube);
            assert!(
                edges.is_empty(),
                "NullLazyLift must return no may-edges for cube {cube}; got {edges:?}"
            );
        }
    }

    #[test]
    fn r5_lazy_kmts_null_lift_expand_cube_out_of_range_returns_empty() {
        let preds = vec![PredicateSpec {
            name: "p0".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let mut null_lift = NullLazyLift::new(preds);
        let out_of_range = null_lift.cube_count() + 5;
        let edges = null_lift.expand_cube(out_of_range);
        assert!(
            edges.is_empty(),
            "expand_cube on out-of-range index must return empty (no panic); got {edges:?}"
        );
    }

    #[test]
    fn r5_lazy_kmts_null_lift_predicates_round_trip() {
        let preds = vec![
            PredicateSpec {
                name: "p_alpha".to_string(),
                register: "reg_a".to_string(),
                value: 42,
            },
            PredicateSpec {
                name: "p_beta".to_string(),
                register: "reg_b".to_string(),
                value: 7,
            },
        ];
        let null_lift = NullLazyLift::new(preds.clone());
        assert_eq!(
            null_lift.predicates(),
            preds.as_slice(),
            "NullLazyLift::predicates must round-trip the constructor input"
        );
    }

    #[test]
    fn r5_lazy_kmts_lazy_expansion_edge_equality() {
        let a = LazyExpansionEdge {
            label: "step".to_string(),
            target_cube: 3,
        };
        let b = LazyExpansionEdge {
            label: "step".to_string(),
            target_cube: 3,
        };
        let c = LazyExpansionEdge {
            label: "step".to_string(),
            target_cube: 4,
        };
        assert_eq!(a, b, "edges with identical fields must be equal");
        assert_ne!(a, c, "edges with different target_cube must differ");
    }

    // R.5 lazy KMTS sub-item 2.2 tests (2026-06-04) — eager
    // wrapper backed by predicate_cube_lift. The wrapper must
    // be a strict drop-in for the lazy trait: cube_count +
    // expand_cube produce results identical to walking the
    // underlying Clts directly.

    #[test]
    fn r5_subitem_22_eager_wrapper_cube_count_matches_lift() {
        // 1 predicate ⇒ 2 cubes. The wrapper's cube_count MUST
        // equal the lift's cube_count.
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let wrapper =
            EagerLazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
                .expect("lift succeeds");
        assert_eq!(wrapper.cube_count(), 2);
        // The trait-level cube_count MUST equal the
        // result-level cube_count.
        assert_eq!(wrapper.cube_count(), wrapper.result().cube_count);
    }

    #[test]
    fn r5_subitem_22_eager_wrapper_expand_cube_matches_clts_outgoing() {
        // For every cube index in range, the wrapper's
        // expand_cube() output MUST be derivable from the
        // underlying Clts's outgoing transitions (one
        // LazyExpansionEdge per transition).
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut wrapper =
            EagerLazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
                .expect("lift succeeds");
        use crate::clts::StateId;
        for cube in 0..wrapper.cube_count() {
            let edges = wrapper.expand_cube(cube);
            // Count outgoing transitions directly from the Clts
            // for this cube — must match the wrapper's edge
            // count.
            let src_id = StateId::<DefaultStateIdx>::from_index(cube).expect("valid cube");
            let direct_count = wrapper.result().clts.outgoing(src_id).len();
            assert_eq!(
                edges.len(),
                direct_count,
                "expand_cube({cube}) edge count must match Clts::outgoing count"
            );
        }
    }

    #[test]
    fn r5_subitem_22_eager_wrapper_expand_cube_out_of_range_returns_empty() {
        // Out-of-range cube_index ⇒ empty Vec, no panic. The
        // trait contract.
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut wrapper =
            EagerLazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
                .expect("lift succeeds");
        let oob = wrapper.cube_count() + 100;
        let edges = wrapper.expand_cube(oob);
        assert!(
            edges.is_empty(),
            "expand_cube({oob}) on out-of-range must return empty"
        );
    }

    #[test]
    fn r5_subitem_22_eager_wrapper_predicates_round_trip() {
        // The wrapper's predicates() MUST return the same set
        // the lift was given.
        let preds = vec![
            PredicateSpec {
                name: "p0".to_string(),
                register: "reg_a".to_string(),
                value: 0,
            },
            PredicateSpec {
                name: "p1".to_string(),
                register: "reg_b".to_string(),
                value: 0,
            },
        ];
        let opts = PredicateCubeLiftOptions::default();
        let wrapper = EagerLazyLift::from_btor2(
            preds.clone(),
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lift succeeds");
        assert_eq!(
            wrapper.predicates(),
            preds.as_slice(),
            "predicates() must round-trip the lift input"
        );
    }

    #[test]
    fn r5_subitem_22_eager_wrapper_from_result_constructor_works() {
        // The alternative `from_result` constructor must accept
        // an already-computed PredicateCubeLiftResult.
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let result = predicate_cube_lift(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
            .expect("lift succeeds");
        let cube_count = result.cube_count;
        let wrapper = EagerLazyLift::from_result(result);
        assert_eq!(wrapper.cube_count(), cube_count);
    }

    // R.5 lazy KMTS sub-item 2.3 tests (2026-06-04) — truly-
    // lazy implementation. Each test ensures the cache + on-
    // demand computation behaviour matches the eager wrapper.

    #[test]
    fn r5_subitem_23_lazy_lift_cube_count_matches_eager() {
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let lazy = LazyLift::from_btor2(
            preds.clone(),
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lazy lift succeeds");
        let eager =
            EagerLazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
                .expect("eager lift succeeds");
        assert_eq!(lazy.cube_count(), eager.cube_count());
    }

    #[test]
    fn r5_subitem_23_lazy_lift_cache_starts_empty_grows_on_visits() {
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut lazy = LazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
            .expect("lazy lift succeeds");
        assert_eq!(lazy.cached_count(), 0, "cache MUST start empty");
        let _ = lazy.expand_cube(0);
        assert_eq!(
            lazy.cached_count(),
            1,
            "cache grows by 1 on first cube visit"
        );
        let _ = lazy.expand_cube(0);
        assert_eq!(
            lazy.cached_count(),
            1,
            "cache MUST NOT grow on repeat visit to the same cube"
        );
        let _ = lazy.expand_cube(1);
        assert_eq!(
            lazy.cached_count(),
            2,
            "cache grows by 1 on second distinct cube visit"
        );
    }

    #[test]
    fn r5_subitem_23_lazy_lift_repeat_expand_returns_identical_edges() {
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut lazy = LazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
            .expect("lazy lift succeeds");
        let edges_1 = lazy.expand_cube(0);
        let edges_2 = lazy.expand_cube(0);
        assert_eq!(edges_1, edges_2);
    }

    #[test]
    fn r5_subitem_23_lazy_lift_out_of_range_returns_empty_and_no_cache_growth() {
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut lazy = LazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
            .expect("lazy lift succeeds");
        let edges = lazy.expand_cube(100);
        assert!(edges.is_empty());
        assert_eq!(
            lazy.cached_count(),
            0,
            "out-of-range visit MUST NOT pollute the cache"
        );
    }

    #[test]
    fn r5_subitem_23_lazy_lift_edges_match_eager_lift() {
        // LOAD-BEARING DRIFT-PROTECTION TEST. The lazy impl
        // currently duplicates the per-cube logic of the eager
        // lifter. This test asserts the two paths produce
        // equivalent edge sets for the same input — any
        // divergence between the duplicated logics surfaces
        // here.
        //
        // Fixture: SMALL_BTOR2 (no inputs ⇒ no may-edges ⇒
        // both paths produce empty edge sets for every cube).
        // This is a degenerate match; richer-fixture tests
        // will live alongside sub-item 2.4 when an input-
        // bearing fixture is wired in.
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let mut lazy = LazyLift::from_btor2(
            preds.clone(),
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lazy lift succeeds");
        let mut eager =
            EagerLazyLift::from_btor2(preds, SMALL_BTOR2, &AdapterOptions::default(), &opts)
                .expect("eager lift succeeds");
        for cube in 0..lazy.cube_count() {
            let lazy_edges = lazy.expand_cube(cube);
            let eager_edges = eager.expand_cube(cube);
            let mut lazy_sorted = lazy_edges;
            lazy_sorted.sort_by(|a, b| {
                a.label
                    .cmp(&b.label)
                    .then_with(|| a.target_cube.cmp(&b.target_cube))
            });
            let mut eager_sorted = eager_edges;
            eager_sorted.sort_by(|a, b| {
                a.label
                    .cmp(&b.label)
                    .then_with(|| a.target_cube.cmp(&b.target_cube))
            });
            assert_eq!(
                lazy_sorted, eager_sorted,
                "LazyLift and EagerLazyLift MUST agree on edge set for cube {cube}"
            );
        }
    }

    #[test]
    fn r5_subitem_23_lazy_lift_predicates_round_trip() {
        let preds = vec![PredicateSpec {
            name: "p_alpha".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let opts = PredicateCubeLiftOptions::default();
        let lazy = LazyLift::from_btor2(
            preds.clone(),
            SMALL_BTOR2,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lazy lift succeeds");
        assert_eq!(lazy.predicates(), preds.as_slice());
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 stage 3.c.2a — encode_design_for_lift theory pick
    // ─────────────────────────────────────────────────────────────

    /// A memory-bearing BTOR2 (one array-sorted state cell).
    const MEM_BTOR_FOR_LIFT: &str = r#"
1 sort bitvec 1
2 sort bitvec 5
3 sort bitvec 8
4 sort array 2 3
5 state 4 mem
6 input 2 a
7 input 3 v
8 write 4 5 6 7
9 next 4 5 8
10 read 3 8 6
11 zero 3
12 eq 1 10 11
13 bad 12
"#;

    #[test]
    fn encode_design_for_lift_uses_array_theory_for_memory_design() {
        // A design with an array state cell must encode (not error)
        // through encode_design_for_lift, because the helper selects
        // Theory::BvUfArray. Under the bare BvOnly `encode_design`
        // this same design errors with ArraySortUnsupportedInBvOnly.
        let file = crate::adapter::btor2::parser::parse(MEM_BTOR_FOR_LIFT).expect("parse");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_for_lift(&file);
            assert!(
                view.is_ok(),
                "encode_design_for_lift must select BvUfArray + encode a memory design"
            );
            let view = view.unwrap();
            assert_eq!(
                view.state_curr_arr.len(),
                1,
                "the array (memory) state cell must be encoded as a Z3 Array"
            );
        });
    }

    #[test]
    fn encode_design_for_lift_uses_bvonly_for_memory_free_design() {
        // A memory-free design must still encode via BvOnly (the
        // pre-stage-3.c.2a path; array maps empty).
        let file = crate::adapter::btor2::parser::parse(SMALL_BTOR2).expect("parse");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_for_lift(&file).expect("memory-free encodes");
            assert!(
                view.state_curr_arr.is_empty(),
                "a memory-free design must have no array cells"
            );
        });
    }

    // ---- must ⊆ may soundness fix (Tier 0) ------------------------------------

    // A 2-bit counter `s` (s' = s + 1) with two predicates on the SAME register:
    // p0 = (s == 0), p1 = (s == 1). Cube 3 (0b11) asserts `s == 0 ∧ s == 1`, which
    // is UNSATISFIABLE — no concrete state inhabits it. Every must-check fabricates
    // a vacuous `Must` out of such an empty cube (`src ∧ trans ∧ ¬tgt` is trivially
    // UNSAT when `src` is UNSAT), which broke `R_must ⊆ R_may` and produced the
    // false-`Violated` / `TritBdd` panic on `$past` models
    // (docs/design/past-shadow-soundness.md §6). The fix drops must-promotion out of
    // proven-empty cubes while keeping it for the feasible ones.
    const UNSAT_CUBE_COUNTER: &str = "\
1 sort bitvec 2
2 state 1 s
3 one 1
4 add 1 2 3
5 zero 1
6 init 1 2 5
7 next 1 2 4
";

    fn unsat_cube_preds() -> Vec<PredicateSpec> {
        vec![
            PredicateSpec {
                name: "p0".into(),
                register: "s".into(),
                value: 0,
            },
            PredicateSpec {
                name: "p1".into(),
                register: "s".into(),
                value: 1,
            },
        ]
    }

    fn must_edge_sources(r: &PredicateCubeLiftResult) -> std::collections::BTreeSet<usize> {
        use crate::clts::{StateId, TransitionModality};
        let mut set = std::collections::BTreeSet::new();
        for i in 0..r.cube_count {
            let sid = StateId::<DefaultStateIdx>::from_index(i).expect("state id");
            for t in r.clts.outgoing(sid) {
                if matches!(
                    t.modality(),
                    TransitionModality::Sharp | TransitionModality::MustHyperOnly(_)
                ) {
                    set.insert(i);
                }
            }
        }
        set
    }

    fn has_sharp_edge(r: &PredicateCubeLiftResult, from: usize, to: usize) -> bool {
        use crate::clts::{StateId, TransitionModality};
        let sid = StateId::<DefaultStateIdx>::from_index(from).expect("state id");
        r.clts
            .outgoing(sid)
            .iter()
            .any(|t| matches!(t.modality(), TransitionModality::Sharp) && t.target().index() == to)
    }

    #[test]
    fn must_subset_may_no_vacuous_must_from_unsat_cube_all_pairs() {
        // Fix A — SmtAllPairs may + the ∀∃ must restricted to the emitted may-edges.
        let opts = PredicateCubeLiftOptions {
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            must_edge_inference: MustEdgeInference::SmtPerTargetStandard,
            ..Default::default()
        };
        let r = predicate_cube_lift(
            unsat_cube_preds(),
            UNSAT_CUBE_COUNTER,
            &AdapterOptions::default(),
            &opts,
        )
        .expect("lift Ok (assert_must_subset_may passed)");
        assert_eq!(r.cube_count, 4);
        // The unsat cube 3 sources NO must-edge (it has no may-edge to restrict to).
        assert!(
            !must_edge_sources(&r).contains(&3),
            "unsat cube 3 must not source a must-edge"
        );
        // A feasible, deterministic cube keeps its must-edge (no over-restriction):
        // cube 1 (s==0) → cube 2 (s==1) is the single successor.
        assert!(
            has_sharp_edge(&r, 1, 2),
            "feasible cube 1 (s==0) must keep its Sharp edge to cube 2 (s==1)"
        );
    }

    #[test]
    fn must_subset_may_no_vacuous_must_from_unsat_cube_sampling() {
        // Fix B — the sampling may path (default) under each SMT must mode. The
        // canonical-representative sampler emits spurious may-edges out of the unsat
        // cube, so the must post-pass gates on a PROVEN-feasible source rather than
        // on the mere presence of a sampled edge.
        for must in [
            MustEdgeInference::SmtPerTarget,
            MustEdgeInference::SmtPerTargetStandard,
            MustEdgeInference::SmtHyperMust,
        ] {
            let opts = PredicateCubeLiftOptions {
                may_edge_inference: MayEdgeInference::Off, // sampling
                must_edge_inference: must,
                ..Default::default()
            };
            let r = predicate_cube_lift(
                unsat_cube_preds(),
                UNSAT_CUBE_COUNTER,
                &AdapterOptions::default(),
                &opts,
            )
            .unwrap_or_else(|e| panic!("lift Ok for {must:?}: {}", e.message));
            assert_eq!(r.cube_count, 4);
            assert!(
                !must_edge_sources(&r).contains(&3),
                "{must:?}: unsat cube 3 must not source a must-edge"
            );
            assert!(
                has_sharp_edge(&r, 1, 2),
                "{must:?}: feasible cube 1 (s==0) must keep its Sharp edge to cube 2 (s==1)"
            );
        }
    }

    #[test]
    fn assert_must_subset_may_rejects_hyper_target_outside_may() {
        use crate::clts::TransitionModality;
        // s0 --MustHyperOnly({s1, s2})--> primary s1 is s0's ONLY edge, so
        // may_succ(s0) = {s1}. s2 is reachable only via s1 (s1 --MayOnly--> s2), so
        // it is NOT a may-successor of s0 → the hyper-must breaks R_must ⊆ R_may.
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for i in 0..3 {
            b.state(format!("s{i}"));
        }
        b.initial("s0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..3)
            .map(|i| b.state_id_or_insert(format!("s{i}")).unwrap())
            .collect();
        b.transition_ids_with_modality(ids[1], &[step], ids[2], TransitionModality::MayOnly);
        let hyper: smallvec::SmallVec<[_; 4]> = smallvec::smallvec![ids[1], ids[2]];
        b.transition_ids_with_modality(
            ids[0],
            &[step],
            ids[1],
            TransitionModality::must_hyper(hyper),
        );
        let clts = b.build().expect("builds");
        let err =
            assert_must_subset_may(&clts).expect_err("must reject the uncovered hyper target");
        assert_eq!(
            err.kind,
            crate::adapter::AdapterErrorKind::IrConsistencyError
        );
        assert!(err.message.contains("R_must ⊆ R_may"));
    }

    #[test]
    fn assert_must_subset_may_accepts_covered_hyper() {
        use crate::clts::TransitionModality;
        // Same shape, but a MayOnly s0 → s2 makes both hyper targets may-successors
        // of s0 (the primary s1 via the hyper edge itself, s2 via the may-edge).
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for i in 0..3 {
            b.state(format!("s{i}"));
        }
        b.initial("s0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..3)
            .map(|i| b.state_id_or_insert(format!("s{i}")).unwrap())
            .collect();
        b.transition_ids_with_modality(ids[0], &[step], ids[2], TransitionModality::MayOnly);
        let hyper: smallvec::SmallVec<[_; 4]> = smallvec::smallvec![ids[1], ids[2]];
        b.transition_ids_with_modality(
            ids[0],
            &[step],
            ids[1],
            TransitionModality::must_hyper(hyper),
        );
        let clts = b.build().expect("builds");
        assert!(
            assert_must_subset_may(&clts).is_ok(),
            "a hyper-must whose targets are all may-successors is well-formed"
        );
    }
}
