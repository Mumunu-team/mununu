//! `verify.toml` schema — the user-facing entry point to the general
//! verification framework.
//!
//! A `verify.toml` declares:
//! - `[project]` — name + optional description.
//! - `[[sources]]` — N typed sources (id + adapter + files + options).
//! - `[alphabet]` — how labels across sources synchronise.
//! - `[composition]` — composition semantics + members + name.
//! - `[[properties]]` — properties to verify.
//!
//! Example:
//!
//! ```toml
//! [project]
//! name = "uart_codesign"
//!
//! [[sources]]
//! id = "fw"
//! adapter = "c-codesign"
//! files = ["firmware/firmware.c"]
//! [sources.options]
//! include_paths = ["firmware/include"]
//! defines = ["F_CPU=64000000"]
//! cmsis_stubs = true
//! register_map = "register_map.json"
//! synthesize_automaton = true
//!
//! [[sources]]
//! id = "periph"
//! adapter = "ctxdsl"
//! files = ["spec/uart_protocol.ctxdsl"]
//!
//! [alphabet]
//! strategy = "direct"
//!
//! [composition]
//! semantics = "asynchronous"
//! members = ["fw", "periph"]
//! name = "System"
//!
//! [[properties]]
//! name = "no_double_start"
//! template = "label_blocked_in_state"
//! args = { STATE = "fw.Transmitting", LABEL = "wr_ctrl_tx_start" }
//! over = "System"
//! ```
//!
//! ## Validation
//!
//! [`VerifyConfig::validate`] returns a `Vec<ConfigIssue>` describing
//! every structural problem found, so the orchestrator can surface a
//! single report instead of failing the pipeline mid-step. The
//! orchestrator refuses to proceed when validation returns a
//! non-empty vector.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Parsed `verify.toml` document.
///
/// Does not derive `Eq` because `SourceSection.options` holds
/// `toml::Value`s, which carry floating-point literals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyConfig {
    /// `[project]` block.
    pub project: ProjectSection,
    /// `[[sources]]` array — one entry per source model.
    #[serde(default)]
    pub sources: Vec<SourceSection>,
    /// `[alphabet]` block — alphabet-binding strategy. Optional;
    /// defaults to `{ strategy = "direct" }` when omitted.
    #[serde(default)]
    pub alphabet: AlphabetSection,
    /// `[composition]` block.
    pub composition: CompositionSection,
    /// `[[properties]]` array.
    #[serde(default)]
    pub properties: Vec<PropertySection>,
    /// mununu#595 — `[[assumptions]]` array: named environment assumptions (fairness
    /// constraints) a property may be verified UNDER. Declared once here, referenced by name
    /// from `[[properties]].assumptions`. See [`AssumptionSection`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<AssumptionSection>,
    /// R4W-3 (R.4 clustered-COI wiring) — optional Jaccard similarity
    /// floor for the clustered-COI comparison the BTOR2 bit-blaster
    /// reports on each source's `PartitionSummary.cluster_coi`. `None`
    /// (the default) → the recommended `0.5`. Settable directly in
    /// `verify.toml` (`cluster_similarity_floor = 0.7`), and overridden
    /// by the CLI `mununu verify --cluster-coi-floor <F>` flag or the
    /// API verify request's `cluster_similarity_floor` field. Tighter
    /// floors (→ `1.0`) approach per-property COI; looser (→ `0.0`)
    /// collapse toward joint COI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_similarity_floor: Option<f64>,
    /// mununu#594 — the counterexample witness's step cap (`counterexample_max_steps = 50`
    /// in `verify.toml`), overridden by `mununu verify --counterexample-max-steps <N>` or the
    /// API verify request's `counterexample_max_steps`. `None` → 20, the historical default.
    /// The cap bounds the WALK; on the safety shape `nu X. (phi && [] X)` the witness is a
    /// shortest path to a `!phi` state and rarely needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterexample_max_steps: Option<usize>,
}

/// `[project]` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSection {
    /// Project name. Used in the report header and as a default
    /// for `[composition].name` (`<project.name>System`).
    pub name: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Opt-in (`[project] safety_cube = true`): also run the KMTS 3-valued safety cube
    /// (`AG ¬bad`, [`crate::adapter::recoverability::verify_safety_scalable`] —
    /// enumeration + the emergent-K interpolation discovery) on every `btor2`-adapter
    /// source that carries a `bad` obligation, reporting the per-source verdict in
    /// [`crate::verify::report::VerifyReport::safety_cube_results`] alongside the
    /// mu-calculus property verdicts. Default `false`. A best-effort pass: a source with
    /// no `bad` node (or a parse error) is silently skipped.
    #[serde(default)]
    pub safety_cube: bool,
}

