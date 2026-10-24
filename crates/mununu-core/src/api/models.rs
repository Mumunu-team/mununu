use serde::{Deserialize, Serialize};

pub use crate::trace::{
    CexCellView, TestbenchView, Trace, TraceFault, TraceKind, TraceReplay, TraceStep, cex_cells,
};
use std::collections::BTreeMap;

// ============================================================================
// Common Types
// ============================================================================

/// File content with optional name
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct FileContent {
    pub name: String,
    pub content: String,
}

/// Sidecar file content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SidecarFile {
    pub name: String,
    pub content: String,
}

// ============================================================================
// Context Summarize Endpoints
// ============================================================================

/// Request for context summarization
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct ContextSummarizeRequest {
    pub context: FileContent,
    #[serde(default)]
    pub sidecars: Vec<SidecarFile>,
    #[serde(default = "default_summarize_format")]
    pub format: SummarizeFormat,
}

fn default_summarize_format() -> SummarizeFormat {
    SummarizeFormat::Json
}

/// Summary output format
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SummarizeFormat {
    Json,
    Table,
}

/// Response from context summarization
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ContextSummarizeResponse {
    pub success: bool,
    pub summary: ContextSummary,
}

/// Context summary information
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContextSummary {
    pub context_name: String,
    pub automata: Vec<AutomatonSummary>,
    pub formulas_count: usize,
    pub controllers_count: usize,
    #[serde(default)]
    pub controllers: Vec<ControllerSummary>,
}

/// Automaton summary information
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AutomatonSummary {
    pub name: String,
    pub states_count: usize,
    pub transitions_count: usize,
}

/// Controller summary information
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ControllerSummary {
    pub name: String,
    pub source: String,
    pub formula: String,
    pub realizable: bool,
    pub states_count: usize,
    pub transitions_count: usize,
}

// ============================================================================
// Context Synthesize Endpoints
// ============================================================================

/// Request for controller synthesis
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct ContextSynthesizeRequest {
    pub context: FileContent,
    #[serde(default)]
    pub sidecars: Vec<SidecarFile>,
    pub automaton: String,
    /// Formula name to synthesise (mutually exclusive with `template_ref`).
    pub formula: Option<String>,
    /// Template reference to instantiate (mutually exclusive with `formula`).
    #[serde(default)]
    pub template_ref: Option<crate::adapter::templates::TemplateRef>,
    #[serde(default)]
    pub options: SynthesisOptions,
}

/// Synthesis options
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SynthesisOptions {
    #[serde(default)]
    pub minimize: bool,
    #[serde(default)]
    pub diagnostics: DiagnosticsOptions,
    /// Legacy positional-strategy flag. When `controller_mode` is set, that
    /// takes precedence and `extract_strategy` is ignored. Kept for
    /// backwards compatibility.
    #[serde(default)]
    pub extract_strategy: bool,
    /// Controller extraction mode. Case-insensitive. One of:
    /// `"projection"` (default), `"functional"`, `"permissive"`,
    /// `"signature-memory"`, `"product-game"`, `"parity-game"`.
    /// When `Some`, overrides `extract_strategy`.
    #[serde(default)]
    pub controller_mode: Option<String>,
    /// Output format for the controller: "ctxdsl" (default), "xstate", "systemverilog".
    pub output_format: Option<String>,
}

/// Diagnostics options
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct DiagnosticsOptions {
    #[serde(default)]
    pub counterexample: bool,
    #[serde(default)]
    pub counterstrategy: bool,
    #[serde(default)]
    pub deadlock_traces: bool,
    pub max_counter_traces: Option<u32>,
}

/// Response from controller synthesis
#[derive(Debug, Serialize, PartialEq)]
pub struct ContextSynthesizeResponse {
    pub success: bool,
    /// The verdict envelope: `holds` iff a controller exists (`realizable`), else `violated`.
    pub outcome: String,
    pub realizable: bool,
    pub controller: Option<FileContent>,
    /// Controller in the requested native format (xstate, systemverilog).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_native: Option<FileContent>,
    pub diagnostics: SynthesisDiagnostics,
    /// Counterstrategy graph for unrealizable cases (environment's winning strategy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterstrategy: Option<CounterstrategyResult>,
}

/// Request for sound GR(1) controller synthesis from an LTL assume/guarantee
/// spec (TLSF today). Unlike `/context/synthesize`, this runs the sound GR(1)
/// pipeline (`ControllerMode::Gr1`) directly on the structured LTL spec.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Gr1SynthesizeRequest {
    /// The source spec.
    pub context: FileContent,
    /// Adapter to interpret the source (defaults to `tlsf`).
    #[serde(default)]
    pub adapter: Option<String>,
    /// Module name for the emitted controller (defaults to `gr1_controller`).
    #[serde(default)]
    pub module: Option<String>,
}

/// Response from GR(1) controller synthesis.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Gr1SynthesizeResponse {
    /// The verdict envelope: `holds` iff the spec is realizable, else `violated`.
    pub outcome: String,
    /// Whether the spec is realizable (sound GR(1) verdict).
    pub realizable: bool,
    /// The synthesized controller as SystemVerilog, if realizable and a strategy
    /// was extracted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_sv: Option<String>,
    /// Number of game states (env + ctrl + BAD).
    pub game_states: usize,
    /// Number of monitor bits in the game state.
    pub monitor_bits: usize,
    /// Human-readable notes (e.g. unsupported multi-guarantee memory).
    pub notes: Vec<String>,
}

/// Synthesis diagnostics (matches ControllerDiagnostics structure)
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct SynthesisDiagnostics {
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(default)]
    pub violating_initials: Vec<String>,
    /// The minimal counterexample path (a finite [`Trace`] over state names).
    pub counterexample: Option<Trace>,
    #[serde(default)]
    pub counterstrategy_traces: Vec<Trace>,
    #[serde(default)]
    pub deadlock_traces: Vec<Trace>,
    pub minimization: Option<MinimizationReport>,
    #[serde(default)]
    pub proof_obligations: Vec<ProofObligation>,
    /// Lasso traces for liveness counterexamples (`lasso` [`Trace`]s with labels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lasso_traces: Vec<Trace>,
}

/// Minimization report
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MinimizationReport {
    pub removed_states: usize,
    pub removed_transitions: usize,
    #[serde(default)]
    pub merged_states: Vec<String>,
}

/// Proof obligation
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProofObligation {
    pub state: String,
    pub detail: Option<String>,
}

// ============================================================================
// Context Import Endpoint
// ============================================================================

/// Request for importing an external format into CTXDSL.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextImportRequest {
    /// Raw file content in the source format.
    pub content: String,
    /// Source format hint: "auto", "tlsf", "aiger", "btor2", "promela", "xstate",
    /// "systemverilog" (hand-written SV adapter), "sv-yosys" (Yosys-driven SV
    /// elaboration → BTOR2 → CLTS — Phase 1 RTL roadmap), "extraction".
    #[serde(default = "default_import_format")]
    pub format: String,
    /// Original filename (used for extension-based detection if format is "auto").
    pub filename: Option<String>,
    /// Optional sidecar content (.mununu.json for SV, .espec.json for extraction).
    /// When provided, the adapter uses this for abstraction/property configuration.
    #[serde(default)]
    pub sidecar: Option<String>,
    /// Additional source files (for multi-module SV compositions).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// When `format == "sv-yosys"`, opt in to the `sv2v` preprocessor pass
    /// before Yosys elaboration. Required for modern open-source SV
    /// dialects (SV2009/2012 module-header `import pkg::*;` etc.) that
    /// Yosys's built-in `read_verilog -sv` parser does not accept.
    /// Requires `sv2v` (zachjs/sv2v) on `$PATH` or in `MUNUNU_SV2V_PATH`.
    /// Mirrors the CLI's `--preprocessor sv2v` flag. Ignored by all
    /// other formats.
    #[serde(default)]
    pub use_sv2v: bool,
    /// R.6.7 / V.6 (2026-06-09) — predicate set for the
    /// controllability-aware predicate-cube lift. Each entry is a
    /// `{name, register, value}` triple identifying a register-value
    /// equality predicate that bounds the abstraction. When non-empty
    /// AND `controllable_inputs` is non-empty AND `format == "btor2"`
    /// (or `"sv-yosys"`, which produces BTOR2 internally), the
    /// `predicate_cube_lift` is invoked + the resulting KMTS is
    /// returned. When empty, the import path is unchanged
    /// (legacy CTXDSL emit).
    ///
    /// Mirrors the CLI's `--predicate [NAME:]REG == VALUE` flag on
    /// `mununu btor2 cegar`.
    #[serde(default)]
    pub predicates: Vec<PredicateSpecRequest>,
    /// R.6.7 / V.6 (2026-06-09) — names of BTOR2 input symbols the
    /// controller drives. Mirrors the CLI's `--controllable`
    /// flag. When non-empty AND `predicates` is non-empty, opts the
    /// import path into the R.6.6 controllability-aware lift —
    /// boolean inputs are partitioned into env (uncontrollable) +
    /// ctrl (controllable) classes per-symbol-name + the lift emits
    /// per-combo dual-label transitions.
    #[serde(default)]
    pub controllable: Vec<String>,

    /// R-S2b.6 / P3 (§Phase 11 slot-3 close follow-up, 2026-06-12)
    /// — filesystem path to the original SystemVerilog source for
    /// the BTOR2 input. Mirrors the CLI's `--sv-source <PATH>`
    /// flag. When set AND the supplied `sidecar` declares a
    /// `simulate_reset` block AND a Verilator binary is
    /// discoverable on the server, the BTOR2 bit-blaster runs a
    /// short concrete reset simulation and feeds post-reset
    /// register valuations into the EnumValues discriminators
    /// (§Phase 9 R-S2b strategy). Default `None` preserves the
    /// legacy "no reset simulation" behaviour.
    #[serde(default)]
    pub sv_source_path: Option<String>,

    /// R-S6.6 / P3 (§Phase 11 slot-3 close follow-up, 2026-06-12)
    /// — filesystem path to the sidecar JSON file. Mirrors the
    /// CLI's `--sidecar <PATH>` flag. When set, the bit-blaster's
    /// `apply_vcd_trace_seeding` orchestration uses this path's
    /// parent directory to resolve relative `vcd_traces` entries
    /// declared in the sidecar (§Phase 9 R-S6 strategy). The
    /// `sidecar` field above carries the file's CONTENT; this
    /// field carries the file's PATH so the bit-blaster can
    /// resolve relative paths within the sidecar's directory
    /// scope. Default `None` — relative VCD paths emit an
    /// AdapterWarning and fall through.
    #[serde(default)]
    pub sidecar_path: Option<String>,
}

/// R.6.7 / V.6 (2026-06-09) — predicate-spec request shape. Mirrors
/// the CLI's `--predicate [NAME:]REG == VALUE` triple format. Bridges to
/// [`crate::adapter::btor2::kmts_lift::PredicateSpec`].
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct PredicateSpecRequest {
    /// Human-readable predicate name (e.g. `"burst_zero"`).
    pub name: String,
    /// BTOR2 register symbol the predicate is anchored on.
    pub register: String,
    /// Integer value the predicate witnesses (`register == value`).
    pub value: u64,
}

fn default_import_format() -> String {
    "auto".to_string()
}

/// Response from importing an external format.
#[derive(Debug, Serialize, PartialEq)]
pub struct ContextImportResponse {
    pub success: bool,
    /// Translated CTXDSL content.
    pub ctxdsl: String,
    /// Detected source format.
    pub source_format: String,
    /// Translation warnings.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Metadata about the translated source.
    pub signal_count: usize,
    pub state_count: usize,
    pub property_count: usize,
    /// State valuations for structured predicate matching (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_valuations: Option<serde_json::Value>,
    /// Per-transition Mealy observations (when the adapter emits them).
    /// Keyed by automaton name; each entry is a list of
    /// `{source, target, labels, observations}` rows. Used by trace renderers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_observations: Option<serde_json::Value>,
}

// ============================================================================
// Context Graphs Endpoints
// ============================================================================

/// Request for graph generation
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct ContextGraphsRequest {
    pub context: FileContent,
    #[serde(default)]
    pub sidecars: Vec<SidecarFile>,
    pub automaton: Option<String>,
    #[serde(default = "default_graph_types")]
    pub graph_types: Vec<GraphType>,
    #[serde(default)]
    pub include_controllers: bool,
    /// Override the minimize setting for controller graphs.
    pub minimize_controllers: Option<bool>,
}

fn default_graph_types() -> Vec<GraphType> {
    vec![GraphType::Dsl]
}

/// Graph type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GraphType {
    Dsl,
    Unrolled,
}

/// Response from graph generation
#[derive(Debug, Serialize, PartialEq)]
pub struct ContextGraphsResponse {
    pub success: bool,
    pub context: ContextSummary,
    pub graphs: Vec<GraphData>,
}

/// Graph data with metadata
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphData {
    pub automaton: String,
    pub graph_type: GraphTypeResponse,
    pub elements: Vec<GraphElement>,
    pub metadata: GraphMetadata,
}

/// Graph type in response (serialized as string)
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GraphTypeResponse {
    Dsl,
    Unrolled,
    Controller,
}

impl From<GraphType> for GraphTypeResponse {
    fn from(gt: GraphType) -> Self {
        match gt {
            GraphType::Dsl => GraphTypeResponse::Dsl,
            GraphType::Unrolled => GraphTypeResponse::Unrolled,
        }
    }
}

/// Graph metadata
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphMetadata {
    pub states_count: usize,
    pub transitions_count: usize,
    #[serde(default)]
    pub initial_states: Vec<String>,
}

/// Graph element (node or edge) for visualization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphElement {
    pub data: GraphElementData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<GraphPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classes: Option<String>,
}

/// Graph element data (node or edge)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GraphElementData {
    Node {
        id: String,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        vars: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        actions: Vec<String>,
        /// Structured per-state variable valuations (e.g. `{is_red: "0", phase: "green"}`).
        /// Sourced from `Clts::state_valuation()` — populated by adapter side-channels
        /// (SV Kripke, BTOR2, extraction) injected through `ContextDoc.state_valuations`.
        #[serde(skip_serializing_if = "Option::is_none")]
        valuations: Option<BTreeMap<String, String>>,
    },
    Edge {
        id: String,
        source: String,
        target: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        action_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        guard: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        effect: Option<String>,
        /// R.5 Item K sub-item K.4 (2026-06-05) — CTXDSL transition
        /// modality, rendered as `"sharp"` / `"may_only"` /
        /// `"must_hyper_only"`. Absent on edges that don't come from
        /// a CLTS transition (start-arrows, controller bridges).
        /// Default (Sharp) is also serialized as `None` to keep
        /// pre-K.4 JSON byte-for-byte compatible for the dominant case.
        #[serde(skip_serializing_if = "Option::is_none")]
        modality: Option<String>,
    },
}

/// Graph position coordinates
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphPosition {
    pub x: f64,
    pub y: f64,
}

// ============================================================================
// Context Verify Endpoints
// ============================================================================

/// Request for context verification (formula evaluation)
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct ContextVerifyRequest {
    pub context: FileContent,
    #[serde(default)]
    pub sidecars: Vec<SidecarFile>,
    /// If omitted, evaluate ALL formulas defined in the context.
    /// Mutually exclusive with `template_ref`.
    pub formula: Option<String>,
    /// Template reference to instantiate (mutually exclusive with `formula`).
    #[serde(default)]
    pub template_ref: Option<crate::adapter::templates::TemplateRef>,
    /// If omitted, use the formula's target automaton(s).
    pub automaton: Option<String>,
    /// When true, compute counterstrategy for failed formulas via formula inversion.
    #[serde(default)]
    pub counterstrategy: bool,
    /// When true and counterstrategy is enabled, minimize the counterstrategy automaton.
    #[serde(default)]
    pub minimize_counterstrategy: bool,
    /// Labels to hide (reclassify as internal) before evaluation.
    #[serde(default)]
    pub hide: Vec<String>,
    /// When true, apply bisimulation minimization before evaluation.
    #[serde(default)]
    pub minimize: bool,
    /// Stub .espec.json content to compose as sidecars (interface automata).
    #[serde(default)]
    pub stubs: Vec<SidecarFile>,
    /// Run the structural checks (`context eval --sanity`) on `automaton`, or on every automaton
    /// the evaluated formulas target: deadlocks with a path, unreachable states, labels that never
    /// fire, internal livelocks.
    #[serde(default)]
    pub sanity: bool,
}

