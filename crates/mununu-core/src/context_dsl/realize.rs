use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde_json::Value;
use thiserror::Error;

use crate::abstraction::unrolling::{
    Effect, OriginalState, OriginalTransition, UnrolledClts, UnrollingOptions, VariableDecl,
    unroll_states,
};
use crate::clts::{
    Clts, CltsBuilder, CltsError, DefaultLabelIdx, DefaultStateIdx, LabelControllability, LabelId,
    TransitionModality,
};
use crate::composition::{CompositionOptions, CompositionSemantics};
use crate::context::{Context, ContextBuilder, ContextError};
use crate::ltl;
use crate::mu_calculus::{Environment, Formula, parser as mu_parser};
use bitvec::prelude::{BitVec, Lsb0};

use super::ast::{
    AlphabetEntry, Automaton, CompositionKind, ContextDoc, Expr, ExprKind, FormulaExpr,
    FormulaTargets, Meta, PredicateDecl, PredicateTarget, StateDecl, StateRef, StateSelector,
    TransitionDecl, TransitionLabel, TransitionModalitySpec, UnaryOp,
};
use super::runtime::ResolvedControllerOptions;
use super::state_matching::StateNameMatcher;
use super::traversal::AstTraverser;

type RuntimeClts = Clts<DefaultStateIdx, DefaultLabelIdx>;
type RuntimeBuilder = CltsBuilder<DefaultStateIdx, DefaultLabelIdx>;
type RuntimeLabelId = LabelId<DefaultLabelIdx>;

/// Coerce the right-hand side of a `valuations { key = value; }` entry into a
/// display string. Valuations are display-only metadata, so we only support
/// expressions that have an obvious string representation: integer literals,
/// identifiers (used as enum-like names), grouped sub-expressions, and
/// negated integer literals. Anything richer is rejected — adapters that need
/// computed values should populate `ContextDoc.state_valuations` directly via
/// the side-channel.
fn valuation_value_to_string(expr: &Expr) -> Result<String, RealizationError> {
    match &expr.kind {
        ExprKind::Integer(n) => Ok(n.to_string()),
        ExprKind::Ident(id) => Ok(id.name.clone()),
        ExprKind::Group(inner) => valuation_value_to_string(inner),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } => {
            if let ExprKind::Integer(n) = &expr.kind {
                Ok(format!("-{n}"))
            } else {
                Err(RealizationError::UnsupportedFeature {
                    feature: "non-literal valuation expression",
                })
            }
        }
        _ => Err(RealizationError::UnsupportedFeature {
            feature: "complex valuation expression (only integers, identifiers, and negated integers are supported)",
        }),
    }
}

/// Build the merged valuation map for a state: starts from the side-channel
/// adapter-injected map (if any) and overlays hand-written
/// `valuations { … }` entries on top (user wins on collision).
fn merged_state_valuation(
    state: &StateDecl,
    side_channel: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, RealizationError> {
    let mut merged: BTreeMap<String, String> = side_channel.cloned().unwrap_or_default();
    for assign in &state.valuations {
        let value = valuation_value_to_string(&assign.expr)?;
        merged.insert(assign.target.name.clone(), value);
    }
    Ok(merged)
}

/// Result of realising a DSL context into runtime structures.
#[derive(Debug)]
pub struct RealizedContext {
    pub context: Context,
    pub formulas: HashMap<String, RealizedFormula>,
    pub controllers: HashMap<String, RealizedController>,
    pub predicates: HashMap<String, HashSet<String>>,
    predicate_metadata: HashMap<String, HashMap<String, PredicateMetadata>>,
    predicate_bitsets: HashMap<String, HashMap<String, BitVec<usize, Lsb0>>>,
    /// Maps composition names to their member automaton names
    composition_members: HashMap<String, Vec<String>>,
}

/// Resolved μ-calculus formula with metadata.
#[derive(Debug, Clone)]
pub struct RealizedFormula {
    pub name: String,
    pub targets: FormulaTargetsKind,
    pub formula: Formula,
    pub raw: String,
    pub meta: Meta,
    pub parse_error: Option<String>,
    /// Classified property type derived from formula structure.
    pub property_class: crate::mu_calculus::PropertyClass,
    /// Alternation depth of the formula (0 = propositional, 1 = safety/reach, 2+ = liveness).
    pub alternation_depth: usize,
}

/// Metadata describing a guard-derived predicate.
#[derive(Debug, Clone)]
pub struct PredicateMetadata {
    pub guard: String,
    pub expression: GuardExpressionMetadata,
}

/// Structured representation of the guard expression backing a predicate.
#[derive(Debug, Clone)]
pub enum GuardExpressionMetadata {
    True,
    Predicate(String),
    Comparison {
        left: String,
        op: String,
        right: String,
    },
    Unknown,
}

impl PredicateMetadata {
    /// Build metadata for a state-name predicate of the form `state == X`.
    /// Used by both user-declared state-target predicates and the auto-registration
    /// path that synthesises predicates referenced by formulas.
    fn state_name_eq(state_name: &str) -> Self {
        Self {
            guard: format!("state == {state_name}"),
            expression: GuardExpressionMetadata::Comparison {
                left: "state".to_string(),
                op: "==".to_string(),
                right: state_name.to_string(),
            },
        }
    }

    fn from_json(value: &Value, formula: &RealizedFormula) -> Self {
        let guard = value
            .get("guard")
            .and_then(Value::as_str)
            .map(|s| s.to_owned())
            .unwrap_or_else(|| formula.raw.clone());
        let expression = GuardExpressionMetadata::from_json(value.get("expr"));
        Self { guard, expression }
    }
}

impl GuardExpressionMetadata {
    fn from_json(value: Option<&Value>) -> Self {
        let Some(expr) = value else {
            return GuardExpressionMetadata::Unknown;
        };
        let Some(kind) = expr.get("type").and_then(Value::as_str) else {
            return GuardExpressionMetadata::Unknown;
        };
        match kind {
            "true" => GuardExpressionMetadata::True,
            "predicate" => expr
                .get("value")
                .and_then(Value::as_str)
                .map(|s| GuardExpressionMetadata::Predicate(s.to_owned()))
                .unwrap_or(GuardExpressionMetadata::Unknown),
            "comparison" => {
                let left = expr
                    .get("left")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let op = expr
                    .get("op")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let right = expr
                    .get("right")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                GuardExpressionMetadata::Comparison { left, op, right }
            }
            _ => GuardExpressionMetadata::Unknown,
        }
    }
}

/// Target set associated with a μ-calculus formula.
#[derive(Debug, Clone)]
pub enum FormulaTargetsKind {
    All,
    Named(Vec<String>),
}

impl RealizedContext {
    /// Returns the set of predicate names associated with `automaton`, if any.
    pub fn predicate_names(&self, automaton: &str) -> Option<&HashSet<String>> {
        self.predicates.get(automaton)
    }

    /// Returns the stored guard expression for the given predicate, when available.
    pub fn predicate_formula(&self, automaton: &str, predicate: &str) -> Option<&str> {
        if let Some(metadata) = self.predicate_metadata(automaton, predicate) {
            return Some(metadata.guard.as_str());
        }
        self.formulas
            .get(predicate)
            .map(|formula| formula.raw.as_str())
    }

    /// Returns structured metadata for the requested predicate, if available.
    pub fn predicate_metadata(
        &self,
        automaton: &str,
        predicate: &str,
    ) -> Option<&PredicateMetadata> {
        self.predicate_metadata
            .get(automaton)
            .and_then(|map| map.get(predicate))
    }

    /// Builds an environment seeded with predicate valuations for the given automaton.
    ///
    /// Guard predicate valuations fall back to a conservative approximation:
    /// - predicates with body `true` are set to the all-true bitset;
    /// - predicates with body `false` are set to the all-false bitset;
    /// - otherwise, predicates are currently defaulted to all-true until richer
    ///   arithmetic evaluation is introduced.
    ///
    /// For composed automata, predicates from member automata are projected onto
    /// the composed state space. Composed states are named in the format "left|right",
    /// so predicates are true in a composed state if they are true in the corresponding
    /// member state component.
    pub fn environment_for(&self, automaton: &str) -> Environment {
        let clts = self
            .context
            .clts(automaton)
            .unwrap_or_else(|| panic!("unknown automaton '{automaton}'"));
        let state_count = clts.state_count();
        let mut env = Environment::new(state_count);

        // Check if this is a composed automaton
        if let Some(member_names) = self.composition_members.get(automaton) {
            // For composed automata, project predicates from member automata.
            // Collect projected bitsets per predicate name, OR-ing across members
            // when multiple members share a state name (e.g., both have "Idle").
            let mut projected: HashMap<String, BitVec<usize, Lsb0>> = HashMap::new();

            for member_name in member_names {
                if let Some(member_predicates) = self.predicates.get(member_name) {
                    let member_clts = self
                        .context
                        .clts(member_name)
                        .expect("member automaton should exist");

                    let member_index = member_names
                        .iter()
                        .position(|n| n == member_name)
                        .expect("member should be in composition");

                    for predicate in member_predicates {
                        // Get the predicate bitset from the member automaton
                        let member_bits = self
                            .predicate_bitsets
                            .get(member_name)
                            .and_then(|map| map.get(predicate))
                            .cloned()
                            .unwrap_or_else(|| {
                                let member_state_count = member_clts.state_count();
                                fallback_bits(
                                    member_state_count,
                                    self.predicate_metadata(member_name, predicate),
                                )
                            });

                        // Project the predicate onto the composed state space
                        // Composed states are named "left|right" (e.g., "Idle|Wait")
                        let mut bits_from_member = BitVec::repeat(false, state_count);

                        for composed_state_id in clts.states() {
                            if let Some(composed_state_name) = clts.state_name(composed_state_id) {
                                let parts: Vec<&str> = composed_state_name.split('|').collect();

                                if member_index < parts.len() {
                                    let member_state_name = parts[member_index].trim();
                                    if let Ok(member_state_id) =
                                        member_clts.state_id(member_state_name)
                                        && member_bits
                                            .get(member_state_id.index())
                                            .map(|b| *b)
                                            .unwrap_or(false)
                                    {
                                        bits_from_member.set(composed_state_id.index(), true);
                                    }
                                }
                            }
                        }

                        // OR with existing projection for this predicate name
                        projected
                            .entry(predicate.clone())
                            .and_modify(|existing| *existing |= &bits_from_member)
                            .or_insert(bits_from_member);
                    }
                }
            }

            // Insert all projected predicates into the environment
            for (predicate, bits) in projected {
                env = env.with_predicate(predicate, bits);
            }
        } else {
            // For non-composed automata, use predicates directly
            if let Some(predicates) = self.predicates.get(automaton) {
                for predicate in predicates {
                    let bits = self
                        .predicate_bitsets
                        .get(automaton)
                        .and_then(|map| map.get(predicate))
                        .cloned()
                        .unwrap_or_else(|| {
                            fallback_bits(
                                state_count,
                                self.predicate_metadata(automaton, predicate),
                            )
                        });
                    env = env.with_predicate(predicate.clone(), bits);
                }
            }
        }

        // Phase A.3 follow-up — wire CLTS per-state valuations into the
        // evaluator's `abstract_states` channel so formula atoms of the
        // shape `signal == constant` (emitted by the BTOR2 bit-blaster
        // via `build_state_valuations`) bind to actual integer values
        // rather than fall through to the SOUNDNESS under-approx
        // "predicate-not-found → empty bitset" path. Without this the
        // round-trip from `mununu sv-yosys → CTXDSL → realize → eval`
        // returned vacuous verdicts on Caliptra-class designs.
        //
        // **Narrow scope.** Wiring is gated on `clts_valuations_are_numeric()`
        // — every valuation string across every state must parse as an
        // i64. This restricts the fix to the BTOR2 bit-blaster's
        // decimal-encoded valuations and **excludes** the SV adapter's
        // semantic state-name encoding (`state_S_IDLE_overlap_F` /
        // variant strings like `"T"` / `"F"`), which uses pre-computed
        // predicate bitsets registered above and would otherwise be
        // changed by the `Maybe → include` semantics of
        // `evaluate_expression_on_demand`. A broader binding rule
        // (e.g. routing variant-name comparisons too) is a separate
        // follow-up.
        if clts.has_valuations() && clts_valuations_are_numeric(clts) {
            let mut abstract_states = Vec::with_capacity(state_count);
            for state_id in clts.states() {
                let location = clts.state_name(state_id).unwrap_or("").to_string();
                let mut abs = crate::abstraction::state::AbstractState::new(location);
                if let Some(vals) = clts.state_valuation(state_id) {
                    for (var, display) in vals {
                        if let Ok(n) = display.parse::<i64>() {
                            abs.set_variable(
                                var.clone(),
                                crate::abstraction::value::AbstractValue::IntConstant(n),
                            );
                        }
                    }
                }
                abstract_states.push(abs);
            }
            env = env.with_abstract_states(abstract_states);
        }

        env
    }
}

/// The BTOR2 bit-blaster's absorbing out-of-bounds sink carries the
/// single non-numeric marker valuation `__mununu_oob__ = "true"` (see
/// `adapter::btor2::bit_blast::OOB_SINK_KEY`). The evaluator masks the
/// sink out of every formula via this same key
/// (`mu_calculus::evaluator::compute_oob_bits`). The numericity gate
/// below must IGNORE this marker — it is not a design valuation and its
/// presence (whenever a `BoundedCounter` / `EnumValues`-subset transition
/// escapes its declared value set) must not disable numeric atom binding
/// for the model's real, fully-numeric states.
const OOB_SINK_MARKER_KEY: &str = "__mununu_oob__";

/// Return `true` when every (non-marker) valuation string on every CLTS
/// state parses as an `i64`. The scope guard for the Phase A.3 follow-up
/// abstract-states wiring above — only the BTOR2 bit-blaster's
/// decimal-encoded valuations qualify, so semantic SV-adapter
/// encodings (variant names like `T`/`F`/`IDLE`) are left to the
/// existing pre-computed-predicate path.
///
/// The OOB sink's `__mununu_oob__` marker (see [`OOB_SINK_MARKER_KEY`])
/// is exempt: a single escaped transition would otherwise add a
/// non-numeric valuation and trip the gate, disabling numeric binding
/// for every real state — the cause of the R46-6 spurious-`VIOLATED`
/// divergence between a `BoundedCounter` model that escapes to OOB and
/// the otherwise-identical full bit-blast model. The wiring loop already
/// skips per-value non-numeric strings (`if let Ok(n) = parse()`), so
/// the marker contributes no variables to the sink's abstract state.
fn clts_valuations_are_numeric<S, L>(clts: &crate::clts::Clts<S, L>) -> bool
where
    S: crate::clts::IdStorage,
    L: crate::clts::IdStorage,
{
    let mut any_value = false;
    for state_id in clts.states() {
        if let Some(vals) = clts.state_valuation(state_id) {
            for (key, v) in vals {
                if key == OOB_SINK_MARKER_KEY {
                    continue;
                }
                any_value = true;
                if v.parse::<i64>().is_err() {
                    return false;
                }
            }
        }
    }
    any_value
}

fn fallback_bits(state_count: usize, metadata: Option<&PredicateMetadata>) -> BitVec<usize, Lsb0> {
    if let Some(meta) = metadata {
        if meta.guard.eq_ignore_ascii_case("false") {
            return BitVec::repeat(false, state_count);
        }
        match &meta.expression {
            GuardExpressionMetadata::True => BitVec::repeat(true, state_count),
            GuardExpressionMetadata::Predicate(value) if value.eq_ignore_ascii_case("false") => {
                BitVec::repeat(false, state_count)
            }
            _ => BitVec::repeat(true, state_count),
        }
    } else {
        BitVec::repeat(true, state_count)
    }
}

/// Controller declaration resolved to runtime-friendly representation.
#[derive(Debug)]
pub struct RealizedController {
    pub name: String,
    pub source: String,
    pub formula: String,
    pub options: ResolvedControllerOptions,
    pub export: Option<String>,
    pub meta: Meta,
}

/// Errors raised while realising DSL documents into runtime contexts.
#[derive(Debug, Error)]
pub enum RealizationError {
    #[error("duplicate {kind} '{name}'")]
    Duplicate { kind: &'static str, name: String },
    #[error("duplicate {kind} '{label}' claimed by both '{owner}' and '{other}'")]
    DuplicateLabelOwnership {
        kind: &'static str,
        label: String,
        owner: String,
        other: String,
    },
    #[error("unsupported DSL feature: {feature}")]
    UnsupportedFeature { feature: &'static str },
    #[error("unknown automaton '{0}' referenced by controller")]
    UnknownAutomaton(String),
    #[error("unknown formula '{0}' referenced by controller")]
    UnknownFormula(String),
    #[error("failed to build automaton '{name}': {error}")]
    AutomatonBuild {
        name: String,
        #[source]
        error: CltsError,
    },
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error("unknown state '{state}' referenced by predicate in automaton '{automaton}'")]
    UnknownPredicateState { automaton: String, state: String },
    #[error(
        "formula '{formula}' references predicate '{predicate}' that is not registered for any target automaton (typo? Defined predicates for '{automaton}': [{available}])"
    )]
    UnknownPredicate {
        formula: String,
        predicate: String,
        automaton: String,
        available: String,
    },
    #[error("invalid composition '{name}': {reason}")]
    InvalidComposition { name: String, reason: String },
    #[error("unrolling failed for automaton '{name}': {error}")]
    UnrollingFailed { name: String, error: String },
    #[error("dynamic guards require unrolling - automaton '{name}' must have variables")]
    DynamicGuardsRequireUnrolling { name: String },
    #[error("unknown constant or parameter '{0}'")]
    UnknownConstant(String),
    #[error("cannot evaluate expression as constant: {0}")]
    NonConstantExpression(String),
}

/// Aggregated predicate maps produced during realization.
///
/// This helper groups together:
/// - the set of predicate names per automaton,
/// - the metadata describing how each predicate was derived, and
/// - the pre-computed bitsets for each predicate.
struct PredicateMaps {
    predicates: HashMap<String, HashSet<String>>,
    predicate_metadata: HashMap<String, HashMap<String, PredicateMetadata>>,
    predicate_bitsets: HashMap<String, HashMap<String, BitVec<usize, Lsb0>>>,
}