/// One entry in the `[[sources]]` array.
///
/// Each source has a unique `id` (referenced by `[composition].members`
/// and by property `over` targets), an `adapter` name dispatched
/// through the existing `FormatAdapter` registry, a list of `files`
/// to feed into the adapter, and an `options` map containing
/// adapter-specific configuration.
///
/// Does not derive `Eq` because `options` holds `toml::Value`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSection {
    /// Stable identifier for this source. Must be a valid CTXDSL
    /// identifier (alphanumeric + underscore, starting with a letter
    /// or underscore). Referenced from `[composition].members` and
    /// from property `over` targets.
    pub id: String,
    /// Adapter name. Dispatched through the registry assembled in
    /// A2.4. Recognised values today (subject to A2 evolution):
    /// `"c-codesign"` (firmware C via `codesign::c_extract_llvm`),
    /// `"sv-yosys"` / `"yosys"` (SystemVerilog via sv2v→Yosys→BTOR2 —
    /// the sole SV route since S.2b; `multi_module = true` opts into
    /// top-netlist composition), `"ctxdsl"` (raw
    /// hand-authored automaton), `"xstate"`, `"crewai"` (CrewAI
    /// agentic JSON), `"langgraph"` (LangGraph StateGraph JSON),
    /// `"microcode"` (restricted JSON microcode form — plan Part 5.5),
    /// `"extraction"`, `"tlsf"`, `"aiger"`, `"btor2"`, `"promela"`.
    pub adapter: String,
    /// Source files to feed the adapter. Order is significant for
    /// adapters that accept multiple files; for single-file adapters
    /// the orchestrator will iterate and merge outputs (see A2.4).
    pub files: Vec<PathBuf>,
    /// Adapter-specific options as a free-form TOML table. The
    /// orchestrator passes this to the adapter dispatcher which is
    /// responsible for mapping individual keys onto each adapter's
    /// strongly-typed options struct (e.g. `LlvmExtractOptions`,
    /// `AdapterOptions`, …). Schema validation of inner keys happens
    /// inside the adapter; this layer only catches well-formedness.
    #[serde(default)]
    pub options: BTreeMap<String, toml::Value>,
    /// Instance count for parameterised expansion. When `>= 2`, the
    /// orchestrator expands this `[[sources]]` entry into `count`
    /// virtual sources named `<id>_0`, `<id>_1`, …, `<id>_<count-1>`.
    /// Each virtual instance's file content has `{instance_id}`
    /// occurrences substituted with `<id>_<i>` *before* the adapter
    /// sees the content, so the emitted automaton names and state
    /// names stay unique per instance.
    ///
    /// `None` (or `1`) means "single instance" — no expansion, no
    /// placeholder substitution, full backwards compatibility with
    /// existing fixtures.
    ///
    /// To reference parameterised instances from `[composition].members`,
    /// use either the wildcard form `<id>.*` (expands to every
    /// instance) or a specific instance id `<id>_0`. The validator
    /// accepts both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Declarative posture marker for sources that model memory
    /// regions (chaotic stubs, tracked-memory templates, ad-hoc
    /// memory CTXDSL). When set, the validator and the `mununu
    /// memory check` audit (Stream B2b) cross-reference the
    /// declared posture against property templates to surface
    /// soundness mismatches (e.g. liveness properties under a
    /// chaotic stub — Doc C §C.5: optimistic).
    ///
    /// Plan Stream B2a / `docs/abstraction.md` memory section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_abstraction: Option<MemoryAbstractionPosture>,
}

/// Declarative posture marker for memory-modelling sources. Each
/// field is optional so the user can ship only the constraints they
/// care about; missing fields default to the legacy "no posture
/// declared" behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryAbstractionPosture {
    /// One of `"chaotic"`, `"tracked_addresses"`, `"tracked_with_values"`,
    /// or `"full_concrete"`. See `docs/abstraction.md` § Memory.
    pub kind: String,
    /// Tracked memory region identifiers. Only meaningful for
    /// `kind = "tracked_addresses"` or `"tracked_with_values"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked: Vec<String>,
    /// Symbol set the source uses for per-address state. Only
    /// meaningful for `kind = "tracked_with_values"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub value_symbol_set: Vec<String>,
    /// One of `"global_barrier"` (default), `"release_acquire"`, or
    /// `"rvwmo"`. The verify framework currently always models
    /// fences as global barriers; declaring this field lets future
    /// tooling (Stream B2b's audit) flag weak-memory-sensitive
    /// property templates with a `WeakMemoryUnmodelled` warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fence_semantics: Option<String>,
    /// Free-form note. Surfaced in `mununu memory check` output and
    /// the `--print-alphabet` introspection report. Useful for
    /// pointing readers at the docs section that justifies the
    /// posture choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Valid `MemoryAbstractionPosture.kind` values.
pub const VALID_MEMORY_ABSTRACTION_KINDS: &[&str] = &[
    "chaotic",
    "tracked_addresses",
    "tracked_with_values",
    "full_concrete",
];

/// Valid `MemoryAbstractionPosture.fence_semantics` values.
pub const VALID_FENCE_SEMANTICS: &[&str] = &["global_barrier", "release_acquire", "rvwmo"];

/// `[alphabet]` block — how labels across sources synchronise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlphabetSection {
    /// Binding strategy. `"direct"` (default), `"renamings"`, or
    /// `"register_map"`.
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// For `strategy = "renamings"`: list of `{ from, to }` entries.
    /// Each `from` is `"<source_id>.<local_label>"`; each `to` is
    /// the canonical name the orchestrator rewrites it to before
    /// composition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renamings: Vec<Renaming>,
    /// For `strategy = "register_map"`: path to a register-map JSON
    /// sidecar. The orchestrator derives renamings from the sidecar's
    /// `sv_signal` / `c_accessor` fields via
    /// `codesign::coupling::rendezvous_label_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub register_map: Option<PathBuf>,
    /// When `true`, peripheral-only labels (labels on the
    /// peripheral-side source not used by the firmware-side source)
    /// are tolerated instead of treated as a reconcile-gate failure.
    /// Mirrors `mununu codesign reconcile-labels
    /// --allow-peripheral-superset`.
    #[serde(default)]
    pub allow_peripheral_superset: bool,
}

impl Default for AlphabetSection {
    fn default() -> Self {
        Self {
            strategy: default_strategy(),
            renamings: Vec::new(),
            register_map: None,
            allow_peripheral_superset: false,
        }
    }
}

/// Single entry in `[alphabet].renamings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renaming {
    /// `<source_id>.<local_label>` qualified reference.
    pub from: String,
    /// Canonical label the orchestrator rewrites `from` to.
    pub to: String,
}

/// `[composition]` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionSection {
    /// `"synchronous"`, `"asynchronous"`, or `"superset"`. Mirrors
    /// `composition::CompositionSemantics`.
    pub semantics: String,
    /// Source IDs to compose. Each must match a `[[sources]].id`.
    /// Order is preserved for the iterative left-associative
    /// `compose(compose(a, b), c)` walk the realiser performs.
    pub members: Vec<String>,
    /// Composition name. Used as the default `over` target for
    /// properties that don't supply one. Defaults to
    /// `<project.name>System` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One entry in the `[[properties]]` array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertySection {
    /// Property name. Must be unique within the config.
    pub name: String,
    /// Template ID. Mutually exclusive with `formula`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Raw mu-calculus formula. Mutually exclusive with `template`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// Template arguments. Keys match the template's parameter names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
    /// Automaton or composition to evaluate over. Defaults to
    /// `[composition].name` (or the implicit default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
    /// mununu#595 — names of `[[assumptions]]` this property is verified UNDER. Only the
    /// fairness templates (`fair_response`, `fair_always_eventually`) take assumptions, and
    /// they require at least one — with none they are their unconditional cousins, and the
    /// validator refuses that spelling so a bare `holds` cannot pass for a conditional one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