/// Response from context verification
#[derive(Debug, Serialize, PartialEq)]
pub struct ContextVerifyResponse {
    pub success: bool,
    /// The worst `outcome` over `results`, with a sanity finding counting as `unknown` when no
    /// formula is `violated` (the same reading as `context eval`'s exit code).
    pub outcome: String,
    pub results: Vec<FormulaVerificationResult>,
    /// One structural report per checked automaton, when `sanity` was requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sanity: Vec<crate::context::sanity::SanityReport>,
}

/// Verification result for a single formula–automaton pair
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FormulaVerificationResult {
    pub formula_name: String,
    pub automaton: String,
    /// `holds` iff every initial state satisfies the formula, else `violated`.
    pub outcome: String,
    pub total_states: usize,
    pub satisfying_states: usize,
    pub initial_states: Vec<String>,
    pub initial_satisfying: Vec<String>,
    pub initial_violating: Vec<String>,
    pub satisfying_state_names: Vec<String>,
    /// Present when counterstrategy was requested and formula is not satisfied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterstrategy: Option<CounterstrategyResult>,
}

/// Counterstrategy result: the environment's winning region and strategy as a graph.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CounterstrategyResult {
    /// States where the environment can force the property to be violated.
    pub environment_winning_states: Vec<String>,
    /// Cytoscape graph elements for the counterstrategy automaton.
    pub graph_elements: Vec<GraphElement>,
    /// The inverted formula (for transparency/debugging).
    pub inverted_formula: String,
    /// Whether bisimulation minimization was applied.
    pub minimized: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_names_its_kind_from_its_shape() {
        let cell = |v: u64| {
            vec![CexCellView {
                register: "st".into(),
                value: Some(v),
                hex: format!("0x{v:x}"),
                bits: 2,
            }]
        };
        let finite = Trace::from_cells(vec![cell(0), cell(1)], vec![], vec![], vec![], false);
        assert_eq!(
            (finite.kind, finite.steps.len(), finite.loop_start),
            (TraceKind::Finite, 2, None)
        );
        let lasso = Trace::from_cells(vec![cell(0)], vec![cell(1), cell(2)], vec![], vec![], false);
        assert_eq!(
            (lasso.kind, lasso.steps.len(), lasso.loop_start),
            (TraceKind::Lasso, 3, Some(1))
        );
        let trap = Trace::from_cells(vec![cell(0), cell(3)], vec![], vec![], vec![], true);
        assert_eq!(trap.kind, TraceKind::Trap);
        let none = Trace::from_cells(vec![], vec![], vec![], vec!["st == 3".into()], false);
        assert_eq!(none.kind, TraceKind::Unreachable);
        let attack = Trace::attack(
            Some(3),
            &[crate::adapter::btor2::fault::FaultEvent {
                cycle: 2,
                site: "st".into(),
                value: crate::adapter::btor2::cex_value::CexValue::from_u64(1, 2),
            }],
        );
        assert_eq!((attack.kind, attack.steps.len()), (TraceKind::Attack, 4));
        assert_eq!(attack.steps[2].faults.len(), 1);
        assert!(attack.steps[1].faults.is_empty());
        let json = serde_json::to_value(&lasso).unwrap();
        assert_eq!(json["kind"], "lasso");
        assert_eq!(json["loop_start"], 1);
    }

    #[test]
    fn a_game_under_faults_reads_realizable_as_an_attack_that_violates_resistance() {
        assert_eq!(game_outcome(false, true).0, "holds");
        assert_eq!(game_outcome(false, false).0, "violated");
        assert_eq!(game_outcome(true, true).0, "violated");
        assert_eq!(game_outcome(true, false).0, "holds");
        assert!(game_outcome(true, true).1.contains("attack"));
    }

    #[test]
    fn test_file_content_serialization() {
        let file = FileContent {
            name: "test.ctxdsl".to_string(),
            content: "context test {}".to_string(),
        };
        let json = serde_json::to_string(&file).unwrap();
        let deserialized: FileContent = serde_json::from_str(&json).unwrap();
        assert_eq!(file, deserialized);
    }

    #[test]
    fn test_graph_type_conversion() {
        assert_eq!(
            GraphTypeResponse::from(GraphType::Dsl),
            GraphTypeResponse::Dsl
        );
        assert_eq!(
            GraphTypeResponse::from(GraphType::Unrolled),
            GraphTypeResponse::Unrolled
        );
    }

    #[test]
    fn test_graph_type_serialization() {
        let graph_type = GraphType::Dsl;
        let json = serde_json::to_string(&graph_type).unwrap();
        assert_eq!(json, "\"dsl\"");

        let deserialized: GraphType = serde_json::from_str("\"unrolled\"").unwrap();
        assert_eq!(deserialized, GraphType::Unrolled);
    }

    #[test]
    fn test_graph_element_data_node() {
        let node = GraphElementData::Node {
            id: "node1".to_string(),
            label: "State 1".to_string(),
            parent: Some("parent".to_string()),
            vars: vec!["x".to_string(), "y".to_string()],
            actions: vec!["start".to_string()],
            valuations: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: GraphElementData = serde_json::from_str(&json).unwrap();
        assert_eq!(node, deserialized);
    }

    #[test]
    fn test_graph_element_data_edge() {
        let edge = GraphElementData::Edge {
            id: "edge1".to_string(),
            source: "node1".to_string(),
            target: "node2".to_string(),
            label: Some("transition".to_string()),
            action: Some("action".to_string()),
            action_type: Some("controllable".to_string()),
            guard: Some("x > 0".to_string()),
            effect: Some("x := x + 1".to_string()),
            modality: None,
        };
        let json = serde_json::to_string(&edge).unwrap();
        let deserialized: GraphElementData = serde_json::from_str(&json).unwrap();
        assert_eq!(edge, deserialized);
    }

    #[test]
    fn test_graph_element_serialization() {
        let element = GraphElement {
            data: GraphElementData::Node {
                id: "node1".to_string(),
                label: "State 1".to_string(),
                parent: None,
                vars: vec![],
                actions: vec![],
                valuations: None,
            },
            position: Some(GraphPosition { x: 100.0, y: 200.0 }),
            classes: Some("state start".to_string()),
        };
        let json = serde_json::to_string(&element).unwrap();
        let deserialized: GraphElement = serde_json::from_str(&json).unwrap();
        assert_eq!(element, deserialized);
    }

    #[test]
    fn test_default_graph_types() {
        let types = default_graph_types();
        assert_eq!(types, vec![GraphType::Dsl]);
    }

    #[test]
    fn test_default_summarize_format() {
        let format = default_summarize_format();
        assert_eq!(format, SummarizeFormat::Json);
    }

    #[test]
    fn test_context_graphs_request_deserialization() {
        let json = r#"
        {
            "context": {
                "name": "test.ctxdsl",
                "content": "context test {}"
            },
            "sidecars": [],
            "automaton": null,
            "graph_types": ["dsl", "unrolled"]
        }
        "#;
        let request: ContextGraphsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.context.name, "test.ctxdsl");
        assert_eq!(request.automaton, None);
        assert_eq!(request.graph_types.len(), 2);
    }

    #[test]
    fn test_synthesis_options_defaults() {
        let options: SynthesisOptions = serde_json::from_str("{}").unwrap();
        assert!(!options.minimize);
        assert!(!options.diagnostics.counterexample);
    }
}

// ============================================================================
// Extraction Endpoints
// ============================================================================

/// Response for listing available domain profiles.
#[derive(Debug, Serialize)]
pub struct ExtractionDomainsResponse {
    pub profiles: Vec<DomainProfileInfo>,
}

/// Summary of a domain profile.
#[derive(Debug, Serialize)]
pub struct DomainProfileInfo {
    pub name: String,
    pub language: String,
    pub description: String,
}

/// Response for listing supported composition modes (sync vs async).
/// Surfaces the same options the espec / extract config accepts in
/// `composition.type`. Mirrors the shape of `ExtractionDomainsResponse`.
#[derive(Debug, Serialize)]
pub struct CompositionModesResponse {
    pub modes: Vec<CompositionModeInfo>,
}

/// Description of one composition mode.
#[derive(Debug, Serialize)]
pub struct CompositionModeInfo {
    pub name: &'static str,
    pub description: &'static str,
}

/// Phase B request: scan source for concurrency idioms and propose
/// `composition.instances[]` / `shared[]` blocks. Output is
/// suggestion-grade — the user reviews each finding before promoting
/// it into the extract config.
#[derive(Debug, Deserialize)]
pub struct ProposeCompositionRequest {
    /// Source content to scan.
    pub source: String,
    /// Source language (`typescript` / `python` / `rust`).
    /// When omitted, the API requires the caller to specify it — there
    /// is no `source.file` extension to infer from at this endpoint.
    pub language: Option<String>,
}

/// Phase B response: list of detected concurrency findings, in source
/// order. An empty `findings` list is the common case (no concurrency
/// patterns present); not an error.
///
/// The `findings` field is gated behind `ast-extract` because its element
/// type lives behind that feature. Without `ast-extract` the struct is a
/// well-formed empty response type: the `#[cfg(not(feature = "ast-extract"))]`
/// stub of `extraction_propose_composition_handler` returns an error and never
/// constructs it, so it is only ever serialized on the `ast-extract` path. This
/// lets `cargo … --features api` (without `ast-extract`) compile — matching the
/// handler stubs' intent that the API build standalone.
#[derive(Debug, Serialize)]
pub struct ProposeCompositionResponse {
    #[cfg(feature = "ast-extract")]
    pub findings:
        Vec<crate::adapter::extraction::ast_extract::concurrency_detect::DetectedConcurrency>,
}

/// Request for AST-based extraction from source code.
#[derive(Debug, Deserialize)]
pub struct ExtractionExtractRequest {
    /// Extraction config content (.extract.json).
    pub config: String,
    /// Source code content.
    pub source: String,
    /// Source language (typescript, python, rust). Auto-detected if omitted.
    pub language: Option<String>,
}

/// Response from AST-based extraction.
#[derive(Debug, Serialize)]
pub struct ExtractionExtractResponse {
    pub success: bool,
    /// Generated .espec.json content.
    pub espec: String,
    /// Extraction warnings.
    pub warnings: Vec<String>,
    /// Automata extracted.
    pub automata: Vec<ExtractionAutomatonInfo>,
}

/// Summary of an extracted automaton.
#[derive(Debug, Serialize)]
pub struct ExtractionAutomatonInfo {
    pub id: String,
    pub state_count: usize,
    pub transition_count: usize,
}

// ============================================================================
// Extraction Validate Endpoint
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ExtractionValidateRequest {
    pub spec: String,
    pub source: String,
    #[serde(default = "default_drift_window")]
    pub drift_window: usize,
}

fn default_drift_window() -> usize {
    5
}

#[derive(Debug, Serialize)]
pub struct ExtractionValidateResponse {
    pub success: bool,
    pub summary: ValidationSummaryApi,
    pub anchors: Vec<AnchorResultApi>,
    pub uncovered: Vec<UncoveredAccessApi>,
    pub commit_match: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ValidationSummaryApi {
    pub total: usize,
    pub exact: usize,
    pub drifted: usize,
    pub mismatch: usize,
    pub error: usize,
    pub uncovered_accesses: usize,
}

#[derive(Debug, Serialize)]
pub struct AnchorResultApi {
    pub id: String,
    pub section: String,
    pub status: String,
    pub line: Option<u32>,
    pub found_line: Option<u32>,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UncoveredAccessApi {
    pub line: u32,
    pub field: String,
    pub content: String,
}

// ============================================================================
// Context Predicates Endpoint
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct ContextPredicatesRequest {
    pub context: FileContent,
    #[serde(default)]
    pub sidecars: Vec<SidecarFile>,
    pub automaton: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContextPredicatesResponse {
    pub success: bool,
    pub predicates: std::collections::HashMap<String, Vec<String>>,
}

// ============================================================================
// BTOR2 CEGAR Endpoint (U.0 slot-6 refinement-trace viewer)
// ============================================================================

/// U.0 (slot 6) — request for the CEGAR refinement endpoint
/// (`POST /api/v1/btor2/cegar`). Mirrors the CLI `mununu btor2 cegar`:
/// runs the predicate-abstraction-refinement loop over a BTOR2 design and
/// returns the per-iteration refinement trace the UI viewer renders.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Btor2CegarRequest {
    /// BTOR2 source content.
    pub content: String,
    /// μ-calculus formula evaluated over the lifted KMTS.
    pub formula: String,
    /// Initial predicate set (bootstraps the `2^|P|` cube space). At least
    /// one entry is required. Reuses the `PredicateSpecRequest` shape.
    pub predicates: Vec<PredicateSpecRequest>,
    /// Optional R.6.6 controllability split — controller-driven input
    /// symbols (mirrors `--controllable`).
    #[serde(default)]
    pub controllable: Vec<String>,
    /// Predicate-discovery source: `"wp"` (default) | `"craig"`.
    #[serde(default)]
    pub predicate_source: Option<String>,
    /// Max CEGAR iterations (default 16).
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Must-edge inference policy (kebab-case; default `"off"`):
    /// `"sampling-confluence"` | `"smt-per-target"` |
    /// `"smt-per-target-standard"` | `"smt-hyper-must"`.
    #[serde(default)]
    pub must_edge_inference: Option<String>,
    /// May-edge inference policy (kebab-case; default `"off"`):
    /// `"smt-all-pairs"` for the sound all-pairs may-relation. Mirrors the
    /// CLI `--may-edge-inference`.
    #[serde(default)]
    pub may_edge_inference: Option<String>,
    /// R-S8 symbolic-init config-values, one entry per register as
    /// `"REG=v1,v2,..."` (mirrors the CLI `--init-values`). Each register's
    /// admissible power-up set seeds the predicate-cube initial states. Empty =
    /// no synthetic init sidecar.
    #[serde(default)]
    pub init_values: Vec<String>,
    /// CTXDSL Phase 2 (2026-06-22) — opt-in (default `false`): when set, the
    /// response gains a `ctxdsl` field carrying the final refined cube model
    /// and the checked formula as a self-contained CTXDSL document (the
    /// `predicates_3v` Kleene labels, transition modality, and a
    /// `mu_formulas` block). Mirrors the CLI `--emit-ctxdsl`.
    #[serde(default)]
    pub emit_ctxdsl: bool,
    /// R-F5.4.2b (2026-07-03) — predicate-cube engine: `"explicit"` (default,
    /// SMT edges + CEGAR refinement) or `"symbolic"` (R-F5 BDD relation,
    /// single-shot, no per-cube-pair SMT). Mirrors the CLI `--engine`. The
    /// symbolic path handles only simple equality predicates + the bare
    /// `[]`/`<>` fragment, and does not refine (`iterations` is empty,
    /// `terminated_with` is `"symbolic-single-shot"`).
    #[serde(default)]
    pub engine: Option<String>,
}

/// Request for the multi-engine safety portfolio endpoint
/// (`POST /api/v1/btor2/verify`). Mirrors the CLI `mununu btor2 verify`: decides
/// `bad`-reachability of a BTOR2 design across every available sound engine.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2VerifyRequest {
    /// BTOR2 source content.
    pub content: String,
    /// Fault-injection posture (`"REG[:k]"`, one global budget, default 1): the design is
    /// rewritten as `btor2 inject-fault` does before the portfolio runs; `"violated"` then
    /// reads "an attack within the budget reaches `bad`". Mirrors the CLI `--fault`. Default
    /// empty.
    #[serde(default)]
    pub faults: Vec<String>,
    /// Cycles before the first injection may fire (default 10). Mirrors `--fault-warmup`.
    #[serde(default)]
    pub fault_warmup: Option<u32>,
    /// The k-fault adversary's goal (with `faults`): a boolean atom the attacker tries to reach
    /// instead of the design's own `bad` lines. Mirrors the CLI `--target`. A `target` without
    /// `faults` is a 400.
    #[serde(default)]
    pub target: Option<String>,
    /// Safety engine: `"portfolio"` (default, the bit-level reachability portfolio) or `"cube"`
    /// (the KMTS 3-valued predicate cube, the CLI's `--engine cube`). `cube` takes none of
    /// `faults`, `target`, `witness`. (The CLI's experimental `--engine ic3` has no API surface.)
    #[serde(default)]
    pub engine: Option<String>,
    /// On a `violated` verdict, re-derive the shallowest reset → `bad` path with native BMC and
    /// return it as `trace` (the CLI's `--witness`). Default `false`.
    #[serde(default)]
    pub witness: bool,
    /// The BMC bound for `witness` (the CLI's `--witness-max-k`, default 200).
    #[serde(default)]
    pub witness_max_k: Option<u32>,
}

/// One injection of an attack witness (`attack[]`).
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct FaultEventView {
    /// The frame the fault input was set in.
    pub cycle: usize,
    /// The register overwritten.
    pub site: String,
    /// The value written, when it fits 64 bits.
    pub value: Option<u64>,
    /// The value in hexadecimal, at any width.
    pub hex: String,
}

/// Response for `POST /api/v1/btor2/verify` — the merged portfolio verdict plus
/// which engines reached each definite conclusion. Mirrors
/// [`crate::adapter::reach_portfolio::ReachOutcome`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2VerifyResponse {
    /// Canonical property verdict — `"holds"` (`bad` unreachable) | `"violated"`
    /// (reachable) | `"unknown"` (undecided / contradiction), via
    /// [`crate::verdict::PropertyVerdict`]. The reachability *detail* is in the
    /// `reachable_by` / `unreachable_by` lists + the `contradiction` alarm.
    pub outcome: String,
    /// Engines that found `bad` reachable (a real counterexample).
    pub reachable_by: Vec<String>,
    /// Engines that proved `bad` unreachable (a real safety proof).
    pub unreachable_by: Vec<String>,
    /// `true` when two sound engines disagree — a soundness alarm, not a guess.
    pub contradiction: bool,
    /// The single engine's own diagnosis under `engine: "cube"`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The fault posture the design was decided under (`faults`), as one label; absent without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<String>,
    /// The k-fault adversary's target (`target`); absent without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The counterexample: with a `target` and a `violated` verdict, the shallowest attack found
    /// (`kind: "attack"`, the injections on `steps[].faults`); with `witness` and a `violated`
    /// verdict, the shallowest reset → `bad` path (`kind: "finite"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
    /// With `witness`: the bound within which no path was re-derived although the verdict is
    /// `violated` (another engine found a deeper one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_unavailable_within_k: Option<u32>,
}