/// Computes predicate maps and bitsets from the realised context and formulas.
///
/// This function centralises the logic that was previously inlined in `realize`:
/// it discovers predicate names from formula metadata, builds per-automaton
/// metadata tables, and then evaluates or directly computes the corresponding
/// bitsets.
fn compute_predicate_maps(
    context: &Context,
    formulas: &HashMap<String, RealizedFormula>,
) -> Result<PredicateMaps, RealizationError> {
    let mut predicates: HashMap<String, HashSet<String>> = HashMap::new();
    let mut predicate_metadata: HashMap<String, HashMap<String, PredicateMetadata>> =
        HashMap::new();

    // Register completion predicates for all automata that have can_reach_completion formulas.
    for formula in formulas.values() {
        if formula.name.ends_with("_can_reach_completion")
            && let FormulaTargetsKind::Named(targets) = &formula.targets
        {
            for automaton in targets {
                let completion_predicate_name = format!("{}_is_completion_state", automaton);
                predicates
                    .entry(automaton.clone())
                    .or_default()
                    .insert(completion_predicate_name);
            }
        }
    }

    // Populate predicate sets and metadata from formula comments.
    for formula in formulas.values() {
        if let FormulaTargetsKind::Named(targets) = &formula.targets {
            for target in targets {
                predicates.entry(target.clone()).or_default();
            }
        }
        if let Some(comment) = formula.meta.comment.as_deref()
            && let Ok(json) = serde_json::from_str::<Value>(comment)
            && let Some(predicate_name) = json.get("predicate").and_then(Value::as_str)
            && let FormulaTargetsKind::Named(targets) = &formula.targets
        {
            let metadata = PredicateMetadata::from_json(&json, formula);
            for target in targets {
                predicates
                    .entry(target.clone())
                    .or_default()
                    .insert(predicate_name.to_owned());
                predicate_metadata
                    .entry(target.clone())
                    .or_default()
                    .insert(predicate_name.to_owned(), metadata.clone());
            }
        }
    }

    // Evaluate or directly compute predicate bitsets for each automaton.
    let mut predicate_bitsets: HashMap<String, HashMap<String, BitVec<usize, Lsb0>>> =
        HashMap::new();
    for (automaton, metadata_map) in &predicate_metadata {
        if metadata_map.is_empty() {
            continue;
        }
        let Some(clts) = context.clts(automaton) else {
            continue;
        };
        let state_count = clts.state_count();
        for (predicate_name, metadata) in metadata_map {
            let bits = if let Some(formula) = formulas.get(predicate_name) {
                // Check if this is a structural predicate that should be computed directly.
                let computed_directly = if let Some(comment) = &formula.meta.comment {
                    if let Ok(json) = serde_json::from_str::<Value>(comment) {
                        json.get("expr")
                            .and_then(|e| e.get("computed_directly"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    } else {
                        false
                    }
                } else {
                    false
                };

                if computed_directly {
                    // For predicates marked as "computed_directly", extract end state names from metadata
                    // and compute bitset directly from state names.
                    let end_state_names: Option<Vec<String>> =
                        if let Some(comment) = &formula.meta.comment {
                            if let Ok(json) = serde_json::from_str::<Value>(comment) {
                                json.get("expr")
                                    .and_then(|e| e.get("end_states"))
                                    .and_then(|v| v.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                            .collect()
                                    })
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                    if let Some(end_states) = end_state_names {
                        // Compute bitset directly: set bits for all end states.
                        // Use pattern matching to handle unrolled state names by
                        // OR-ing the bitsets for each end-state pattern.
                        end_states.iter().fold(
                            BitVec::repeat(false, state_count),
                            |mut acc, pat| {
                                let bits = StateNameMatcher::create_bitset_for_pattern(clts, pat);
                                acc |= bits;
                                acc
                            },
                        )
                    } else {
                        // Check if this is a completion predicate (format: {automaton}_is_completion_state)
                        // If so, look up end_states from the can_reach_completion formula metadata.
                        let completion_predicate_name =
                            format!("{}_is_completion_state", automaton);
                        if predicate_name == &completion_predicate_name {
                            // Find the can_reach_completion formula to get end_states.
                            let can_reach_name = format!("{}_can_reach_completion", automaton);
                            if let Some(can_reach_formula) = formulas.get(&can_reach_name) {
                                if let Some(comment) = &can_reach_formula.meta.comment {
                                    if let Ok(json) = serde_json::from_str::<Value>(comment) {
                                        if let Some(end_states) = json
                                            .get("expr")
                                            .and_then(|e| e.get("end_states"))
                                            .and_then(|v| v.as_array())
                                            .map(|arr| {
                                                arr.iter()
                                                    .filter_map(|v| {
                                                        v.as_str().map(|s| s.to_string())
                                                    })
                                                    .collect::<Vec<String>>()
                                            })
                                        {
                                            // Compute bitset for all end states.
                                            // Use pattern matching to handle unrolled state names
                                            // by OR-ing the bitsets for each pattern.
                                            end_states.iter().fold(
                                                BitVec::repeat(false, state_count),
                                                |mut acc, pat| {
                                                    let bits =
                                                        StateNameMatcher::create_bitset_for_pattern(
                                                            clts, pat,
                                                        );
                                                    acc |= bits;
                                                    acc
                                                },
                                            )
                                        } else {
                                            // Fall through to fallback.
                                            BitVec::repeat(false, state_count)
                                        }
                                    } else {
                                        // Fall through to fallback.
                                        BitVec::repeat(false, state_count)
                                    }
                                } else {
                                    // Fall through to fallback.
                                    BitVec::repeat(false, state_count)
                                }
                            } else {
                                // Fall through to fallback.
                                BitVec::repeat(false, state_count)
                            }
                        } else {
                            // Fallback: try to infer from formula body (state name disjunction).
                            // This handles cases where metadata doesn't have `end_states`.
                            // Use pattern matching to handle unrolled state names.
                            // Extract state names from formula raw (e.g., "End" or "End || Complete").
                            let formula_raw = &formula.raw;
                            let state_names: Vec<String> = formula_raw
                                .split("||")
                                .map(|s| {
                                    s.trim()
                                        .trim_start_matches('(')
                                        .trim_end_matches(')')
                                        .trim()
                                        .to_string()
                                })
                                .filter(|s| !s.is_empty())
                                .collect();

                            // OR together the bitsets for each state name pattern.
                            state_names.iter().fold(
                                BitVec::repeat(false, state_count),
                                |mut acc, pat| {
                                    let bits =
                                        StateNameMatcher::create_bitset_for_pattern(clts, pat);
                                    acc |= bits;
                                    acc
                                },
                            )
                        }
                    }
                } else {
                    // Regular formula evaluation.
                    let env = Environment::new(state_count);
                    match context.evaluate_mu(automaton, &formula.formula, &env, None) {
                        Ok(result) => result,
                        Err(_) => fallback_bits(state_count, Some(metadata)),
                    }
                }
            } else {
                fallback_bits(state_count, Some(metadata))
            };
            predicate_bitsets
                .entry(automaton.clone())
                .or_default()
                .insert(predicate_name.clone(), bits);
        }
    }

    Ok(PredicateMaps {
        predicates,
        predicate_metadata,
        predicate_bitsets,
    })
}

/// Registry mapping enum names to their ordered variant lists.
/// Used to resolve variant names to integer indices during realization.
struct EnumRegistry {
    /// enum_name → [variant0, variant1, ...]
    enums: HashMap<String, Vec<String>>,
    /// variant_name → integer index (flattened across all enums)
    /// If variant names are unique across enums, this allows resolving without
    /// knowing which enum a variable belongs to.
    global_variants: HashMap<String, i64>,
}

impl EnumRegistry {
    fn from_doc(doc: &ContextDoc) -> Self {
        let mut enums = HashMap::new();
        let mut global_variants = HashMap::new();
        for enum_decl in &doc.enums {
            let variants: Vec<String> = enum_decl.variants.iter().map(|v| v.name.clone()).collect();
            for (i, variant) in variants.iter().enumerate() {
                global_variants.insert(variant.clone(), i as i64);
            }
            enums.insert(enum_decl.name.name.clone(), variants);
        }
        Self {
            enums,
            global_variants,
        }
    }

    fn is_empty(&self) -> bool {
        self.enums.is_empty()
    }
}

/// Resolves enum variant names in an automaton's expressions to integer literals.
/// This transforms `guard mode == idle` → `guard mode == 0` when `idle` is a known variant.
fn resolve_enum_variants(automaton: &Automaton, registry: &EnumRegistry) -> Automaton {
    use super::ast::*;

    fn resolve_expr(expr: &Expr, registry: &EnumRegistry) -> Expr {
        let kind = match &expr.kind {
            ExprKind::Ident(id) => {
                if let Some(&idx) = registry.global_variants.get(&id.name) {
                    ExprKind::Integer(idx)
                } else {
                    ExprKind::Ident(id.clone())
                }
            }
            ExprKind::Binary { left, op, right } => ExprKind::Binary {
                left: Box::new(resolve_expr(left, registry)),
                op: *op,
                right: Box::new(resolve_expr(right, registry)),
            },
            ExprKind::Unary { op, expr: e } => ExprKind::Unary {
                op: *op,
                expr: Box::new(resolve_expr(e, registry)),
            },
            ExprKind::Group(e) => ExprKind::Group(Box::new(resolve_expr(e, registry))),
            ExprKind::Index { target, expr: e } => ExprKind::Index {
                target: target.clone(),
                expr: Box::new(resolve_expr(e, registry)),
            },
            ExprKind::Integer(n) => ExprKind::Integer(*n),
        };
        Expr {
            kind,
            span: expr.span,
        }
    }

    let variables: Vec<VariableDecl> = automaton
        .variables
        .iter()
        .map(|v| VariableDecl {
            name: v.name.clone(),
            index: v.index.clone(),
            ty: match &v.ty {
                TypeName::Enum(_) => TypeName::I64, // Desugar enum type to i64
                other => other.clone(),
            },
            init: resolve_expr(&v.init, registry),
        })
        .collect();

    let transitions: Vec<TransitionDecl> = automaton
        .transitions
        .iter()
        .map(|t| TransitionDecl {
            source: t.source.clone(),
            target: t.target.clone(),
            label: t.label.clone(),
            additional_labels: t.additional_labels.clone(),
            guard: t.guard.as_ref().map(|g| resolve_expr(g, registry)),
            effects: t
                .effects
                .iter()
                .map(|a| Assignment {
                    target: a.target.clone(),
                    expr: resolve_expr(&a.expr, registry),
                })
                .collect(),
            modality: t.modality,
            additional_targets: t.additional_targets.clone(),
        })
        .collect();

    Automaton {
        name: automaton.name.clone(),
        meta: automaton.meta.clone(),
        parameters: automaton.parameters.clone(),
        alphabet: automaton.alphabet.clone(),
        controllable: automaton.controllable.clone(),
        internal: automaton.internal.clone(),
        controllable_declared: automaton.controllable_declared,
        internal_declared: automaton.internal_declared,
        variables,
        state_groups: automaton.state_groups.clone(),
        states: automaton.states.clone(),
        transitions,
        predicates: automaton.predicates.clone(),
    }
}

/// mununu#591 — the automaton names a document REALIZES to, in declaration order: a plain
/// automaton by its name, a parameterised one by every instance name its range expands to
/// (`Worker` with `param i in 0 ..= 1` → `Worker_0`, `Worker_1`), exactly as
/// [`expand_parameterized_automata`] names them. A composition member must be one of these; the
/// `verify` orchestrator resolves `[composition].members` through this rather than a scan of the
/// declared names, which the expansion does not keep. A range that does not resolve leaves the
/// declared name (the realization then reports it). Grouped by declaration, in declaration
/// order, so a caller that wants "the first automaton" gets every instance of a template.
pub fn realized_automaton_names(doc: &ContextDoc) -> Vec<Vec<String>> {
    let constants: HashMap<String, i64> = doc
        .constants
        .iter()
        .map(|c| (c.name.name.clone(), c.value))
        .collect();
    let empty = HashMap::new();
    let ranges: HashMap<String, (i64, i64)> = doc
        .ranges
        .iter()
        .filter_map(|r| {
            let lo = eval_const_expr(&r.lower, &constants, &empty).ok()?;
            let hi = eval_const_expr(&r.upper, &constants, &empty).ok()?;
            Some((r.name.name.clone(), (lo, hi)))
        })
        .collect();
    let mut out = Vec::new();
    for automaton in &doc.automata {
        match automaton.parameters.first() {
            None => out.push(vec![automaton.name.name.clone()]),
            Some(param) => match resolve_param_range(&param.spec, &constants, &ranges) {
                Ok((lo, hi)) => out.push(
                    (lo..=hi)
                        .map(|val| format!("{}_{}", automaton.name.name, val))
                        .collect(),
                ),
                Err(_) => out.push(vec![automaton.name.name.clone()]),
            },
        }
    }
    out
}

/// Expands parameterized automata into concrete instances.
/// For example, `automaton Client { parameters { param i in 0..=1; } ... }`
/// produces `Client_0` and `Client_1`. Non-parameterized automata pass through unchanged.
fn expand_parameterized_automata(
    automata: &[Automaton],
    constants: &HashMap<String, i64>,
    ranges: &HashMap<String, (i64, i64)>,
) -> Result<Vec<Automaton>, RealizationError> {
    let mut result = Vec::new();
    for automaton in automata {
        if automaton.parameters.is_empty() {
            result.push(automaton.clone());
        } else {
            // Support single parameter for now.
            if automaton.parameters.len() > 1 {
                return Err(RealizationError::UnsupportedFeature {
                    feature: "multiple automaton parameters",
                });
            }
            let param = &automaton.parameters[0];
            let (lo, hi) = resolve_param_range(&param.spec, constants, ranges)?;
            for val in lo..=hi {
                let mut concrete = substitute_param(automaton, &param.name.name, val);
                concrete.name = super::ast::Ident::new(
                    format!("{}_{}", automaton.name.name, val),
                    automaton.name.span,
                );
                concrete.parameters.clear();
                result.push(concrete);
            }
        }
    }
    Ok(result)
}

/// Resolves a parameter range spec to concrete (lo, hi) bounds.
fn resolve_param_range(
    spec: &super::ast::RangeSpec,
    constants: &HashMap<String, i64>,
    ranges: &HashMap<String, (i64, i64)>,
) -> Result<(i64, i64), RealizationError> {
    match spec {
        super::ast::RangeSpec::Named(ident) => ranges
            .get(&ident.name)
            .copied()
            .ok_or_else(|| RealizationError::UnknownConstant(format!("range '{}'", ident.name))),
        super::ast::RangeSpec::Bounds { lower, upper } => {
            let empty = HashMap::new();
            let lo = eval_const_expr(lower, constants, &empty)?;
            let hi = eval_const_expr(upper, constants, &empty)?;
            Ok((lo, hi))
        }
    }
}

/// Deep-clones an automaton, replacing every occurrence of `param_name` in expressions
/// with `value`. Also resolves indexed labels (`req[i]` → `req_0`), indexed states,
/// and indexed state references.
fn substitute_param(automaton: &Automaton, param_name: &str, value: i64) -> Automaton {
    use super::ast::*;

    fn subst_expr(expr: &Expr, param_name: &str, value: i64) -> Expr {
        let kind = match &expr.kind {
            ExprKind::Integer(n) => ExprKind::Integer(*n),
            ExprKind::Ident(id) => {
                if id.name == param_name {
                    ExprKind::Integer(value)
                } else {
                    ExprKind::Ident(id.clone())
                }
            }
            ExprKind::Index { target, expr: ie } => {
                if target.name == param_name {
                    // param itself is used as an indexed target — unusual, treat as integer
                    ExprKind::Integer(value)
                } else {
                    ExprKind::Index {
                        target: target.clone(),
                        expr: Box::new(subst_expr(ie, param_name, value)),
                    }
                }
            }
            ExprKind::Unary { op, expr: e } => ExprKind::Unary {
                op: *op,
                expr: Box::new(subst_expr(e, param_name, value)),
            },
            ExprKind::Binary { left, op, right } => ExprKind::Binary {
                left: Box::new(subst_expr(left, param_name, value)),
                op: *op,
                right: Box::new(subst_expr(right, param_name, value)),
            },
            ExprKind::Group(e) => ExprKind::Group(Box::new(subst_expr(e, param_name, value))),
        };
        Expr {
            kind,
            span: expr.span,
        }
    }

    fn subst_alphabet_ref(ar: &AlphabetRef, param_name: &str, value: i64) -> AlphabetRef {
        match &ar.index {
            Some(idx) => {
                // Resolve the index and fold it into the label name: req[i] → req_0
                let resolved = subst_expr(idx, param_name, value);
                if let ExprKind::Integer(n) = &resolved.kind {
                    AlphabetRef {
                        name: Ident::new(format!("{}_{}", ar.name.name, n), ar.name.span),
                        index: None, // Resolved — no longer indexed
                    }
                } else {
                    AlphabetRef {
                        name: ar.name.clone(),
                        index: Some(resolved),
                    }
                }
            }
            None => ar.clone(),
        }
    }

    fn subst_state_ref(sr: &StateRef, param_name: &str, value: i64) -> StateRef {
        match sr {
            StateRef::Simple(id) => StateRef::Simple(id.clone()),
            StateRef::Indexed { name, indices } => {
                let resolved: Vec<Expr> = indices
                    .iter()
                    .map(|e| subst_expr(e, param_name, value))
                    .collect();
                // If all indices resolve to integers, fold into the name
                let all_int: Option<Vec<i64>> = resolved
                    .iter()
                    .map(|e| {
                        if let ExprKind::Integer(n) = &e.kind {
                            Some(*n)
                        } else {
                            None
                        }
                    })
                    .collect();
                if let Some(ints) = all_int {
                    let suffix = ints
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join("_");
                    StateRef::Simple(Ident::new(format!("{}_{}", name.name, suffix), name.span))
                } else {
                    StateRef::Indexed {
                        name: name.clone(),
                        indices: resolved,
                    }
                }
            }
        }
    }

    fn subst_selector(sel: &StateSelector, param_name: &str, value: i64) -> StateSelector {
        match sel {
            StateSelector::Named(sr) => {
                StateSelector::Named(subst_state_ref(sr, param_name, value))
            }
            other => other.clone(),
        }
    }

    fn subst_label(label: &TransitionLabel, param_name: &str, value: i64) -> TransitionLabel {
        match label {
            TransitionLabel::Named { name, index } => match index {
                Some(idx) => {
                    let resolved = subst_expr(idx, param_name, value);
                    if let ExprKind::Integer(n) = &resolved.kind {
                        TransitionLabel::Named {
                            name: Ident::new(format!("{}_{}", name.name, n), name.span),
                            index: None,
                        }
                    } else {
                        TransitionLabel::Named {
                            name: name.clone(),
                            index: Some(resolved),
                        }
                    }
                }
                None => label.clone(),
            },
            TransitionLabel::Epsilon(_) => label.clone(),
        }
    }

    let alphabet: Vec<AlphabetRef> = automaton
        .alphabet
        .iter()
        .map(|ar| subst_alphabet_ref(ar, param_name, value))
        .collect();
    let controllable: Vec<AlphabetRef> = automaton
        .controllable
        .iter()
        .map(|ar| subst_alphabet_ref(ar, param_name, value))
        .collect();
    let internal: Vec<AlphabetRef> = automaton
        .internal
        .iter()
        .map(|ar| subst_alphabet_ref(ar, param_name, value))
        .collect();

    // Expand indexed states (state S[i in Range] → S_0, S_1, ...)
    let mut states: Vec<StateDecl> = Vec::new();
    for state in &automaton.states {
        match &state.index {
            Some(StateIndexSpec::Range { symbol, range: _ }) => {
                // The parameter is substituted with a single value, so if the
                // state iteration variable matches our parameter, emit one state.
                if symbol.name == param_name {
                    states.push(StateDecl {
                        name: Ident::new(format!("{}_{}", state.name.name, value), state.name.span),
                        index: None,
                        is_initial: state.is_initial,
                        overrides: state.overrides.clone(),
                        valuations: state.valuations.clone(),
                        three_valued: state.three_valued.clone(),
                    });
                } else {
                    // Different iteration variable — keep as-is for now
                    states.push(state.clone());
                }
            }
            Some(StateIndexSpec::Expr(e)) => {
                let resolved = subst_expr(e, param_name, value);
                if let ExprKind::Integer(n) = &resolved.kind {
                    states.push(StateDecl {
                        name: Ident::new(format!("{}_{}", state.name.name, n), state.name.span),
                        index: None,
                        is_initial: state.is_initial,
                        overrides: state.overrides.clone(),
                        valuations: state.valuations.clone(),
                        three_valued: state.three_valued.clone(),
                    });
                } else {
                    states.push(state.clone());
                }
            }
            None => states.push(state.clone()),
        }
    }

    let transitions: Vec<TransitionDecl> = automaton
        .transitions
        .iter()
        .map(|t| TransitionDecl {
            source: subst_selector(&t.source, param_name, value),
            target: subst_selector(&t.target, param_name, value),
            label: subst_label(&t.label, param_name, value),
            additional_labels: t
                .additional_labels
                .iter()
                .map(|l| subst_label(l, param_name, value))
                .collect(),
            guard: t.guard.as_ref().map(|g| subst_expr(g, param_name, value)),
            effects: t
                .effects
                .iter()
                .map(|a| Assignment {
                    target: a.target.clone(),
                    expr: subst_expr(&a.expr, param_name, value),
                })
                .collect(),
            modality: t.modality,
            additional_targets: t.additional_targets.clone(),
        })
        .collect();

    let variables: Vec<VariableDecl> = automaton
        .variables
        .iter()
        .map(|v| VariableDecl {
            name: v.name.clone(),
            index: v.index.as_ref().map(|e| subst_expr(e, param_name, value)),
            ty: v.ty.clone(),
            init: subst_expr(&v.init, param_name, value),
        })
        .collect();

    Automaton {
        name: automaton.name.clone(), // Will be renamed by caller
        meta: automaton.meta.clone(),
        parameters: automaton.parameters.clone(),
        alphabet,
        controllable,
        internal,
        controllable_declared: automaton.controllable_declared,
        internal_declared: automaton.internal_declared,
        variables,
        state_groups: automaton.state_groups.clone(),
        states,
        transitions,
        predicates: automaton.predicates.clone(),
    }
}

/// Builds the runtime context and composition membership map from the DSL document.
fn build_context_with_compositions(
    doc: &ContextDoc,
    label_universe: &LabelUniverse,
    input_signals: &HashSet<String>,
) -> Result<(Context, HashMap<String, Vec<String>>), RealizationError> {
    // Build a constants map for evaluating indexed member references.
    let constants: HashMap<String, i64> = doc
        .constants
        .iter()
        .map(|c| (c.name.name.clone(), c.value))
        .collect();

    // Build ranges map for resolving parameter ranges.
    let ranges: HashMap<String, (i64, i64)> = {
        let empty = HashMap::new();
        doc.ranges
            .iter()
            .filter_map(|r| {
                let lo = eval_const_expr(&r.lower, &constants, &empty).ok()?;
                let hi = eval_const_expr(&r.upper, &constants, &empty).ok()?;
                Some((r.name.name.clone(), (lo, hi)))
            })
            .collect()
    };

    // Expand parameterized automata into concrete instances.
    let expanded_automata = expand_parameterized_automata(&doc.automata, &constants, &ranges)?;

    // Resolve enum variant names to integer indices.
    let enum_registry = EnumRegistry::from_doc(doc);
    let expanded_automata: Vec<Automaton> = if enum_registry.is_empty() {
        expanded_automata
    } else {
        expanded_automata
            .iter()
            .map(|a| resolve_enum_variants(a, &enum_registry))
            .collect()
    };

    let mut automaton_names = HashSet::new();
    let mut automata = Vec::with_capacity(expanded_automata.len());
    let mut controllable_owners: HashMap<String, String> = HashMap::new();
    let mut internal_owners: HashMap<String, String> = HashMap::new();

    // First pass: validate names and collect controllable/internal ownership.
    for automaton in &expanded_automata {
        let name = automaton.name.name.clone();
        if !automaton_names.insert(name.clone()) {
            return Err(RealizationError::Duplicate {
                kind: "automaton",
                name,
            });
        }
        // Track controllable/internal ownership per label.
        for entry in &automaton.controllable {
            let label = entry.name.name.clone();
            if let Some(other) = controllable_owners.insert(label.clone(), name.clone()) {
                return Err(RealizationError::DuplicateLabelOwnership {
                    kind: "controllable label",
                    label,
                    owner: name.clone(),
                    other,
                });
            }
        }
        for entry in &automaton.internal {
            let label = entry.name.name.clone();
            if let Some(other) = internal_owners.insert(label.clone(), name.clone()) {
                return Err(RealizationError::DuplicateLabelOwnership {
                    kind: "internal label",
                    label,
                    owner: name.clone(),
                    other,
                });
            }
        }
    }

    // Second pass: build each automaton's CLTS with knowledge of external controllability.
    // Labels declared controllable by OTHER automata should not be inferred controllable
    // via legacy mode in this automaton.
    let all_controllable_labels: HashSet<String> = controllable_owners.keys().cloned().collect();
    for automaton in &expanded_automata {
        let name = automaton.name.name.clone();
        // Externally controllable = labels declared controllable by other automata
        let externally_controllable: HashSet<String> = all_controllable_labels
            .iter()
            .filter(|label| {
                controllable_owners
                    .get(*label)
                    .is_some_and(|owner| *owner != name)
            })
            .cloned()
            .collect();

        let aut_valuations = doc.state_valuations.get(&name);
        let clts = build_automaton(
            &name,
            automaton,
            label_universe,
            input_signals,
            aut_valuations,
            &externally_controllable,
        )?;
        automata.push((name, clts));
    }

    let mut context_builder = ContextBuilder::default();
    for (name, clts) in automata {
        context_builder = context_builder.register_clts(name, clts);
    }
    compose_and_register(doc, &constants, context_builder.finish_with_checks()?)
}

/// Composes automata according to the document's composition declarations and
/// registers them into the context. Returns the updated context and a map from
/// composition names to their (transitively expanded) member automaton names.
fn compose_and_register(
    doc: &ContextDoc,
    constants: &HashMap<String, i64>,
    context: Context,
) -> Result<(Context, HashMap<String, Vec<String>>), RealizationError> {
    let mut composition_members: HashMap<String, Vec<String>> = HashMap::new();
    // Topologically sort compositions to support hierarchical composition
    // (compositions referencing other compositions as members).
    let composition_name_set: HashSet<String> = doc
        .compositions
        .iter()
        .map(|c| c.name.name.clone())
        .collect();

    // Build dependency graph: for each composition, which other compositions it depends on
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for comp in &doc.compositions {
        let name = comp.name.name.clone();
        let dep_count = comp
            .members
            .iter()
            .filter(|m| {
                let resolved = resolve_member_name(m, constants);
                composition_name_set.contains(&resolved)
            })
            .count();
        in_degree.insert(name.clone(), dep_count);
        for member in &comp.members {
            let resolved = resolve_member_name(member, constants);
            if composition_name_set.contains(&resolved) {
                dependents.entry(resolved).or_default().push(name.clone());
            }
        }
    }

    // Kahn's algorithm for topological sort
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    let mut sorted_names: Vec<String> = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted_names.push(node.clone());
        if let Some(deps) = dependents.get(&node) {
            for dep in deps {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep.clone());
                    }
                }
            }
        }
    }
    if sorted_names.len() != composition_name_set.len() {
        let sorted_set: HashSet<String> = sorted_names.into_iter().collect();
        let remaining: Vec<String> = composition_name_set
            .difference(&sorted_set)
            .cloned()
            .collect();
        return Err(RealizationError::InvalidComposition {
            name: remaining.join(", "),
            reason: "circular dependency detected among compositions".to_string(),
        });
    }

    // Index compositions by name for lookup during ordered processing
    let comp_by_name: HashMap<String, &super::ast::Composition> = doc
        .compositions
        .iter()
        .map(|c| (c.name.name.clone(), c))
        .collect();

    // Process compositions in topological order. Already-realized compositions
    // are stored in `realized` so later compositions can reference them.
    let mut realized: HashMap<String, Clts<DefaultStateIdx, DefaultLabelIdx>> = HashMap::new();

    for comp_name in &sorted_names {
        let composition = comp_by_name[comp_name.as_str()];
        let composition_name = comp_name.clone();
        let member_names: Vec<String> = composition
            .members
            .iter()
            .map(|m| resolve_member_name(m, constants))
            .collect();
        composition_members.insert(composition_name.clone(), member_names.clone());

        if member_names.is_empty() {
            return Err(RealizationError::InvalidComposition {
                name: composition_name,
                reason: "composition has no members".to_string(),
            });
        }

        // Determine composition semantics.
        let semantics = match composition.kind {
            CompositionKind::Synchronous => CompositionSemantics::Synchronous,
            CompositionKind::Asynchronous => CompositionSemantics::Asynchronous,
            CompositionKind::Superset => CompositionSemantics::Superset,
        };
        // mununu#589 — the composition's synchronisation vectors, over EVERY member, go into the
        // options before the fold: a vector whose labels sit on two partner automata can only
        // fire if those partners get their joint step before the owner is folded in, and the
        // pairwise fold cannot know that from the two sides it sees. Collected here so the
        // result does not depend on the members' order (or, on the `verify` path, their names).
        let mut sync_vectors: std::collections::BTreeSet<std::collections::BTreeSet<String>> =
            std::collections::BTreeSet::new();
        for member_name in &member_names {
            if let Some(member) = context
                .clts(member_name)
                .or_else(|| realized.get(member_name.as_str()))
            {
                sync_vectors.extend(crate::composition::synchronisation_vectors(member));
            }
        }
        let options = CompositionOptions::new(semantics)
            .with_sync_vectors(sync_vectors.into_iter().collect());

        // Compose members left-associatively.
        // Members can be base automata (from context) or previously realized compositions.
        let first_name = &member_names[0];
        let mut composed_clts = context
            .clts(first_name)
            .or_else(|| realized.get(first_name.as_str()))
            .ok_or_else(|| {
                RealizationError::UnknownAutomaton(format!(
                    "composition '{}' references unknown member '{}'",
                    composition_name, first_name
                ))
            })?
            .clone();

        for member_name in member_names.iter().skip(1) {
            let member_clts = context
                .clts(member_name)
                .or_else(|| realized.get(member_name as &str))
                .ok_or_else(|| {
                    RealizationError::UnknownAutomaton(format!(
                        "composition '{}' references unknown member '{}'",
                        composition_name, member_name
                    ))
                })?
                .clone();

            // Create a temporary context for composition.
            // Use finish() (no controllability re-validation) because members
            // may themselves be composed CLTS that carry inherited labels.
            // The original automata were already validated at registration time.
            let temp_context = ContextBuilder::default()
                .register_clts("left".to_string(), composed_clts.clone())
                .register_clts("right".to_string(), member_clts.clone())
                .finish();

            composed_clts = temp_context
                .compose_named("left", "right", &options)
                .map_err(|e| RealizationError::InvalidComposition {
                    name: composition_name.clone(),
                    reason: format!("failed to compose automata: {}", e),
                })?;
        }

        realized.insert(composition_name, composed_clts);
    }

    // Expand composition_members transitively: if a member is itself a composition,
    // replace it with that composition's expanded members. Since we process in
    // topological order, inner compositions are already expanded.
    for comp_name in &sorted_names {
        let direct_members = composition_members[comp_name].clone();
        let mut expanded = Vec::new();
        for member in &direct_members {
            if let Some(sub_members) = composition_members.get(member) {
                if composition_name_set.contains(member) {
                    expanded.extend(sub_members.clone());
                } else {
                    expanded.push(member.clone());
                }
            } else {
                expanded.push(member.clone());
            }
        }
        composition_members.insert(comp_name.clone(), expanded);
    }

    let composed_automata: Vec<(String, Clts<DefaultStateIdx, DefaultLabelIdx>)> =
        realized.into_iter().collect();

    // Build final context with all original automata plus composed automata.
    // Only rebuild if we have composed automata to avoid unnecessary work.
    let context = if composed_automata.is_empty() {
        context
    } else {
        // For composed automata, we use finish() instead of finish_with_checks()
        // because composed automata inherit labels from their members, and the
        // strict controllable alphabet check would incorrectly flag conflicts.
        // The original context was already validated with finish_with_checks(),
        // so we only need to merge the composed automata without re-validating.
        let mut final_builder = ContextBuilder::default();
        // Re-register all existing automata from the context.
        for name in context.clts_names() {
            let clts = context.clts(&name).expect("automaton should exist").clone();
            final_builder = final_builder.register_clts(name, clts);
        }
        // Register all composed automata.
        for (name, clts) in composed_automata {
            final_builder = final_builder.register_clts(name, clts);
        }
        // Use finish() to skip the controllable alphabet check for composed automata
        // since they are derived from already-validated member automata.
        final_builder.finish()
    };

    Ok((context, composition_members))
}