/// mununu#595 — one `[[assumptions]]` entry: a named environment assumption, i.e. an
/// unconditional fairness constraint every considered path must satisfy infinitely often.
///
/// ```toml
/// [[assumptions]]
/// name = "env_progress"
/// kind = "edge"          # "state" | "edge" | "edges" | "weak"
/// atom = "go"            # state: a state predicate; edge / weak: a label
/// # labels = ["a", "b"]  # edges: any one of these labels, infinitely often
/// ```
///
/// The encoding each kind expands to is in [`super::fairness`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssumptionSection {
    /// Unique name, referenced from `[[properties]].assumptions`.
    pub name: String,
    /// `"state"` (`GF P`), `"edge"` (`GF ⟨l⟩`), `"edges"` (`GF ⟨l₁⟩ ∨ … ∨ ⟨lₙ⟩`) or `"weak"`
    /// (weak fairness of `l`: taken infinitely often or disabled infinitely often).
    pub kind: String,
    /// The state predicate (`state`) or the label (`edge`, `weak`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom: Option<String>,
    /// The labels (`edges`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

impl AssumptionSection {
    /// The encoder's view of this entry, or `None` when it is malformed (the validator reports
    /// the malformation; the orchestrator never sees one).
    pub fn to_assumption(&self) -> Option<super::fairness::Assumption> {
        use super::fairness::Assumption;
        match self.kind.as_str() {
            "state" => self.atom.clone().map(|atom| Assumption::State { atom }),
            "edge" => self.atom.clone().map(|label| Assumption::Edge { label }),
            "weak" => self.atom.clone().map(|label| Assumption::Weak { label }),
            "edges" if !self.labels.is_empty() => Some(Assumption::Edges {
                labels: self.labels.clone(),
            }),
            _ => None,
        }
    }
}

/// mununu#595 — the template ids that take assumptions.
pub const FAIR_TEMPLATES: &[&str] = &["fair_response", "fair_always_eventually"];