/// Request for the response-liveness endpoint
/// (`POST /api/v1/btor2/verify-liveness`). Mirrors the CLI
/// `mununu btor2 verify-liveness`: decides `AG(request → AF grant)` via the l2s
/// reduction + the portfolio. `request` / `grant` are register-comparison atom
/// strings (`"st == 1"`).
#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Btor2VerifyLivenessRequest {
    /// BTOR2 source content (with `justice`, ASCII AIGER 1.9 `aag …` is accepted too).
    pub content: String,
    /// The request atom (`"REG op VALUE"`) of `AG(request → AF grant)`.
    #[serde(default)]
    pub request: Option<String>,
    /// The grant atom that must eventually follow on every path.
    #[serde(default)]
    pub grant: Option<String>,
    /// Response pairs `"ANTE => CONS"`: the conjunction `⋀ AG(aᵢ → AF gᵢ)` instead of one
    /// request / grant (0.5 merges `/verify-liveness` here).
    #[serde(default)]
    pub responses: Vec<String>,
    /// Fairness atoms assumed to hold infinitely often, with `request` / `grant` (0.5 merges
    /// `/verify-liveness` here).
    #[serde(default)]
    pub fairness: Vec<String>,
    /// Decide the design's own `justice` lines (HWMCC semantics) instead of a response property
    /// (0.5 merges `/verify-liveness` here).
    #[serde(default)]
    pub justice: bool,
    /// With `justice`: `"exact"`, `"l2s"` or `"both"` (default).
    #[serde(default)]
    pub rung: Option<String>,
    /// With `justice`: wall-clock budget per justice set in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// With `justice` and `rung: "both"`: run both rungs concurrently.
    #[serde(default)]
    pub parallel_rungs: Option<bool>,
}

/// Request for `POST /api/v1/btor2/verify-liveness` — the HTTP peer of `mununu btor2
/// verify-liveness --justice`: every `justice` line of the design (BTOR2 text, or ASCII AIGER 1.9 — the
/// binary `.aig` form must be converted with `btor2 from-aiger` first) decided by the exact
/// fair-cycle fixpoint and/or liveness-to-safety + the reachability portfolio.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2VerifyJusticeRequest {
    /// BTOR2 source content, or ASCII AIGER 1.9 (`aag …`).
    pub content: String,
    /// `"exact"`, `"l2s"` or `"both"` (default).
    #[serde(default)]
    pub rung: Option<String>,
    /// Wall-clock budget per justice set in milliseconds (default: the engines' own budgets).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Under `both`, run the exact ladder and the l2s rung concurrently (first definite wins).
    #[serde(default)]
    pub parallel_rungs: Option<bool>,
}

/// One justice set's verdict in [`Btor2VerifyJusticeResponse`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2JusticeEntry {
    /// Position among the design's `justice` lines (0-based).
    pub index: usize,
    /// Number of literals in the set.
    pub size: usize,
    /// `"holds"` (no fair infinite path — HWMCC `uns`), `"violated"` (one exists — `sat`),
    /// `"unknown"`.
    pub outcome: String,
    /// The rung and engines that decided it.
    pub decided_by: Vec<String>,
    /// Abstention reasons, when a rung abstained.
    pub notes: Vec<String>,
}

/// Response for `POST /api/v1/btor2/verify-liveness`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2VerifyJusticeResponse {
    /// Worst verdict over the sets: `"violated"` if any, else `"unknown"` if any, else `"holds"`.
    pub outcome: String,
    /// Whether the content was ASCII AIGER lowered to BTOR2.
    pub converted_from_aiger: bool,
    pub justice: Vec<Btor2JusticeEntry>,
}

/// Response for `POST /api/v1/btor2/verify-liveness`. Mirrors
/// [`crate::adapter::liveness_rescue::LivenessVerdict`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2VerifyLivenessResponse {
    /// Canonical property verdict — `"holds"` | `"violated"` | `"unknown"`, via
    /// [`crate::verdict::PropertyVerdict`].
    pub outcome: String,
    /// The reduced property, echoed for provenance:
    /// `AG((<request>) -> AF (<grant>))`.
    pub property: String,
    /// Portfolio engines that decided the reduced `bad`-reachability query.
    pub decided_by: Vec<String>,
    /// With `justice`: one entry per justice line (`decided_by` above is empty then).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub justice: Vec<Btor2JusticeEntry>,
    /// With `justice`: whether the content was ASCII AIGER lowered to BTOR2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converted_from_aiger: Option<bool>,
}

/// Request for the conjunctive response-liveness endpoint
/// (`POST /api/v1/btor2/verify-liveness`). Mirrors the CLI
/// `mununu btor2 verify-liveness --response`: decides `⋀ᵢ AG(aᵢ → AF bᵢ)`. Each `responses`
/// entry is a `"ANTE => CONS"` pair of register-comparison atoms. At least one
/// required. Reuses [`Btor2VerifyLivenessResponse`] for the reply.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2VerifyLivenessAllRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The response pairs, each `"ANTE => CONS"`.
    pub responses: Vec<String>,
}

/// Request for the fair-cycle response-liveness endpoint
/// (`POST /api/v1/btor2/verify-liveness`, mununu#477 Option B).
/// Mirrors the CLI `mununu btor2 verify-liveness --fairness`: decides
/// `(⋀ GF fair) → AG(request → AF grant)` via the Emerson–Lei fair-cycle l2s
/// (see [`crate::adapter::btor2::l2s_monitor::emit_response_l2s_monitor_under_fairness`])
/// plus the reachability portfolio. Empty `fairness` recovers
/// [`Btor2VerifyLivenessRequest`] exactly. Reuses [`Btor2VerifyLivenessResponse`] for the reply.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2VerifyLivenessUnderFairnessRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The request atom (`"REG op VALUE"`).
    pub request: String,
    /// The grant atom that must eventually follow on every path.
    pub grant: String,
    /// Fairness atoms — each a `"REG op VALUE"` register-comparison atom whose
    /// satisfaction is assumed to hold infinitely often on every path
    /// considered. Empty recovers the plain `verify-liveness` semantics.
    #[serde(default)]
    pub fairness: Vec<String>,
}

/// Request for the recoverability endpoint
/// (`POST /api/v1/btor2/verify-recoverability`). Mirrors the CLI
/// `mununu btor2 verify-recoverability`: decides `AG EF target` — "from every
/// reachable state, can the design get back to `target`?". `target` is a single
/// register-comparison atom string (`"state_q == 3"`).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Btor2VerifyRecoverabilityRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The `good` atom to recover to (`"REG op VALUE"`).
    pub target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation, each
    /// `"[NAME:]REG == VALUE"` (P2 Slice 1). Used only when the exact engine abstains
    /// (over the bit cap — 40 by default, auto-raised to 192); the escalation is automatic even with none.
    #[serde(default)]
    pub predicates: Vec<String>,
    /// Also compute a structured [`crate::verdict::VerdictRefinement`] alongside the verdict — a
    /// `vacuous` witness when the target is never reachable, an auto `config_partition` over the
    /// design's detected reset when recovery depends on it, and a "why ⊥" hint. Diagnostic-only:
    /// it never changes the canonical verdict. (Mirrors the CLI `--refine`.)
    #[serde(default)]
    pub refine: bool,
    /// Config-partition (capability A): config inputs to split the verdict over, each
    /// `"NAME=v1,v2,..."`. The refinement then carries a `config_partition` decided exactly per config,
    /// every named value included — a single value is a scoped pin, reported as its one cell (#634).
    /// Implies the refined output. Mirrors the CLI `--partition`.
    #[serde(default)]
    pub partitions: Vec<String>,
    /// Assumption discovery (capability B): when the property does NOT hold, search for an environment
    /// assumption φ under which it becomes a non-vacuous HOLDS → the refinement carries `holds_under`.
    /// CONDITIONAL-only (never changes the canonical verdict). Implies the refined output. Mirrors the
    /// CLI `--discover-assumptions`.
    #[serde(default)]
    pub discover_assumptions: bool,
}

/// Response for `POST /api/v1/btor2/verify-recoverability`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2VerifyRecoverabilityResponse {
    /// Canonical property verdict — `"holds"` (every reachable state can reach
    /// `target`) | `"violated"` (a reachable trap cannot) | `"unknown"` (over the
    /// exact engine's cap; try the cube + `smt-hyper-must` path), via
    /// [`crate::verdict::PropertyVerdict`].
    pub outcome: String,
    /// The decided property, echoed for provenance: `AG EF (<target>)`.
    pub property: String,
    /// The structured refinement, when `refine` was requested and it produced one. `None` otherwise.
    /// Diagnostic detail carried ALONGSIDE the verdict — never changes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinement: Option<crate::verdict::VerdictRefinement>,
    /// Which rung of the recoverability ladder produced the canonical verdict: `"exact"` (the
    /// exact-symbolic ROBDD engine), `"exact-reach-restricted"` (its reach-restricted stage) or
    /// `"cube-ladder"` (the predicate-cube ladder, after the exact engine abstained). Absent only
    /// when no engine ran (the target did not parse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// When the exact engine abstained before the cube ladder decided: the typed reading of its
    /// abstention — `bit-cap`, `arena-overflow`, `iteration-budget`, `latency`, `arena-safety`,
    /// `deadline`, `cancelled`, `refused-input-atom`, `reach-declined` or `other`. Absent when the
    /// exact engine decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_abstention: Option<String>,
}

/// Request for the auto FSM illegal-encoding scan
/// (`POST /api/v1/btor2/check-fsm`). Mirrors the CLI `mununu btor2 check-fsm`:
/// auto-discovers the FSM-like state registers and checks, from the reset state,
/// whether any illegal encoding (a value outside the register's legal set) is
/// reachable — with **no user input** (the legal set is derived from the design).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2CheckFsmRequest {
    /// BTOR2 source content.
    pub content: String,
    /// Max state-register width treated as an FSM (wider = datapath/counter, skipped).
    #[serde(default = "default_fsm_max_width")]
    pub max_width: u32,
    /// Also run the liveness inventory (`check-fsm --liveness`).
    #[serde(default)]
    pub liveness: bool,
    /// Intended absorbing states, each `"REG=VALUE"` (`--terminal`).
    #[serde(default)]
    pub terminal: Vec<String>,
}

fn default_fsm_max_width() -> u32 {
    crate::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH
}

/// One state register's illegal-encoding result in a [`Btor2CheckFsmResponse`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FsmRegisterFinding {
    /// The state register's symbol — the cell's own, or a value-identical alias of it
    /// (`state_q`, the port that mirrors it); never a next-value wire (#633).
    pub register: String,
    /// `"fsm"` — an enumeration-valued register, checked; `"counter"` — a register its
    /// own next-state logic steps by ±1, set aside with no legal set and no verdict
    /// (its values are a range, not an enumeration; #633).
    pub kind: String,
    /// The legal encodings the register's own logic recognizes (sorted); empty for a
    /// `"counter"`.
    pub legal_encodings: Vec<u64>,
    /// Canonical verdict — `"holds"` (stays within its encoding) | `"violated"` (an
    /// illegal encoding is reachable) | `"unknown"` (the portfolio could not decide) |
    /// `"skipped"` (a `"counter"`, not checked).
    pub outcome: String,
    /// `true` when an illegal encoding is reachable (a finding).
    pub illegal_encoding_reachable: bool,
}

/// Response for `POST /api/v1/btor2/check-fsm`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2CheckFsmResponse {
    /// Number of `"fsm"`-kind registers scanned (a `"counter"` entry in `registers` is
    /// listed, not counted).
    pub fsm_registers_checked: usize,
    /// Number of registers with a reachable illegal encoding (`verdict == "violated"`).
    pub illegal_encodings_found: usize,
    /// Per-register results.
    pub registers: Vec<FsmRegisterFinding>,
    /// The liveness inventory, when `liveness` was requested.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub liveness: Vec<FsmLivenessView>,
}

/// One register's liveness inventory (`check-fsm --liveness`): out of reset, resets pinned
/// inactive.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FsmLivenessView {
    pub register: String,
    /// The register's value out of reset, when the reset logic gives one.
    pub reset_value: Option<u64>,
    /// Legal encodings no path from reset reaches.
    pub unreachable: Vec<u64>,
    /// Reachable encodings the register can never leave, not declared intended.
    pub absorbing: Vec<u64>,
    /// Absorbing encodings declared intended with `terminal`.
    pub expected_terminal: Vec<u64>,
    /// `AG EF (reg == reset_value)`: `holds | violated | unknown | skipped` (no reset value).
    pub returns_to_reset: String,
    /// Encodings no engine decided.
    pub undecided: Vec<u64>,
}

impl From<&crate::adapter::fsm_scan::FsmLivenessFinding> for FsmLivenessView {
    fn from(f: &crate::adapter::fsm_scan::FsmLivenessFinding) -> Self {
        Self {
            register: f.register.clone(),
            reset_value: f.reset_value,
            unreachable: f.unreachable.clone(),
            absorbing: f.absorbing.clone(),
            expected_terminal: f.expected_terminal.clone(),
            returns_to_reset: f.returns_to_reset.as_str().to_string(),
            undecided: f.undecided.clone(),
        }
    }
}