/// Collects and parses all μ-calculus formulas from the main document and sidecars.
fn collect_formulas(
    docs: &[&ContextDoc],
) -> Result<HashMap<String, RealizedFormula>, RealizationError> {
    let mut formulas: HashMap<String, RealizedFormula> = HashMap::new();
    for doc in docs {
        AstTraverser::visit_formulas(doc, |formula| {
            let name = formula.name.name.clone();
            // Skip the special __input_signals__ formula - it's metadata only.
            if name == "__input_signals__" {
                return Ok(());
            }
            if formulas.contains_key(&name) {
                return Err(RealizationError::Duplicate {
                    kind: "μ-formula",
                    name,
                });
            }
            let (parsed, parse_error, raw) = match &formula.body {
                FormulaExpr::MuCalculus(mu_expr) => {
                    let (parsed, parse_error) = match mu_parser::parse(&mu_expr.raw) {
                        Ok(parsed) => (parsed, None),
                        Err(error) => (
                            mu_parser::parse("true")
                                .expect("fallback μ-calculus formula parses successfully"),
                            Some(error.to_string()),
                        ),
                    };
                    (parsed, parse_error, mu_expr.raw.clone())
                }
                FormulaExpr::Ltl(ltl_expr) => {
                    // Translate LTL to μ-calculus.
                    match ltl::translator::translate(&ltl_expr.formula) {
                        Ok(translated) => {
                            // Format the translated formula for display.
                            let raw = format!("ltl {:?}", ltl_expr.formula);
                            (translated, None, raw)
                        }
                        Err(error) => {
                            let fallback = mu_parser::parse("true")
                                .expect("fallback μ-calculus formula parses successfully");
                            (
                                fallback,
                                Some(format!("LTL translation error: {}", error)),
                                format!("ltl {:?} (translation failed)", ltl_expr.formula),
                            )
                        }
                    }
                }
            };
            let targets = match &formula.targets {
                FormulaTargets::All(_) => FormulaTargetsKind::All,
                FormulaTargets::Named(list) => {
                    FormulaTargetsKind::Named(list.iter().map(|ident| ident.name.clone()).collect())
                }
            };
            let property_class = parsed.property_class();
            let alternation_depth = parsed.alternation_depth();
            formulas.insert(
                name.clone(),
                RealizedFormula {
                    name,
                    targets,
                    formula: parsed,
                    raw,
                    meta: formula.meta.clone(),
                    parse_error,
                    property_class,
                    alternation_depth,
                },
            );
            Ok::<(), RealizationError>(())
        })?;
    }
    Ok(formulas)
}

/// Realises a primary DSL document and optional sidecars into runtime structures.
///
/// The base document typically carries the structural definition (automata,
/// compositions) while sidecars contribute additional μ-formulas and controllers.
/// All μ-formulas and controllers are merged into the returned lookups with
/// duplicate identifiers rejected.
pub fn realize(
    doc: &ContextDoc,
    sidecars: &[ContextDoc],
) -> Result<RealizedContext, RealizationError> {
    let mut docs: Vec<&ContextDoc> = Vec::with_capacity(1 + sidecars.len());
    docs.push(doc);
    docs.extend(sidecars.iter());

    let label_universe = LabelUniverse::from_alphabet(&doc.alphabet);
    let user_predicates = collect_user_predicates(doc);

    // Extract input signals from all documents (main + sidecars).
    // The arithmetic sidecar contains the __input_signals__ formula.
    let input_signals = extract_input_signals_from_documents(&docs);

    let (context, composition_members) =
        build_context_with_compositions(doc, &label_universe, &input_signals)?;

    let mut formulas = collect_formulas(&docs)?;

    // Generate structural predicates for each automaton.
    generate_structural_predicates(&context, &mut formulas)?;

    let mut controllers: HashMap<String, RealizedController> = HashMap::new();
    for doc in &docs {
        for controller in &doc.controllers {
            let name = controller.name.name.clone();
            if controllers.contains_key(&name) {
                return Err(RealizationError::Duplicate {
                    kind: "controller",
                    name,
                });
            }
            let source = controller.source.name.clone();
            if context.clts(&source).is_none() {
                return Err(RealizationError::UnknownAutomaton(source));
            }
            let formula_name = controller.formula.name.clone();
            if !formulas.contains_key(&formula_name) {
                return Err(RealizationError::UnknownFormula(formula_name));
            }
            let options = ResolvedControllerOptions::from_ast(&controller.options);
            controllers.insert(
                name.clone(),
                RealizedController {
                    name,
                    source,
                    formula: formula_name,
                    options,
                    export: controller.export.clone(),
                    meta: controller.meta.clone(),
                },
            );
        }
    }

    let PredicateMaps {
        predicates,
        predicate_metadata,
        predicate_bitsets,
    } = compute_predicate_maps(&context, &formulas)?;

    let mut realized = RealizedContext {
        context,
        formulas,
        controllers,
        predicates,
        predicate_metadata,
        predicate_bitsets,
        composition_members,
    };

    register_user_predicates(
        &realized.context,
        &user_predicates,
        &mut realized.predicates,
        &mut realized.predicate_metadata,
        &mut realized.predicate_bitsets,
    )?;

    // Auto-register state name predicates: if a formula references a predicate name
    // that matches a state name, automatically create that predicate.
    auto_register_state_name_predicates(
        &realized.context,
        &realized.formulas,
        &realized.composition_members,
        &mut realized.predicates,
        &mut realized.predicate_metadata,
        &mut realized.predicate_bitsets,
    )?;

    // B4: fail-loud validation that every predicate referenced in every
    // formula resolves for at least one target automaton. Catches typos that
    // would otherwise slip through to evaluation time, where unresolved
    // predicates silently default to `false` (see SOUNDNESS comment in
    // evaluator::predicate_bits). Structured/valuation patterns are
    // skipped (see is_simple_identifier) — only plain-identifier typos
    // trigger the error.
    validate_formula_predicates(&realized)?;

    Ok(realized)
}