fn default_strategy() -> String {
    "direct".to_string()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Issues surfaced by [`VerifyConfig::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ConfigIssue {
    /// `[project].name` was empty.
    EmptyProjectName,
    /// `[[sources]]` was empty (no sources to verify).
    NoSources,
    /// `[[sources]]` entry had an empty `id`.
    EmptySourceId { index: usize },
    /// `[[sources]]` entry's `id` is not a valid CTXDSL identifier.
    InvalidSourceId { id: String },
    /// Two `[[sources]]` entries share the same `id`.
    DuplicateSourceId(String),
    /// `[[sources]]` entry had an empty `adapter` field.
    EmptyAdapter { source_id: String },
    /// `[[sources]]` entry had an empty `files` list.
    SourceNoFiles { source_id: String },
    /// `[[sources]]` entry had `count = 0` — meaningless. Use `count
    /// = 1` (or omit the field) for a single instance, `>= 2` for
    /// parameterised expansion.
    SourceCountZero { source_id: String },
    /// `[[sources]]` entry had a `memory_abstraction.kind` value
    /// outside the allowed set (`chaotic`, `tracked_addresses`,
    /// `tracked_with_values`, `full_concrete`).
    MemoryAbstractionInvalidKind { source_id: String, kind: String },
    /// `[[sources]]` entry had a `memory_abstraction.fence_semantics`
    /// value outside the allowed set (`global_barrier`,
    /// `release_acquire`, `rvwmo`).
    MemoryAbstractionInvalidFenceSemantics {
        source_id: String,
        fence_semantics: String,
    },
    /// `memory_abstraction.tracked` is non-empty but
    /// `kind = "chaotic"` doesn't track individual addresses.
    MemoryAbstractionTrackedWithoutTracking { source_id: String, kind: String },
    /// `memory_abstraction.value_symbol_set` is non-empty but
    /// `kind` is not `tracked_with_values` — symbol sets only
    /// apply when tracking per-address state.
    MemoryAbstractionValuesWithoutTracking { source_id: String, kind: String },
    /// `[alphabet].strategy` is not one of `direct` / `renamings` /
    /// `register_map`.
    UnknownAlphabetStrategy(String),
    /// `[alphabet]` strategy is `renamings` but the `renamings` list
    /// is empty.
    RenamingsStrategyWithoutEntries,
    /// `[alphabet]` strategy is `renamings` and a `from` field is
    /// malformed (not `<source_id>.<label>` shape).
    MalformedRenamingFrom { from: String },
    /// `[alphabet]` strategy is `renamings` and a `from` field
    /// references a source ID that doesn't exist.
    RenamingUnknownSource { source_id: String, from: String },
    /// `[alphabet]` strategy is `register_map` but no
    /// `register_map` path is set.
    RegisterMapStrategyWithoutPath,
    /// `[composition].semantics` is not one of `synchronous` /
    /// `asynchronous` / `superset`.
    UnknownCompositionSemantics(String),
    /// `[composition].members` is empty.
    CompositionNoMembers,
    /// `[composition].members` references an unknown source ID.
    CompositionUnknownMember { id: String },
    /// `[[properties]]` entry had both `template` and `formula` set,
    /// or neither.
    PropertyFormulaXorViolation {
        name: String,
        has_template: bool,
        has_formula: bool,
    },
    /// `[[properties]]` entry had an empty `name`.
    EmptyPropertyName,
    /// Two `[[properties]]` entries share the same `name`.
    DuplicatePropertyName(String),
    /// mununu#595 — `[[assumptions]]` entry had an empty `name`.
    EmptyAssumptionName,
    /// mununu#595 — two `[[assumptions]]` entries share the same `name`.
    DuplicateAssumptionName(String),
    /// mununu#595 — an `[[assumptions]]` entry's `kind` / `atom` / `labels` do not fit together.
    MalformedAssumption { name: String, kind: String },
    /// mununu#595 — a property references an assumption no `[[assumptions]]` entry declares.
    PropertyUnknownAssumption {
        property: String,
        assumption: String,
    },
    /// mununu#595 — a property lists assumptions but is not a fairness template.
    PropertyAssumptionsNotApplicable { property: String },
    /// mununu#595 — a fairness template with no assumptions (its unconditional cousin exists).
    FairTemplateWithoutAssumptions { property: String, template: String },
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigIssue::EmptyProjectName => write!(f, "[project].name is empty"),
            ConfigIssue::NoSources => {
                write!(f, "[[sources]] is empty; at least one source is required")
            }
            ConfigIssue::EmptySourceId { index } => {
                write!(f, "[[sources]] entry at index {index} has empty `id`")
            }
            ConfigIssue::InvalidSourceId { id } => write!(
                f,
                "[[sources]] entry `id = \"{id}\"` is not a valid CTXDSL identifier (must match [A-Za-z_][A-Za-z0-9_]*)"
            ),
            ConfigIssue::DuplicateSourceId(id) => {
                write!(f, "[[sources]]: duplicate source id `{id}`")
            }
            ConfigIssue::EmptyAdapter { source_id } => {
                write!(f, "[[sources]] `{source_id}` has empty `adapter`")
            }
            ConfigIssue::SourceCountZero { source_id } => write!(
                f,
                "[[sources]] `{source_id}` has `count = 0` (use 1 or omit for single-instance; >= 2 for parameterised expansion)"
            ),
            ConfigIssue::MemoryAbstractionInvalidKind { source_id, kind } => write!(
                f,
                "[[sources]] `{source_id}`: memory_abstraction.kind = `{kind}` is not one of {}",
                VALID_MEMORY_ABSTRACTION_KINDS.join(" | ")
            ),
            ConfigIssue::MemoryAbstractionInvalidFenceSemantics {
                source_id,
                fence_semantics,
            } => write!(
                f,
                "[[sources]] `{source_id}`: memory_abstraction.fence_semantics = `{fence_semantics}` is not one of {}",
                VALID_FENCE_SEMANTICS.join(" | ")
            ),
            ConfigIssue::MemoryAbstractionTrackedWithoutTracking { source_id, kind } => write!(
                f,
                "[[sources]] `{source_id}`: memory_abstraction.tracked is non-empty but kind = `{kind}` does not track individual addresses (use `tracked_addresses` or `tracked_with_values`)"
            ),
            ConfigIssue::MemoryAbstractionValuesWithoutTracking { source_id, kind } => write!(
                f,
                "[[sources]] `{source_id}`: memory_abstraction.value_symbol_set is non-empty but kind = `{kind}` does not declare per-address values (use `tracked_with_values`)"
            ),
            ConfigIssue::SourceNoFiles { source_id } => write!(
                f,
                "[[sources]] `{source_id}` has empty `files` list; at least one path is required"
            ),
            ConfigIssue::UnknownAlphabetStrategy(s) => write!(
                f,
                "[alphabet].strategy = \"{s}\" is not recognised (valid: \"direct\", \"renamings\", \"register_map\")"
            ),
            ConfigIssue::RenamingsStrategyWithoutEntries => write!(
                f,
                "[alphabet].strategy = \"renamings\" but the `renamings` list is empty"
            ),
            ConfigIssue::MalformedRenamingFrom { from } => write!(
                f,
                "[alphabet].renamings: `from = \"{from}\"` is malformed (expected \"<source_id>.<label>\")"
            ),
            ConfigIssue::RenamingUnknownSource { source_id, from } => write!(
                f,
                "[alphabet].renamings: `from = \"{from}\"` references unknown source id `{source_id}`"
            ),
            ConfigIssue::RegisterMapStrategyWithoutPath => write!(
                f,
                "[alphabet].strategy = \"register_map\" but `register_map` path is not set"
            ),
            ConfigIssue::UnknownCompositionSemantics(s) => write!(
                f,
                "[composition].semantics = \"{s}\" is not recognised (valid: \"synchronous\", \"asynchronous\", \"superset\")"
            ),
            ConfigIssue::CompositionNoMembers => write!(
                f,
                "[composition].members is empty; at least one member is required"
            ),
            ConfigIssue::CompositionUnknownMember { id } => {
                write!(f, "[composition].members: unknown source id `{id}`")
            }
            ConfigIssue::PropertyFormulaXorViolation {
                name,
                has_template,
                has_formula,
            } => {
                if *has_template && *has_formula {
                    write!(
                        f,
                        "[[properties]] `{name}`: both `template` and `formula` are set; exactly one is required"
                    )
                } else {
                    write!(
                        f,
                        "[[properties]] `{name}`: neither `template` nor `formula` is set; exactly one is required"
                    )
                }
            }
            ConfigIssue::EmptyPropertyName => write!(f, "[[properties]] entry has empty `name`"),
            ConfigIssue::DuplicatePropertyName(n) => {
                write!(f, "[[properties]]: duplicate property name `{n}`")
            }
            ConfigIssue::EmptyAssumptionName => {
                write!(f, "[[assumptions]] entry has empty `name`")
            }
            ConfigIssue::DuplicateAssumptionName(n) => {
                write!(f, "[[assumptions]]: duplicate assumption name `{n}`")
            }
            ConfigIssue::MalformedAssumption { name, kind } => write!(
                f,
                "[[assumptions]] `{name}`: kind = \"{kind}\" needs `atom` (state / edge / weak) or a \
                 non-empty `labels` (edges); valid kinds: \"state\", \"edge\", \"edges\", \"weak\""
            ),
            ConfigIssue::PropertyUnknownAssumption {
                property,
                assumption,
            } => write!(
                f,
                "[[properties]] `{property}`: unknown assumption `{assumption}` (declare it under [[assumptions]])"
            ),
            ConfigIssue::PropertyAssumptionsNotApplicable { property } => write!(
                f,
                "[[properties]] `{property}`: `assumptions` apply only to the fairness templates \
                 (`fair_response`, `fair_always_eventually`); an inline `formula` encodes its own \
                 fairness (see `verify::fairness`)"
            ),
            ConfigIssue::FairTemplateWithoutAssumptions { property, template } => write!(
                f,
                "[[properties]] `{property}`: template `{template}` needs `assumptions = [...]` — with \
                 none it is its unconditional cousin (`response` / `always_eventually`), use that"
            ),
        }
    }
}

const VALID_STRATEGIES: &[&str] = &["direct", "renamings", "register_map"];
const VALID_SEMANTICS: &[&str] = &["synchronous", "asynchronous", "superset"];