/// `"REG=VALUE"` terminal declarations → `(register, value)` (decimal or `0x` hex).
pub fn parse_terminal_states(specs: &[String]) -> Result<Vec<(String, u64)>, String> {
    specs
        .iter()
        .map(|s| {
            let (r, v) = s
                .split_once('=')
                .ok_or_else(|| format!("terminal state `{s}`: expected REG=VALUE"))?;
            let v = v.trim();
            let value = match v.strip_prefix("0x") {
                Some(h) => u64::from_str_radix(h, 16),
                None => v.parse::<u64>(),
            }
            .map_err(|_| format!("terminal state `{s}`: `{v}` is not an integer"))?;
            Ok((r.trim().to_string(), value))
        })
        .collect()
}

/// The two-player game winning objective (`objective` field). Mirrors the CLI `--objective`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameObjective {
    /// Force `good` at least ONCE — the reachability game `μX. good ∨ ⟨ctrl⟩X`.
    #[default]
    Reach,
    /// Force `good` INFINITELY OFTEN — the Büchi game `νZ.μY. (good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y`.
    Recurrence,
}

/// Request for the two-player controllable game (`POST /api/v1/btor2/game`). Mirrors the CLI
/// `mununu btor2 game`: decide whether the CONTROLLER can force the design to `good` (once for `reach`,
/// infinitely often for `recurrence`) against every environment move, and synthesize the winner's strategy.
#[derive(Debug, Deserialize)]
pub struct Btor2GameRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The reachability target `good` state atom (`"REG op VALUE"`, e.g. `"state_q == 44"`).
    pub good: String,
    /// The controller-owned primary inputs; every other primary input is the (adversarial) environment's.
    /// A name that is not a real primary input is a `BadRequest`.
    #[serde(default)]
    pub controllable: Vec<String>,
    /// The winning objective: `"reach"` (default) = force `good` ONCE; `"recurrence"` = force `good`
    /// INFINITELY OFTEN (the Büchi game). Strategy + assumption discovery apply to `reach` only today.
    /// Mirrors the CLI `--objective`.
    #[serde(default)]
    pub objective: GameObjective,
    /// When the game is UNREALIZABLE, also search for an ENVIRONMENT ASSUMPTION under which the controller
    /// wins (the assume-guarantee wedge) → `holds_under`. CONDITIONAL — never flips `realizable`. No-op
    /// when the game is already realizable or under `objective = "recurrence"`. Mirrors the CLI
    /// `--discover-assumptions`.
    #[serde(default)]
    pub discover_assumptions: bool,
    /// Model the clock and reset as a sound POSTURE instead of adversarial inputs — pin the detected reset
    /// inactive (+ post-reset init) and the clock to a constant, so the environment cannot spuriously
    /// FREEZE THE CLOCK or HOLD RESET (a modeling artifact of the raw lift). Recommended for real RTL.
    /// Mirrors the CLI `--assume-clock-reset`.
    #[serde(default)]
    pub assume_clock_reset: bool,
    /// The k-fault adversary as a game: these fault sites' inputs (`"REG[:k]"`, one global budget,
    /// default 1) join the controllable side, so `realizable` means an attack wins against every
    /// environment behaviour. Mirrors the CLI `--fault`. Default empty (no change).
    #[serde(default)]
    pub faults: Vec<String>,
    /// Cycles before the first injection may fire (default 10). Mirrors `--fault-warmup`.
    #[serde(default)]
    pub fault_warmup: Option<u32>,
}

/// The verdict envelope of a game: the `outcome` of the property the verb decides and a `detail`
/// naming whose objective that is. Without faults the property is the controller's objective
/// (`holds` iff realizable). With faults the controller is the attacker and the property is the
/// design's resistance, so a realizable game (an attack wins against every environment) is
/// `violated`. The CLI exit code reads the same `outcome`.
pub fn game_outcome(under_fault: bool, realizable: bool) -> (&'static str, String) {
    let outcome = if realizable != under_fault {
        "holds"
    } else {
        "violated"
    };
    let detail = match (under_fault, realizable) {
        (false, true) => "the controller forces the objective against every environment move",
        (false, false) => "the environment can prevent the objective (no controller wins)",
        (true, true) => {
            "an attack within the fault budget wins against every environment behaviour; the \
             design does not resist it"
        }
        (true, false) => "no attack within the fault budget wins; the design resists it",
    };
    (outcome, detail.to_string())
}

/// The verdict of a CTXDSL evaluation (`context eval`, `/context/verify`): the formulas' (`holds`
/// iff every initial state satisfies every one), else the sanity checks' (`unknown` when they found
/// something, a finding being a model defect to review rather than a refuted property).
/// `formula_holds` is `None` when only the sanity checks ran.
pub fn context_outcome(formula_holds: Option<bool>, sanity_clean: bool) -> &'static str {
    match formula_holds {
        Some(false) => "violated",
        _ if !sanity_clean => "unknown",
        _ => "holds",
    }
}

/// `holds` / `violated` for a realizability verdict (synthesis, GR(1)): the controller exists.
pub fn realizable_outcome(realizable: bool) -> &'static str {
    if realizable { "holds" } else { "violated" }
}

/// Response for `POST /api/v1/btor2/game`.
#[derive(Debug, Serialize)]
pub struct Btor2GameResponse {
    /// The verdict envelope: `holds` | `violated` for the property [`game_outcome`] describes.
    pub outcome: String,
    /// Whose objective `outcome` is about, in words (the `--fault` polarity is stated here).
    pub detail: String,
    /// `true` = the controller can force `good` against every environment move (the spec is realizable);
    /// `false` = the environment can prevent it (unrealizable). For `objective = "recurrence"` this is
    /// "force `good` infinitely often" (the Büchi game).
    pub realizable: bool,
    /// The `good` atom, echoed for provenance.
    pub good: String,
    /// The winning objective decided, echoed for provenance (`reach` or `recurrence`).
    pub objective: GameObjective,
    /// The controller-owned inputs, echoed for provenance.
    pub controllable: Vec<String>,
    /// Discovered environment assumption(s) under which the (unrealizable) game becomes realizable —
    /// each a CONDITIONAL `HoldsUnder(φ)` (never flips `realizable`). Only when `discover_assumptions`
    /// was requested and the game is unrealizable; empty otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub holds_under: Vec<crate::verdict::DiscoveredAssumption>,
    /// The winner's strategy: a `controller_strategy` (Mealy) or an `environment_counterstrategy`
    /// (positional) — the [`crate::adapter::btor2::symbolic_bitblast::TwoPlayerStrategy`] serialized with a
    /// `"kind"` tag. For `objective = "reach"` it is the controller's attractor strategy when realizable,
    /// else the environment's positional counterstrategy. For `objective = "recurrence"` it is the
    /// CONTROLLER's Büchi strategy (force `good` infinitely often) when realizable, and `null` when not —
    /// the unrealizable recurrence witness is `stall_lasso` below. **Present only for a STATE-register
    /// `good`**: the strategy is state-indexed, so it is omitted (`null`) for a combinational-output /
    /// relational target (a FIFO's `full_o`), where `realizable` + `holds_under` still apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<crate::adapter::btor2::symbolic_bitblast::TwoPlayerStrategy>,
    /// P2.5-F (b) — the ENVIRONMENT STARVATION LASSO (a `lasso` [`Trace`]) for an unrealizable RECURRENCE game: a concrete
    /// play (reset → repeating `¬good` `cycle`, with the env's per-step `inputs`) proving the environment
    /// can starve `good` forever — the actionable Büchi counterpart of `realizable = false`. Present only
    /// for `objective = "recurrence"` when unrealizable AND a simple reachable force-`¬good`-forever
    /// region exists; `null` for realizable games, the `reach` objective (whose witness is `strategy`),
    /// or a subtler co-Büchi with no such region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
    /// Why `strategy` is absent when the verdict is known: the extraction's own error (e.g. a goal
    /// over no state register). Absent when a strategy was extracted or none was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_error: Option<String>,
    /// With `faults`: the posture, as one label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<String>,
    /// With `faults`: `realizable` read as "an attack wins against every environment behaviour".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_exists: Option<bool>,
}

/// The SV → BTOR2 lift inputs shared by the SV-direct verb endpoints
/// (`/api/v1/sv/verify`, `/sv/verify-liveness`, `/sv/verify-recoverability`). These
/// lift the module (sv2v + Yosys) and then decide the corresponding BTOR2 property,
/// returning the same `Btor2Verify*Response` shapes — one call, no `emit-btor2` step.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
}

/// Request for `POST /api/v1/sv/verify-liveness` — the SV lift fields plus the
/// response-liveness atoms.
#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct SvVerifyLivenessRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// The request atom (`"REG op VALUE"`) of `AG(request → AF grant)`.
    #[serde(default)]
    pub request: Option<String>,
    /// The grant atom that must eventually follow on every path.
    #[serde(default)]
    pub grant: Option<String>,
    /// Response pairs `"ANTE => CONS"`: the conjunction instead of one request / grant (0.5
    /// merges `/sv/verify-liveness` here).
    #[serde(default)]
    pub responses: Vec<String>,
    /// Fairness atoms assumed to hold infinitely often, with `request` / `grant`.
    #[serde(default)]
    pub fairness: Vec<String>,
}

/// Request for `POST /api/v1/sv/verify-liveness` — the SV lift fields plus the
/// conjunction's `"ANTE => CONS"` response pairs. Mirrors the CLI
/// `mununu sv verify-liveness --response`; reuses [`Btor2VerifyLivenessResponse`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyLivenessAllRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// The response pairs, each `"ANTE => CONS"`. At least one required.
    pub responses: Vec<String>,
}

/// Request for `POST /api/v1/sv/verify-recoverability` — the SV lift fields plus the
/// recoverability target atom.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvVerifyRecoverabilityRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// The `good` atom to recover to (`"REG op VALUE"`).
    pub target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation, each
    /// `"[NAME:]REG == VALUE"` (P2 Slice 1). Used only when the exact engine abstains.
    #[serde(default)]
    pub predicates: Vec<String>,
    /// Also compute a structured `refinement` alongside the verdict (mirrors the CLI `--refine`).
    /// Diagnostic-only — never changes the canonical verdict.
    #[serde(default)]
    pub refine: bool,
    /// Config-partition config inputs, each `"NAME=v1,v2,..."` (mirrors the CLI `--partition`).
    #[serde(default)]
    pub partitions: Vec<String>,
    /// Assumption discovery (capability B): search for an enabling φ when the property does not hold →
    /// `holds_under` (conditional-only). Mirrors the CLI `--discover-assumptions`.
    #[serde(default)]
    pub discover_assumptions: bool,
}

/// Request for `POST /api/v1/sv/check-fsm` — the SV lift fields plus the FSM width
/// bound. Lifts the module and auto-scans every FSM register for a reachable illegal
/// encoding (no property to name). Returns a [`Btor2CheckFsmResponse`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvCheckFsmRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// Max state-register width treated as an FSM (wider = datapath/counter, skipped).
    #[serde(default = "default_fsm_max_width")]
    pub max_width: u32,
    /// Also run the liveness inventory (`check-fsm --liveness`).
    #[serde(default)]
    pub liveness: bool,
    /// Intended absorbing states, each `"REG=VALUE"` (`--terminal`).
    #[serde(default)]
    pub terminal: Vec<String>,
}

/// The RTL front end of a lift: `auto` (the environment's default), `verilog` (yosys
/// `read_verilog`), `slang` (the yosys-slang plugin) or `sv2v` (sv2v, then `read_verilog`).
/// The CLI's `--frontend` takes the same four values.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SvFrontendName {
    #[default]
    Auto,
    Verilog,
    Slang,
    Sv2v,
}

/// The SV design and how to lift it, shared by every `sv` route: the API peer of the CLI's
/// lift flags (`SV_FILE` / `--source`, `--top`, `--frontend`, `--param`, `--lift-override`,
/// `--cutpoint`, `--merge-clock-domains`). Unknown keys are refused.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvLiftRequest {
    /// The primary SystemVerilog source.
    pub source: String,
    /// The primary source's file name, which `lift_overrides` match (default `<top>.sv`, or
    /// `top.sv` without a `top`).
    #[serde(default)]
    pub source_name: Option<String>,
    /// Additional SV sources (packages, includes, submodules).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module (auto-detected when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// The RTL front end (default `auto`).
    #[serde(default)]
    pub frontend: SvFrontendName,
    /// Parameter overrides, `NAME=VALUE` or `MODULE.NAME=VALUE`.
    #[serde(default)]
    pub params: Vec<String>,
    /// Lift-only replacements of a source by file name (an assertion-macro shim, a synthesis
    /// variant). `sv verify-auto` still extracts its SVA from the original sources.
    #[serde(default)]
    pub lift_overrides: Vec<FileContent>,
    /// Control-slice cut points: each named net becomes a free input (an over-approximation).
    #[serde(default)]
    pub cutpoints: Vec<String>,
    /// Lift a multi-clock design onto the model's one implicit clock instead of refusing it.
    /// An UNDER-approximation: the domains only ever tick together, so a HOLDS does not transfer.
    #[serde(default)]
    pub merge_clock_domains: bool,
}

impl SvLiftRequest {
    /// The core lift this request describes; a malformed `params` entry is an error.
    pub fn to_sv_lift(&self) -> Result<crate::adapter::sv_verify::SvLift, String> {
        use crate::adapter::yosys::SvFrontend;
        let (frontend, use_sv2v) = match self.frontend {
            SvFrontendName::Auto => (SvFrontend::Auto, false),
            SvFrontendName::Verilog => (SvFrontend::Verilog, false),
            SvFrontendName::Slang => (SvFrontend::Slang, false),
            SvFrontendName::Sv2v => (SvFrontend::Verilog, true),
        };
        Ok(crate::adapter::sv_verify::SvLift {
            primary_name: self
                .source_name
                .clone()
                .unwrap_or_else(|| format!("{}.sv", self.top.as_deref().unwrap_or("top"))),
            source: self.source.clone(),
            additional_sources: self
                .additional_sources
                .iter()
                .map(|f| (f.name.clone(), f.content.clone()))
                .collect(),
            top: self.top.clone(),
            use_sv2v,
            include_dirs: Vec::new(),
            frontend,
            merge_clock_domains: self.merge_clock_domains,
            params: crate::adapter::sv_verify::parse_params(&self.params)?,
            cutpoints: self.cutpoints.clone(),
            lift_overrides: self
                .lift_overrides
                .iter()
                .map(|f| (f.name.clone(), f.content.clone()))
                .collect(),
        })
    }
}

/// Request for `POST /api/v1/sv/lint` — the SV-direct partial-write preflight.
/// Lifts SystemVerilog and reports every register whose partial-write lift the
/// verifier cannot keep faithfully (monono#partsel), with no model checking.
/// Surface peer of the CLI `mununu sv lint`.
#[derive(Debug, Deserialize)]
pub struct SvLintRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
}

/// Request for `POST /api/v1/sv/cdc-lint` — the structural clock-domain-crossing lint. Lifts
/// SystemVerilog with its clock domains merged and classifies every crossing of the flattened
/// netlist. Surface peer of the CLI `mununu sv cdc-lint`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvCdcLintRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
}

/// One clock domain: a clock net, its edge and how many storage cells fire on it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ClockDomainView {
    pub net: String,
    pub posedge: bool,
    pub cells: usize,
}

/// One `sv cdc-lint` row: a crossing, a dual-clock memory or a reconvergence point.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CdcCrossingView {
    /// `combinational-before-sync` | `unsynchronized` | `multi-bit-bus` | `qualified-data` |
    /// `reconvergence` | `dual-clock-memory` | `synchronized`.
    pub kind: String,
    /// `holds` (a safe crossing) | `violated` (an unsafe one) | `unknown` (safe only under the
    /// functional condition `detail` names).
    pub outcome: String,
    pub source: String,
    pub source_clock: String,
    pub destination: String,
    pub destination_clock: String,
    /// Synchroniser depth (`synchronized`, `multi-bit-bus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<usize>,
    /// The combinational cells between source and destination, with source locations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    pub detail: String,
}