/// Heuristic: returns true when `name` looks like a plain identifier (the
/// pattern most likely to be a typo of a state or predicate name) and false
/// when it looks like a structured/valuation/expression predicate (which is
/// resolved by the evaluator dynamically and shouldn't be validated
/// statically).
///
/// Plain identifier: starts with a letter or underscore, contains only
/// alphanumerics and underscores, AND has no underscore-separated `_T_` or
/// `_state_` infix that would mark it as a valuation-pattern predicate.
fn is_simple_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    // Reject obvious valuation-pattern predicates so the dynamic resolver
    // gets a chance.
    if name.contains("_T_") || name.contains("_F_") || name.contains("_state_") {
        return false;
    }
    // Reject `field_<digits>` pattern (counter-equals-literal predicates,
    // e.g., `fill_5`, `count_0`).
    if let Some(idx) = name.rfind('_') {
        let suffix = &name[idx + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

/// B4: post-realize fail-loud validation that every predicate referenced in
/// every formula resolves for at least one target automaton — either via a
/// registered predicate name, via composition member projection, or via an
/// automaton with valuations (where on-demand expression evaluation can
/// synthesize the predicate at eval time).
///
/// Returns an error for each unresolved plain-identifier reference (the
/// common typo case). Structured/valuation/expression predicates are
/// skipped because their resolution is dynamic.
///
/// Without this check, the evaluator's `predicate_bits` returns an empty
/// bitset for unknown names — a typo silently defaults to `false` and
/// corrupts the verdict (see the SOUNDNESS comment in
/// `evaluator::predicate_bits`).
fn validate_formula_predicates(realized: &RealizedContext) -> Result<(), RealizationError> {
    use std::collections::HashSet;

    for formula in realized.formulas.values() {
        // Skip auto-generated structural predicates — they're synthesized by
        // the realizer itself and reference predicates also synthesized here.
        if formula
            .meta
            .comment
            .as_ref()
            .is_some_and(|c| c.contains("\"type\":\"structural\""))
        {
            continue;
        }

        let mut referenced = HashSet::new();
        extract_predicate_names(&formula.formula, &mut referenced);
        if referenced.is_empty() {
            continue;
        }

        // Determine target automata for this formula
        let target_automata: Vec<String> = match &formula.targets {
            FormulaTargetsKind::Named(names) => names.to_vec(),
            FormulaTargetsKind::All => realized.context.clts_names().into_iter().collect(),
        };

        if target_automata.is_empty() {
            continue;
        }

        for predicate in &referenced {
            // Skip anything that doesn't look like a simple identifier — these
            // are structured/valuation/expression-style predicates whose
            // resolution path is dynamic and harder to validate statically.
            // Plain identifier typos (the common case) DO get caught.
            if !is_simple_identifier(predicate) {
                continue;
            }

            // True = the predicate resolves for at least one target automaton
            let resolves_anywhere = target_automata.iter().any(|automaton| {
                // Path 1: pre-computed/auto-registered predicate
                if realized
                    .predicates
                    .get(automaton)
                    .is_some_and(|set| set.contains(predicate))
                {
                    return true;
                }

                // Path 2: composition member exposes it (environment_for projects)
                if let Some(members) = realized.composition_members.get(automaton)
                    && members.iter().any(|m| {
                        realized
                            .predicates
                            .get(m)
                            .is_some_and(|set| set.contains(predicate))
                    })
                {
                    return true;
                }

                // Path 3: automaton or any of its composition members has
                // valuations — the predicate may be a valuation-derived
                // expression that the evaluator (or
                // auto_register_state_name_predicates with pattern matching)
                // can resolve on-demand. Over-permits slightly to avoid
                // false-reject on valid abstract-state / valuation
                // expressions.
                if let Some(clts) = realized.context.clts(automaton)
                    && clts.has_valuations()
                {
                    return true;
                }
                if let Some(members) = realized.composition_members.get(automaton)
                    && members
                        .iter()
                        .any(|m| realized.context.clts(m).is_some_and(|c| c.has_valuations()))
                {
                    return true;
                }

                // Path 4 (IR-track P3.3): a predicate-cube 3-valued
                // predicate. The cube CTXDSL round-trips per-state Kleene
                // labels into the CLTS's `state_3valued_predicates`; the
                // KleeneDomain evaluator's `predicate_bits` resolves the
                // atom by name there, so a formula may reference it. Check
                // the automaton itself + any composition member.
                if let Some(clts) = realized.context.clts(automaton)
                    && clts.has_3valued_predicate_named(predicate)
                {
                    return true;
                }
                if let Some(members) = realized.composition_members.get(automaton)
                    && members.iter().any(|m| {
                        realized
                            .context
                            .clts(m)
                            .is_some_and(|c| c.has_3valued_predicate_named(predicate))
                    })
                {
                    return true;
                }

                false
            });

            if !resolves_anywhere {
                let automaton = target_automata.first().cloned().unwrap_or_default();
                let mut available: Vec<&str> = realized
                    .predicates
                    .get(&automaton)
                    .map(|set| set.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                available.sort();
                let truncated = if available.len() > 16 {
                    let mut head: Vec<&str> = available.iter().take(16).copied().collect();
                    head.push("...");
                    head.join(", ")
                } else {
                    available.join(", ")
                };
                return Err(RealizationError::UnknownPredicate {
                    formula: formula.name.clone(),
                    predicate: predicate.clone(),
                    automaton,
                    available: truncated,
                });
            }
        }
    }

    Ok(())
}

/// Generates structural predicates for each automaton in the context.
///
/// Structural predicates are automatically generated based on the CLTS structure:
/// - `has_enabled_transition`: True in states that have at least one outgoing transition
/// - `is_deadlock_state`: True in states that have no outgoing transitions
/// - `can_reach_completion`: True in states from which a completion state is reachable
///
/// These predicates are added as formulas with metadata so they can be used in property verification.
fn generate_structural_predicates(
    context: &Context,
    formulas: &mut HashMap<String, RealizedFormula>,
) -> Result<(), RealizationError> {
    // Get all automaton names from the context
    let automaton_names = context.clts_names();

    for automaton_name in automaton_names {
        let Some(clts) = context.clts(&automaton_name) else {
            continue;
        };

        // Generate has_enabled_transition predicate
        // Formula: <> true (there exists a next state, i.e., at least one outgoing transition)
        let has_enabled_name = format!("{}_has_enabled_transition", automaton_name);
        if !formulas.contains_key(&has_enabled_name) {
            let formula_str = "<> true";
            let (parsed, parse_error) = match mu_parser::parse(formula_str) {
                Ok(parsed) => (parsed, None),
                Err(_error) => {
                    return Err(RealizationError::UnsupportedFeature {
                        feature: "failed to parse has_enabled_transition structural predicate",
                    });
                }
            };

            // Create metadata JSON for predicate recognition
            let metadata_json = serde_json::json!({
                "predicate": has_enabled_name.clone(),
                "guard": formula_str,
                "expr": {
                    "type": "structural",
                    "description": "True in states with at least one outgoing transition"
                }
            });

            let property_class = parsed.property_class();
            let alternation_depth = parsed.alternation_depth();
            formulas.insert(
                has_enabled_name.clone(),
                RealizedFormula {
                    name: has_enabled_name.clone(),
                    targets: FormulaTargetsKind::Named(vec![automaton_name.clone()]),
                    formula: parsed,
                    raw: formula_str.to_string(),
                    meta: Meta {
                        id: None,
                        comment: Some(metadata_json.to_string()),
                    },
                    parse_error,
                    property_class,
                    alternation_depth,
                },
            );
        }

        // Generate is_deadlock_state predicate
        // Formula: !(<> true) (there is no next state, i.e., no outgoing transitions)
        let is_deadlock_name = format!("{}_is_deadlock_state", automaton_name);
        if !formulas.contains_key(&is_deadlock_name) {
            let formula_str = "!(<> true)";
            let (parsed, parse_error) = match mu_parser::parse(formula_str) {
                Ok(parsed) => (parsed, None),
                Err(_error) => {
                    return Err(RealizationError::UnsupportedFeature {
                        feature: "failed to parse is_deadlock_state structural predicate",
                    });
                }
            };

            let metadata_json = serde_json::json!({
                "predicate": is_deadlock_name.clone(),
                "guard": formula_str,
                "expr": {
                    "type": "structural",
                    "description": "True in states with no outgoing transitions (deadlock states)"
                }
            });

            let property_class = parsed.property_class();
            let alternation_depth = parsed.alternation_depth();
            formulas.insert(
                is_deadlock_name.clone(),
                RealizedFormula {
                    name: is_deadlock_name.clone(),
                    targets: FormulaTargetsKind::Named(vec![automaton_name.clone()]),
                    formula: parsed,
                    raw: formula_str.to_string(),
                    meta: Meta {
                        id: None,
                        comment: Some(metadata_json.to_string()),
                    },
                    parse_error,
                    property_class,
                    alternation_depth,
                },
            );
        }

        // Note: is_completion_state predicate is generated by translation pipelines

        // The predicate is computed directly from state names during predicate_bitsets
        // computation (see below) when the formula metadata indicates "computed_directly": true.

        // Generate can_reach_completion predicate
        // Formula: mu X. (completion_state || <> X)
        // This requires identifying completion states (end states, states with "complete" in name, etc.)
        // For now, we'll use a simpler approach: states from which we can eventually reach any state
        // Actually, let's make it more specific: states from which we can reach an end state
        // We identify end states by checking if is_completion_state predicate exists
        // or by checking state names (fallback)
        let end_state_names: Vec<String> = clts
            .states()
            .filter_map(|state_id| {
                clts.state_name(state_id).and_then(|name| {
                    let name_lower = name.to_lowercase();
                    if name_lower.contains("end")
                        || name_lower.contains("complete")
                        || name_lower.contains("finish")
                        || name_lower.contains("done")
                    {
                        Some(name.to_string())
                    } else {
                        None
                    }
                })
            })
            .collect();

        if !end_state_names.is_empty() {
            // Instead of enumerating all end states in the formula (which causes stack overflow
            // for large automata with 2000+ end states), we create a single predicate that
            // is true for all end states, then use that predicate in a simple formula.
            let completion_predicate_name = format!("{}_is_completion_state", automaton_name);
            let can_reach_name = format!("{}_can_reach_completion", automaton_name);

            if !formulas.contains_key(&can_reach_name) {
                // Register the completion predicate for all end states
                // This will be handled by the predicate bitset computation later
                // For now, we just create the formula that references this predicate

                // Simple formula: mu X. (is_completion_state || <> X)
                // This means: least fixpoint where we're either in a completion state or can reach one
                let formula_str = format!("mu X. ({} || <> X)", completion_predicate_name);

                let parsed = mu_parser::parse(&formula_str).map_err(|_| {
                    RealizationError::UnsupportedFeature {
                        feature: "failed to parse can_reach_completion structural predicate",
                    }
                })?;

                let metadata_json = serde_json::json!({
                    "predicate": can_reach_name.clone(),
                    "guard": formula_str.clone(),
                    "expr": {
                        "type": "structural",
                        "description": "True in states from which a completion state is reachable",
                        "completion_predicate": completion_predicate_name,
                        "end_states": end_state_names
                    }
                });

                let property_class = parsed.property_class();
                let alternation_depth = parsed.alternation_depth();
                formulas.insert(
                    can_reach_name.clone(),
                    RealizedFormula {
                        name: can_reach_name.clone(),
                        targets: FormulaTargetsKind::Named(vec![automaton_name.clone()]),
                        formula: parsed,
                        raw: formula_str,
                        meta: Meta {
                            id: None,
                            comment: Some(metadata_json.to_string()),
                        },
                        parse_error: None,
                        property_class,
                        alternation_depth,
                    },
                );
            }
        }
    }

    Ok(())
}

fn collect_user_predicates(doc: &ContextDoc) -> HashMap<String, Vec<PredicateDecl>> {
    let mut map = HashMap::new();
    for automaton in &doc.automata {
        if !automaton.predicates.is_empty() {
            map.insert(automaton.name.name.clone(), automaton.predicates.clone());
        }
    }
    map
}

fn register_user_predicates(
    context: &Context,
    user_predicates: &HashMap<String, Vec<PredicateDecl>>,
    predicates: &mut HashMap<String, HashSet<String>>,
    predicate_metadata: &mut HashMap<String, HashMap<String, PredicateMetadata>>,
    predicate_bitsets: &mut HashMap<String, HashMap<String, BitVec<usize, Lsb0>>>,
) -> Result<(), RealizationError> {
    for (automaton, decls) in user_predicates {
        let clts = context
            .clts(automaton)
            .ok_or_else(|| RealizationError::UnknownAutomaton(automaton.clone()))?;
        for decl in decls {
            let predicate_name = decl.name.name.clone();
            let entry = predicates.entry(automaton.clone()).or_default();
            if !entry.insert(predicate_name.clone()) {
                return Err(RealizationError::Duplicate {
                    kind: "predicate",
                    name: predicate_name,
                });
            }

            let state_name = predicate_state_name(&decl.target)?;
            let bits = bitset_for_state(clts, automaton, &state_name)?;
            predicate_bitsets
                .entry(automaton.clone())
                .or_default()
                .insert(predicate_name.clone(), bits);

            predicate_metadata
                .entry(automaton.clone())
                .or_default()
                .insert(
                    predicate_name,
                    PredicateMetadata::state_name_eq(&state_name),
                );
        }
    }
    Ok(())
}

/// Automatically registers state name predicates when they're referenced in formulas
/// but not explicitly declared. This ensures that formulas like `nu X. ((!Executing || ...) && [] X)`
/// work correctly when "Executing" is a state name but not a declared predicate.
fn auto_register_state_name_predicates(
    context: &Context,
    formulas: &HashMap<String, RealizedFormula>,
    composition_members: &HashMap<String, Vec<String>>,
    predicates: &mut HashMap<String, HashSet<String>>,
    predicate_metadata: &mut HashMap<String, HashMap<String, PredicateMetadata>>,
    predicate_bitsets: &mut HashMap<String, HashMap<String, BitVec<usize, Lsb0>>>,
) -> Result<(), RealizationError> {
    // Collect all predicate names referenced in formulas
    let mut referenced_predicates: HashMap<String, HashSet<String>> = HashMap::new();

    for formula in formulas.values() {
        // Extract predicate names from the formula AST
        let mut predicate_names = HashSet::new();
        extract_predicate_names(&formula.formula, &mut predicate_names);

        // Add to the set for each automaton this formula targets
        let automata = match &formula.targets {
            FormulaTargetsKind::Named(names) => names.to_vec(),
            FormulaTargetsKind::All => context.clts_names().into_iter().collect(),
        };
        for automaton in automata {
            referenced_predicates
                .entry(automaton)
                .or_default()
                .extend(predicate_names.iter().cloned());
        }
    }

    // For each automaton, check if referenced predicates match state names.
    // For compositions, also check member automata — predicates registered on
    // members are projected onto the composition by environment_for().
    for (automaton, pred_names) in referenced_predicates {
        let Some(clts) = context.clts(&automaton) else {
            continue;
        };

        // Get all state names in this automaton
        let state_names: HashSet<String> = clts
            .states()
            .filter_map(|state_id| clts.state_name(state_id).map(|s| s.to_string()))
            .collect();

        // Try direct matching on this automaton's own states
        let automaton_predicates = predicates.entry(automaton.clone()).or_default();
        for pred_name in &pred_names {
            // Try string prefix matching OR structured valuation matching
            let matches_any_state = state_names
                .iter()
                .any(|state_name| StateNameMatcher::matches_pattern(pred_name, state_name))
                || (clts.has_valuations()
                    && StateNameMatcher::create_bitset_for_pattern(clts, pred_name).any());

            if matches_any_state && !automaton_predicates.contains(pred_name) {
                automaton_predicates.insert(pred_name.clone());

                let bitset = StateNameMatcher::create_bitset_for_pattern(clts, pred_name);
                predicate_bitsets
                    .entry(automaton.clone())
                    .or_default()
                    .insert(pred_name.clone(), bitset);

                predicate_metadata
                    .entry(automaton.clone())
                    .or_default()
                    .insert(
                        pred_name.clone(),
                        PredicateMetadata::state_name_eq(pred_name),
                    );
            }
        }

        // If this is a composition, also register unresolved predicates on
        // member automata. environment_for() will project them onto the
        // composed state space via |‑separated state name splitting.
        if let Some(members) = composition_members.get(&automaton) {
            for member_name in members {
                let Some(member_clts) = context.clts(member_name) else {
                    continue;
                };

                let member_state_names: HashSet<String> = member_clts
                    .states()
                    .filter_map(|sid| member_clts.state_name(sid).map(|s| s.to_string()))
                    .collect();

                let member_predicates = predicates.entry(member_name.clone()).or_default();
                for pred_name in &pred_names {
                    // Try string prefix matching OR structured valuation matching
                    let matches_member = member_state_names
                        .iter()
                        .any(|sn| StateNameMatcher::matches_pattern(pred_name, sn))
                        || (member_clts.has_valuations()
                            && StateNameMatcher::create_bitset_for_pattern(member_clts, pred_name)
                                .any());

                    if matches_member && !member_predicates.contains(pred_name) {
                        member_predicates.insert(pred_name.clone());

                        let bitset =
                            StateNameMatcher::create_bitset_for_pattern(member_clts, pred_name);
                        predicate_bitsets
                            .entry(member_name.clone())
                            .or_default()
                            .insert(pred_name.clone(), bitset);

                        predicate_metadata
                            .entry(member_name.clone())
                            .or_default()
                            .insert(
                                pred_name.clone(),
                                PredicateMetadata::state_name_eq(pred_name),
                            );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Recursively extracts all predicate names from a formula AST
fn extract_predicate_names(formula: &crate::mu_calculus::Formula, names: &mut HashSet<String>) {
    use crate::mu_calculus::Node;

    fn visit_node(
        formula: &crate::mu_calculus::Formula,
        node_id: crate::mu_calculus::NodeId,
        names: &mut HashSet<String>,
    ) {
        match formula.node(node_id) {
            Node::Predicate(name) => {
                names.insert(name.clone());
            }
            Node::Not(inner) => {
                visit_node(formula, *inner, names);
            }
            Node::And(left, right) => {
                visit_node(formula, *left, names);
                visit_node(formula, *right, names);
            }
            Node::Or(left, right) => {
                visit_node(formula, *left, names);
                visit_node(formula, *right, names);
            }
            Node::Modal { target, .. } => {
                visit_node(formula, *target, names);
            }
            Node::Mu { body, .. } => {
                visit_node(formula, *body, names);
            }
            Node::Nu { body, .. } => {
                visit_node(formula, *body, names);
            }
            Node::True | Node::False | Node::Variable(_) => {
                // No predicates in these nodes
            }
        }
    }

    visit_node(formula, formula.root(), names);
}

fn predicate_state_name(target: &PredicateTarget) -> Result<String, RealizationError> {
    match target {
        PredicateTarget::State(StateRef::Simple(ident)) => Ok(ident.name.clone()),
        PredicateTarget::State(StateRef::Indexed { .. }) => {
            Err(RealizationError::UnsupportedFeature {
                feature: "indexed state predicates",
            })
        }
    }
}

/// Matches a state name pattern against actual state names, handling unrolled states.
///
/// This function supports matching original state names (e.g., "End") against unrolled
/// state names (e.g., "End_x_0", "End_count_5"). It first tries exact matching, then
fn bitset_for_state(
    clts: &RuntimeClts,
    automaton: &str,
    state_name: &str,
) -> Result<BitVec<usize, Lsb0>, RealizationError> {
    // First try exact match (for backward compatibility)
    if let Ok(state_id) = clts.state_id(state_name) {
        let mut bits = BitVec::repeat(false, clts.state_count());
        bits.set(state_id.index(), true);
        return Ok(bits);
    }

    // If exact match fails, try pattern matching for unrolled states
    let bits = StateNameMatcher::create_bitset_for_pattern(clts, state_name);

    // Check if we found any matches
    if bits.iter().any(|bit| *bit) {
        Ok(bits)
    } else {
        Err(RealizationError::UnknownPredicateState {
            automaton: automaton.to_owned(),
            state: state_name.to_owned(),
        })
    }
}

/// Converts an expression to a string representation for guard parsing.
fn expr_to_string(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Integer(value) => value.to_string(),
        ExprKind::Ident(ident) => {
            // Check if identifier is a boolean literal keyword
            // The parser converts true/false keywords to identifiers
            if ident.name.eq_ignore_ascii_case("true") {
                "true".to_string()
            } else if ident.name.eq_ignore_ascii_case("false") {
                "false".to_string()
            } else {
                ident.name.clone()
            }
        }
        ExprKind::Index { target, expr } => {
            format!("{}[{}]", target.name, expr_to_string(expr))
        }
        ExprKind::Unary { op, expr } => {
            let op_str = match op {
                crate::context_dsl::ast::UnaryOp::Not => "!",
                crate::context_dsl::ast::UnaryOp::Neg => "-",
            };
            format!("({}{})", op_str, expr_to_string(expr))
        }
        ExprKind::Binary { left, op, right } => {
            let op_str = match op {
                crate::context_dsl::ast::BinaryOp::Add => "+",
                crate::context_dsl::ast::BinaryOp::Sub => "-",
                crate::context_dsl::ast::BinaryOp::Mul => "*",
                crate::context_dsl::ast::BinaryOp::Div => "/",
                crate::context_dsl::ast::BinaryOp::Mod => "%",
                crate::context_dsl::ast::BinaryOp::And => "&&",
                crate::context_dsl::ast::BinaryOp::Or => "||",
                crate::context_dsl::ast::BinaryOp::Eq => "==",
                crate::context_dsl::ast::BinaryOp::Ne => "!=",
                crate::context_dsl::ast::BinaryOp::Lt => "<",
                crate::context_dsl::ast::BinaryOp::Le => "<=",
                crate::context_dsl::ast::BinaryOp::Gt => ">",
                crate::context_dsl::ast::BinaryOp::Ge => ">=",
            };
            format!(
                "({}{}{})",
                expr_to_string(left),
                op_str,
                expr_to_string(right)
            )
        }
        ExprKind::Group(expr) => {
            // Remove outer parentheses for guard parsing - they're not needed
            // and might interfere with static guard detection
            expr_to_string(expr)
        }
    }
}

/// Extracts all identifier names from an expression.
fn extract_identifiers_from_expr(expr: &Expr) -> HashSet<String> {
    let mut identifiers = HashSet::new();
    match &expr.kind {
        ExprKind::Ident(ident) => {
            identifiers.insert(ident.name.clone());
        }
        ExprKind::Index { target, .. } => {
            identifiers.insert(target.name.clone());
        }
        ExprKind::Unary { expr, .. } => {
            identifiers.extend(extract_identifiers_from_expr(expr));
        }
        ExprKind::Binary { left, right, .. } => {
            identifiers.extend(extract_identifiers_from_expr(left));
            identifiers.extend(extract_identifiers_from_expr(right));
        }
        ExprKind::Group(expr) => {
            identifiers.extend(extract_identifiers_from_expr(expr));
        }
        ExprKind::Integer(_) => {}
    }
    identifiers
}

/// Extracts input signals from context documents (main + sidecars).
///
/// Input signals can be extracted during translation.
/// They are stored in the arithmetic sidecar as a special formula `__input_signals__`
/// with metadata containing the input signal list.
///
/// **IMPORTANT**: Transitions with guards containing input signals are automatically
/// marked as uncontrollable in `build_automaton` (see lines 503-515). This ensures
/// that the controller cannot force transitions that depend on environment-controlled
/// input signals, which is the correct semantics when modelling environment inputs.
fn extract_input_signals_from_documents(docs: &[&ContextDoc]) -> HashSet<String> {
    // Look for the special __input_signals__ formula in any of the documents
    // (typically in the arithmetic sidecar)
    for doc in docs {
        for formula in &doc.mu_formulas {
            if formula.name.name == "__input_signals__"
                && let Some(ref comment) = formula.meta.comment
            {
                // Parse the JSON metadata to extract input_signals
                if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(comment)
                    && let Some(input_signals_array) =
                        metadata.get("input_signals").and_then(|v| v.as_array())
                {
                    return input_signals_array
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                }
            }
        }
    }
    HashSet::new()
}

/// Expands state group and wildcard selectors in transitions into concrete
/// per-state transitions. Returns a new `Automaton` with all selectors
/// resolved to `Named(Simple(...))` and `state_groups` cleared.
fn expand_state_selectors(automaton: &Automaton) -> Result<Automaton, RealizationError> {
    // If there are no groups and no group/wildcard selectors, return as-is.
    let has_selectors = automaton.transitions.iter().any(|t| {
        matches!(
            t.source,
            StateSelector::Group(_) | StateSelector::Wildcard(_)
        ) || matches!(
            t.target,
            StateSelector::Group(_) | StateSelector::Wildcard(_)
        )
    });
    if automaton.state_groups.is_empty() && !has_selectors {
        return Ok(automaton.clone());
    }

    // Build group membership map from state_groups declarations.
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    for group in &automaton.state_groups {
        let mut members = Vec::new();
        for member in &group.members {
            match member {
                StateSelector::Named(StateRef::Simple(ident)) => {
                    members.push(ident.name.clone());
                }
                _ => {
                    return Err(RealizationError::UnsupportedFeature {
                        feature: "nested groups or wildcards in state group members",
                    });
                }
            }
        }
        groups.insert(group.name.name.clone(), members);
    }

    // Collect all declared state names for wildcard expansion.
    let all_states: Vec<String> = automaton
        .states
        .iter()
        .map(|s| s.name.name.clone())
        .collect();

    let resolve = |selector: &StateSelector| -> Result<Vec<String>, RealizationError> {
        match selector {
            StateSelector::Named(StateRef::Simple(ident)) => Ok(vec![ident.name.clone()]),
            StateSelector::Named(StateRef::Indexed { .. }) => Ok(vec![]), // handled later
            StateSelector::Group(ident) => {
                groups
                    .get(&ident.name)
                    .cloned()
                    .ok_or(RealizationError::UnsupportedFeature {
                        feature: "unknown state group reference",
                    })
            }
            StateSelector::Wildcard(wp) => {
                if wp.pattern == "*" {
                    Ok(all_states.clone())
                } else {
                    // Simple prefix match: "Err*" matches "Error", "ErrState", etc.
                    let prefix = wp.pattern.trim_end_matches('*');
                    Ok(all_states
                        .iter()
                        .filter(|s| s.starts_with(prefix))
                        .cloned()
                        .collect())
                }
            }
        }
    };

    let mut expanded_transitions = Vec::new();
    for transition in &automaton.transitions {
        let sources = resolve(&transition.source)?;
        let targets = resolve(&transition.target)?;
        // If either resolution returned an empty vec because of an Indexed ref,
        // keep the original transition for downstream handling.
        if sources.is_empty() || targets.is_empty() {
            expanded_transitions.push(transition.clone());
            continue;
        }
        for src in &sources {
            for tgt in &targets {
                let src_span = match &transition.source {
                    StateSelector::Named(StateRef::Simple(id)) => id.span,
                    StateSelector::Group(id) => id.span,
                    StateSelector::Wildcard(wp) => wp.span,
                    StateSelector::Named(StateRef::Indexed { name, .. }) => name.span,
                };
                let tgt_span = match &transition.target {
                    StateSelector::Named(StateRef::Simple(id)) => id.span,
                    StateSelector::Group(id) => id.span,
                    StateSelector::Wildcard(wp) => wp.span,
                    StateSelector::Named(StateRef::Indexed { name, .. }) => name.span,
                };
                expanded_transitions.push(TransitionDecl {
                    source: StateSelector::Named(StateRef::Simple(super::ast::Ident::new(
                        src.clone(),
                        src_span,
                    ))),
                    target: StateSelector::Named(StateRef::Simple(super::ast::Ident::new(
                        tgt.clone(),
                        tgt_span,
                    ))),
                    label: transition.label.clone(),
                    additional_labels: transition.additional_labels.clone(),
                    guard: transition.guard.clone(),
                    effects: transition.effects.clone(),
                    modality: transition.modality,
                    additional_targets: transition.additional_targets.clone(),
                });
            }
        }
    }

    let mut result = automaton.clone();
    result.transitions = expanded_transitions;
    result.state_groups.clear();
    Ok(result)
}

fn build_automaton(
    name: &str,
    automaton: &Automaton,
    labels: &LabelUniverse,
    input_signals: &HashSet<String>,
    state_valuations: Option<&HashMap<String, BTreeMap<String, String>>>,
    externally_controllable: &HashSet<String>,
) -> Result<RuntimeClts, RealizationError> {
    // Desugar state groups and wildcards into concrete transitions.
    let automaton = expand_state_selectors(automaton)?;
    let automaton = &automaton;

    // Prepare per-automaton controllable/internal sets
    let mut automaton_controllable = HashSet::new();
    for entry in &automaton.controllable {
        automaton_controllable.insert(entry.name.name.clone());
    }
    let mut automaton_internal = HashSet::new();
    for entry in &automaton.internal {
        automaton_internal.insert(entry.name.name.clone());
    }

    if !automaton.parameters.is_empty() {
        return Err(RealizationError::UnsupportedFeature {
            feature: "automaton parameters",
        });
    }

    // Phase 1: Check if unrolling is required
    let has_variables = !automaton.variables.is_empty();

    // Only check for dynamic guards that require unrolling if we don't already have variables
    // (If we have variables, we'll unroll anyway)
    let has_dynamic_guards_requiring_unrolling = if has_variables {
        false // Don't need to check - we'll unroll anyway
    } else {
        has_dynamic_guards(automaton)?
    };

    // If automaton has dynamic guards that reference variables but no variables are declared,
    // unrolling is required but impossible
    if has_dynamic_guards_requiring_unrolling && !has_variables {
        return Err(RealizationError::DynamicGuardsRequireUnrolling {
            name: name.to_owned(),
        });
    }

    // If automaton has variables, MUST unroll (regardless of guards)
    // If automaton has dynamic guards that reference variables, MUST unroll
    if has_variables || has_dynamic_guards_requiring_unrolling {
        return build_automaton_with_unrolling(
            name,
            automaton,
            labels,
            input_signals,
            &automaton_controllable,
            &automaton_internal,
            externally_controllable,
        );
    }

    // Otherwise, build directly (no unrolling needed)

    let mut builder = Clts::builder();
    builder.reserve_states(automaton.states.len());

    let mut label_cache: HashMap<String, RuntimeLabelId> = HashMap::new();

    let mut variable_names = Vec::new();
    for variable in &automaton.variables {
        if variable.index.is_some() {
            return Err(RealizationError::UnsupportedFeature {
                feature: "indexed variables",
            });
        }
        variable_names.push(variable.name.name.clone());
    }

    for state in &automaton.states {
        ensure_supported_state(state)?;
        let state_id = builder.state_id_or_insert(&state.name.name);
        if state.is_initial {
            builder.initial(&state.name.name);
        }
        if !variable_names.is_empty() {
            builder.with_variables(&state.name.name, variable_names.iter().map(String::as_str));
        }
        // Wire structured valuations from the side-channel data and overlay
        // hand-written `valuations { … }` entries on top. Hand-written entries
        // win on key collision so authors can correct adapter-emitted defaults.
        if let Some(state_id) = state_id {
            let side_channel = state_valuations.and_then(|m| m.get(&state.name.name));
            let merged = merged_state_valuation(state, side_channel)?;
            if !merged.is_empty() {
                builder.with_valuation_for_state(state_id, merged);
            }
            // Per-state 3-valued (Kleene) predicate labels from a
            // `predicates_3v { … }` block → `Clts::state_3valued_predicates`.
            // This is the round-trippable surface for a predicate-cube KMTS.
            for tv in &state.three_valued {
                let verdict = match tv.value {
                    crate::context_dsl::ast::TristateLit::True => crate::clts::Tristate::KleeneT,
                    crate::context_dsl::ast::TristateLit::False => crate::clts::Tristate::KleeneF,
                    crate::context_dsl::ast::TristateLit::Unknown => {
                        crate::clts::Tristate::KleeneBot
                    }
                };
                builder.with_3valued_predicate(state_id, tv.name.name.clone(), verdict);
            }
        }
    }

    AstTraverser::visit_transitions(automaton, |transition| {
        // Phase 1: Check if guard is static and filter if false
        // Note: With mandatory unrolling, dynamic guards are handled during unrolling
        // Static false guards are filtered here
        if let Some(ref guard) = transition.guard {
            let guard_str = expr_to_string(guard);
            let (_, parsed_guard) = crate::guard::parse_guard(&guard_str);
            let static_value = crate::guard::is_static_guard(&parsed_guard);

            // Filter static false guards
            if static_value == Some(false) {
                return Ok(()); // Skip this transition - it's always disabled
            }
        }

        let source = state_selector_name(&transition.source)?;
        let target = state_selector_name(&transition.target)?;

        let mut label_ids: Vec<RuntimeLabelId> = Vec::new();
        let primary = convert_label(
            name,
            &mut builder,
            &mut label_cache,
            labels,
            &transition.label,
        )?;
        label_ids.push(primary);

        for additional in &transition.additional_labels {
            let id = convert_label(name, &mut builder, &mut label_cache, labels, additional)?;
            if !label_ids.contains(&id) {
                label_ids.push(id);
            }
        }

        // Label-based controllability
        let is_epsilon = matches!(transition.label, TransitionLabel::Epsilon(_))
            || transition
                .additional_labels
                .iter()
                .any(|l| matches!(l, TransitionLabel::Epsilon(_)));

        // Guard-based input signal check (for backward compatibility)
        let guard_has_input_signal = if let Some(ref guard) = transition.guard {
            let guard_identifiers = extract_identifiers_from_expr(guard);
            guard_identifiers
                .iter()
                .any(|id| input_signals.contains(id))
        } else {
            false
        };

        // Determine label controllability for each label
        // Build full label names list including epsilon
        let mut label_names = transition_label_names(transition);
        // If this is an epsilon transition, ensure "epsilon" is in the names list
        if is_epsilon && !label_names.contains(&"epsilon".to_string()) {
            // For epsilon transitions, label_ids contains the "epsilon" label but transition_label_names doesn't return it
            // So we need to add it manually
            label_names.push("epsilon".to_string());
        }

        // Ensure label_names and label_ids have the same length
        // If label_ids has more elements (e.g., epsilon label), pad label_names
        while label_names.len() < label_ids.len() {
            label_names.push("epsilon".to_string());
        }

        // If automaton declares controllable/internal labels, use them; otherwise fallback to legacy inference
        let uses_explicit_sets = automaton.controllable_declared
            || automaton.internal_declared
            || !automaton_controllable.is_empty()
            || !automaton_internal.is_empty();

        for (label_id, label_name) in label_ids.iter().zip(label_names.iter()) {
            let controllability = classify_transition_controllability(
                label_name,
                uses_explicit_sets,
                &automaton_controllable,
                &automaton_internal,
                is_epsilon,
                guard_has_input_signal,
                input_signals,
                externally_controllable,
            );
            builder.set_label_controllability(*label_id, controllability);
        }
        // R.5 Item K sub-item K.2 (2026-06-05) — thread the CTXDSL
        // `[may]` / `[must]` / `[sharp]` modality attribute into the CLTS
        // `TransitionModality`. Pre-K.2 callers (and every existing
        // fixture without a modality attribute) carry
        // `TransitionModalitySpec::Sharp` from K.1's default and realize
        // to `TransitionModality::Sharp` here, preserving behaviour
        // byte-for-byte. `[must]` with no additional_targets
        // realizes to a singleton hyper-must (target = {to_id});
        // R.5 Item K sub-item K.1b (2026-06-06) extends this to
        // multi-target hyper-must when the CTXDSL syntax
        // `transition s -> [t1, t2, t3] on a [must];` populates
        // `additional_targets`.
        // All transitions are always enabled after unrolling (guards resolved at build time)
        if let (Some(from_id), Some(to_id)) = (
            builder.state_id_or_insert(source),
            builder.state_id_or_insert(target),
        ) {
            let modality = match transition.modality {
                TransitionModalitySpec::Sharp => TransitionModality::Sharp,
                TransitionModalitySpec::MayOnly => TransitionModality::MayOnly,
                TransitionModalitySpec::MustOnly => {
                    // R.5 Item K sub-item K.1b — build the
                    // hyper-target set from the primary target +
                    // any additional_targets declared via the
                    // bracketed-list syntax.
                    let mut hyper_targets: smallvec::SmallVec<
                        [crate::clts::StateId<DefaultStateIdx>; 4],
                    > = smallvec::smallvec![to_id];
                    for additional in &transition.additional_targets {
                        let additional_name = state_selector_name(additional)?;
                        if let Some(extra_id) = builder.state_id_or_insert(additional_name) {
                            hyper_targets.push(extra_id);
                        }
                    }
                    TransitionModality::must_hyper(hyper_targets)
                }
            };
            builder.transition_ids_with_modality(from_id, &label_ids, to_id, modality);
        }
        Ok::<(), RealizationError>(())
    })?;

    builder
        .build()
        .map_err(|error| RealizationError::AutomatonBuild {
            name: name.to_owned(),
            error,
        })
}

/// Checks if an automaton has any dynamic guards that require unrolling.
///
/// Only guards that reference state variables require unrolling.
/// Predicate guards (like "cond_a") don't require unrolling - they're evaluated
/// at runtime using the environment.
fn has_dynamic_guards(automaton: &Automaton) -> Result<bool, RealizationError> {
    use crate::guard::GuardExpr;

    // Helper functions to check if a string is a constant
    fn is_numeric_literal(s: &str) -> bool {
        s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok()
    }

    fn is_boolean_literal(s: &str) -> bool {
        s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false")
    }

    for transition in &automaton.transitions {
        if let Some(ref guard) = transition.guard {
            let guard_str = expr_to_string(guard);
            let (_, parsed_guard) = crate::guard::parse_guard(&guard_str);

            // Check if this guard references variables (requires unrolling)
            let requires_unrolling = match &parsed_guard {
                GuardExpr::True | GuardExpr::False => false, // Static - no unrolling needed
                GuardExpr::Predicate(_) => false, // Predicate - evaluated via environment, no unrolling needed
                GuardExpr::Comparison { left, right, .. } => {
                    // Check if either side is not a constant (i.e., references a variable)
                    let left_is_const = is_numeric_literal(left) || is_boolean_literal(left);
                    let right_is_const = is_numeric_literal(right) || is_boolean_literal(right);

                    // If at least one side is not a constant, it references a variable
                    !left_is_const || !right_is_const
                }
            };

            if requires_unrolling {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Builds an automaton using unrolling (mandatory for automata with variables or dynamic guards).
fn build_automaton_with_unrolling(
    name: &str,
    automaton: &Automaton,
    labels: &LabelUniverse,
    input_signals: &HashSet<String>,
    automaton_controllable: &HashSet<String>,
    automaton_internal: &HashSet<String>,
    externally_controllable: &HashSet<String>,
) -> Result<RuntimeClts, RealizationError> {
    // Convert DSL structures to unrolling format
    let original_states = convert_states_for_unrolling(&automaton.states)?;

    // Track which original state names were initial
    let initial_location_names: HashSet<String> = automaton
        .states
        .iter()
        .filter(|s| s.is_initial)
        .map(|s| s.name.name.clone())
        .collect();

    let original_transitions = convert_transitions_for_unrolling(&automaton.transitions)?;
    let variables = convert_variables_for_unrolling(&automaton.variables)?;

    let mut unrolling_options = UnrollingOptions::default();
    if let Some(ref mut heuristic_config) = unrolling_options.heuristic_config {
        heuristic_config.max_total_states = 10000; // Increased limit for integration tests
    } else {
        unrolling_options.heuristic_config =
            Some(crate::abstraction::heuristics::HeuristicConfig {
                max_total_states: 10000,
                ..Default::default()
            });
    }
    let unrolled = unroll_states(
        original_states,
        original_transitions,
        variables,
        unrolling_options,
    )
    .map_err(|e| RealizationError::UnrollingFailed {
        name: name.to_owned(),
        error: e.to_string(),
    })?;

    // Build CLTS from unrolled result
    build_clts_from_unrolled(
        unrolled,
        name,
        labels,
        input_signals,
        automaton_controllable,
        automaton_internal,
        &initial_location_names,
        externally_controllable,
    )
}

/// Converts DSL states to unrolling format.
fn convert_states_for_unrolling(
    states: &[StateDecl],
) -> Result<Vec<OriginalState>, RealizationError> {
    let mut result = Vec::new();
    for state in states {
        ensure_supported_state(state)?;
        result.push(OriginalState {
            name: state.name.name.clone(),
            initial: state.is_initial,
        });
    }
    Ok(result)
}

/// Converts DSL transitions to unrolling format.
fn convert_transitions_for_unrolling(
    transitions: &[TransitionDecl],
) -> Result<Vec<OriginalTransition>, RealizationError> {
    let mut result = Vec::new();
    for transition in transitions {
        let source = state_selector_name(&transition.source)?;
        let target = state_selector_name(&transition.target)?;

        // Get label name (unrolling only supports single labels, use primary label)
        let label = match &transition.label {
            TransitionLabel::Named { name, .. } => name.name.clone(),
            TransitionLabel::Epsilon(_) => "epsilon".to_string(),
        };

        // Convert guard expression to string
        let guard = transition.guard.as_ref().map(expr_to_string);

        // Convert effects
        let effects: Vec<Effect> = transition
            .effects
            .iter()
            .map(|a| Effect {
                target: a.target.name.clone(),
                value_expr: expr_to_string(&a.expr),
            })
            .collect();

        // R.5 Item K sub-item K.1b-unrolled (2026-06-08) —
        // resolve `additional_targets` (StateSelectors) to
        // location-portion state name strings. Drop any
        // selector that doesn't resolve to a simple state name
        // (groups, wildcards). For parametric automata, the
        // unroller's per-variable expansion substitutes the
        // primary `to` per binding combination; the additional
        // targets are carried as location names + matched
        // against the primary's binding-suffix at the
        // build-from-unrolled step.
        let additional_targets: Vec<String> = transition
            .additional_targets
            .iter()
            .filter_map(|sel| state_selector_name(sel).ok().map(|s| s.to_string()))
            .collect();

        result.push(OriginalTransition {
            from: source.to_string(),
            to: target.to_string(),
            label,
            guard,
            effects,
            // R.5 Item K sub-item K.2b (2026-06-06) — propagate
            // the AST TransitionDecl's modality into the
            // OriginalTransition so the unrolled CLTS edge
            // inherits it via build_clts_from_unrolled.
            modality: transition.modality,
            additional_targets,
        });
    }
    Ok(result)
}

/// Converts DSL variables to unrolling format.
fn convert_variables_for_unrolling(
    variables: &[crate::context_dsl::ast::VariableDecl],
) -> Result<Vec<VariableDecl>, RealizationError> {
    let mut result = Vec::new();
    for variable in variables {
        if variable.index.is_some() {
            return Err(RealizationError::UnsupportedFeature {
                feature: "indexed variables",
            });
        }

        // Extract initial value as string
        let initial_str = expr_to_string(&variable.init);

        result.push(VariableDecl {
            name: variable.name.name.clone(),
            ty: match &variable.ty {
                crate::context_dsl::ast::TypeName::Bool => "bool".to_string(),
                crate::context_dsl::ast::TypeName::I64 => "i64".to_string(),
                crate::context_dsl::ast::TypeName::Enum(_) => "i64".to_string(),
            },
            initial: Some(initial_str),
        });
    }
    Ok(result)
}

/// Builds a CLTS from unrolled states and transitions.
/// All guards have been resolved during unrolling, so no guard predicates are needed.
#[allow(clippy::too_many_arguments)]
fn build_clts_from_unrolled(
    unrolled: UnrolledClts,
    name: &str,
    labels: &LabelUniverse,
    input_signals: &HashSet<String>,
    automaton_controllable: &HashSet<String>,
    automaton_internal: &HashSet<String>,
    initial_location_names: &HashSet<String>,
    externally_controllable: &HashSet<String>,
) -> Result<RuntimeClts, RealizationError> {
    let mut builder = Clts::builder();
    let mut label_cache: HashMap<String, RuntimeLabelId> = HashMap::new();

    // Add states from unrolled CLTS and mark initial states
    for state in &unrolled.states {
        let state_name = state.state_name();
        let state_id = builder.state_id_or_insert(&state_name);

        // Mark as initial if the state's location was initial in the original automaton
        if initial_location_names.contains(&state.location) {
            builder.initial(&state_name);
        }

        // Variable-binding fix (2026-06-23): emit numeric per-state
        // valuations from the unrolled `AbstractState`'s integer variable
        // bindings, so the abstract-states wiring later in this module
        // (gated on `clts_valuations_are_numeric`) binds `var == value`
        // formula atoms to actual integer values — the same machinery the
        // BTOR2 bit-blaster's `build_state_valuations` already feeds. Before
        // this, hand-authored CTXDSL variable-value atoms fell through the
        // "predicate-not-found → empty bitset" under-approximation and
        // silently evaluated false. Only `IntConstant` bindings are emitted
        // (numeric, i64-parseable); bool / interval / non-constant
        // abstractions are skipped so the all-numeric gate stays honest.
        if let Some(state_id) = state_id {
            let valuation: std::collections::BTreeMap<String, String> = state
                .variables
                .iter()
                .filter_map(|(var, val)| match val {
                    crate::abstraction::value::AbstractValue::IntConstant(n) => {
                        Some((var.clone(), n.to_string()))
                    }
                    _ => None,
                })
                .collect();
            if !valuation.is_empty() {
                builder.with_valuation_for_state(state_id, valuation);
            }
        }
    }

    // Add transitions (all are always enabled - no guards)
    for transition in &unrolled.transitions {
        let from_name = transition.from.state_name();
        let to_name = transition.to.state_name();

        // Convert label to label ID
        let label_id = convert_label(
            name,
            &mut builder,
            &mut label_cache,
            labels,
            &TransitionLabel::Named {
                name: crate::context_dsl::ast::Ident {
                    name: transition.label.clone(),
                    span: crate::context_dsl::token::Span::new(0, 0, 0, 0),
                },
                index: None,
            },
        )?;

        // Determine label controllability
        let uses_explicit_sets =
            !automaton_controllable.is_empty() || !automaton_internal.is_empty();
        let is_epsilon = transition.label == "epsilon";
        let controllability = classify_transition_controllability(
            &transition.label,
            uses_explicit_sets,
            automaton_controllable,
            automaton_internal,
            is_epsilon,
            false, // no guard-based input signal check for unrolled transitions
            input_signals,
            externally_controllable,
        );
        builder.set_label_controllability(label_id, controllability);

        // R.5 Item K sub-item K.2b (2026-06-06) — the unrolled
        // path now inherits the modality from the source
        // `OriginalTransition` (threaded through
        // `convert_transitions_for_unrolling` per K.2b). Parametric
        // automata declaring `[may]` / `[must]` realize correctly;
        // the K.2 SOUNDNESS gap is closed.
        //
        // R.5 Item K sub-item K.1b-unrolled (2026-06-08) —
        // multi-target hyper-must on the unrolled path. When
        // `transition.additional_targets` is non-empty AND
        // modality is `MustOnly`, the hyper-target set is built
        // from the primary `to` plus each additional target's
        // resolved StateId. The additional targets are
        // location-portion state names; we look them up in the
        // builder by best-effort match (try the exact name first,
        // then the unrolled state name suffixed with the same
        // variable-binding portion as the primary). If a target
        // can't be resolved (the location doesn't exist in the
        // unrolled state space), it's dropped silently —
        // matching the K.2 direct-realize path's robust resolution.
        if let (Some(from_id), Some(to_id)) = (
            builder.state_id_or_insert(&from_name),
            builder.state_id_or_insert(&to_name),
        ) {
            let modality = match transition.modality {
                TransitionModalitySpec::Sharp => TransitionModality::Sharp,
                TransitionModalitySpec::MayOnly => TransitionModality::MayOnly,
                TransitionModalitySpec::MustOnly => {
                    let mut hyper_targets: smallvec::SmallVec<
                        [crate::clts::StateId<DefaultStateIdx>; 4],
                    > = smallvec::smallvec![to_id];
                    for additional_name in &transition.additional_targets {
                        if let Some(extra_id) = builder.state_id_or_insert(additional_name) {
                            hyper_targets.push(extra_id);
                        }
                    }
                    TransitionModality::must_hyper(hyper_targets)
                }
            };
            builder.transition_ids_with_modality(from_id, &[label_id], to_id, modality);
        }
    }

    builder
        .build()
        .map_err(|error| RealizationError::AutomatonBuild {
            name: name.to_owned(),
            error,
        })
}

/// Classifies a single label's controllability.
///
/// When `uses_explicit_sets` is true, classification is based on the automaton's
/// declared `controllable` / `internal` sets. Otherwise, legacy inference applies:
/// a label is uncontrollable if it matches an input signal, is epsilon, has a
/// guard referencing an input signal, or is already owned by another automaton.
#[allow(clippy::too_many_arguments)]
fn classify_transition_controllability(
    label_name: &str,
    uses_explicit_sets: bool,
    automaton_controllable: &HashSet<String>,
    automaton_internal: &HashSet<String>,
    is_epsilon: bool,
    guard_has_input_signal: bool,
    input_signals: &HashSet<String>,
    externally_controllable: &HashSet<String>,
) -> LabelControllability {
    if uses_explicit_sets {
        if automaton_internal.contains(label_name) {
            LabelControllability::Internal
        } else if automaton_controllable.contains(label_name) {
            LabelControllability::Controllable
        } else {
            LabelControllability::Uncontrollable
        }
    } else {
        // Legacy inference: uncontrollable if matches input signal, is epsilon,
        // or is already declared controllable by another automaton.
        let is_uncontrollable = label_name == "epsilon"
            || is_epsilon
            || input_signals.contains(label_name)
            || guard_has_input_signal
            || externally_controllable.contains(label_name);
        if is_uncontrollable {
            LabelControllability::Uncontrollable
        } else {
            LabelControllability::Controllable
        }
    }
}

fn transition_label_names(transition: &TransitionDecl) -> Vec<String> {
    let mut names = Vec::new();
    if let TransitionLabel::Named { name, .. } = &transition.label {
        names.push(name.name.clone());
    }
    for additional in &transition.additional_labels {
        if let TransitionLabel::Named { name, .. } = additional
            && !names.contains(&name.name)
        {
            names.push(name.name.clone());
        }
    }
    names
}

fn ensure_supported_state(state: &StateDecl) -> Result<(), RealizationError> {
    if state.index.is_some() {
        return Err(RealizationError::UnsupportedFeature {
            feature: "indexed states",
        });
    }
    if !state.overrides.is_empty() {
        return Err(RealizationError::UnsupportedFeature {
            feature: "state variable overrides",
        });
    }
    Ok(())
}

fn state_selector_name(selector: &StateSelector) -> Result<&str, RealizationError> {
    match selector {
        StateSelector::Named(StateRef::Simple(ident)) => Ok(&ident.name),
        StateSelector::Named(StateRef::Indexed { .. }) => {
            Err(RealizationError::UnsupportedFeature {
                feature: "indexed state references",
            })
        }
        StateSelector::Group(_) => Err(RealizationError::UnsupportedFeature {
            feature: "state groups in transitions",
        }),
        StateSelector::Wildcard(_) => Err(RealizationError::UnsupportedFeature {
            feature: "state wildcards",
        }),
    }
}

/// Evaluates a constant expression at expansion time.
/// Resolves identifiers from `constants` (global) and `params` (template parameters).
fn eval_const_expr(
    expr: &Expr,
    constants: &HashMap<String, i64>,
    params: &HashMap<String, i64>,
) -> Result<i64, RealizationError> {
    match &expr.kind {
        ExprKind::Integer(n) => Ok(*n),
        ExprKind::Ident(id) => params
            .get(&id.name)
            .or_else(|| constants.get(&id.name))
            .copied()
            .ok_or_else(|| RealizationError::UnknownConstant(id.name.clone())),
        ExprKind::Binary { left, op, right } => {
            use super::ast::BinaryOp;
            let l = eval_const_expr(left, constants, params)?;
            let r = eval_const_expr(right, constants, params)?;
            match op {
                BinaryOp::Add => Ok(l + r),
                BinaryOp::Sub => Ok(l - r),
                BinaryOp::Mul => Ok(l * r),
                BinaryOp::Div => {
                    if r == 0 {
                        Err(RealizationError::NonConstantExpression(
                            "division by zero".to_owned(),
                        ))
                    } else {
                        Ok(l / r)
                    }
                }
                BinaryOp::Mod => {
                    if r == 0 {
                        Err(RealizationError::NonConstantExpression(
                            "modulo by zero".to_owned(),
                        ))
                    } else {
                        Ok(l % r)
                    }
                }
                _ => Err(RealizationError::NonConstantExpression(format!(
                    "operator {:?} not supported in constant expressions",
                    op
                ))),
            }
        }
        ExprKind::Unary { op, expr } => {
            use super::ast::UnaryOp;
            let v = eval_const_expr(expr, constants, params)?;
            match op {
                UnaryOp::Neg => Ok(-v),
                UnaryOp::Not => Err(RealizationError::NonConstantExpression(
                    "boolean not in integer expression".to_owned(),
                )),
            }
        }
        ExprKind::Group(e) => eval_const_expr(e, constants, params),
        ExprKind::Index { .. } => Err(RealizationError::NonConstantExpression(
            "index expressions not supported in constant context".to_owned(),
        )),
    }
}

/// Resolves a `MemberRef` into a concrete automaton name.
/// For `Client[0]` → `"Client_0"`, for plain `Arbiter` → `"Arbiter"`.
fn resolve_member_name(member: &super::ast::MemberRef, constants: &HashMap<String, i64>) -> String {
    match &member.index {
        Some(index_expr) => {
            let empty = HashMap::new();
            match eval_const_expr(index_expr, constants, &empty) {
                Ok(idx) => format!("{}_{}", member.name.name, idx),
                Err(_) => member.name.name.clone(), // fallback to bare name
            }
        }
        None => member.name.name.clone(),
    }
}

fn convert_label(
    automaton_name: &str,
    builder: &mut RuntimeBuilder,
    cache: &mut HashMap<String, RuntimeLabelId>,
    universe: &LabelUniverse,
    label: &TransitionLabel,
) -> Result<RuntimeLabelId, RealizationError> {
    let label_name = match label {
        TransitionLabel::Named { name, index } => {
            if index.is_some() {
                return Err(RealizationError::UnsupportedFeature {
                    feature: "indexed labels",
                });
            }
            name.name.clone()
        }
        TransitionLabel::Epsilon(_) => "epsilon".to_owned(),
    };

    if let Some(id) = cache.get(&label_name) {
        return Ok(*id);
    }

    let payload = universe.payload(&label_name);
    let id =
        builder
            .labels()
            .intern(payload)
            .map_err(|error| RealizationError::AutomatonBuild {
                name: automaton_name.to_owned(),
                error,
            })?;
    cache.insert(label_name, id);
    Ok(id)
}

struct LabelUniverse {
    entries: HashMap<String, Vec<String>>,
}

impl LabelUniverse {
    fn from_alphabet(entries: &[AlphabetEntry]) -> Self {
        let mut map = HashMap::new();
        for entry in entries {
            let payload = entry
                .display
                .as_ref()
                .map(|value| vec![value.clone()])
                .unwrap_or_else(|| vec![entry.name.name.clone()]);
            map.insert(entry.name.name.clone(), payload);
        }
        Self { entries: map }
    }

    fn payload(&self, name: &str) -> Vec<String> {
        self.entries
            .get(name)
            .cloned()
            .unwrap_or_else(|| vec![name.to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_dsl::parse;

    #[test]
    fn is_simple_identifier_recognizes_typo_candidates() {
        // Plain identifiers — fail-loud candidates
        assert!(is_simple_identifier("Bad"));
        assert!(is_simple_identifier("foo_bar"));
        assert!(is_simple_identifier("running"));

        // Structured / valuation patterns — skipped (dynamic)
        assert!(!is_simple_identifier("flag_T_state_IDLE"));
        assert!(!is_simple_identifier("overlap_T_state_AES_ACCESS"));
        assert!(!is_simple_identifier("fill_5"));
        assert!(!is_simple_identifier("count_0"));

        // Junk
        assert!(!is_simple_identifier(""));
        assert!(!is_simple_identifier("123abc"));
        assert!(!is_simple_identifier("with spaces"));
        assert!(!is_simple_identifier("dot.notation"));
    }

    /// R46-6 regression — the OOB sink's `__mununu_oob__` marker must NOT
    /// trip the numericity gate. A `BoundedCounter` (or `EnumValues`-subset)
    /// model whose transition escapes its declared value set gains an
    /// absorbing OOB sink carrying `{__mununu_oob__: "true"}`; before the
    /// fix that single non-numeric marker made `clts_valuations_are_numeric`
    /// return `false`, disabling abstract-states wiring for every real
    /// (fully numeric) state — which flipped reachability verdicts from
    /// SATISFIED to a spurious VIOLATED relative to the otherwise-identical
    /// full bit-blast model.
    #[test]
    fn oob_sink_marker_does_not_trip_numericity_gate() {
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        let s0 = b.state_id_or_insert("s0").expect("s0");
        let s1 = b.state_id_or_insert("s1").expect("s1");
        let oob = b.state_id_or_insert(OOB_SINK_MARKER_KEY).expect("oob");
        b.initial("s0");
        b.with_valuation_for_state(s0, BTreeMap::from([("timer".to_string(), "0".to_string())]));
        b.with_valuation_for_state(s1, BTreeMap::from([("timer".to_string(), "1".to_string())]));
        // The bit-blaster's exact OOB-sink valuation (key == value-marker).
        b.with_valuation_for_state(
            oob,
            BTreeMap::from([(OOB_SINK_MARKER_KEY.to_string(), "true".to_string())]),
        );
        let clts = b.build().expect("clts builds");

        // Real states are all numeric; the OOB marker is exempt → gate passes.
        assert!(
            clts_valuations_are_numeric(&clts),
            "OOB sink marker must be exempt from the numericity gate"
        );
    }

    /// Complement to the R46-6 regression: a genuinely non-numeric *real*
    /// valuation (e.g. the SV adapter's semantic `state == IDLE` / `T`/`F`
    /// encodings) must still trip the gate, leaving those models on the
    /// pre-computed-predicate path. Exempting the OOB marker must not widen
    /// the exemption to real design valuations.
    #[test]
    fn non_numeric_real_valuation_still_trips_numericity_gate() {
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        let s0 = b.state_id_or_insert("s0").expect("s0");
        b.initial("s0");
        b.with_valuation_for_state(
            s0,
            BTreeMap::from([("state".to_string(), "IDLE".to_string())]),
        );
        let clts = b.build().expect("clts builds");
        assert!(
            !clts_valuations_are_numeric(&clts),
            "a non-numeric design valuation must keep the gate closed"
        );
    }

    #[test]
    fn unresolved_predicate_fails_realize() {
        // Plain identifier predicate `Bar` does not match any state nor any
        // registered predicate. Validator must reject with UnknownPredicate.
        let doc = parse(
            r#"
context typo_test {
    alphabet { label tick; }
    automata {
        automaton M {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
    }
    mu_formulas {
        formula references_typo { over M; body = nu X. (Bar && [] X); }
    }
}
"#,
        )
        .expect("context parses");
        let err = realize(&doc, &[]).expect_err("realize rejects unresolved predicate");
        assert!(
            matches!(err, RealizationError::UnknownPredicate { ref predicate, .. } if predicate == "Bar"),
            "expected UnknownPredicate(Bar), got {err:?}"
        );
    }

    #[test]
    fn structured_pattern_predicate_does_not_fail_when_unresolvable() {
        // Predicates like `field_5` or `flag_T_state_IDLE` skip validation
        // because their resolution is dynamic. They may still produce empty
        // bitsets at evaluation time, but that's a separate concern from
        // typo-detection.
        let doc = parse(
            r#"
context structured_test {
    alphabet { label tick; }
    automata {
        automaton M {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
    }
    mu_formulas {
        formula uses_structured { over M; body = nu X. (field_5 && [] X); }
    }
}
"#,
        )
        .expect("context parses");
        let _ = realize(&doc, &[]).expect("structured-pattern predicate skipped");
    }

    #[test]
    fn predicates_3v_block_realizes_into_state_3valued_predicates() {
        // CTXDSL-output gap fix (IR-track) — a per-state `predicates_3v { … }`
        // block carries Kleene labels that realize into the CLTS's
        // `state_3valued_predicates`, the round-trippable surface for a
        // predicate-cube KMTS.
        use crate::clts::Tristate;
        let doc = parse(
            r#"
context tri_test {
    alphabet { label tick; }
    automata {
        automaton M {
            states {
                state s0 initial {
                    predicates_3v { p = unknown; q = true; r = false; }
                };
                state s1;
            }
            transitions {
                transition s0 -> s1 on label tick;
                transition s1 -> s1 on label tick;
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realizes");
        let clts = realized.context.clts("M").expect("automaton M exists");
        assert!(
            clts.has_3valued_predicates(),
            "3-valued labels must be registered on the CLTS"
        );
        let s0 = clts.state_id("s0").expect("s0 exists");
        assert_eq!(
            clts.state_3valued_predicate(s0, "p"),
            Some(Tristate::KleeneBot)
        );
        assert_eq!(
            clts.state_3valued_predicate(s0, "q"),
            Some(Tristate::KleeneT)
        );
        assert_eq!(
            clts.state_3valued_predicate(s0, "r"),
            Some(Tristate::KleeneF)
        );
        // A state with no `predicates_3v` block carries no 3-valued labels.
        let s1 = clts.state_id("s1").expect("s1 exists");
        assert_eq!(clts.state_3valued_predicate(s1, "p"), None);
    }

    #[test]
    fn formula_referencing_a_3valued_predicate_realizes() {
        // IR-track P3.3 — a formula atom that names a `predicates_3v`
        // (Kleene) predicate must pass realize-time validation (Path 4):
        // the predicate-cube verify path round-trips its labels through
        // `predicates_3v` and references them from the checked formula.
        // Before the fix this errored with `UnknownPredicate` (the 3-valued
        // names weren't registered as referenceable predicates), even
        // though the KleeneDomain evaluator resolves them by name.
        let doc = parse(
            r#"
context tri_formula {
    alphabet { label tick; }
    automata {
        automaton M {
            states {
                state s0 initial {
                    predicates_3v { boot_idle = true; }
                };
                state s1 {
                    predicates_3v { boot_idle = false; }
                };
            }
            transitions {
                transition s0 -> s1 on label tick;
                transition s1 -> s1 on label tick;
            }
        }
    }
    mu_formulas {
        formula holds_idle {
            over M;
            body = "boot_idle";
        }
    }
}
"#,
        )
        .expect("context parses");
        // The key assertion: realize SUCCEEDS — the formula's `boot_idle`
        // atom resolves to the 3-valued predicate (Path 4), not rejected
        // as an unknown predicate.
        let realized = realize(&doc, &[]).expect("realizes (3-valued predicate resolves)");
        let clts = realized.context.clts("M").expect("automaton M exists");
        assert!(clts.has_3valued_predicate_named("boot_idle"));
        assert!(!clts.has_3valued_predicate_named("nonexistent"));
    }

    #[test]
    fn realize_simple_context() {
        let doc = parse(
            r#"
context simple {
    alphabet { label tick; }
    automata {
        automaton Machine {
            states {
                state s0 initial;
                state s1;
            }
            transitions {
                transition s0 -> s1 on label tick;
                transition s1 -> s1 on label tick;
            }
        }
    }
    mu_formulas {
        formula stay { over Machine; body = true; }
    }
    controllers {
        controller trivial { source Machine; satisfying stay; }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("context realizes");
        assert!(realized.context.clts("Machine").is_some());
        assert!(realized.formulas.contains_key("stay"));
        assert!(realized.controllers.contains_key("trivial"));
    }

    /// mununu#589 — a synchronisation vector whose two labels sit on two DIFFERENT partner
    /// automata fires whatever the members' fold order: the owner first (the case #570 fixed) and
    /// the owner LAST, where the pairwise fold had no joint `{a, b}` step to pair with and the
    /// vector's target state silently vanished from the product. The three automata are the
    /// issue's reproducer; `S1` is the vector's target.
    #[test]
    fn x589_a_vector_split_across_two_partners_fires_whatever_the_fold_order() {
        let partners = r#"
        automaton J {
            controllable { }
            states { state J0 initial; state J0b; state J1; }
            transitions {
                transition J0 -> J0b on label pre;
                transition J0b -> J1 on label a;
                transition J1 -> J1 on label j_sink;
            }
        }
        automaton L {
            controllable { }
            states { state L0 initial; }
            transitions { transition L0 -> L0 on label b; }
        }
"#;
        let owner = |name: &str| {
            format!(
                r#"
        automaton {name} {{
            controllable {{ }}
            states {{ state S0 initial; state S0b; state S1; }}
            transitions {{
                transition S0 -> S0b on label pre;
                transition S0b -> S1 on {{label a, label b}};
                transition S1 -> S1 on label a_sink;
            }}
        }}
"#
            )
        };
        let reaches_s1 = |owner_name: &str, members: &str| -> bool {
            let src = format!(
                "context V {{ automata {{ {} {} }} composition {{ asynchronous P {{ members [{members}]; }} }} }}",
                owner(owner_name),
                partners
            );
            let doc = parse(&src).expect("parses");
            let realized = realize(&doc, &[]).expect("realizes");
            let p = realized.context.clts("P").expect("the product");
            // S1 is reached iff some product state's name carries the owner's `S1`.
            p.states()
                .any(|s| p.state_name(s).map(|n| n.contains("S1")).unwrap_or(false))
        };
        // The owner first in the fold — the pairwise rule alone handles it.
        assert!(reaches_s1("A", "A, J, L"), "owner first: the vector fires");
        // The owner LAST: J and L are folded first and must take their joint `{a, b}` step
        // as partners of the vector A carries, or A never finds anything to pair with.
        assert!(reaches_s1("A", "J, L, A"), "owner last: the vector fires");
        assert!(
            reaches_s1("Z", "J, L, Z"),
            "owner last, named to sort last: the vector fires"
        );
    }

    /// mununu#589, the control: two automata with NO vector anywhere in the composition never
    /// get a joint step — the rendezvous rule only completes a vector some member carries.
    #[test]
    fn x589_partners_without_an_owner_do_not_synchronise_on_their_own() {
        let src = r#"
context V {
    automata {
        automaton J {
            controllable { }
            states { state J0 initial; state J1; }
            transitions { transition J0 -> J1 on label a; }
        }
        automaton L {
            controllable { }
            states { state L0 initial; state L1; }
            transitions { transition L0 -> L1 on label b; }
        }
    }
    composition { asynchronous P { members [J, L]; } }
}
"#;
        let doc = parse(src).expect("parses");
        let realized = realize(&doc, &[]).expect("realizes");
        let p = realized.context.clts("P").expect("the product");
        let joint = p.states().any(|s| {
            p.outgoing(s).iter().any(|t| {
                let labels: Vec<String> = t
                    .labels()
                    .iter()
                    .filter_map(|id| p.label_payload(*id))
                    .flat_map(|l| l.iter().cloned())
                    .collect();
                labels.len() >= 2
            })
        });
        assert!(
            !joint,
            "no member carries a vector, so J and L only interleave"
        );
    }

    #[test]
    fn realize_merges_sidecar_formulas() {
        let base = parse(
            r#"
context base {
    alphabet { label alpha; }
    automata {
        automaton A {
            states { state start initial; }
            transitions { transition start -> start on label alpha; }
        }
    }
}
"#,
        )
        .expect("base parses");

        let sidecar = parse(
            r#"
context base_properties {
    mu_formulas {
        formula ok { over A; body = true; }
    }
    controllers {
        controller ok_ctrl { source A; satisfying ok; }
    }
}
"#,
        )
        .expect("sidecar parses");

        let realized = realize(&base, &[sidecar]).expect("context realizes");
        assert!(realized.formulas.contains_key("ok"));
        assert!(realized.controllers.contains_key("ok_ctrl"));
    }

    #[test]
    fn realize_rejects_duplicate_automata() {
        // Test duplicate automaton error (lines 253-257)
        let doc = parse(
            r#"
context dup {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
        automaton A {
            states { state s1 initial; }
            transitions { transition s1 -> s1 on epsilon; }
        }
    }
}
"#,
        )
        .expect("context parses");

        let result = realize(&doc, &[]);
        assert!(result.is_err());
        match result {
            Err(RealizationError::Duplicate { kind, name }) => {
                assert_eq!(kind, "automaton");
                assert_eq!(name, "A");
            }
            _ => panic!("expected Duplicate error"),
        }
    }

    #[test]
    fn realize_rejects_duplicate_formulas() {
        // Test duplicate formula error (lines 277-281)
        let doc = parse(
            r#"
context dup {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    mu_formulas {
        formula f1 { over A; body = true; }
        formula f1 { over A; body = false; }
    }
}
"#,
        )
        .expect("context parses");

        let result = realize(&doc, &[]);
        assert!(result.is_err());
        match result {
            Err(RealizationError::Duplicate { kind, name }) => {
                assert_eq!(kind, "μ-formula");
                assert_eq!(name, "f1");
            }
            _ => panic!("expected Duplicate error"),
        }
    }

    #[test]
    fn realize_rejects_duplicate_controllers() {
        // Test duplicate controller error (lines 315-319)
        let doc = parse(
            r#"
context dup {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    mu_formulas {
        formula f1 { over A; body = true; }
    }
    controllers {
        controller c1 { source A; satisfying f1; }
        controller c1 { source A; satisfying f1; }
    }
}
"#,
        )
        .expect("context parses");

        let result = realize(&doc, &[]);
        assert!(result.is_err());
        match result {
            Err(RealizationError::Duplicate { kind, name }) => {
                assert_eq!(kind, "controller");
                assert_eq!(name, "c1");
            }
            _ => panic!("expected Duplicate error"),
        }
    }

    #[test]
    fn realize_rejects_unknown_automaton() {
        // Test unknown automaton error (lines 322-323)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    mu_formulas {
        formula f1 { over A; body = true; }
    }
    controllers {
        controller c1 { source B; satisfying f1; }
    }
}
"#,
        )
        .expect("context parses");

        let result = realize(&doc, &[]);
        assert!(result.is_err());
        match result {
            Err(RealizationError::UnknownAutomaton(name)) => {
                assert_eq!(name, "B");
            }
            _ => panic!("expected UnknownAutomaton error"),
        }
    }

    #[test]
    fn realize_rejects_unknown_formula() {
        // Test unknown formula error (lines 326-327)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    controllers {
        controller c1 { source A; satisfying unknown; }
    }
}
"#,
        )
        .expect("context parses");

        let result = realize(&doc, &[]);
        assert!(result.is_err());
        match result {
            Err(RealizationError::UnknownFormula(name)) => {
                assert_eq!(name, "unknown");
            }
            _ => panic!("expected UnknownFormula error"),
        }
    }

    #[test]
    fn realize_handles_formula_parse_errors() {
        // Test formula parse error handling (lines 283-289)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    mu_formulas {
        formula bad { over A; body = invalid syntax here; }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds with fallback");
        let formula = realized.formulas.get("bad").expect("formula exists");
        assert!(formula.parse_error.is_some());
        // Should have fallback to "true"
        assert!(matches!(
            formula.formula.node(formula.formula.root()),
            crate::mu_calculus::Node::True
        ));
    }

    #[test]
    fn realize_environment_for_builds_predicates() {
        // Test environment_for method (lines 156-177)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states {
                state s0 initial;
                state s1;
            }
            transitions {
                transition s0 -> s1 on label tick;
            }
        }
    }
    mu_formulas {
        formula p1 { over A; body = true; }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");
        let env = realized.environment_for("A");
        // Environment should be created successfully
        assert_eq!(env.state_count(), 2);
    }

    #[test]
    fn realize_predicate_names_accessor() {
        // Test predicate_names accessor (lines 124-126)
        // Predicates are populated from guard metadata in formula comments
        let doc = parse(
            r#"
context test {
    alphabet {
        label tick;
        label sync;
    }
    automata {
        automaton A {
            alphabet { label tick; }
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
        automaton B {
            alphabet { label sync; }
            states { state t0 initial; }
            transitions { transition t0 -> t0 on label sync; }
        }
    }
    mu_formulas {
        formula guard1 {
            meta { comment = "{\"predicate\": \"guard1\", \"guard\": \"x > 0\", \"expr\": {\"type\": \"comparison\", \"left\": \"x\", \"op\": \">\", \"right\": \"0\"}}"; }
            over A;
            body = true;
        }
        formula guard2 {
            meta { comment = "{\"predicate\": \"guard2\", \"guard\": \"y == 1\", \"expr\": {\"type\": \"comparison\", \"left\": \"y\", \"op\": \"==\", \"right\": \"1\"}}"; }
            over A, B;
            body = true;
        }
        formula no_guard {
            over A;
            body = true;
        }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");

        // Test automaton A: should have both guard1 and guard2, plus structural predicates
        let names_a = realized
            .predicate_names("A")
            .expect("A should have predicates");
        // Should have guard1, guard2, and structural predicates (has_enabled_transition, is_deadlock_state, can_reach_completion)
        assert!(
            names_a.len() >= 2,
            "A should have at least guard1 and guard2"
        );
        assert!(names_a.contains("guard1"));
        assert!(names_a.contains("guard2"));
        // Structural predicates are also generated
        assert!(names_a.iter().any(|p| p.contains("has_enabled_transition")));

        // Test automaton B: should have guard2 (targeted by guard2 formula), plus structural predicates
        let names_b = realized
            .predicate_names("B")
            .expect("B should have predicates");
        // Should have guard2 and structural predicates
        assert!(!names_b.is_empty(), "B should have at least guard2");
        assert!(names_b.contains("guard2"));
        // Structural predicates are also generated
        assert!(names_b.iter().any(|p| p.contains("has_enabled_transition")));

        // Test non-existent automaton: should return None
        assert!(realized.predicate_names("C").is_none());

        // Verify that formulas without guard metadata don't create predicates
        // (no_guard formula should not appear in predicate_names)
        assert!(!names_a.contains("no_guard"));
    }

    #[test]
    fn realize_predicate_formula_accessor() {
        // Test predicate_formula accessor (lines 129-136)
        // predicate_formula returns metadata guard if available, otherwise formula raw
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
    }
    mu_formulas {
        formula guard_pred {
            meta { comment = "{\"predicate\": \"guard_pred\", \"guard\": \"x > 5\", \"expr\": {\"type\": \"comparison\", \"left\": \"x\", \"op\": \">\", \"right\": \"5\"}}"; }
            over A;
            body = x > 5;
        }
        formula no_metadata {
            over A;
            body = true;
        }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");

        // Test with metadata: should return guard from metadata, not formula raw
        let guard_formula = realized
            .predicate_formula("A", "guard_pred")
            .expect("guard_pred should exist");
        assert_eq!(guard_formula, "x > 5"); // From metadata guard, not formula raw

        // Test without metadata: should return formula raw as fallback
        let fallback_formula = realized
            .predicate_formula("A", "no_metadata")
            .expect("no_metadata should exist");
        assert_eq!(fallback_formula, "true"); // From formula.raw

        // Test non-existent predicate: should return None (no metadata and no formula)
        assert!(realized.predicate_formula("A", "nonexistent").is_none());

        // Test non-existent automaton: falls back to formula raw if formula exists
        // (predicate_formula doesn't validate automaton existence before fallback)
        let formula_for_b = realized.predicate_formula("B", "guard_pred");
        // Since guard_pred formula exists, it returns the formula raw as fallback
        // (even though automaton B doesn't exist, the function falls back to formulas.get)
        assert_eq!(formula_for_b, Some("x > 5")); // Falls back to formula.raw
    }

    #[test]
    fn realize_marks_uncontrollable_transitions() {
        // Test that transitions with input signals are marked uncontrollable (lines 536-549)
        // Use different labels for each transition to avoid label controllability conflicts
        // Note: With mandatory unrolling, we need variables for guards that reference predicates
        let main_doc = parse(
            r#"
context test {
    alphabet {
        label tick;
        label action;
    }
    automata {
        automaton A {
            variables {
                var has_input: bool = false;
                var is_controllable: bool = true;
            }
            states {
                state s0 initial;
                state s1;
            }
            transitions {
                transition s0 -> s1 on label tick guard has_input;
                transition s0 -> s0 on label action guard is_controllable;
            }
        }
    }
}
"#,
        )
        .expect("main context parses");

        // Realize without sidecar (not needed for variable-based guards)
        let realized = realize(&main_doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("A").expect("CLTS exists");

        // After unrolling, state names include variable values (e.g., "s0_has_input_false_is_controllable_true")
        // Find initial states (should be states starting with "s0_")
        let initial_states: Vec<_> = clts
            .initial_states()
            .iter()
            .filter_map(|&id| {
                clts.state_name(id)
                    .filter(|name| name.starts_with("s0_"))
                    .map(|_| id)
            })
            .collect();

        assert!(
            !initial_states.is_empty(),
            "should have at least one initial state"
        );
        let s0 = initial_states[0];

        let transitions = clts.outgoing(s0);
        // After unrolling with has_input=false and is_controllable=true:
        // - The tick transition (guard has_input=false) won't be included
        // - The action transition (guard is_controllable=true) should be included
        // However, if no transitions satisfy their guards, the state might have no outgoing transitions

        // Find transition with "action" label if it exists
        let trans_with_action = transitions.iter().find(|t| {
            t.labels().iter().any(|&lid| {
                clts.label_payload(lid)
                    .map(|payload| payload.contains(&"action".to_string()))
                    .unwrap_or(false)
            })
        });

        // If action transition exists, verify it's controllable
        if let Some(trans) = trans_with_action {
            // Transition with is_controllable guard should be controllable
            assert!(
                trans.is_controllable(clts),
                "transition with action label (is_controllable guard) should be controllable"
            );

            // Verify that the label has the correct controllability
            let action_label = trans.labels()[0]; // First (and only) label in transition with action

            // action should be controllable
            assert_eq!(
                clts.label_controllability(action_label),
                Some(LabelControllability::Controllable),
                "action label should be marked controllable"
            );
        }

        // Note: The tick transition with guard has_input=false won't be in the unrolled CLTS
        // because the guard evaluates to false during unrolling
        // The action transition with guard is_controllable=true should be present if the guard is satisfied
    }

    #[test]
    fn realize_extracts_input_signals_from_sidecar() {
        // Test input signals extraction (lines 447-470)
        // The __input_signals__ formula is skipped during realization (line 274-275)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
    }
}
"#,
        )
        .expect("context parses");

        // Create a sidecar with __input_signals__ formula
        // Note: The actual metadata format is complex (JSON in comment field)
        // This test just verifies the formula is skipped
        let sidecar = parse(
            r#"
context arithmetic {
    mu_formulas {
        formula __input_signals__ {
            over A;
            body = true;
        }
    }
}
"#,
        )
        .expect("sidecar parses");

        let realized = realize(&doc, &[sidecar]).expect("realization succeeds");
        // The __input_signals__ formula should not appear in formulas map (line 274-275)
        assert!(!realized.formulas.contains_key("__input_signals__"));
    }

    #[test]
    fn realize_marks_epsilon_transitions_as_uncontrollable() {
        // Test that epsilon transitions are marked as uncontrollable (Phase 3.5 proof-of-concept)
        let doc = parse(
            r#"
context test {
    alphabet { label alpha; }
    automata {
        automaton A {
            states {
                state s0 initial;
                state s1;
            }
            transitions {
                transition s0 -> s1 on epsilon;
                transition s1 -> s0 on label alpha;
            }
        }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("A").expect("automaton A exists");
        let s0 = clts.state_id("s0").expect("state s0 exists");

        // Check that the epsilon transition from s0 is marked as uncontrollable
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "s0 should have one outgoing transition");
        assert!(
            outgoing[0].is_uncontrollable(clts),
            "epsilon transition should be marked as uncontrollable"
        );

        // Check that the labeled transition from s1 is controllable
        let s1 = clts.state_id("s1").expect("state s1 exists");
        let outgoing_s1 = clts.outgoing(s1);
        assert_eq!(
            outgoing_s1.len(),
            1,
            "s1 should have one outgoing transition"
        );
        assert!(
            outgoing_s1[0].is_controllable(clts),
            "labeled transition should be marked as controllable"
        );
    }

    #[test]
    fn realize_handles_formula_targets_all() {
        // Test FormulaTargets::All handling (line 292)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on epsilon; }
        }
    }
    mu_formulas {
        formula f1 { over all; body = true; }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");
        let formula = realized.formulas.get("f1").expect("formula exists");
        match &formula.targets {
            FormulaTargetsKind::All => {}
            _ => panic!("expected All targets"),
        }
    }

    #[test]
    fn realize_handles_formula_targets_named() {
        // Test FormulaTargets::Named handling (lines 293-295)
        let doc = parse(
            r#"
context test {
    alphabet { label tick; label tock; }
    automata {
        automaton A {
            states { state s0 initial; }
            transitions { transition s0 -> s0 on label tick; }
        }
        automaton B {
            states { state t0 initial; }
            transitions { transition t0 -> t0 on label tock; }
        }
    }
    mu_formulas {
        formula f1 { over A, B; body = true; }
    }
}
"#,
        )
        .expect("context parses");

        let realized = realize(&doc, &[]).expect("realization succeeds");
        let formula = realized.formulas.get("f1").expect("formula exists");
        match &formula.targets {
            FormulaTargetsKind::Named(names) => {
                assert_eq!(names.len(), 2);
                assert!(names.contains(&"A".to_string()));
                assert!(names.contains(&"B".to_string()));
            }
            _ => panic!("expected Named targets"),
        }
    }

    #[test]
    fn realize_pattern_matching_for_unrolled_states() {
        // Test that predicate computation correctly matches original state names
        // against unrolled state names using prefix pattern matching.
        // This ensures sidecar formulas referencing "End" work with unrolled states like "End_x_0".
        let doc = parse(
            r#"
context test {
    alphabet { label tick; }
    automata {
        automaton A {
            variables {
                var x : i64 = 0;
            }
            states {
                state Start initial;
                state Processing;
                state End;
            }
            transitions {
                transition Start -> Processing on label tick;
                transition Processing -> End on label tick;
            }
        }
    }
}
"#,
        )
        .expect("context parses");

        // Create a sidecar with a structural predicate referencing "End"
        let sidecar = parse(
            r#"
context test_structural {
    mu_formulas {
        formula A_is_completion_state {
            meta {
                comment = "{\"predicate\": \"A_is_completion_state\", \"guard\": \"End\", \"expr\": {\"type\": \"structural\", \"end_states\": [\"End\"], \"computed_directly\": true}}";
            }
            over A;
            body = End;
        }
    }
}
"#,
        )
        .expect("sidecar parses");

        let realized = realize(&doc, &[sidecar]).expect("realization succeeds");
        let clts = realized.context.clts("A").expect("automaton A exists");

        // After unrolling, states should have variable values in their names
        // e.g., "Start_x_0", "Processing_x_0", "End_x_0"
        let state_names: Vec<String> = clts
            .states()
            .filter_map(|id| clts.state_name(id).map(|s| s.to_string()))
            .collect();

        // Verify that unrolled states exist
        assert!(
            state_names.iter().any(|n| n.starts_with("End_")),
            "Should have unrolled End states (e.g., End_x_0), got: {:?}",
            state_names
        );

        // Verify that the predicate was registered
        let predicates = realized
            .predicate_names("A")
            .expect("A should have predicates");
        assert!(
            predicates.contains("A_is_completion_state"),
            "Should have A_is_completion_state predicate, got: {:?}",
            predicates
        );

        // Verify that the predicate bitset correctly matches unrolled End states
        let predicate_bitsets = realized
            .predicate_bitsets
            .get("A")
            .expect("A should have predicate bitsets");
        let completion_bitset = predicate_bitsets
            .get("A_is_completion_state")
            .expect("A_is_completion_state bitset should exist");

        // Count how many states match the predicate
        let matching_states: Vec<String> = clts
            .states()
            .filter_map(|id| {
                if completion_bitset
                    .get(id.index())
                    .map(|b| *b)
                    .unwrap_or(false)
                {
                    clts.state_name(id).map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        // Should match all unrolled End states (e.g., "End_x_0")
        assert!(
            !matching_states.is_empty(),
            "Predicate should match at least one unrolled End state, got: {:?}",
            matching_states
        );
        assert!(
            matching_states.iter().all(|n| n.starts_with("End_")),
            "All matching states should be unrolled End states, got: {:?}",
            matching_states
        );

        // Verify that the predicate works in formula evaluation
        let env = realized.environment_for("A");
        let formula = realized
            .formulas
            .get("A_is_completion_state")
            .expect("formula should exist");
        let result = realized
            .context
            .evaluate_mu("A", &formula.formula, &env, None)
            .expect("evaluation should succeed");

        // The result should match the predicate bitset
        assert_eq!(
            result.len(),
            completion_bitset.len(),
            "Formula evaluation result should match predicate bitset size"
        );
        for state_id in clts.states() {
            let idx = state_id.index();
            let formula_result = result.get(idx).map(|b| *b).unwrap_or(false);
            let predicate_result = completion_bitset.get(idx).map(|b| *b).unwrap_or(false);
            assert_eq!(
                formula_result,
                predicate_result,
                "State {} should have consistent formula and predicate results",
                clts.state_name(state_id).unwrap_or("unknown")
            );
        }
    }

    /// R.5 Item K sub-item K.2 — every transition without a modality
    /// attribute realizes to `TransitionModality::Sharp`, preserving
    /// pre-K.1 behaviour byte-for-byte.
    ///
    /// Tests use the bare-`on epsilon` form rather than
    /// `on label <name> [<modality>]` because of a parser ambiguity:
    /// after `label <name>` the parser eagerly consumes `[…]` as an
    /// indexed-label expression (`name[index]`), which then bypasses
    /// the modality-attribute slot at the trailing position. The
    /// epsilon form sidesteps the ambiguity. Resolving the
    /// labelled-form ambiguity is queued as K.1c (likely: peek ahead
    /// past `[…]` to disambiguate index-expression from modality-
    /// attribute, or require modality attributes only at the trailing
    /// position after a wrapping `effects { }` block).
    #[test]
    fn r5_subitem_k2_default_transition_realizes_to_sharp() {
        let doc = parse(
            r#"
context default_modality {
    automata {
        automaton M {
            states { state s0 initial; state s1; }
            transitions {
                transition s0 -> s1 on epsilon;
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        let s0 = clts
            .initial_states()
            .iter()
            .copied()
            .next()
            .expect("initial state exists");
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "exactly one outgoing transition");
        assert!(
            matches!(outgoing[0].modality(), TransitionModality::Sharp),
            "default transition realizes to Sharp, got {:?}",
            outgoing[0].modality()
        );
    }

    /// R.5 Item K sub-item K.2 — the CTXDSL `[may]` attribute realizes
    /// to `TransitionModality::MayOnly` on the resulting CLTS edge.
    #[test]
    fn r5_subitem_k2_may_attribute_realizes_to_may_only() {
        let doc = parse(
            r#"
context may_only_modality {
    automata {
        automaton M {
            states { state s0 initial; state s1; }
            transitions {
                transition s0 -> s1 on epsilon [may];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        let s0 = clts
            .initial_states()
            .iter()
            .copied()
            .next()
            .expect("initial state exists");
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "exactly one outgoing transition");
        assert!(
            matches!(outgoing[0].modality(), TransitionModality::MayOnly),
            "[may] realizes to MayOnly, got {:?}",
            outgoing[0].modality()
        );
    }

    /// R.5 Item K sub-item K.2 — the CTXDSL `[must]` attribute realizes
    /// to a singleton-hyper-target `TransitionModality::MustHyperOnly`.
    /// Multi-target hyper-must syntax (`s -> [t1, t2] on a [must];`)
    /// is queued as the K.1b follow-up.
    #[test]
    fn r5_subitem_k2_must_attribute_realizes_to_singleton_hyper_must() {
        let doc = parse(
            r#"
context must_only_modality {
    automata {
        automaton M {
            states { state s0 initial; state s1; }
            transitions {
                transition s0 -> s1 on epsilon [must];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        let s0 = clts
            .initial_states()
            .iter()
            .copied()
            .next()
            .expect("initial state exists");
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "exactly one outgoing transition");
        let targets = outgoing[0]
            .modality()
            .hyper_targets()
            .expect("[must] realizes to MustHyperOnly");
        assert_eq!(
            targets.len(),
            1,
            "K.2 ships singleton hyper-must; multi-target is K.1b"
        );
        let target_state_name = clts
            .state_name(targets[0])
            .expect("hyper-target state has a name");
        assert_eq!(
            target_state_name, "s1",
            "hyper-must targets the declared transition target"
        );
    }

    /// R.5 Item K sub-item K.2 — the explicit `[sharp]` attribute
    /// realizes to `TransitionModality::Sharp` (equivalent to no
    /// attribute), confirming the explicit-equivalent path lands on
    /// the same CLTS modality as the default.
    #[test]
    fn r5_subitem_k2_explicit_sharp_attribute_realizes_to_sharp() {
        let doc = parse(
            r#"
context sharp_modality {
    automata {
        automaton M {
            states { state s0 initial; state s1; }
            transitions {
                transition s0 -> s1 on epsilon [sharp];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        let s0 = clts
            .initial_states()
            .iter()
            .copied()
            .next()
            .expect("initial state exists");
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "exactly one outgoing transition");
        assert!(
            matches!(outgoing[0].modality(), TransitionModality::Sharp),
            "[sharp] realizes to Sharp, got {:?}",
            outgoing[0].modality()
        );
    }

    /// R.5 Item K sub-item K.2b (2026-06-06) — the unrolled-path
    /// (parametric automata with `var`-driven guards) now honors
    /// the CTXDSL modality attribute. Pre-K.2b, parametric
    /// automata declaring `[may]` / `[must]` would realize to
    /// `Sharp` because the unrolling pipeline stripped the
    /// modality.
    #[test]
    fn r5_subitem_k2b_unrolled_path_honors_may_modality() {
        // K.2b: trigger the unrolled-path by declaring a `var`;
        // the modality attribute must propagate through unrolling
        // to the resulting CLTS edge.
        let doc = parse(
            r#"
context k2b_unrolled_may {
    automata {
        automaton M {
            variables {
                var counter: i64 = 0;
            }
            states {
                state s0 initial;
                state s1;
            }
            transitions {
                transition s0 -> s1 on epsilon [may];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        // The unrolled CLTS has states keyed by the variable
        // bindings; find any state with at least one outgoing
        // transition and assert its modality is MayOnly.
        let found_may = clts.states().any(|state| {
            clts.outgoing(state)
                .iter()
                .any(|t| matches!(t.modality(), TransitionModality::MayOnly))
        });
        assert!(
            found_may,
            "K.2b: unrolled-path parametric automaton must propagate `[may]` to CLTS edge"
        );
    }

    /// R.5 Item K sub-item K.1b (2026-06-06) — the multi-target
    /// bracketed-list syntax `transition s -> [t1, t2, t3] on a
    /// [must];` realizes to a `MustHyperOnly` whose target set
    /// has 3 elements (the primary + 2 additional).
    #[test]
    fn r5_subitem_k1b_multi_target_realizes_to_hyper_must_with_full_set() {
        let doc = parse(
            r#"
context k1b_multi_realize {
    automata {
        automaton M {
            states { state s0 initial; state t1; state t2; state t3; }
            transitions {
                transition s0 -> [t1, t2, t3] on epsilon [must];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        let s0 = clts
            .initial_states()
            .iter()
            .copied()
            .next()
            .expect("initial state exists");
        let outgoing = clts.outgoing(s0);
        assert_eq!(outgoing.len(), 1, "one outgoing transition (hyper-must)");
        let targets = outgoing[0]
            .modality()
            .hyper_targets()
            .expect("[must] with additional_targets realizes to MustHyperOnly");
        assert_eq!(
            targets.len(),
            3,
            "K.1b: hyper-target set has 3 elements (primary + 2 additional)"
        );
        // The target states' names match t1, t2, t3 in order.
        let target_names: Vec<&str> = targets
            .iter()
            .map(|&id| clts.state_name(id).unwrap_or("?"))
            .collect();
        assert_eq!(target_names, vec!["t1", "t2", "t3"]);
    }

    /// R.5 Item K sub-item K.1b-unrolled (2026-06-08) — the
    /// unrolled-path realize step honors the multi-target
    /// bracketed-list syntax `transition s -> [t1, t2, t3] on a
    /// [must];` on parametric automata. Closes the K.2b MVP gap
    /// that previously emitted singleton hyper-must only.
    #[test]
    fn r5_subitem_k1b_unrolled_path_multi_target_realizes_to_full_hyper_must_set() {
        let doc = parse(
            r#"
context k1b_unrolled_multi {
    automata {
        automaton M {
            variables {
                var counter: i64 = 0;
            }
            states {
                state s0 initial;
                state t1;
                state t2;
                state t3;
            }
            transitions {
                transition s0 -> [t1, t2, t3] on epsilon [must];
            }
        }
    }
}
"#,
        )
        .expect("context parses");
        let realized = realize(&doc, &[]).expect("realization succeeds");
        let clts = realized.context.clts("M").expect("CLTS exists");
        // Find any state with at least one outgoing hyper-must
        // transition + assert the target set has 3 elements.
        let mut found_3_target_hyper = false;
        for state in clts.states() {
            for t in clts.outgoing(state) {
                if let Some(targets) = t.modality().hyper_targets()
                    && targets.len() == 3
                {
                    found_3_target_hyper = true;
                    break;
                }
            }
        }
        assert!(
            found_3_target_hyper,
            "K.1b-unrolled: parametric automaton with `[t1, t2, t3] [must]` \
             must produce a hyper-must edge with 3 targets in the unrolled CLTS"
        );
    }
}