impl VerifyConfig {
    /// Parse a TOML document.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Run all structural checks. Empty vector ⇔ config is well-formed.
    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();

        if self.project.name.is_empty() {
            issues.push(ConfigIssue::EmptyProjectName);
        }

        // Sources
        if self.sources.is_empty() {
            issues.push(ConfigIssue::NoSources);
        }
        let mut seen_source_ids: HashSet<String> = HashSet::new();
        for (index, source) in self.sources.iter().enumerate() {
            if source.id.is_empty() {
                issues.push(ConfigIssue::EmptySourceId { index });
            } else if !is_valid_identifier(&source.id) {
                issues.push(ConfigIssue::InvalidSourceId {
                    id: source.id.clone(),
                });
            }
            if !seen_source_ids.insert(source.id.clone()) {
                issues.push(ConfigIssue::DuplicateSourceId(source.id.clone()));
            }
            if source.adapter.is_empty() {
                issues.push(ConfigIssue::EmptyAdapter {
                    source_id: source.id.clone(),
                });
            }
            if source.files.is_empty() {
                issues.push(ConfigIssue::SourceNoFiles {
                    source_id: source.id.clone(),
                });
            }
            // Memory-abstraction posture: validate kind + fence_semantics
            // against the allowed sets, plus a couple of consistency
            // checks between fields. Soundness audits (liveness under
            // chaotic, weak-memory-sensitive templates) are handled
            // by `mununu memory check` (Stream B2b), not here — this
            // layer is strictly schema correctness.
            if let Some(ma) = source.memory_abstraction.as_ref() {
                if !VALID_MEMORY_ABSTRACTION_KINDS.contains(&ma.kind.as_str()) {
                    issues.push(ConfigIssue::MemoryAbstractionInvalidKind {
                        source_id: source.id.clone(),
                        kind: ma.kind.clone(),
                    });
                }
                if let Some(fs) = ma.fence_semantics.as_deref()
                    && !VALID_FENCE_SEMANTICS.contains(&fs)
                {
                    issues.push(ConfigIssue::MemoryAbstractionInvalidFenceSemantics {
                        source_id: source.id.clone(),
                        fence_semantics: fs.to_string(),
                    });
                }
                if !ma.tracked.is_empty()
                    && ma.kind != "tracked_addresses"
                    && ma.kind != "tracked_with_values"
                {
                    issues.push(ConfigIssue::MemoryAbstractionTrackedWithoutTracking {
                        source_id: source.id.clone(),
                        kind: ma.kind.clone(),
                    });
                }
                if !ma.value_symbol_set.is_empty() && ma.kind != "tracked_with_values" {
                    issues.push(ConfigIssue::MemoryAbstractionValuesWithoutTracking {
                        source_id: source.id.clone(),
                        kind: ma.kind.clone(),
                    });
                }
            }
            // Parameterisation: `count = N` expands to N instances
            // `<id>_0` .. `<id>_<N-1>`. count == 0 is meaningless;
            // count == 1 is identical to omitting the field.
            if let Some(c) = source.count {
                if c == 0 {
                    issues.push(ConfigIssue::SourceCountZero {
                        source_id: source.id.clone(),
                    });
                }
                if c >= 2 {
                    // Register every expanded instance id as a
                    // visible source for composition-member checks.
                    for i in 0..c {
                        let instance_id = format!("{}_{}", source.id, i);
                        if !seen_source_ids.insert(instance_id.clone()) {
                            issues.push(ConfigIssue::DuplicateSourceId(instance_id));
                        }
                    }
                }
            }
        }

        // Alphabet
        if !VALID_STRATEGIES.contains(&self.alphabet.strategy.as_str()) {
            issues.push(ConfigIssue::UnknownAlphabetStrategy(
                self.alphabet.strategy.clone(),
            ));
        }
        match self.alphabet.strategy.as_str() {
            "renamings" => {
                if self.alphabet.renamings.is_empty() {
                    issues.push(ConfigIssue::RenamingsStrategyWithoutEntries);
                }
                for r in &self.alphabet.renamings {
                    match parse_renaming_from(&r.from) {
                        Some((source_id, _label)) => {
                            if !seen_source_ids.contains(source_id) {
                                issues.push(ConfigIssue::RenamingUnknownSource {
                                    source_id: source_id.to_string(),
                                    from: r.from.clone(),
                                });
                            }
                        }
                        None => {
                            issues.push(ConfigIssue::MalformedRenamingFrom {
                                from: r.from.clone(),
                            });
                        }
                    }
                }
            }
            "register_map" if self.alphabet.register_map.is_none() => {
                issues.push(ConfigIssue::RegisterMapStrategyWithoutPath);
            }
            _ => {} // "direct" / "register_map" with path set / unknown — no extra checks here
        }

        // Composition
        if !VALID_SEMANTICS.contains(&self.composition.semantics.as_str()) {
            issues.push(ConfigIssue::UnknownCompositionSemantics(
                self.composition.semantics.clone(),
            ));
        }
        if self.composition.members.is_empty() {
            issues.push(ConfigIssue::CompositionNoMembers);
        }
        for m in &self.composition.members {
            // Wildcard expansion: `<src>.*` resolves to every automaton
            // emitted by `<src>`. The validator only enforces source
            // existence; the assembler does the expansion.
            let bare = m.strip_suffix(".*").unwrap_or(m.as_str());
            if !seen_source_ids.contains(bare) {
                issues.push(ConfigIssue::CompositionUnknownMember { id: m.clone() });
            }
        }