/// Response of `POST /api/v1/sv/cdc-lint` and the JSON of `mununu sv cdc-lint`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SvCdcLintResponse {
    /// The primary source file (CLI only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The worst row outcome (`holds` when there is no crossing).
    pub outcome: String,
    pub domains: Vec<ClockDomainView>,
    pub crossings: Vec<CdcCrossingView>,
    /// What the lint does not see (primary inputs, asynchronous pins, black boxes).
    pub notes: Vec<String>,
}

impl From<&crate::adapter::cdc_lint::CdcReport> for SvCdcLintResponse {
    fn from(r: &crate::adapter::cdc_lint::CdcReport) -> Self {
        SvCdcLintResponse {
            file: None,
            outcome: r.worst().as_str().to_string(),
            domains: r
                .domains
                .iter()
                .map(|d| ClockDomainView {
                    net: d.net.clone(),
                    posedge: d.posedge,
                    cells: d.cells,
                })
                .collect(),
            crossings: r
                .crossings
                .iter()
                .map(|c| CdcCrossingView {
                    kind: c.kind.as_str().to_string(),
                    outcome: c.outcome.as_str().to_string(),
                    source: c.source.clone(),
                    source_clock: c.source_clock.clone(),
                    destination: c.destination.clone(),
                    destination_clock: c.destination_clock.clone(),
                    stages: c.stages,
                    path: c.path.clone(),
                    detail: c.detail.clone(),
                })
                .collect(),
            notes: r.notes.clone(),
        }
    }
}

/// One `sv lint` finding — a named signal a structural check flagged. Element of
/// [`SvLintResponse`].
#[derive(Debug, Serialize)]
pub struct SvLintFinding {
    /// The offending signal's symbol (register name or a combinational output of one).
    pub signal: String,
    /// `"register"` (the root — the register itself) | `"output"` (a downstream
    /// combinational output that reads it).
    pub kind: String,
    /// Which check fired:
    /// `"undriven-partial-write"` — the partial-write lift reaches a free input,
    /// so a state predicate over the signal would be *refused* by the verifier;
    /// `"registered-array-read-moving-address"` (mununu#496) — a registered array
    /// read whose address register can change in the same cycle its data is
    /// consumed, with nothing recording the correspondence.
    pub rule: String,
    /// Human-readable specifics (the address register and the fix, for the
    /// array-read rule). Absent when the rule needs no elaboration.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// Response for `POST /api/v1/sv/lint`.
#[derive(Debug, Serialize)]
pub struct SvLintResponse {
    /// Number of named signals flagged (registers + downstream outputs).
    pub signals_flagged: usize,
    /// Number of the flagged signals that are state registers (the root findings).
    pub registers_flagged: usize,
    /// Per-signal findings (sorted by rule, then by name).
    pub findings: Vec<SvLintFinding>,
}

/// Request for `POST /api/v1/sv/mutate` (#468) — apply a NAMED structural mutation
/// and re-verify to check the property verdicts flip (property adequacy). Surface
/// peer of the CLI `mununu sv mutate`. Pinned by `docs/api-schemas/sv-mutate-request.schema.json`
/// (mununu#725).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvMutateRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// The mutation to apply, one of four kinds (mununu#635): `"stick:<reg>"`
    /// (freeze a register), `"drop-reset:<reg>"` (remove a register's reset arm),
    /// `"off-by-one:<reg>[@<const_nid>][:±1]"` (shift a constant the register is
    /// compared against), `"invert-cond:<sig>"` (negate a 1-bit signal). The `_`
    /// spellings the `list` response uses as keys are accepted as aliases.
    /// Required unless `list` is true.
    #[serde(default)]
    pub mutation: Option<String>,
    /// When true, ignore `mutation` and return the available mutation TARGETS
    /// (the `--list` discovery mode) instead of running a mutation.
    #[serde(default)]
    pub list: bool,
    /// Engine selector (as `verify-auto`): `"explicit"` | `"symbolic"` |
    /// `"exact-symbolic"` | `"portfolio-sequential"` | `"portfolio-parallel"`.
    /// Default (unspecified) ⇒ `portfolio-sequential`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Reset-gating (pin the reset inactive + inject the post-reset init). Default `true`.
    #[serde(default)]
    pub gate_reset: Option<bool>,
    /// Must-edge inference (`"smt-hyper-must"` gives sound νμ verdicts for a
    /// recoverability property under `drop-reset`). Default (absent or `"auto"`,
    /// mununu#663): per-target must for Liveness/Mixed-class properties, may-only for the
    /// rest; `"off"` = may-only everywhere.
    #[serde(default)]
    pub must_edge_inference: Option<String>,
    /// mununu#725 — config concretization for the baseline AND the mutant, as
    /// `sv/verify-auto`'s `config_values`: each entry `"signal=value"` pins a config
    /// input to a constant (e.g. `"rst_ni=1"`). A malformed entry or a non-decimal
    /// value is a 400 before the lift. Mirrors the CLI `--pin`. Default empty.
    #[serde(default)]
    pub pins: Vec<String>,
    /// Fault-injection posture: registers the adversary may overwrite (`"REG"` / `"REG:k"`,
    /// one global budget, default 1; `"REG:unbounded"`). Mirrors the CLI `--fault`. Default empty.
    #[serde(default)]
    pub faults: Vec<String>,
    /// Fault-injection posture from the design's countermeasures: `"sec-cm"` / `"sec-cm:k"`
    /// (`"sec-cm:0"` selects and runs the antecedent-vacuity check only). Alternative to
    /// `faults`; both is a 400. Mirrors the CLI `--fault-model`.
    #[serde(default)]
    pub fault_model: Option<String>,
    /// Cycles before the first injection may fire (default 10). Mirrors `--fault-warmup`.
    #[serde(default)]
    pub fault_warmup: Option<u32>,
}

/// The mutation targets available in a design (the `list` response payload).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvMutateTargets {
    /// Every named register — a `stick:<reg>` target.
    pub stick: Vec<String>,
    /// Every named register whose next is a reset mux — a `drop-reset:<reg>` target.
    pub drop_reset: Vec<String>,
    /// Every named register compared against a constant — an `off-by-one:<reg>` target.
    pub off_by_one: Vec<String>,
    /// Every named 1-bit signal — an `invert-cond:<sig>` target.
    pub invert_cond: Vec<String>,
}

/// One property's baseline-vs-mutant verdict in a [`SvMutateResponse`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvMutatePropertyFlip {
    /// The property (assertion) name.
    pub name: String,
    /// Canonical verdict on the design as-lifted.
    pub baseline: String,
    /// Canonical verdict on the mutated design.
    pub mutant: String,
    /// `baseline != mutant` — the mutation was caught by this property.
    pub flipped: bool,
}

/// Response for `POST /api/v1/sv/mutate`. Pinned by
/// `docs/api-schemas/sv-mutate-response.schema.json` (mununu#725).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvMutateResponse {
    /// The applied mutation's label (null on a `list` request).
    pub mutation: Option<String>,
    /// Available targets (present on a `list` request, null otherwise).
    pub targets: Option<SvMutateTargets>,
    /// Per-property flips (empty on a `list` request).
    pub properties: Vec<SvMutatePropertyFlip>,
    /// Count of properties the mutation flipped (caught it).
    pub flipped: usize,
    /// Count the mutation did NOT flip — the property-adequacy finding.
    pub unflipped: usize,
}

/// cegar-extraction Stage 2 (2026-06-22) — request for the SV-direct
/// CEGAR endpoint (`POST /api/v1/sv/cegar`). Mirrors the CLI
/// `mununu sv cegar`: lifts SystemVerilog to a single flattened BTOR2
/// (sv2v + Yosys) in one call, then runs the same predicate-abstraction
/// refinement loop as `/btor2/cegar` and returns the same
/// [`Btor2CegarResponse`]. The CEGAR fields below are identical to
/// [`Btor2CegarRequest`]; only the source half differs (SV + Yosys
/// options instead of raw BTOR2 `content`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SvCegarRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// Yosys `setundef -anyseq` (per-cycle havoc on undefined nets).
    /// Mirrors the CLI `--setundef-anyseq`.
    #[serde(default)]
    pub setundef_anyseq: bool,
    /// Yosys `setundef -anyconst` (one nondeterministic constant input
    /// per undefined bit — the Caliptra CWE-1245 power-up policy).
    /// Mirrors the CLI `--setundef-anyconst`.
    #[serde(default)]
    pub setundef_anyconst: bool,

    // --- CEGAR parameters (identical to Btor2CegarRequest) ---
    /// μ-calculus formula evaluated over the lifted KMTS.
    pub formula: String,
    /// Initial predicate set (bootstraps the `2^|P|` cube space).
    pub predicates: Vec<PredicateSpecRequest>,
    /// R.6.6 controllability split — controller-driven input symbols.
    #[serde(default)]
    pub controllable: Vec<String>,
    /// Predicate-discovery source: `"wp"` (default) | `"craig"`.
    #[serde(default)]
    pub predicate_source: Option<String>,
    /// Max CEGAR iterations (default 16).
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Must-edge inference policy (default `"off"`).
    #[serde(default)]
    pub must_edge_inference: Option<String>,
    /// May-edge inference policy (default `"off"`).
    #[serde(default)]
    pub may_edge_inference: Option<String>,
    /// R-S8 symbolic-init config-values (`"REG=v1,v2,..."`).
    #[serde(default)]
    pub init_values: Vec<String>,
    /// CTXDSL Phase 2 — opt-in CTXDSL of the final refined model + formula.
    #[serde(default)]
    pub emit_ctxdsl: bool,
    /// R-F5.4.2b (2026-07-03) — predicate-cube engine: `"explicit"` (default)
    /// or `"symbolic"` (R-F5 BDD relation, single-shot). Mirrors `--engine`.
    #[serde(default)]
    pub engine: Option<String>,
}

/// XL.6a — SVA-extraction endpoint (`POST /api/v1/sv/extract-sva`). Mirrors the
/// CLI `mununu sv extract-sva`: runs the slang SVA front-end over the SV
/// source(s) and returns the translated mu-calculus property set. slang is a
/// full SV-2017 parser, so no sv2v / Yosys / `top` is needed; no model
/// verification happens here (that is `/sv/verify-auto`).
#[derive(Debug, Deserialize)]
pub struct SvExtractSvaRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / `include` targets — e.g. the standard
    /// OpenTitan `prim_assert` macros), staged alongside + on the include path.
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
}

/// Response for `/api/v1/sv/extract-sva` — mirrors
/// [`crate::adapter::slang::translate::TranslationReport`].
#[derive(Debug, Serialize)]
pub struct SvExtractSvaResponse {
    pub translated: Vec<TranslatedAssertionView>,
    pub unsupported: Vec<UnsupportedAssertionView>,
    pub required_shadows: Vec<ShadowSignalView>,
}

/// A successfully translated assertion (mirrors `TranslatedAssertion`).
#[derive(Debug, Serialize)]
pub struct TranslatedAssertionView {
    pub name: String,
    /// `"assert"` | `"assume"` | `"cover"`.
    pub kind: String,
    /// mu-calculus formula (parses via the evaluator's parser).
    pub formula: String,
    /// XL.2 `AG EF` recoverability companion — `Some` only for covers.
    pub recoverability_companion: Option<String>,
}

/// An assertion outside the supported fragment (mirrors `UnsupportedAssertion`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct UnsupportedAssertionView {
    pub name: String,
    pub kind: Option<String>,
    pub reason: String,
}

/// A `__past` shadow register a translated formula needs (mirrors `ShadowSignal`).
#[derive(Debug, Serialize)]
pub struct ShadowSignalView {
    pub base: String,
    pub width: u32,
    /// Deepest `$past` history depth synthesised for `base` (1 for the
    /// depth-1 family; `k` for `$past(base, k)`).
    pub depth: u32,
}

/// XL.6b — automated SVA verification endpoint (`POST /api/v1/sv/verify-auto`).
/// Mirrors the CLI `mununu sv verify-auto`: extract the design's SVA, lift, and
/// verify each property against the model with no sidecar. slang parses SV
/// directly, but the verify lift uses sv2v + Yosys, so `top` / `use_sv2v` apply.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvVerifyAutoRequest {
    /// The SV design and how to lift it (the CLI's shared lift flags).
    pub lift: SvLiftRequest,
    /// Max CEGAR iterations per property (default 16).
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Must-edge inference per property. Default (absent or `"auto"`, mununu#663): a
    /// Liveness/Mixed-class formula — a μ under a ν, `AG EF` / `AG AF` — runs its cube
    /// member with `"smt-per-target-standard"` (its 3-valued `◇` turns true only through must edges;
    /// may-only is structurally ⊥ there), the rest may-only. `"off"` switches that resolution
    /// off; `"smt-per-target"`, `"smt-per-target-standard"`, `"smt-hyper-must"` apply to every
    /// property.
    #[serde(default)]
    pub must_edge_inference: Option<String>,
    /// Reset-gating: drop recognized `disable iff (reset)` guards and pin the
    /// reset input inactive at the model level (default `true`). Set `false`
    /// to keep the guard and leave the reset free.
    #[serde(default)]
    pub gate_reset: Option<bool>,
    /// Auto-inject behavioral stubs for cut flop primitives (e.g. OpenTitan's
    /// `prim_sparse_fsm_flop`) so the register survives the lift (default
    /// `true`). Set `false` to leave cut flops reported as black-boxed.
    #[serde(default)]
    pub auto_stub_flops: Option<bool>,
    /// H.J.b — config concretization: pin wide config inputs to constants so
    /// comparisons against them become decidable. Each entry `"signal=value"`
    /// (e.g. `"cfg_detect_timer_i=7"`). The verdicts are then SCOPED to these
    /// values (surfaced as a `config-concretization` note). Default empty.
    #[serde(default)]
    pub pins: Vec<String>,
    /// H.H — counter upper bounds: seed a `signal <= value` cube-partition to
    /// refine a counter-monotonicity property (`cnt_q >= $past(cnt_q)`) whose ⊥ is
    /// caused by the abstract 32-bit wraparound. Each entry `"signal<=value"` (the
    /// `"signal=value"` spelling is also accepted, same meaning). Sound (a
    /// partition, not an assumption — the must-edges verify it); requires
    /// `must_edge_inference` on. Bounds are also auto-derived from `config_values`;
    /// a manual entry here overrides the inferred one. Surfaced as a `counter-bound`
    /// note. Default empty.
    #[serde(default)]
    pub counter_bounds: Vec<String>,
    /// Abstraction-predicate hints, mirroring the CLI `--predicate` and the in-source
    /// `// @mununu_predicate <expr>` annotation (all three are merged). Each entry is a
    /// predicate expression (`"reg == value"`, `"reg == reg"`, `"reg >= K"`) seeded as a
    /// cube dimension for EVERY property, even when absent from the property formula.
    /// Sound by monotonicity — a hint only refines the cube (a ⊥ can become definite; a
    /// definite verdict never flips; an unclassifiable hint is dropped). Default empty.
    #[serde(default)]
    pub predicate: Vec<String>,
    /// Property templates to check besides the design's assertions, each `{template, args}`
    /// (`{"template": "rtl_response", "args": {"TRIGGER": "trip == 1", "RESPONSE": "state_q ==
    /// SCRAM"}}`), instantiated against the server's catalog. Each becomes a row labelled
    /// `tmpl.<instance>`. Default empty.
    #[serde(default)]
    pub templates: Vec<crate::adapter::templates::TemplateRef>,
    /// Return a Verilator replay testbench with every counterexample (`--emit-testbench`).
    #[serde(default)]
    pub emit_testbench: bool,
    /// The clock port the testbench toggles (default: the first of `clk`, `clk_i`, `clock`,
    /// `i_clk`, `aclk`, `clk_sys` the source declares).
    #[serde(default)]
    pub tb_clock: Option<String>,
    /// Engine selector, mirroring the CLI `--engine`: `"explicit"`, `"symbolic"`,
    /// `"exact-symbolic"`, `"portfolio-sequential"`, `"portfolio-parallel"`.
    /// **Unspecified ⇒ the default `"portfolio-sequential"`** (2026-07-06): run
    /// exact → symbolic → explicit, stopping when every property is decided — the
    /// most precise sound choice, no slower than the former `explicit` default on
    /// designs `explicit` already decided. Resolved by `verify_auto::engine_selection`.
    #[serde(default)]
    pub engine: Option<String>,
    /// The ⊥ escalations (default `true`, the CLI's `--no-rescue` sets `false`): a property the
    /// cube leaves ⊥ is retried by the lane for its shape — a reducible safety invariant by the
    /// reachability portfolio, a box-AF response by liveness-to-safety plus the portfolio, an
    /// `AG EF (REG == VALUE)` recoverability by the cube with `smt-hyper-must`. `false` reports
    /// the cube's ⊥ unchanged.
    #[serde(default)]
    pub rescue: Option<bool>,
    /// mununu#629 — antecedent constant propagation on the exact-symbolic engine (default
    /// `true`): a same-cycle implication `sig == K |-> C` has `K` propagated through `C`'s cone
    /// (a read mux folds to the selected arm), so a property about one field of a wide packed
    /// record decides instead of skipping on the bit cap. `false` leaves the formula as lifted
    /// (the differential-oracle / debug knob; the CLI's `--no-antecedent-propagate`,
    /// `MUNUNU_NO_ANTECEDENT_PROPAGATE=1`).
    #[serde(default)]
    pub antecedent_propagate: Option<bool>,
    /// mununu#656 — inputs in the state on the exact-symbolic engine (default `true`): a property
    /// that reads a primary input, or a signal combinationally driven by one, is decided over the
    /// registers with that input sampled in the same cycle. `false` restores the engine's refusal
    /// of such atoms (the differential-oracle knob; the CLI's `--no-input-state`,
    /// `MUNUNU_NO_INPUT_STATE=1`).
    #[serde(default)]
    pub input_state: Option<bool>,
    /// Fault-injection posture: registers the adversary may overwrite (`"REG"` / `"REG:k"`,
    /// one global budget, default 1; `"REG:unbounded"`). Mirrors the CLI `--fault`. Default empty.
    #[serde(default)]
    pub faults: Vec<String>,
    /// Fault-injection posture from the design's countermeasures: `"sec-cm"` / `"sec-cm:k"`
    /// (`"sec-cm:0"` selects and runs the antecedent-vacuity check only). Alternative to
    /// `faults`; both is a 400. Mirrors the CLI `--fault-model`.
    #[serde(default)]
    pub fault_model: Option<String>,
    /// Cycles before the first injection may fire (default 10). Mirrors `--fault-warmup`.
    #[serde(default)]
    pub fault_warmup: Option<u32>,
    /// How SVA `assume` statements are treated: `"precondition"` (default — the guarantees are
    /// checked on the traces along which every applied assumption has held), `"check"` (each
    /// assumption is checked like an assertion and constrains nothing) or `"ignore"`. Mirrors the
    /// CLI's `--assumes`. Any other value is a 400.
    #[serde(default)]
    pub assumes: Option<String>,
    /// mununu#537 — assert that this run produces the verdicts the caller claims. Omitted ⇒ no
    /// claim, and the response carries no `expectations` block.
    ///
    /// The HTTP peer of the CLI's `--expect-all-hold` / `--expect-count` / `--expect-violated` /
    /// `--expect`. A gate driving mununu over HTTP wants the same three assertions a gate driving
    /// it on the command line does.
    #[serde(default)]
    pub expectations: Option<ExpectationsRequest>,
}

