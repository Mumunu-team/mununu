//! Report + error types for the verify orchestrator (A2.4).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

/// mununu#594 — the historical witness cap: enough for the shipped fixtures' violations without
/// a wall of state names, and the floor a `LengthLimit` is read against.
pub const DEFAULT_COUNTEREXAMPLE_MAX_STEPS: usize = 20;

fn default_counterexample_max_steps() -> usize {
    DEFAULT_COUNTEREXAMPLE_MAX_STEPS
}

/// Top-level result of [`crate::verify::orchestrator::verify_project`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    /// Project name from the config.
    pub project: String,
    /// One summary per source declared in the config.
    pub sources: Vec<SourceSummary>,
    /// Composition shape used for evaluation.
    pub composition: CompositionInfo,
    /// One verdict per `[[properties]]` entry.
    pub property_verdicts: Vec<PropertyVerdict>,
    /// mununu#594 — the step cap every counterexample witness in this report was built under
    /// (`counterexample_max_steps` in the config, the CLI flag or the API field; 20 by
    /// default). A `LengthLimit` termination is relative to it.
    #[serde(default = "default_counterexample_max_steps")]
    pub counterexample_max_steps: usize,
    /// Per-source KMTS safety-cube (`AG ¬bad`) verdicts — populated only when
    /// `[project].safety_cube = true` (or the config field `safety_cube`). Empty
    /// otherwise. See [`crate::verify::config::ProjectSection::safety_cube`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_cube_results: Vec<SafetyCubeResult>,
}

/// One safety-cube result from the opt-in `safety_cube` pass.
///
/// A `btor2` source yields one result (its single `bad` obligation, `property = None`).
/// An `sv-yosys` source yields one result **per SVA assertion** — its `property` is the
/// slang-extracted assertion name — since the slang front-end parses each `assert
/// property` into its own cube obligation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyCubeResult {
    /// `[[sources]].id` the cube ran on.
    pub source_id: String,
    /// The source file (relative to the project base dir) the cube read/lifted.
    pub file: String,
    /// The SVA assertion name for an `sv-yosys` source; `None` for a `btor2` source
    /// (a single anonymous `bad` obligation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// The verdict string — `"holds"` | `"violated"` | `"unknown"` (a `btor2` source's
    /// `verify_safety_scalable` verdict, or an `sv-yosys` property's `verify_auto`
    /// outcome, which additionally uses `"skipped"` for a non-cube-bindable atom).
    pub verdict: String,
}

/// Per-source diagnostic information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSummary {
    /// `[[sources]].id` from the config.
    pub id: String,
    /// `[[sources]].adapter` from the config.
    pub adapter: String,
    /// Resolved automaton name used in the composition (post
    /// `AutomatonDiscovery`).
    pub automaton: Option<String>,
    /// Auto-partition telemetry (Phase A.3 step 3.6) — present for
    /// adapters that ran the cone-of-influence pass during translation
    /// (currently SV / BTOR2; extraction is preview-only). `None` for
    /// adapters that did not run the partition (xstate, ctxdsl, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_summary: Option<crate::adapter::partition::PartitionSummary>,
}

/// Composition shape used for evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionInfo {
    /// `synchronous` / `asynchronous` / `superset`.
    pub semantics: String,
    /// Resolved composition name.
    pub name: String,
    /// Automaton names (post member resolution) that participate in
    /// the composition.
    pub members: Vec<String>,
}

/// IR-track P3.1 (2026-06-22) — a 3-valued { KleeneT, KleeneF,
/// KleeneBot } cell-count summary. Same shape as the cegar
/// `CegarVerdictSummary` so the UI can reuse the `Trit` renderer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreeValuedSummary {
    /// States with a definite KleeneT (satisfying) verdict.
    pub true_cells: usize,
    /// States with a definite KleeneF (violating) verdict.
    pub false_cells: usize,
    /// States with an indefinite KleeneBot (⊥) verdict — "needs
    /// refinement". Always 0 on the 2-valued bit-blast verify path
    /// (the explicit-state model is exact); populated by the
    /// predicate-cube path (IR-track P3.3).
    pub unknown_cells: usize,
}

/// Verdict for one property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyVerdict {
    /// `[[properties]].name`.
    pub name: String,
    /// How the formula was sourced (template instantiation vs.
    /// inline mu-calculus).
    pub formula_source: PropertyFormulaSource,
    /// Concrete mu-calculus text the verifier evaluated.
    pub formula: String,
    /// Target automaton/composition name.
    pub over: String,
    /// `true` if every initial state of the target satisfies the
    /// formula. Mirrors the CLI verdict convention.
    pub satisfied: bool,
    /// Total number of states in the target automaton/composition.
    pub total_states: usize,
    /// Number of states satisfying the formula.
    pub satisfying_states: usize,
    /// Initial-state names.
    pub initial_states: Vec<String>,
    /// Subset of `initial_states` that satisfy the formula.
    pub initial_satisfying: Vec<String>,
    /// IR-track P3.1 (2026-06-22) — 3-valued { T, F, ⊥ } summary over
    /// the **initial** states. On the 2-valued bit-blast verify path
    /// the model is exact, so `unknown_cells` is 0 and this mirrors
    /// `satisfied` (T = satisfying, F = not). The predicate-cube path
    /// (P3.3) populates `unknown_cells` with genuine KleeneBot cells.
    /// `#[serde(default)]` so pre-P3 reports still deserialize.
    #[serde(default)]
    pub initial_verdict_summary: ThreeValuedSummary,
    /// Witness path from a violating initial state, when the
    /// verdict is unsatisfied and a witness can be constructed. The
    /// orchestrator emits this opportunistically — `None` is also
    /// emitted for satisfied verdicts and when the walk hits a
    /// degenerate case (no violating initials, no outgoing
    /// transitions from violating initials). See [`TraceWitness`]
    /// for the trace shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<TraceWitness>,
}