        // Properties
        let mut seen_property_names: HashSet<String> = HashSet::new();
        for p in &self.properties {
            if p.name.is_empty() {
                issues.push(ConfigIssue::EmptyPropertyName);
            }
            let has_template = p.template.is_some();
            let has_formula = p.formula.is_some();
            if has_template == has_formula {
                issues.push(ConfigIssue::PropertyFormulaXorViolation {
                    name: p.name.clone(),
                    has_template,
                    has_formula,
                });
            }
            if !p.name.is_empty() && !seen_property_names.insert(p.name.clone()) {
                issues.push(ConfigIssue::DuplicatePropertyName(p.name.clone()));
            }
            // mununu#595 — assumptions: declared, and only where they mean something.
            let is_fair_template = p
                .template
                .as_deref()
                .is_some_and(|t| FAIR_TEMPLATES.contains(&t));
            for a in &p.assumptions {
                if !self.assumptions.iter().any(|d| &d.name == a) {
                    issues.push(ConfigIssue::PropertyUnknownAssumption {
                        property: p.name.clone(),
                        assumption: a.clone(),
                    });
                }
            }
            if !p.assumptions.is_empty() && !is_fair_template {
                issues.push(ConfigIssue::PropertyAssumptionsNotApplicable {
                    property: p.name.clone(),
                });
            }
            if is_fair_template && p.assumptions.is_empty() {
                issues.push(ConfigIssue::FairTemplateWithoutAssumptions {
                    property: p.name.clone(),
                    template: p.template.clone().unwrap_or_default(),
                });
            }
        }

        // mununu#595 — [[assumptions]]: named, unique, well-formed.
        let mut seen_assumption_names: HashSet<String> = HashSet::new();
        for a in &self.assumptions {
            if a.name.is_empty() {
                issues.push(ConfigIssue::EmptyAssumptionName);
            } else if !seen_assumption_names.insert(a.name.clone()) {
                issues.push(ConfigIssue::DuplicateAssumptionName(a.name.clone()));
            }
            if a.to_assumption().is_none() {
                issues.push(ConfigIssue::MalformedAssumption {
                    name: a.name.clone(),
                    kind: a.kind.clone(),
                });
            }
        }

        issues
    }

    /// Resolve a property's `over` target, falling back to the
    /// composition name (or its implicit default).
    pub fn resolve_over(&self, p: &PropertySection) -> String {
        if let Some(o) = p.over.as_ref() {
            return o.clone();
        }
        self.composition_name()
    }

    /// Return the composition name (explicit if set, otherwise the
    /// project-derived default `<project.name>System`).
    pub fn composition_name(&self) -> String {
        self.composition
            .name
            .clone()
            .unwrap_or_else(|| format!("{}System", self.project.name))
    }
}

/// Parse `"<source_id>.<label>"`. Returns `None` if there's no dot or
/// either side is empty.
fn parse_renaming_from(s: &str) -> Option<(&str, &str)> {
    let (sid, label) = s.split_once('.')?;
    if sid.is_empty() || label.is_empty() {
        return None;
    }
    Some((sid, label))
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_DIRECT: &str = r#"
[project]
name = "two_xstate_machines"

[[sources]]
id = "a"
adapter = "xstate"
files = ["a.xstate.json"]

[[sources]]
id = "b"
adapter = "xstate"
files = ["b.xstate.json"]

[alphabet]
strategy = "direct"

[composition]
semantics = "synchronous"
members = ["a", "b"]
name = "Pair"

[[properties]]
name = "no_deadlock"
template = "no_deadlock"
"#;

    const VALID_RENAMINGS: &str = r#"
[project]
name = "renamed_pair"

[[sources]]
id = "left"
adapter = "ctxdsl"
files = ["left.ctxdsl"]

[[sources]]
id = "right"
adapter = "ctxdsl"
files = ["right.ctxdsl"]

[[alphabet.renamings]]
from = "left.go"
to = "shared.go"

[[alphabet.renamings]]
from = "right.start"
to = "shared.go"

[alphabet]
strategy = "renamings"

[composition]
semantics = "asynchronous"
members = ["left", "right"]

[[properties]]
name = "reachable"
formula = "mu X. (Init || <> X)"
over = "Pair"
"#;

    const VALID_REGISTER_MAP: &str = r#"
[project]
name = "uart_codesign"

[[sources]]
id = "fw"
adapter = "c-codesign"
files = ["firmware.c"]

[[sources]]
id = "periph"
adapter = "sv-yosys"
files = ["uart.sv"]

[alphabet]
strategy = "register_map"
register_map = "register_map.json"
allow_peripheral_superset = true

[composition]
semantics = "asynchronous"
members = ["fw", "periph"]
name = "UARTSystem"

[[properties]]
name = "init_reachable"
template = "reachable"
over = "UARTSystem"
"#;

    #[test]
    fn parses_direct_strategy_config() {
        let cfg = VerifyConfig::from_toml(VALID_DIRECT).expect("valid TOML");
        assert_eq!(cfg.project.name, "two_xstate_machines");
        assert_eq!(cfg.sources.len(), 2);
        assert_eq!(cfg.sources[0].id, "a");
        assert_eq!(cfg.alphabet.strategy, "direct");
        assert_eq!(cfg.composition.semantics, "synchronous");
        assert_eq!(cfg.composition_name(), "Pair");
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn parses_renamings_strategy_config() {
        let cfg = VerifyConfig::from_toml(VALID_RENAMINGS).expect("valid TOML");
        assert_eq!(cfg.alphabet.strategy, "renamings");
        assert_eq!(cfg.alphabet.renamings.len(), 2);
        assert_eq!(cfg.alphabet.renamings[0].from, "left.go");
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn parses_register_map_strategy_config() {
        let cfg = VerifyConfig::from_toml(VALID_REGISTER_MAP).expect("valid TOML");
        assert_eq!(cfg.alphabet.strategy, "register_map");
        assert!(cfg.alphabet.allow_peripheral_superset);
        assert!(cfg.alphabet.register_map.is_some());
        assert_eq!(cfg.composition.semantics, "asynchronous");
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn alphabet_section_defaults_to_direct_when_omitted() {
        let toml_src = r#"
[project]
name = "minimal"

[[sources]]
id = "only"
adapter = "ctxdsl"
files = ["only.ctxdsl"]

[composition]
semantics = "synchronous"
members = ["only"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        assert_eq!(cfg.alphabet.strategy, "direct");
        assert!(cfg.alphabet.renamings.is_empty());
        assert!(cfg.alphabet.register_map.is_none());
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn rejects_empty_project_name() {
        let toml_src = r#"
[project]
name = ""

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::EmptyProjectName))
        );
    }

    #[test]
    fn rejects_empty_sources_list() {
        let toml_src = r#"
[project]
name = "x"

[composition]
semantics = "synchronous"
members = []
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(i, ConfigIssue::NoSources)));
    }

    #[test]
    fn rejects_invalid_source_id() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "1bad"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["1bad"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::InvalidSourceId { id } if id == "1bad"))
        );
    }

    #[test]
    fn rejects_duplicate_source_id() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "dup"