/// mununu#537 — the caller's claims about a run's verdicts.
///
/// There is deliberately **no** "tolerate undecided" field. Naming a property `unknown` in
/// `named` is the sound version of that wish: it pins the abstention as a claim that fails when
/// the property becomes decidable, instead of excusing it. See
/// [`crate::adapter::slang::expectations`] for the incident that settled this.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ExpectationsRequest {
    /// Every property must hold, with nothing unsupported, unknown or skipped.
    #[serde(default)]
    pub all_hold: bool,
    /// The property count must be exactly this — catches a binding that stopped binding, whose
    /// smaller all-green set otherwise reads as a clean pass.
    #[serde(default)]
    pub count: Option<usize>,
    /// These must be VIOLATED **and every other property must hold**.
    #[serde(default)]
    pub violated: Vec<String>,
    /// `name -> verdict` (`"holds"` | `"violated"` | `"unknown"` | `"skipped"`). Unnamed
    /// properties are ignored, except that an unnamed VIOLATED is still a failure.
    #[serde(default)]
    pub named: std::collections::BTreeMap<String, String>,
}

impl ExpectationsRequest {
    /// Convert to the core type. `Err` names the bad verdict spelling rather than dropping the
    /// claim, because a silently-dropped claim gates on nothing.
    pub fn to_expectations(
        &self,
    ) -> Result<crate::adapter::slang::expectations::Expectations, String> {
        use crate::adapter::slang::expectations::{Expectations, ExpectedVerdict};
        let mut named = Vec::new();
        for (name, verdict) in &self.named {
            let v = ExpectedVerdict::parse(verdict).ok_or_else(|| {
                format!(
                    "expectations.named[{name}]: `{verdict}` is not a verdict — use holds |                      violated | unknown | skipped"
                )
            })?;
            named.push((name.clone(), v));
        }
        Ok(Expectations {
            all_hold: self.all_hold,
            count: self.count,
            violated: self.violated.clone(),
            named,
        })
    }
}

/// Response for `/api/v1/sv/verify-auto`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvVerifyAutoResponse {
    pub properties: Vec<PropertyVerdictView>,
    /// Assertions that did not translate (reuses the extract-sva view shape;
    /// `kind` is `None` here).
    pub unsupported: Vec<UnsupportedAssertionView>,
    /// Model-level diagnostics: state-register count + black-boxed (cut)
    /// modules. Lets a SKIPPED outcome point at its root cause.
    pub diagnostics: ModelDiagnosticsView,
    /// H.J — human-facing provenance notes: every abstraction / scoping decision
    /// the run made (config concretizations, reset-gating, flop stubs, cut
    /// modules, the abstraction posture, the coverage summary), so a verdict's
    /// scope and caveats are explicit in the payload.
    #[serde(default)]
    pub notes: Vec<VerificationNoteView>,
    /// mununu#537 — the result of checking this run against the caller's declared expectations.
    /// `None` (and omitted) when no expectations were declared, in which case the ordinary
    /// verdict gate applies.
    ///
    /// Present so a consumer learns **which** claim failed without parsing prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expectations: Option<ExpectationResultView>,
    /// OpenTitan coverage, Phase 4 — what the run did with the design's SVA `assume`
    /// statements. Omitted when the design has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumptions: Option<AssumptionsView>,
    /// mununu#715 — the full commit sha of the binary that produced this report. Omitted when
    /// the build was not told its commit; never a guess. The `--version` line carries its short
    /// form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// mununu#715 — `true` when no tracked file differed from `build` at build time.
    /// Present exactly when `build` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_clean: Option<bool>,
}

/// What `sv verify-auto` did with the design's `assume` statements.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AssumptionsView {
    /// `precondition` (the default: guarantees are checked on the traces that satisfy the
    /// applied assumptions), `check` (assumptions are checked like assertions) or `ignore`.
    pub mode: String,
    /// Assumptions applied as preconditions.
    pub applied: Vec<String>,
    /// Assumptions not applied, with the reason. The guarantees were checked without them.
    pub unapplied: Vec<AssumptionNotAppliedView>,
    /// Guarantees checked WITHOUT the assumptions (their formula could not be relativized).
    pub unguarded: Vec<AssumptionNotAppliedView>,
    /// `consistent`, `contradictory` (no trace satisfies every applied assumption: every HOLDS
    /// is vacuous) or `unknown`; omitted when nothing was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consistency: Option<String>,
}

/// A named item with the reason it was not applied / not relativized.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AssumptionNotAppliedView {
    pub name: String,
    pub reason: String,
}

/// mununu#537 — one unmet claim. `property` is absent for a run-level claim (the count).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ExpectationFailureView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    pub reason: String,
}

/// mununu#537 — the verdict on a caller's declared expectations.
///
/// `satisfied: false` corresponds to CLI exit code **4** — "a verdict was not what you claimed",
/// deliberately distinct from a failed run (1) and from the ordinary verdict gate (2 / 3).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ExpectationResultView {
    pub satisfied: bool,
    pub failures: Vec<ExpectationFailureView>,
}

impl From<&crate::adapter::slang::expectations::ExpectationResult> for ExpectationResultView {
    fn from(r: &crate::adapter::slang::expectations::ExpectationResult) -> Self {
        Self {
            satisfied: r.satisfied,
            failures: r
                .failures
                .iter()
                .map(|f| ExpectationFailureView {
                    property: f.property.clone(),
                    reason: f.reason.clone(),
                })
                .collect(),
        }
    }
}

/// mununu#536 — the ONE report→wire conversion, shared by the HTTP handler and the CLI's
/// `--json`.
///
/// # Why this exists
///
/// There used to be two hand-written serializers for the same report: the API handler built
/// [`SvVerifyAutoResponse`] field by field, and the CLI built a `serde_json::json!` literal.
/// Only the API shape was described by `docs/api-schemas/sv-verify-auto-response.schema.json`
/// and guarded by the drift test, so **a consumer reading the published schema and then
/// running the CLI got a different document** — and the two had already drifted: `detail` was
/// an object on one and a string on the other, the CLI dropped `unreachable_target`, and
/// neither carried the front end used.
///
/// Two serializers for one documented shape cannot be kept in step by discipline; the fix is
/// for there to be one. With both surfaces going through here, the schema drift test covers
/// the CLI output too, because it is literally the same type.
impl From<&crate::adapter::slang::verify_auto::AutoVerifyReport> for SvVerifyAutoResponse {
    fn from(report: &crate::adapter::slang::verify_auto::AutoVerifyReport) -> Self {
        use crate::adapter::slang::translate::SvaKind;
        use crate::adapter::slang::verify_auto::{NoteLevel, VerifyOutcome};

        let cells = |v: &[Vec<(String, crate::adapter::btor2::cex_value::CexValue)>]| -> Vec<Vec<CexCellView>> {
            v.iter()
                .map(|st| {
                    st.iter()
                        .map(|(register, value)| CexCellView {
                            register: register.clone(),
                            value: value.as_u64(),
                            hex: value.to_hex(),
                            bits: value.bits(),
                        })
                        .collect()
                })
                .collect()
        };

        let properties = report
            .properties
            .iter()
            .map(|p| {
                // `detail` stays prose for existing consumers; the structured fields beside it
                // are what a gate should read.
                let (outcome, detail, false_cells, unknown_cells, skip_reason) = match &p.outcome {
                    VerifyOutcome::Holds => ("holds", None, None, None, None),
                    VerifyOutcome::Violated { false_cells } => (
                        "violated",
                        Some(format!("{false_cells} cell(s)")),
                        Some(*false_cells),
                        None,
                        None,
                    ),
                    VerifyOutcome::Unknown { unknown_cells } => (
                        "unknown",
                        Some(format!("{unknown_cells} cell(s)")),
                        None,
                        Some(*unknown_cells),
                        None,
                    ),
                    VerifyOutcome::Skipped { reason } => (
                        "skipped",
                        Some(reason.clone()),
                        None,
                        None,
                        Some(reason.clone()),
                    ),
                };
                PropertyVerdictView {
                    name: p.name.clone(),
                    label: p.label.clone(),
                    kind: match p.kind {
                        SvaKind::Assert => "assert",
                        SvaKind::Assume => "assume",
                        SvaKind::Cover => "cover",
                    }
                    .to_string(),
                    formula: p.formula.clone(),
                    outcome: outcome.to_string(),
                    detail,
                    false_cells,
                    unknown_cells,
                    skip_reason,
                    decided_by: p.decided_by.clone(),
                    vacuity: p.vacuity.as_ref().map(|v| VacuityView {
                        kind: v.kind.tag().to_string(),
                        witness: v.witness.clone(),
                        detail: v.detail.clone(),
                    }),
                    // A `⊥` ALWAYS carries a reason, with `unclassified-bottom` as the floor.
                    //
                    // `skip_serializing_if = "Option::is_none"` omits the key entirely, so `None`
                    // is indistinguishable from a field that was never added — which is precisely
                    // the ambiguity this field exists to remove, reproduced in its own
                    // serialization. A consumer hit it within hours: they had to report "the key is
                    // absent" rather than "the reason is unclassified", and those are different
                    // statements about the engine that they could not tell apart.
                    //
                    // So an undecided property with no classification gets the floor rather than
                    // nothing. Absence now means only one thing: the outcome is definite.
                    bottom_reason: p
                        .bottom_reason
                        .clone()
                        .or_else(|| {
                            matches!(p.outcome, VerifyOutcome::Unknown { .. })
                                .then_some(crate::adapter::slang::verify_auto::BottomReason::UnclassifiedBottom)
                        })
                        .as_ref()
                        .map(|r| {
                        use crate::adapter::slang::verify_auto::BottomReason;
                        BottomReasonView {
                            kind: r.tag().to_string(),
                            detail: r.one_line(),
                            engine: match r {
                                BottomReason::EngineDidNotComplete { engine, .. } => {
                                    Some(engine.clone())
                                }
                                _ => None,
                            },
                            determinism: r.determinism().tag().to_string(),
                            budget: r.engine_budget().map(|b| b.tag().to_string()),
                            budget_knob: r
                                .engine_budget()
                                .and_then(|b| b.knob())
                                .map(str::to_string),
                            registers: match r {
                                BottomReason::UnestablishedInitialState { registers, .. }
                                | BottomReason::UnestablishedInitialCubes { registers, .. } => {
                                    Some(*registers)
                                }
                                _ => None,
                            },
                        }
                    }),
                    seeded_predicates: p.seeded_predicates.clone(),
                    trace: p.counterexample.as_ref().map(|c| {
                        Trace::from_cells(
                            cells(&c.prefix),
                            cells(&c.cycle),
                            cells(&c.inputs),
                            c.unreachable_target.clone(),
                            p.decided_by.as_deref()
                                == Some(crate::adapter::slang::verify_auto::TRAP_CERTIFICATE_DECIDED_BY),
                        )
                    }),
                }
            })
            .collect();

        let a = &report.assumptions;
        let not_applied = |v: &[(String, String)]| -> Vec<AssumptionNotAppliedView> {
            v.iter()
                .map(|(name, reason)| AssumptionNotAppliedView {
                    name: name.clone(),
                    reason: reason.clone(),
                })
                .collect()
        };
        let assumptions = (!a.mode.is_empty()).then(|| AssumptionsView {
            mode: a.mode.clone(),
            applied: a.applied.clone(),
            unapplied: not_applied(&a.unapplied),
            unguarded: not_applied(&a.unguarded),
            consistency: a.consistency.clone(),
        });
        let build = crate::build_info::current();
        SvVerifyAutoResponse {
            build: build.map(|b| b.sha.clone()),
            build_clean: build.map(|b| b.clean),
            assumptions,
            properties,
            unsupported: report
                .unsupported
                .iter()
                .map(|(name, reason)| UnsupportedAssertionView {
                    name: name.clone(),
                    // The core report flattens to `(name, reason)`, so the SVA kind is already
                    // gone by the time it reaches here — always `None`, not "unknown".
                    kind: None,
                    reason: reason.clone(),
                })
                .collect(),
            diagnostics: ModelDiagnosticsView {
                state_register_count: report.diagnostics.state_register_count,
                blackboxed_modules: report.diagnostics.blackboxed_modules.clone(),
                gated_resets: report.diagnostics.gated_resets.clone(),
                auto_provided_stubs: report.diagnostics.auto_provided_stubs.clone(),
                frontend: report.diagnostics.frontend.clone(),
                frontend_fallback_reason: report.diagnostics.frontend_fallback_reason.clone(),
                lift_retry: report.diagnostics.lift_retry.clone(),
                merged_clock_domains: report.diagnostics.merged_clock_domains.clone(),
                config_values: report.diagnostics.applied_config_values.clone(),
                cutpoints: report.diagnostics.cutpoint_signals.clone(),
                fault_model: report.diagnostics.fault_model.clone(),
                fault_sites: report
                    .diagnostics
                    .fault_sites
                    .iter()
                    .map(|s| FaultSiteView {
                        register: s.register.clone(),
                        width: s.width,
                        bound: s.bound.clone(),
                        selected_by: s.selected_by.clone(),
                        rows: s.rows.clone(),
                    })
                    .collect(),
            },
            // Expectations are not a property of the report — they are the caller's claim about
            // it — so the conversion leaves this None and the caller fills it in.
            expectations: None,
            notes: report
                .notes
                .iter()
                .map(|n| VerificationNoteView {
                    kind: n.kind.clone(),
                    level: match n.level {
                        NoteLevel::Info => "info",
                        NoteLevel::ScopeCaveat => "scope-caveat",
                        NoteLevel::SoundnessCaveat => "soundness-caveat",
                    }
                    .to_string(),
                    summary: n.summary.clone(),
                    detail: n.detail.clone(),
                    items: n.items.clone(),
                    property: n.property.clone(),
                })
                .collect(),
        }
    }
}