/// A short witness path from a violating initial state to either a
/// sink, a cycle, or a length-capped step in the composed
/// automaton. The trace is **not** a counterstrategy — it is a
/// straightforward forward walk through the composition, biased
/// toward visiting states that violate the property formula. For
/// safety properties this surfaces "the system can reach this bad
/// state via these transitions"; for reachability properties this
/// surfaces "no path from here reaches the target".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceWitness {
    /// Initial composed-state name where the trace begins.
    pub initial_state: String,
    /// Sequence of transitions taken. `steps[i]` records the label
    /// fired and the state entered.
    pub steps: Vec<TraceStep>,
    /// Why the trace stopped.
    pub termination: TraceTermination,
    /// mununu#594 — on the safety shape `nu X. (phi && [] X)`, the `!phi` state this witness
    /// leads to (the shortest path's end), named even when the printed steps were cut at the
    /// cap, so a truncated trace still says where it was going. `None` on the other shapes,
    /// where the walk has no target to name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub violating_state: Option<String>,
}

/// One transition in a [`TraceWitness`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceStep {
    /// Label payload of the fired transition. A single CTXDSL
    /// `transition s -> t on label a, label b;` produces one entry
    /// joining the labels with `,`.
    pub label: String,
    /// Composed-state name entered after firing this transition.
    pub successor_state: String,
}

/// Reason a [`TraceWitness`] stopped at a particular step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TraceTermination {
    /// The composed-state reached has no outgoing transitions.
    Sink,
    /// The walk revisited a state already in the trace; the
    /// `return_to_step` index is the step where the cycle joins
    /// back. `0` means the cycle closes onto the initial state.
    Cycle { return_to_step: usize },
    /// The orchestrator's length cap (`VerifyReport::counterexample_max_steps`, 20 by
    /// default) was reached before any other terminator fired.
    LengthLimit,
}

/// How a property's formula was sourced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyFormulaSource {
    /// Inline mu-calculus in the config (no template).
    Inline,
    /// Instantiated from a property template.
    Template {
        /// Template ID.
        id: String,
        /// Arguments bound to the template's parameters.
        args: BTreeMap<String, String>,
    },
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors raised by [`crate::verify::orchestrator::verify_project`].
#[derive(Debug)]
pub enum VerifyError {
    /// `VerifyConfig::validate` returned non-empty. The orchestrator
    /// refuses to proceed.
    ConfigValidationFailed(Vec<crate::verify::config::ConfigIssue>),
    /// `AlphabetBinding::from_config` raised an error (e.g.
    /// register-map sidecar not readable).
    AlphabetBinding(crate::verify::binding::BindingError),
    /// `assemble_unified_ctxdsl` raised an error.
    Assemble(crate::verify::assemble::AssembleError),
    /// An I/O error while reading a source file.
    SourceReadFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A source's `adapter` name isn't recognised by the dispatcher.
    UnknownAdapter { source_id: String, adapter: String },
    /// The dispatched adapter failed to translate the source.
    AdapterTranslationFailed {
        source_id: String,
        adapter: String,
        message: String,
    },
    /// A property's `template_ref` failed to instantiate (unknown id,
    /// missing required arg, etc.).
    TemplateInstantiationFailed { property: String, message: String },
    /// The assembled CTXDSL document failed to parse. Indicates a bug
    /// in the assembler or an adapter emitting non-parseable output.
    AssembledCtxdslParseFailed { message: String, snippet: String },
    /// `realize_documents` rejected the assembled document.
    RealizeFailed { message: String },
    /// A property names an `over` automaton/composition that the
    /// realised context doesn't expose.
    UnknownAutomaton {
        property: String,
        over: String,
        known: Vec<String>,
    },
    /// `mu_calculus::evaluate_with_options` returned an error.
    EvaluationFailed { property: String, message: String },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::ConfigValidationFailed(issues) => {
                writeln!(f, "verify config has {} validation issue(s):", issues.len())?;
                for i in issues {
                    writeln!(f, "  - {i}")?;
                }
                Ok(())
            }
            VerifyError::AlphabetBinding(e) => write!(f, "alphabet-binding setup failed: {e}"),
            VerifyError::Assemble(e) => write!(f, "CTXDSL assembly failed: {e}"),
            VerifyError::SourceReadFailed { path, source } => {
                write!(f, "failed to read source file {}: {source}", path.display())
            }
            VerifyError::UnknownAdapter { source_id, adapter } => write!(
                f,
                "source `{source_id}`: unknown adapter `{adapter}` (orchestrator dispatch doesn't know how to call this adapter yet)"
            ),
            VerifyError::AdapterTranslationFailed {
                source_id,
                adapter,
                message,
            } => write!(
                f,
                "source `{source_id}` adapter `{adapter}` failed to translate: {message}"
            ),
            VerifyError::TemplateInstantiationFailed { property, message } => write!(
                f,
                "property `{property}`: template instantiation failed: {message}"
            ),
            VerifyError::AssembledCtxdslParseFailed { message, .. } => write!(
                f,
                "assembled CTXDSL failed to parse: {message}\nthis is a bug in the assembler or an adapter emitting non-parseable output"
            ),
            VerifyError::RealizeFailed { message } => {
                write!(f, "context realization failed: {message}")
            }
            VerifyError::UnknownAutomaton {
                property,
                over,
                known,
            } => write!(
                f,
                "property `{property}`: `over = \"{over}\"` is not declared in the assembled context (known: {})",
                known.join(", ")
            ),
            VerifyError::EvaluationFailed { property, message } => {
                write!(
                    f,
                    "property `{property}`: μ-calculus evaluation failed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for VerifyError {}