adapter = "x"
files = ["a"]

[[sources]]
id = "dup"
adapter = "y"
files = ["b"]

[composition]
semantics = "synchronous"
members = ["dup"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::DuplicateSourceId(s) if s == "dup"))
        );
    }

    #[test]
    fn rejects_source_with_no_files() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = []

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::SourceNoFiles { source_id } if source_id == "a"))
        );
    }

    #[test]
    fn rejects_unknown_alphabet_strategy() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[alphabet]
strategy = "labels-by-vibes"

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(
            |i| matches!(i, ConfigIssue::UnknownAlphabetStrategy(s) if s == "labels-by-vibes")
        ));
    }

    #[test]
    fn rejects_renamings_strategy_without_entries() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[alphabet]
strategy = "renamings"

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::RenamingsStrategyWithoutEntries))
        );
    }

    #[test]
    fn rejects_renaming_with_malformed_from() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[[alphabet.renamings]]
from = "no_dot_here"
to = "canonical"

[alphabet]
strategy = "renamings"

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(
            |i| matches!(i, ConfigIssue::MalformedRenamingFrom { from } if from == "no_dot_here")
        ));
    }

    #[test]
    fn rejects_renaming_with_unknown_source() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[[alphabet.renamings]]
from = "ghost.label"
to = "canonical"

[alphabet]
strategy = "renamings"

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::RenamingUnknownSource { source_id, .. } if source_id == "ghost"))
        );
    }

    #[test]
    fn rejects_register_map_strategy_without_path() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[alphabet]
strategy = "register_map"

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::RegisterMapStrategyWithoutPath))
        );
    }

    #[test]
    fn rejects_unknown_composition_semantics() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "lockstep"
members = ["a"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues.iter().any(
                |i| matches!(i, ConfigIssue::UnknownCompositionSemantics(s) if s == "lockstep")
            )
        );
    }

    #[test]
    fn rejects_composition_with_unknown_member() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a", "ghost"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues.iter().any(
                |i| matches!(i, ConfigIssue::CompositionUnknownMember { id } if id == "ghost")
            )
        );
    }

    #[test]
    fn rejects_property_with_both_template_and_formula() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a"]

[[properties]]
name = "p"
template = "reachable"
formula = "true"
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::PropertyFormulaXorViolation {
                has_template: true,
                has_formula: true,
                ..
            }
        )));
    }

    #[test]
    fn rejects_property_with_neither_template_nor_formula() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a"]

[[properties]]
name = "p"
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::PropertyFormulaXorViolation {
                has_template: false,
                has_formula: false,
                ..
            }
        )));
    }

    #[test]
    fn rejects_duplicate_property_names() {
        let toml_src = r#"
[project]
name = "x"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a"]

[[properties]]
name = "p"
formula = "true"

[[properties]]
name = "p"
template = "reachable"
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ConfigIssue::DuplicatePropertyName(n) if n == "p"))
        );
    }

    #[test]
    fn resolve_over_falls_back_to_composition_name_then_project_default() {
        let cfg = VerifyConfig::from_toml(VALID_DIRECT).unwrap();
        // Explicit `over` wins.
        let mut p = PropertySection {
            name: "x".to_string(),
            template: Some("no_deadlock".to_string()),
            formula: None,
            args: BTreeMap::new(),
            over: Some("Custom".to_string()),
            assumptions: Vec::new(),
        };
        assert_eq!(cfg.resolve_over(&p), "Custom");
        // Missing `over` falls back to composition.name (set in VALID_DIRECT).
        p.over = None;
        assert_eq!(cfg.resolve_over(&p), "Pair");

        // When composition.name is omitted, fall back to `<project>System`.
        let toml_src = r#"
[project]
name = "demo"

[[sources]]
id = "a"
adapter = "x"
files = ["a"]

[composition]
semantics = "synchronous"
members = ["a"]
"#;
        let cfg2 = VerifyConfig::from_toml(toml_src).unwrap();
        assert_eq!(cfg2.composition_name(), "demoSystem");
    }

    // ---- memory_abstraction posture (Stream B2a) -------------------

    const MEMORY_ABSTRACTION_VALID: &str = r#"
[project]
name = "MemAbsValid"