/// mununu#541 — the `sv verify-recoverability` CLI report, schema-pinned.
///
/// # Why these exist as types
///
/// All three `sv` property verbs printed JSON before this — built inline with `serde_json::json!`,
/// so the field set was whatever the format string happened to say that week. rosf consumes mununu
/// as a subprocess and asked for *"the schema-pinned document, never the transcript"*; a document
/// with no type behind it is the transcript in a different font.
///
/// Typed here rather than in the CLI so the **same** shape is what `schemars` derives, what
/// `docs/api-schemas/` publishes, and what the drift detector guards — the arrangement
/// `SvVerifyAutoResponse` already has. A field added to the struct without regenerating the schema
/// fails CI.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvVerifyRecoverabilityReport {
    /// The design file as given on the command line.
    pub file: String,
    /// The property, rendered — e.g. `"AG EF (state == 0)"`.
    pub property: String,
    /// `holds` | `violated` | `unknown` | `skipped`, the same vocabulary `sv verify-auto` uses.
    pub outcome: String,
    /// Present only under `--refine` / `--partition` / `--discover-assumptions`: the
    /// refinement record. Free-form because `VerdictRefinement` is its own evolving surface —
    /// pinning its internals here would couple two schemas that change for different reasons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement: Option<serde_json::Value>,
    /// Which rung decided the canonical verdict — `exact` | `exact-reach-restricted` |
    /// `cube-ladder` (see the API response of the same name). Absent only when no engine ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The exact engine's typed abstention when the cube ladder was what decided (`bit-cap`,
    /// `arena-overflow`, `iteration-budget`, …). Absent when the exact engine decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_abstention: Option<String>,
}

/// mununu#541 — the `sv verify-liveness` CLI report, schema-pinned. See
/// [`SvVerifyRecoverabilityReport`] for why these are types.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvVerifyLivenessReport {
    /// The design file as given on the command line.
    pub file: String,
    /// The property, rendered — e.g. `"AG((req == 1) -> AF (ack == 1))"`.
    pub property: String,
    /// `holds` | `violated` | `unknown` | `skipped`.
    pub outcome: String,
    /// Which portfolio engines reached a conclusion. Empty when none did, which is not the same as
    /// the property being undecided for a modelling reason — read `outcome` for that.
    pub decided_by: Vec<String>,
}

/// mununu#541 — the `sv verify-liveness --response` CLI report, schema-pinned.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvVerifyLivenessAllReport {
    /// The design file as given on the command line.
    pub file: String,
    /// The conjunction, rendered as one property string.
    pub property: String,
    /// The verdict over the WHOLE conjunction — `holds` only if every response does.
    pub outcome: String,
    /// Per-response provenance, so a failing conjunct is identifiable rather than being hidden
    /// behind one aggregate verdict.
    pub responses: Vec<SvLivenessResponseView>,
}

/// One response of a `verify-liveness --response` conjunction (mununu#541).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SvLivenessResponseView {
    /// The response as written on the command line (`"req == 1 => ack == 1"`).
    pub response: String,
    /// Which engines reached a conclusion for this conjunct.
    pub decided_by: Vec<String>,
}

/// One provenance note (mirrors
/// [`crate::adapter::slang::verify_auto::VerificationNote`]).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct VerificationNoteView {
    /// Machine-stable kebab category (e.g. `"config-concretization"`).
    pub kind: String,
    /// `"info"` | `"scope-caveat"` | `"soundness-caveat"`.
    pub level: String,
    /// One-line human summary.
    pub summary: String,
    /// Longer explanation (the why + the soundness/scope implication).
    pub detail: String,
    /// Structured operands (e.g. `["cfg_detect_timer_i=7"]`).
    pub items: Vec<String>,
    /// mununu#548 O-2 — the property this note is ABOUT, when it is about one.
    ///
    /// Absent for a model-level note (reset-gating, control-slice, parameter overrides): those
    /// describe the lift, not a property.
    ///
    /// **Group notes by this, not by parsing `summary`.** The name used to appear only as a
    /// `"<name>: "` prefix inside the prose, so a consumer joining on it inherited two bugs: a
    /// property whose name prefixes another's (`sva_1` vs `sva_10`) matched the wrong note, and any
    /// rewording of the note's opening silently broke the join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
}

/// Model-level lift diagnostics (mirrors
/// [`crate::adapter::slang::verify_auto::ModelDiagnostics`]).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ModelDiagnosticsView {
    /// Number of state register lines in the lifted model.
    pub state_register_count: usize,
    /// Modules instantiated without a body, cut to free inputs (registers they
    /// drive are not modeled as state). Empty for a self-contained design.
    pub blackboxed_modules: Vec<String>,
    /// Reset inputs pinned inactive at the model level (`"<signal>=<value>"`),
    /// with their `disable iff` guards dropped from the formulas. Empty when
    /// reset-gating is off or no `disable iff` reset was recognized.
    pub gated_resets: Vec<String>,
    /// Cut flop-primitive modules for which a behavioral stub was auto-injected
    /// so the register survives the lift (e.g. `prim_sparse_fsm_flop`).
    pub auto_provided_stubs: Vec<String>,
    /// #466 / mununu#536 — the RTL front end that produced the lift (e.g.
    /// `"read_verilog + sv2v"`, `"yosys-slang (read_slang)"`). `--frontend auto` picks one
    /// silently and the two paths differ in SOUNDNESS on partial writes (#464/#465), so
    /// "which lift produced this verdict?" is a question the payload must answer. It was
    /// previously reachable only as `lift-frontend` note prose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontend: Option<String>,
    /// #466 — when `--frontend auto` FELL BACK (an earlier front end errored), the prior
    /// attempt's error. `None` when there was no fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontend_fallback_reason: Option<String>,
    /// The lift rerun its yosys script with `opt_clean` before `write_btor` after the BTOR
    /// backend reported a topological loop (yosys-slang `$buf` cells; the model is the same
    /// design) — the reason, else `None`. The `lift-retry` note carries the same text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_retry: Option<String>,
    /// The clock domains `merge_clock_domains` lifted onto one clock, each
    /// `"<net> (<edge>, <n> cells)"`; empty for a single-clock design. Non-empty means every
    /// verdict assumes the domains tick together (the `clock-domains-merged` note).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_clock_domains: Vec<String>,
    /// mununu#536 — config inputs pinned for this run, each `"<signal>=<value>"`. A verdict
    /// under a pin is a claim about THAT configuration only, so the scope ships with it.
    #[serde(default)]
    pub config_values: Vec<String>,
    /// mununu#536 — signals cut to free inputs by `--cutpoint`: the control slice these
    /// verdicts were reached under.
    #[serde(default)]
    pub cutpoints: Vec<String>,
    /// The fault-injection posture this run was under (`faults` / `fault_model`), as one label;
    /// `null` without one. A verdict under a posture is a claim about the design WITH an
    /// adversary at the listed sites, so the scope ships with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault_model: Option<String>,
    /// The registers the adversary may corrupt under the posture. Empty without one.
    #[serde(default)]
    pub fault_sites: Vec<FaultSiteView>,
}

/// One fault site of the posture (`diagnostics.fault_sites[]`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FaultSiteView {
    /// The register, as the model names it.
    pub register: String,
    /// Its width in bits (0 when the posture was not applied — a zero budget).
    pub width: u32,
    /// The global budget (`"≤1"`, `"unbounded"`).
    pub bound: String,
    /// `"explicit"` (named on the request), `"instance-prefix"` (a `sec-cm` site inside the
    /// fault flag's own instance) or `"cone-fallback"` (every cell the flag compares).
    pub selected_by: String,
    /// The security-countermeasure rows this site serves (`sec-cm` only).
    #[serde(default)]
    pub rows: Vec<String>,
}

/// monono ask 26 — WHY a property is `⊥`, machine-readable.
///
/// The complement to `unknown_cells`: that field says HOW MUCH of the cube space stayed
/// undecided, this one says WHY nothing decided it. A consumer previously had the quantity and
/// not the cause, so the only available move on a `⊥` was to try budget knobs blind.
///
/// Carried as a FIELD rather than a note deliberately: a consumer that does not know about a
/// field cannot silently filter it out, and a consumer that does not know about a note routinely
/// does — which is exactly how the equivalent `bottom-reason` note (shipped in mununu#492) never
/// reached the reader it was written for.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BottomReasonView {
    /// Stable kebab-case tag for gates. **Branch on this, never on the outcome alone** — every
    /// value below previously produced an identical `unknown` with `unknown_cells: 0`.
    ///
    /// | tag | what a gate should do |
    /// |---|---|
    /// | `budget-expired` | **WALL-CLOCK budget** — retry with more time |
    /// | `memory-ceiling-exceeded` | the process-RSS ceiling (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`). **More TIME cannot help** — needs more memory, a smaller cone, or a lower tier. Host-dependent *without* a clock: RSS varies with allocator state and neighbours |
    /// | `engine-did-not-complete` | branch on **`budget`** — it names which engine budget fired, and `detail` carries the engine's own numbers |
    /// | `engine-contradiction` | 🔴 **SOUNDNESS ALARM.** Two engines returned OPPOSITE definite verdicts; one is unsound. **Retrying is actively wrong.** Escalate, do not re-run |
    /// | `report-self-contradiction` | 🔴 **SOUNDNESS ALARM (mununu#579).** Two properties of the SAME model came back with definite verdicts that cannot both be right — one engine disagreeing with itself across properties, where `engine-contradiction` compares engines on one property. **Both** are withheld, deliberately: a contradiction says one verdict is broken, not which. **Retrying is actively wrong**, and so is believing whichever looks more plausible. Escalate |
    /// | `engine-crashed` | the engine process died. Not an abstention; report it |
    /// | `unestablished-initial-state` | 🔴 **the withheld VIOLATED.** The property's cone touches `registers` register(s) with no established cycle-0 value. A free start set over-approximates reachability, which licenses a definite `HOLDS` but never a definite `VIOLATED` — so the violation is withheld. **Retrying cannot help.** Since mununu#578 recoverable reset values are established automatically, so a register still free here has none to recover: it holds through reset, or it is a `--cutpoint` / blackboxed output (free at every cycle by construction). Supply an `init` via a sidecar, or drop the cutpoint |
    /// | `violation-over-free-leaves` | 🔴 **the withheld VIOLATED, again (mununu#750).** The violation was found through an undriven net — an unconnected wire or the output of a module missing from the source set, free at every cycle — so its trace (kept in `counterexample`) is a MAY-trace: the real driver may never produce those values. `detail` names the nets. **Retrying cannot help**; vendor the missing module or provide the driver. A deliberate cut — a `--cutpoint` or a `(* blackbox *)` stub — is not counted (mununu#578) |
    /// | `unestablished-initial-cubes` | **nothing decided (mununu#579).** The property's predicates have `registers` register(s) with no `init` line (plus any free inputs), and the cube path's initial cube set — the product of those free dimensions — is past the 16 it enumerates. An init-less register is free at cycle 0 on every engine; supply an `init` via a sidecar (or let reset-gating establish it) and re-run. **Retrying cannot help.** |
    /// | `safety-shape-not-reducible` | the property's SHAPE is outside the rescue lane — a bigger budget **cannot** help. Reshape, or add a reducer |
    /// | `no-state-model-non-safety` | zero state registers; a modelling issue, not an engine cap |
    /// | `not-attempted` | filtered out before any engine ran |
    /// | `unclassified-bottom` | cause not established — do **not** treat as distinct-from-abstain |
    ///
    /// A gate that retries every `unknown` with raised budgets spins forever on
    /// `safety-shape-not-reducible` and `no-state-model-non-safety`, and re-runs an unsoundness on
    /// `engine-contradiction`.
    pub kind: String,
    /// Human-readable one-liner. For `engine-did-not-complete` this is the ENGINE'S OWN abstention
    /// message, which names the budget it hit and its numbers — e.g. *"abstained on the ITERATION
    /// budget (1048577 > 1048576) … raise MUNUNU_BDD_ITER_BUDGET"*.
    pub detail: String,
    /// Portfolio label of the engine that failed, for `engine-did-not-complete` only.
    ///
    /// Describes a WHOLE-RUN failure over the design; it does **not** assert that this property
    /// caused it. The claim is that this property is `⊥` and the engine most likely to have
    /// decided it stopped for the stated reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// mununu#553 ask 2 — **would the same command on the same commit abstain again?**
    ///
    /// | value | what a gate should do |
    /// |---|---|
    /// | `reproducible` | a property of the PROBLEM. Pin the `unknown` honestly, or raise the budget `budget` names |
    /// | `host-dependent` | a property of the AFTERNOON. Treat as a **configuration result**, not a verdict about the design — the same command may DECIDE on a quieter machine |
    /// | `unestablished` | not determined. Do not read as either |
    ///
    /// This is the field that makes a `⊥` actionable without string-matching `detail`. Note that
    /// `budget-expired` (the 15-min per-property and 1-h whole-run harness clocks) is **on by
    /// default**, so `host-dependent` is reachable on an ordinary run with nothing configured.
    pub determinism: String,
    /// mununu#553 ask 1 — which engine budget fired, for `engine-did-not-complete` only:
    /// `iteration` · `node` · `bit-cap` · `arena-safety` · `fixpoint-latency` · `arena-exhausted`
    /// · `wall-clock` · `unrecognised`.
    ///
    /// Previously all eight arrived as one `kind` and could only be told apart by regexing
    /// `detail` — while the table above told gates to branch on `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    /// The env knob to raise for `budget`, when raising one is the right response
    /// (e.g. `MUNUNU_BDD_ITER_BUDGET`). Absent when no knob applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_knob: Option<String>,
    /// mununu#577 — how many registers in THIS property's cone have no established cycle-0 value,
    /// for `unestablished-initial-state` and `unestablished-initial-cubes` only.
    ///
    /// Per-property, from the property's own cone — not a whole-design count — so it is the number
    /// that sizes the remedy. It is a field for the same reason `budget` is: the count is otherwise
    /// only inside `detail`'s prose, and a gate that greps prose for a number is the defect
    /// mununu#553 was filed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registers: Option<usize>,
}