[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory.ctxdsl"]
memory_abstraction = { kind = "tracked_addresses", tracked = ["mem_x", "mem_y"], fence_semantics = "global_barrier", notes = "tracked-only" }

[composition]
semantics = "asynchronous"
members = ["memory"]
"#;

    #[test]
    fn memory_abstraction_valid_kind_passes() {
        let cfg = VerifyConfig::from_toml(MEMORY_ABSTRACTION_VALID).unwrap();
        let issues = cfg.validate();
        assert!(issues.is_empty(), "got issues: {issues:?}");
        let ma = cfg.sources[0].memory_abstraction.as_ref().unwrap();
        assert_eq!(ma.kind, "tracked_addresses");
        assert_eq!(ma.tracked, vec!["mem_x", "mem_y"]);
        assert_eq!(ma.fence_semantics.as_deref(), Some("global_barrier"));
    }

    #[test]
    fn memory_abstraction_invalid_kind_is_caught() {
        let toml_src = r#"
[project]
name = "Bad"
[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory.ctxdsl"]
memory_abstraction = { kind = "wat" }
[composition]
semantics = "asynchronous"
members = ["memory"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::MemoryAbstractionInvalidKind { source_id, kind }
                if source_id == "memory" && kind == "wat"
        )));
    }

    #[test]
    fn memory_abstraction_invalid_fence_semantics_is_caught() {
        let toml_src = r#"
[project]
name = "BadFence"
[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory.ctxdsl"]
memory_abstraction = { kind = "tracked_addresses", fence_semantics = "vibes" }
[composition]
semantics = "asynchronous"
members = ["memory"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::MemoryAbstractionInvalidFenceSemantics { source_id, .. }
                if source_id == "memory"
        )));
    }

    #[test]
    fn memory_abstraction_tracked_without_tracking_kind_is_caught() {
        let toml_src = r#"
[project]
name = "MismatchedTracked"
[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory.ctxdsl"]
memory_abstraction = { kind = "chaotic", tracked = ["mem_x"] }
[composition]
semantics = "asynchronous"
members = ["memory"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::MemoryAbstractionTrackedWithoutTracking { source_id, kind }
                if source_id == "memory" && kind == "chaotic"
        )));
    }

    #[test]
    fn memory_abstraction_values_without_tracked_with_values_is_caught() {
        let toml_src = r#"
[project]
name = "MismatchedValues"
[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory.ctxdsl"]
memory_abstraction = { kind = "tracked_addresses", tracked = ["x"], value_symbol_set = ["Initial", "Written"] }
[composition]
semantics = "asynchronous"
members = ["memory"]
"#;
        let cfg = VerifyConfig::from_toml(toml_src).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| matches!(
            i,
            ConfigIssue::MemoryAbstractionValuesWithoutTracking { source_id, kind }
                if source_id == "memory" && kind == "tracked_addresses"
        )));
    }

    #[test]
    fn memory_abstraction_omitted_is_legacy_safe() {
        // Sources without memory_abstraction stay valid — backwards-
        // compatible with every shipped fixture.
        let cfg = VerifyConfig::from_toml(VALID_DIRECT).unwrap();
        let issues = cfg.validate();
        assert!(issues.is_empty(), "got: {issues:?}");
        assert!(cfg.sources[0].memory_abstraction.is_none());
    }

    #[test]
    fn memory_abstraction_round_trips_via_serde() {
        let cfg = VerifyConfig::from_toml(MEMORY_ABSTRACTION_VALID).unwrap();
        let toml_text = toml::to_string(&cfg).unwrap();
        let reparsed = VerifyConfig::from_toml(&toml_text).unwrap();
        assert_eq!(cfg, reparsed);
    }

    #[test]
    fn cluster_similarity_floor_defaults_to_none_and_parses_when_set() {
        // R4W-3 — the optional top-level `cluster_similarity_floor`
        // defaults to None (legacy manifests unaffected) and round-trips
        // when present. The key is top-level, so it must precede any
        // table header in the TOML source.
        let default_cfg = VerifyConfig::from_toml(VALID_DIRECT).unwrap();
        assert!(
            default_cfg.cluster_similarity_floor.is_none(),
            "absent field must default to None"
        );

        let with_floor = r#"
cluster_similarity_floor = 0.7

[project]
name = "floored"

[[sources]]
id = "a"
adapter = "xstate"
files = ["a.xstate.json"]

[composition]
semantics = "synchronous"
members = ["a"]
name = "Solo"
"#;
        let cfg = VerifyConfig::from_toml(with_floor).expect("parse with floor");
        assert_eq!(cfg.cluster_similarity_floor, Some(0.7));

        // Serde round-trip preserves it.
        let reparsed = VerifyConfig::from_toml(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed.cluster_similarity_floor, Some(0.7));
    }

    /// mununu#595 — `[[assumptions]]` and `[[properties]].assumptions`: every malformation the
    /// orchestrator must never see, each named, plus a well-formed config that round-trips.
    #[test]
    fn x595_assumptions_are_validated_by_name_kind_and_applicability() {
        let base = VALID_DIRECT.replace(
            "[[properties]]\nname = \"no_deadlock\"\ntemplate = \"no_deadlock\"\n",
            "",
        );
        let cfg = VerifyConfig::from_toml(&format!(
            r#"{base}
[[assumptions]]
name = "fair_go"
kind = "edge"
atom = "go"
[[assumptions]]
name = "fair_go"
kind = "state"
[[assumptions]]
name = ""
kind = "edges"
labels = ["a", "b"]
[[assumptions]]
name = "odd"
kind = "strong"
atom = "x"
[[properties]]
name = "ok"
template = "fair_response"
args = {{ TRIGGER = "Req", RESPONSE = "Ack" }}
assumptions = ["fair_go"]
[[properties]]
name = "unknown_ref"
template = "fair_always_eventually"
args = {{ TARGET = "Idle" }}
assumptions = ["nobody"]
[[properties]]
name = "not_applicable"
template = "response"
args = {{ TRIGGER = "Req", RESPONSE = "Ack" }}
assumptions = ["fair_go"]
[[properties]]
name = "bare_fair"
template = "fair_always_eventually"
args = {{ TARGET = "Idle" }}
"#
        ))
        .unwrap();
        let issues = cfg.validate();
        let has = |want: &ConfigIssue| issues.iter().any(|i| i == want);
        assert!(has(&ConfigIssue::DuplicateAssumptionName("fair_go".into())));
        assert!(
            has(&ConfigIssue::MalformedAssumption {
                name: "fair_go".into(),
                kind: "state".into()
            }),
            "a `state` entry needs `atom`"
        );
        assert!(has(&ConfigIssue::EmptyAssumptionName));
        assert!(has(&ConfigIssue::MalformedAssumption {
            name: "odd".into(),
            kind: "strong".into()
        }));
        assert!(has(&ConfigIssue::PropertyUnknownAssumption {
            property: "unknown_ref".into(),
            assumption: "nobody".into()
        }));
        assert!(has(&ConfigIssue::PropertyAssumptionsNotApplicable {
            property: "not_applicable".into()
        }));
        assert!(has(&ConfigIssue::FairTemplateWithoutAssumptions {
            property: "bare_fair".into(),
            template: "fair_always_eventually".into()
        }));
        // `ok` raises nothing of its own.
        assert!(
            !issues.iter().any(|i| format!("{i}").contains("`ok`")),
            "{issues:?}"
        );
        // Every issue renders.
        for i in &issues {
            assert!(!i.to_string().is_empty());
        }
        // The encoder's view of each well-formed kind, and the round-trip.
        let a = &cfg.assumptions[0];
        assert_eq!(
            a.to_assumption(),
            Some(super::super::fairness::Assumption::Edge { label: "go".into() })
        );
        assert_eq!(
            cfg.assumptions[2].to_assumption(),
            Some(super::super::fairness::Assumption::Edges {
                labels: vec!["a".into(), "b".into()]
            })
        );
        let reparsed = VerifyConfig::from_toml(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed.assumptions, cfg.assumptions);
        assert_eq!(
            reparsed.properties[0].assumptions,
            vec!["fair_go".to_string()]
        );
    }
}