/// One property's auto-verification verdict (mirrors `PropertyVerdict`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PropertyVerdictView {
    /// Positional name: `<module>_sva_<index>` (or `ann_guarantee_<index>`). An insertion
    /// mid-file re-points every later one — prefer `label` when pinning an expectation.
    pub name: String,
    /// mununu#544 — the assertion's SV label, when it has one.
    ///
    /// This is what the author wrote (`a_reseed_only_at_a_span_edge:`), so it survives a reorder
    /// and a rename becomes a hard error instead of a silent re-point. Absent for an unlabelled
    /// assertion and for every `@mununu_guarantee` annotation (which carry no name today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `"assert"` | `"assume"` | `"cover"`.
    pub kind: String,
    pub formula: String,
    /// `"holds"` | `"violated"` | `"unknown"` | `"skipped"`.
    pub outcome: String,
    /// Human-readable detail: `"N cell(s)"` (violated/unknown) or the skip reason.
    ///
    /// **Prefer the structured fields below.** This is prose and stays only so existing
    /// consumers keep working; a gate that needs the figure should read `false_cells` /
    /// `unknown_cells` / `skip_reason` rather than parsing this string.
    pub detail: Option<String>,
    /// mununu#536 — the cell count behind a `violated` verdict, machine-readable.
    /// `None` for every other outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub false_cells: Option<usize>,
    /// mununu#536 — the cell count behind an `unknown` (⊥) verdict, machine-readable.
    /// `None` for every other outcome.
    ///
    /// This is the "cell/budget figure that accompanies a ⊥" the issue asks for: a ⊥ is a
    /// statement about how much of the cube space stayed undecided, and a consumer tracking
    /// whether refinement is helping needs the number, not a sentence containing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_cells: Option<usize>,
    /// mununu#536 — why a `skipped` property was not evaluated. `None` for every other
    /// outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// monono ask 26 — why an `unknown` (⊥) property was not decided. `None` for a definite
    /// outcome. See [`BottomReasonView`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom_reason: Option<BottomReasonView>,
    /// Which portfolio engine produced this property's DEFINITE verdict. `None` for `⊥`.
    ///
    /// **A verdict's PROVENANCE, per property.** The companion to `bottom_reason`: that says why a
    /// `⊥` happened, this says who answered when one did not.
    ///
    /// **Why a gate should read it even when the verdict is `holds`.** Measured: at high arena
    /// occupancy a run still returned `holds`, but the exact engine dropped out and an abstracting
    /// engine supplied the verdict — sound under that engine's posture, but the **precision tier
    /// moved silently**. A gate pinning `holds` sees `holds` either way. Comparing this field
    /// against the previous run is how a consumer notices.
    ///
    /// Previously available only as `decided-by:<engine>=<count>` in a report-level note: an
    /// AGGREGATE, which cannot say WHICH property changed tier, and filterable besides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    /// The cube predicates auto-seeded for this property (atom strings).
    pub seeded_predicates: Vec<String>,
    /// The counterexample ([`Trace`]) of a Violated property: for safety, the reset → violation
    /// path (empty `cycle`) native BMC re-derives whichever engine decided it (absent when none
    /// lies within the bound); D1.8b — a stall lasso for a bare `AF p` decided by the exact
    /// engine; A.4 — the unreachable target of a bare `EF p`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
    /// mununu#599 — a recoverability guarantee (`AG EF(P)`) that HOLDS, judged against the other
    /// verdicts in the same report. Present only on such a guarantee, and only when another
    /// property speaks to it; see [`VacuityView`].
    ///
    /// **Read this next to `outcome == "holds"`.** `AG EF good: HOLDS` beside its non-vacuity
    /// witness `EF good': VIOLATED` is the vacuous pass a two-sided gate exists to prevent; it used
    /// to be visible only to a reader who checked witnesses by habit. The verdict does not change
    /// — a vacuous HOLDS is a true HOLDS on the model — the field says what it is worth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vacuity: Option<VacuityView>,
}

/// mununu#599 — how a recoverability guarantee that HOLDS stands against the rest of its report.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct VacuityView {
    /// Machine-stable tag:
    ///
    /// | `kind` | meaning |
    /// |---|---|
    /// | `invariant-target` | **vacuous.** The target is invariant in this report — `EF(¬P)` VIOLATED or `AG(P)` HOLDS beside `AG EF(P)` HOLDS — so the design never leaves `P` and no recovery is ever exercised. Consistent; the HOLDS stands and says nothing about recovery. |
    /// | `witness-refuted` | **not shown non-vacuous.** A reachability over the guarantee's register at another value (`EF(st_q == S_WAIT)`) is VIOLATED in the same report. It does not make the guarantee false; if that property is the guarantee's non-vacuity witness, the pair is a failed gate, not a pass. |
    /// | `antecedent-unreachable` | **vacuous.** A security-countermeasure chain (`$rose(X) \|-> ##[0:k] alert`) HOLDS, and under the fault posture the reach portfolio proved `X` never rises on this model (the un-faulted lift's fault flags are synthesis constants). `witness` is the row itself. |
    pub kind: String,
    /// The property (`name`) whose verdict establishes it.
    pub witness: String,
    /// One line of prose. **Do not string-match it**; read `kind` and `witness`.
    pub detail: String,
}

/// U.0 — CEGAR refinement trace, JSON-shaped for the refinement-trace
/// viewer. Mirrors [`crate::adapter::btor2::cegar::CegarTrace`].
#[derive(Debug, Serialize)]
pub struct Btor2CegarResponse {
    pub success: bool,
    /// Per-iteration refinement records (iteration 0 = initial evaluation).
    pub iterations: Vec<CegarIterationView>,
    /// Predicate set at termination (initial + every added predicate).
    pub final_predicates: Vec<PredicateView>,
    /// Why the loop stopped: `"converged"` |
    /// `"bounded-iterations-reached"` | `"predicate-source-exhausted"` |
    /// `"budget-expired"` (mununu#504 — the wall clock ran out, as opposed to the refinement
    /// cap being reached; the verdict is the last completed iteration's and is sound).
    pub terminated_with: String,
    /// Cell-count summary of the final 3-valued verdict, over every cell (not only the initial
    /// ones), so it carries no single `outcome`.
    pub cells: CegarVerdictSummary,
    /// `true` when the eager `predicate_cube_lift` was used (R.2.5 MVP).
    pub lazy_lift_pending: bool,
    /// Whether prior-iteration approximants were threaded forward.
    pub approximant_reuse_enabled: bool,
    /// Soundness / advisory warnings produced during the run.
    pub warnings: Vec<String>,
    /// Track I.1 (2026-06-24) — cube cells that **falsify** the formula
    /// (definite-False), each decoded to its predicate valuation. Capped (the
    /// `verdict.false_cells` count is the full total). Empty unless the outcome
    /// is VIOLATED. Makes a failing verdict actionable ("falsified where
    /// `idle=false, err=true`").
    pub violating_cells: Vec<WitnessCellView>,
    /// Track I.1 — cube cells the abstraction **cannot decide** (`Unknown`/⊥),
    /// each decoded to its predicate valuation. Capped (`verdict.unknown_cells`
    /// is the full total). These are the cells a finer predicate set would need
    /// to resolve.
    pub undecided_cells: Vec<WitnessCellView>,
    /// Track I.1 (trace slice, 2026-06-24) — reachability countertrace for a
    /// VIOLATED verdict: a path of definite-False cube cells from an initial
    /// cell to a trap (or the farthest reachable failing cell). Omitted from
    /// the JSON unless the property is actually violated at the initial cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<Trace>,
    /// Track I.1 (undecided-explanation slice, 2026-06-24) — when the final
    /// verdict still carries ⊥ (unknown) cells, the registers the failure
    /// subgame flagged as load-bearing for those indefinite verdicts (deduped,
    /// from `CegarTrace::init_refinement_candidates`). The actionable "why
    /// undecided": adding predicates over these registers — or promoting their
    /// init policy — may resolve the abstraction. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refinement_candidates: Vec<String>,
    /// CTXDSL Phase 2 (2026-06-22) — the final refined cube model + the
    /// checked formula as CTXDSL, present only when the request set
    /// `emit_ctxdsl: true`. Omitted from the JSON otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctxdsl: Option<String>,
}

/// Track I.1 — a cube cell witnessing a non-HOLDS verdict, viewer-shaped:
/// the cube index plus the predicate valuation (`name → holds`) at that cell.
#[derive(Debug, Serialize)]
pub struct WitnessCellView {
    pub cube_index: usize,
    /// Predicate valuation at the cell (`name → holds`), keyed by predicate name.
    pub valuation: std::collections::BTreeMap<String, bool>,
}

/// One CEGAR iteration, viewer-shaped.
#[derive(Debug, Serialize)]
pub struct CegarIterationView {
    pub iteration: usize,
    /// Predicate-set size at the start of this iteration.
    pub predicate_count: usize,
    /// `true` iff this iteration's verdict carried `KleeneBot` cells
    /// (a failure subgame drove a refinement).
    pub had_failure_subgame: bool,
    /// Predicates the source added in response to this iteration.
    pub predicates_added: Vec<PredicateView>,
    /// Proxy counter for game-position evaluations (approximant-reuse
    /// diagnostics).
    pub game_position_evaluations: usize,
    /// Cell-count summary of this iteration's 3-valued verdict.
    pub cells: CegarVerdictSummary,
}

/// Cell counts of a 3-valued (Kleene) verdict over the cube space.
#[derive(Debug, Serialize)]
pub struct CegarVerdictSummary {
    /// KleeneT (definitely-true) cells.
    pub true_cells: usize,
    /// KleeneF (definitely-false) cells.
    pub false_cells: usize,
    /// KleeneBot (unknown — needs refinement) cells.
    pub unknown_cells: usize,
    /// A4 (engine-performance roadmap, 2026-10-07) — cells the lift did not decide: unsatisfiable
    /// cells and, under the explicit engine's reachable-only post-image, cells not reachable from
    /// the initial cubes. They are ⊥ by construction and are NOT in `unknown_cells`; the three
    /// counts above are over the remaining cells. Always 0 on the symbolic engine's summaries.
    #[serde(default)]
    pub unlifted_cells: usize,
}

/// A predicate spec, response-shaped.
#[derive(Debug, Serialize)]
pub struct PredicateView {
    pub name: String,
    pub register: String,
    pub value: u64,
}

#[cfg(test)]
mod bottom_reason_view_tests {
    use super::*;
    use crate::adapter::slang::translate::SvaKind;
    use crate::adapter::slang::verify_auto::{
        AutoVerifyReport, BottomReason, PropertyVerdict, VerifyOutcome,
    };

    fn bottom_prop(reason: Option<BottomReason>) -> PropertyVerdict {
        PropertyVerdict {
            name: "sva_1".into(),
            label: None,
            kind: SvaKind::Assert,
            formula: "AG p".into(),
            outcome: VerifyOutcome::Unknown { unknown_cells: 125 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: reason,
            decided_by: None,
            vacuity: None,
        }
    }

    /// monono ask 26 — the API surface must carry the reason too, not just the CLI.
    ///
    /// Surface Parity: a capability ships on CLI *and* HTTP API in the same PR. The JSON-schema
    /// drift detector pins the SHAPE of this field; nothing else asserts the CONVERSION fills it,
    /// so without this test a refactor could null it out and stay green.
    #[test]
    fn the_api_view_carries_the_bottom_reason_and_its_gateable_tag() {
        let report = AutoVerifyReport {
            properties: vec![bottom_prop(Some(BottomReason::EngineDidNotComplete {
                engine: "exact-symbolic".into(),
                detail: "abstained on the ITERATION budget (1048577 > 1048576) — raise \
                         MUNUNU_BDD_ITER_BUDGET"
                    .into(),
            }))],
            ..Default::default()
        };

        let view = SvVerifyAutoResponse::from(&report);
        let r = view.properties[0]
            .bottom_reason
            .as_ref()
            .expect("an ⊥ property must carry its reason across the API boundary");

        assert_eq!(
            r.kind, "engine-did-not-complete",
            "the tag is what a gate branches on"
        );
        assert_eq!(r.engine.as_deref(), Some("exact-symbolic"));
        assert!(
            r.detail.contains("MUNUNU_BDD_ITER_BUDGET"),
            "the knob to raise must survive to the API consumer: {}",
            r.detail
        );
    }

    /// mununu#577 — the withheld-VIOLATED reason must cross the API boundary with its COUNT as a
    /// field, not only inside `detail`'s prose.
    ///
    /// The count is per-property (each property's own cone), so a gate sizing the remedy reads it
    /// directly. Asserting only on `kind` would pass on an implementation that dropped the number
    /// into prose and forced a consumer to grep for it — which is the defect mununu#553 was filed
    /// against, and it would arrive here through the very field added to fix it.
    #[test]
    fn the_withheld_violated_carries_its_register_count_as_a_field() {
        let report = AutoVerifyReport {
            properties: vec![bottom_prop(Some(BottomReason::UnestablishedInitialState {
                registers: 6,
                gated_resets: vec!["rst_n".into()],
            }))],
            ..Default::default()
        };

        let view = SvVerifyAutoResponse::from(&report);
        let r = view.properties[0]
            .bottom_reason
            .as_ref()
            .expect("a withheld VIOLATED must carry its reason across the API boundary");

        assert_eq!(
            r.kind, "unestablished-initial-state",
            "the tag is what a gate branches on"
        );
        assert_eq!(
            r.registers,
            Some(6),
            "the register count must be a FIELD — a consumer must not have to parse `detail`"
        );
        assert_eq!(
            r.determinism, "reproducible",
            "this is a property of the model, not of the host: re-running will not change it"
        );
        assert!(
            r.budget.is_none() && r.budget_knob.is_none(),
            "no engine budget fired — raising one cannot help, and claiming a knob would send a \
             gate into a retry loop: {r:?}"
        );
    }

    /// `decided_by` must vary PER PROPERTY within a single report.
    ///
    /// A test where every property shares an engine would pass on an implementation storing one
    /// report-level value — which is exactly the aggregate (`decided-by:<engine>=<count>`) this
    /// replaces, and exactly what hid a silent precision-tier change. The assertion has to be that
    /// a MIXED report reports mixedly.
    #[test]
    fn decided_by_distinguishes_properties_within_one_report() {
        let mut exact = bottom_prop(None);
        exact.name = "sva_1".into();
        exact.outcome = VerifyOutcome::Holds;
        exact.decided_by = Some("exact-symbolic".into());

        let mut abstracted = bottom_prop(None);
        abstracted.name = "sva_2".into();
        abstracted.outcome = VerifyOutcome::Holds;
        abstracted.decided_by = Some("symbolic".into());

        let report = AutoVerifyReport {
            properties: vec![exact, abstracted],
            ..Default::default()
        };
        let view = SvVerifyAutoResponse::from(&report);

        assert_eq!(
            view.properties[0].decided_by.as_deref(),
            Some("exact-symbolic")
        );
        assert_eq!(
            view.properties[1].decided_by.as_deref(),
            Some("symbolic"),
            "both properties HOLD, but a different engine answered each — an aggregate count \
             cannot express this, and that is the case it hides"
        );
    }

    /// Absence of a reason means EXACTLY ONE THING: the outcome is definite.
    ///
    /// This test previously asserted that an UNDECIDED property with no classification serialized
    /// nothing — which was true, and was the defect. `skip_serializing_if = "Option::is_none"`
    /// omits the key, so a consumer could not distinguish "mununu considered this and had no
    /// reason" from "mununu never set the field". One did hit exactly that and had to report *"the
    /// key is absent"* rather than *"the reason is unclassified"*.
    ///
    /// A ⊥ now always carries a reason (floor `unclassified-bottom`), so the property worth pinning
    /// inverted: absence is reserved for a DEFINITE verdict, and an undecided property must never
    /// be silent.
    #[test]
    fn only_a_definite_verdict_omits_the_reason() {
        let mut decided = bottom_prop(None);
        decided.outcome = VerifyOutcome::Holds;
        let report = AutoVerifyReport {
            properties: vec![decided],
            ..Default::default()
        };
        assert!(
            SvVerifyAutoResponse::from(&report).properties[0]
                .bottom_reason
                .is_none(),
            "a definite verdict has no ⊥ to explain"
        );

        // …and the inverse, which is the actual fix: an undecided property with NO classification
        // still serializes a reason rather than an absent key.
        let undecided = AutoVerifyReport {
            properties: vec![bottom_prop(None)],
            ..Default::default()
        };
        let v = SvVerifyAutoResponse::from(&undecided);
        let r = v.properties[0]
            .bottom_reason
            .as_ref()
            .expect("an undecided property must NEVER serialize without a reason");
        assert_eq!(
            r.kind, "unclassified-bottom",
            "the floor, so absence can mean only `definite`"
        );
    }
}
