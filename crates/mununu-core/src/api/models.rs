use serde::{Deserialize, Serialize};
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
    pub counterexample_trace: Option<Vec<String>>,
    #[serde(default)]
    pub counterstrategy_traces: Vec<Vec<String>>,
    #[serde(default)]
    pub deadlock_traces: Vec<Vec<String>>,
    pub minimization: Option<MinimizationReport>,
    #[serde(default)]
    pub proof_obligations: Vec<ProofObligation>,
    /// Lasso traces for liveness counterexamples: prefix + repeating cycle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lasso_traces: Vec<LassoTraceApi>,
}

/// Lasso trace: finite prefix followed by infinitely repeating cycle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LassoTraceApi {
    pub prefix: Vec<String>,
    pub cycle: Vec<String>,
    /// Transition labels between consecutive prefix states.
    /// `prefix_labels[i]` is the label from `prefix[i]` to `prefix[i+1]` (or `cycle[0]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix_labels: Vec<String>,
    /// Transition labels between consecutive cycle states.
    /// The last element is the label from the last cycle state back to `cycle[0]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycle_labels: Vec<String>,
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
    /// Mirrors the CLI's `--predicate NAME:REG=VALUE` flag on
    /// `mununu btor2 cegar`.
    #[serde(default)]
    pub predicates: Vec<PredicateSpecRequest>,
    /// R.6.7 / V.6 (2026-06-09) — names of BTOR2 input symbols the
    /// controller drives. Mirrors the CLI's `--controllable-input`
    /// flag. When non-empty AND `predicates` is non-empty, opts the
    /// import path into the R.6.6 controllability-aware lift —
    /// boolean inputs are partitioned into env (uncontrollable) +
    /// ctrl (controllable) classes per-symbol-name + the lift emits
    /// per-combo dual-label transitions.
    #[serde(default)]
    pub controllable_inputs: Vec<String>,

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
/// the CLI's `--predicate NAME:REG=VALUE` triple format. Bridges to
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
}

/// Response from context verification
#[derive(Debug, Serialize, PartialEq)]
pub struct ContextVerifyResponse {
    pub success: bool,
    pub all_satisfied: bool,
    pub results: Vec<FormulaVerificationResult>,
}

/// Verification result for a single formula–automaton pair
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FormulaVerificationResult {
    pub formula_name: String,
    pub automaton: String,
    pub satisfied: bool,
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
pub struct Btor2CegarRequest {
    /// BTOR2 source content.
    pub content: String,
    /// μ-calculus formula evaluated over the lifted KMTS.
    pub formula: String,
    /// Initial predicate set (bootstraps the `2^|P|` cube space). At least
    /// one entry is required. Reuses the `PredicateSpecRequest` shape.
    pub predicates: Vec<PredicateSpecRequest>,
    /// Optional R.6.6 controllability split — controller-driven input
    /// symbols (mirrors `--controllable-input`).
    #[serde(default)]
    pub controllable_inputs: Vec<String>,
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
    /// `"REG=v1,v2,..."` (mirrors the CLI `--config-values`). Each register's
    /// admissible power-up set seeds the predicate-cube initial states. Empty =
    /// no synthetic init sidecar.
    #[serde(default)]
    pub config_values: Vec<String>,
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
    pub verdict: String,
    /// Engines that found `bad` reachable (a real counterexample).
    pub reachable_by: Vec<String>,
    /// Engines that proved `bad` unreachable (a real safety proof).
    pub unreachable_by: Vec<String>,
    /// `true` when two sound engines disagree — a soundness alarm, not a guess.
    pub contradiction: bool,
}

/// Request for the response-liveness endpoint
/// (`POST /api/v1/btor2/verify-liveness`). Mirrors the CLI
/// `mununu btor2 verify-liveness`: decides `AG(request → AF grant)` via the l2s
/// reduction + the portfolio. `request` / `grant` are register-comparison atom
/// strings (`"st == 1"`).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Btor2VerifyLivenessRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The request atom (`"REG op VALUE"`).
    pub request: String,
    /// The grant atom that must eventually follow on every path.
    pub grant: String,
}

/// Response for `POST /api/v1/btor2/verify-liveness`. Mirrors
/// [`crate::adapter::liveness_rescue::LivenessVerdict`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2VerifyLivenessResponse {
    /// Canonical property verdict — `"holds"` | `"violated"` | `"unknown"`, via
    /// [`crate::verdict::PropertyVerdict`].
    pub verdict: String,
    /// The reduced property, echoed for provenance:
    /// `AG((<request>) -> AF (<grant>))`.
    pub property: String,
    /// Portfolio engines that decided the reduced `bad`-reachability query.
    pub decided_by: Vec<String>,
}

/// Request for the conjunctive response-liveness endpoint
/// (`POST /api/v1/btor2/verify-liveness-all`). Mirrors the CLI
/// `mununu btor2 verify-liveness-all`: decides `⋀ᵢ AG(aᵢ → AF bᵢ)`. Each `responses`
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
/// (`POST /api/v1/btor2/verify-liveness-under-fairness`, mununu#477 Option B).
/// Mirrors the CLI `mununu btor2 verify-liveness-under-fairness`: decides
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
pub struct Btor2VerifyRecoverabilityRequest {
    /// BTOR2 source content.
    pub content: String,
    /// The `good` atom to recover to (`"REG op VALUE"`).
    pub target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation, each
    /// `"NAME:REGISTER=VALUE"` (P2 Slice 1). Used only when the exact engine abstains
    /// (over the ~40-bit cone cap); the escalation is automatic even with none.
    #[serde(default)]
    pub predicates: Vec<String>,
    /// Also compute a structured [`crate::verdict::VerdictRefinement`] alongside the verdict — a
    /// `vacuous` witness when the target is never reachable, an auto `config_partition` over the
    /// design's detected reset when recovery depends on it, and a "why ⊥" hint. Diagnostic-only:
    /// it never changes the canonical verdict. (Mirrors the CLI `--refine`.)
    #[serde(default)]
    pub refine: bool,
    /// Config-partition (capability A): config inputs to split the verdict over, each
    /// `"NAME=v1,v2,..."`. The refinement then carries a `config_partition` decided exactly per config.
    /// Implies the refined output. Mirrors the CLI `--config-values`.
    #[serde(default)]
    pub config_values: Vec<String>,
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
    pub verdict: String,
    /// The decided property, echoed for provenance: `AG EF (<target>)`.
    pub property: String,
    /// The structured refinement, when `refine` was requested and it produced one. `None` otherwise.
    /// Diagnostic detail carried ALONGSIDE the verdict — never changes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refinement: Option<crate::verdict::VerdictRefinement>,
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
}

fn default_fsm_max_width() -> u32 {
    crate::adapter::fsm_scan::DEFAULT_FSM_MAX_WIDTH
}

/// One state register's illegal-encoding result in a [`Btor2CheckFsmResponse`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct FsmRegisterFinding {
    /// The state register's symbol.
    pub register: String,
    /// The legal encodings the register's own logic recognizes (sorted).
    pub legal_encodings: Vec<u64>,
    /// Canonical verdict — `"holds"` (stays within its encoding) | `"violated"` (an
    /// illegal encoding is reachable) | `"unknown"` (the portfolio could not decide).
    pub verdict: String,
    /// `true` when an illegal encoding is reachable (a finding).
    pub illegal_encoding_reachable: bool,
}

/// Response for `POST /api/v1/btor2/check-fsm`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Btor2CheckFsmResponse {
    /// Number of FSM-like state registers scanned.
    pub fsm_registers_checked: usize,
    /// Number of registers with a reachable illegal encoding (`verdict == "violated"`).
    pub illegal_encodings_found: usize,
    /// Per-register results.
    pub registers: Vec<FsmRegisterFinding>,
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
}

/// Response for `POST /api/v1/btor2/game`.
#[derive(Debug, Serialize)]
pub struct Btor2GameResponse {
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
    /// P2.5-F (b) — the ENVIRONMENT STARVATION LASSO for an unrealizable RECURRENCE game: a concrete
    /// play (reset → repeating `¬good` `cycle`, with the env's per-step `inputs`) proving the environment
    /// can starve `good` forever — the actionable Büchi counterpart of `realizable = false`. Present only
    /// for `objective = "recurrence"` when unrealizable AND a simple reachable force-`¬good`-forever
    /// region exists; `null` for realizable games, the `reach` objective (whose witness is `strategy`),
    /// or a subtler co-Büchi with no such region.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall_lasso: Option<crate::adapter::btor2::symbolic_bitblast::StallLassoView>,
}

/// The SV → BTOR2 lift inputs shared by the SV-direct verb endpoints
/// (`/api/v1/sv/verify`, `/sv/verify-liveness`, `/sv/verify-recoverability`). These
/// lift the module (sv2v + Yosys) and then decide the corresponding BTOR2 property,
/// returning the same `Btor2Verify*Response` shapes — one call, no `emit-btor2` step.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys (modern SV). Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`) for modern-SV constructs
    /// yosys/sv2v reject (`while` loops, `import pkg::*;`). Requires the
    /// yosys-slang plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
}

/// Request for `POST /api/v1/sv/verify-liveness` — the SV lift fields plus the
/// response-liveness atoms.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyLivenessRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// The request atom (`"REG op VALUE"`).
    pub request: String,
    /// The grant atom that must eventually follow on every path.
    pub grant: String,
}

/// Request for `POST /api/v1/sv/verify-liveness-all` — the SV lift fields plus the
/// conjunction's `"ANTE => CONS"` response pairs. Mirrors the CLI
/// `mununu sv verify-liveness-all`; reuses [`Btor2VerifyLivenessResponse`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyLivenessAllRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// The response pairs, each `"ANTE => CONS"`. At least one required.
    pub responses: Vec<String>,
}

/// Request for `POST /api/v1/sv/verify-recoverability` — the SV lift fields plus the
/// recoverability target atom.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SvVerifyRecoverabilityRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// The `good` atom to recover to (`"REG op VALUE"`).
    pub target: String,
    /// Extra abstraction predicate(s) for the cube-path escalation, each
    /// `"NAME:REGISTER=VALUE"` (P2 Slice 1). Used only when the exact engine abstains.
    #[serde(default)]
    pub predicates: Vec<String>,
    /// Also compute a structured `refinement` alongside the verdict (mirrors the CLI `--refine`).
    /// Diagnostic-only — never changes the canonical verdict.
    #[serde(default)]
    pub refine: bool,
    /// Config-partition config inputs, each `"NAME=v1,v2,..."` (mirrors the CLI `--config-values`).
    #[serde(default)]
    pub config_values: Vec<String>,
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
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// Max state-register width treated as an FSM (wider = datapath/counter, skipped).
    #[serde(default = "default_fsm_max_width")]
    pub max_width: u32,
}

/// Request for `POST /api/v1/sv/lint` — the SV-direct partial-write preflight.
/// Lifts SystemVerilog and reports every register whose partial-write lift the
/// verifier cannot keep faithfully (monono#partsel), with no model checking.
/// Surface peer of the CLI `mununu sv lint`.
#[derive(Debug, Deserialize)]
pub struct SvLintRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
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
/// peer of the CLI `mununu sv mutate`.
#[derive(Debug, Deserialize)]
pub struct SvMutateRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / includes).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys. Default `false`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`). Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// The mutation to apply: `"stick:<reg>"` (freeze a register) or
    /// `"drop-reset:<reg>"` (remove a register's reset arm). Required unless
    /// `list` is true.
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
    /// recoverability property under `drop-reset`). Default off.
    #[serde(default)]
    pub must_edge_inference: Option<String>,
    /// mununu#475 item 4 — parity with `POST /api/v1/sv/verify-auto`'s
    /// `params`. Override module parameters BEFORE elaboration. Each entry
    /// is `"NAME=VALUE"` (top-module scope) or `"MODULE.NAME=VALUE"`
    /// (submodule scope; slang applies it top-level by bare name).
    /// Shrinks a parameterised design so its counters fit the bit-blast
    /// cap during the baseline + mutant re-verify; blocks that need
    /// `params` to verify at all could not previously be mutated via the
    /// API. A malformed entry is a HARD `BadRequest`; yosys errors
    /// downstream on a parameter it cannot apply — never a silent drop.
    #[serde(default)]
    pub params: Vec<String>,
}

/// The mutation targets available in a design (the `list` response payload).
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
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

/// Response for `POST /api/v1/sv/mutate`.
#[derive(Debug, Serialize)]
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
pub struct SvCegarRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV source files (multi-file designs / packages /
    /// `include` targets), staged alongside the primary source.
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module name. Recommended for multi-module designs so Yosys
    /// flattens from the right root; `None` lets Yosys auto-detect.
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys (required for modern SV: module-header
    /// `import pkg::*;`, structs, interfaces). Mirrors the CLI
    /// `--preprocess-sv2v`.
    #[serde(default)]
    pub use_sv2v: bool,
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
    pub controllable_inputs: Vec<String>,
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
    pub config_values: Vec<String>,
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
pub struct SvVerifyAutoRequest {
    /// SystemVerilog primary source content.
    pub source: String,
    /// Additional SV sources (packages / `include` targets).
    #[serde(default)]
    pub additional_sources: Vec<FileContent>,
    /// Top module for the SV → BTOR2 lift (auto-detect when omitted).
    #[serde(default)]
    pub top: Option<String>,
    /// Run sv2v before Yosys (modern SV). Mirrors `--preprocess-sv2v`.
    #[serde(default)]
    pub use_sv2v: bool,
    /// Force the slang RTL front-end (`read_slang`) for modern-SV constructs
    /// yosys/sv2v reject. Mirrors `--frontend slang`. Requires the yosys-slang
    /// plugin. Default `false`.
    #[serde(default)]
    pub use_slang: bool,
    /// Module-parameter overrides applied via yosys `chparam -set` before the lift
    /// (mirrors the CLI `--param`). Each entry is `"NAME=VALUE"` (applied to the
    /// top module) or `"MODULE.NAME=VALUE"` (scoped to `MODULE`). Shrinks a
    /// parameterised timing interval so its counters get smaller — without a
    /// wrapper module (which would rename the SVA atoms). A parameter yosys cannot
    /// apply is an ERROR (never silently dropped); the verdicts are scoped to the
    /// applied values (surfaced as a `parameter-override` note). Default empty.
    #[serde(default)]
    pub params: Vec<String>,
    /// Max CEGAR iterations per property (default 16).
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Must-edge inference per property (default `"off"`; `"smt-hyper-must"`
    /// gives sound νμ verdicts).
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
    pub config_values: Vec<String>,
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
    /// Control-slice cut points, mirroring the CLI `--cutpoint`: net names to replace
    /// with a free `$anyseq` input in the SV → BTOR2 lift (Yosys `cutpoint w:<net>`), so
    /// the FSM's datapath guards drop out of the cone via cone-of-influence — the sound,
    /// netlist-level way to fit `"exact-symbolic"` on a wide control FSM. Each entry is a
    /// bare net name (e.g. `"must_refresh"`). OVER-APPROXIMATION: a definite HOLDS
    /// transfers for a SAFETY / universal property (safety + over-approx); for an
    /// ALTERNATING (νμ) recoverability property (`AG EF`) a HOLDS is sound for the
    /// abstract model only (νμ collapses under approximation) — decide those on the
    /// un-cutpointed cone. A definite VIOLATED is sound only when guard-independent (an
    /// orphaned FSM state). Surfaced as a `control-slice` note. Default empty.
    #[serde(default)]
    pub cutpoint: Vec<String>,
    /// Abstraction-predicate hints, mirroring the CLI `--predicate` and the in-source
    /// `// @mununu_predicate <expr>` annotation (all three are merged). Each entry is a
    /// predicate expression (`"reg == value"`, `"reg == reg"`, `"reg >= K"`) seeded as a
    /// cube dimension for EVERY property, even when absent from the property formula.
    /// Sound by monotonicity — a hint only refines the cube (a ⊥ can become definite; a
    /// definite verdict never flips; an unclassifiable hint is dropped). Default empty.
    #[serde(default)]
    pub predicate: Vec<String>,
    /// Engine selector, mirroring the CLI `--engine`: `"explicit"`, `"symbolic"`,
    /// `"exact-symbolic"`, `"portfolio-sequential"`, `"portfolio-parallel"`.
    /// **Unspecified ⇒ the default `"portfolio-sequential"`** (2026-07-06): run
    /// exact → symbolic → explicit, stopping when every property is decided — the
    /// most precise sound choice, no slower than the former `explicit` default on
    /// designs `explicit` already decided. Resolved by `verify_auto::engine_selection`.
    #[serde(default)]
    pub engine: Option<String>,
    /// Safety-⊥ escalation (default `true`): a *safety* property the cube leaves ⊥
    /// (a reducible AG-invariant) is retried with the multi-engine reachability
    /// portfolio (exact ⊕ native ⊕ spacer ⊕ btormc ⊕ Pono). Set `false` to report the
    /// cube's ⊥ unchanged. Only fires on ⊥ reducible AG-invariants.
    #[serde(default)]
    pub rescue_bottom_safety: Option<bool>,
    /// Liveness-⊥ escalation (default `true`): a *box-AF* liveness property `AG(a → AF
    /// b)` the cube leaves ⊥ is retried via the liveness-to-safety reduction + the
    /// reachability portfolio. Set `false` to report the cube's ⊥ unchanged. Only fires
    /// on the l2s-able box-AF shape (diamond-EF recoverability stays on the νμ path).
    #[serde(default)]
    pub rescue_bottom_liveness: Option<bool>,
    /// Recoverability-⊥ escalation (default `true`): a νμ recoverability property `AG EF
    /// good` the cube leaves ⊥ is retried via the cube + `smt-hyper-must` νμ path. Set
    /// `false` to report the cube's ⊥ unchanged. Only fires on the `AG EF (REG == VALUE)`
    /// shape.
    #[serde(default)]
    pub rescue_bottom_recoverability: Option<bool>,
    /// mununu#476 item 4 — disable antecedent shadow-register synthesis for
    /// the exact-symbolic engine (default `false` ⇒ shadow-synth enabled, the
    /// shipped behaviour). Set `true` to force the Phase A refusal for SVA
    /// `|=>` properties whose antecedent combinationally reaches primary
    /// inputs — the differential-oracle / debug knob mirrored on the CLI as
    /// `--no-antecedent-shadow` and via the process-global
    /// `MUNUNU_NO_ANTECEDENT_SHADOW=1` env var. Per-request opt-out is
    /// thread-safe (unlike the env var, which is process-global).
    #[serde(default)]
    pub no_antecedent_shadow: Option<bool>,
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
}

/// One property's auto-verification verdict (mirrors `PropertyVerdict`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PropertyVerdictView {
    pub name: String,
    /// `"assert"` | `"assume"` | `"cover"`.
    pub kind: String,
    pub formula: String,
    /// `"holds"` | `"violated"` | `"unknown"` | `"skipped"`.
    pub outcome: String,
    /// `false_cells` (violated) / `unknown_cells` (unknown) / the skip reason.
    pub detail: Option<String>,
    /// The cube predicates auto-seeded for this property (atom strings).
    pub seeded_predicates: Vec<String>,
    /// D1.8b — a concrete stall-lasso counterexample, present only for a Violated
    /// bare `AF p` decided by the exact engine (`engine: "exact-symbolic"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterexample: Option<CounterexampleView>,
}

/// D1.8b — a stall-lasso counterexample for the API (mirrors `ExactCounterexample`):
/// reset → `prefix` → repeating `cycle`, each state an ordered list of register cells.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CounterexampleView {
    /// States from the reset state up to (excluding) the cycle entry.
    pub prefix: Vec<Vec<CexCellView>>,
    /// The repeating stall cycle; the last state steps back to `cycle[0]`.
    pub cycle: Vec<Vec<CexCellView>>,
    /// A.4 — for a `Violated` bare `EF p` (reachability): the target predicate atoms that
    /// are UNREACHABLE from reset ("the design never reaches these"). Empty for the
    /// stall-lasso / trap-path (liveness) counterexamples, which carry `prefix`/`cycle`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreachable_target: Vec<String>,
}

/// One register's concrete value in a counterexample state.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CexCellView {
    pub register: String,
    pub value: u64,
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
    /// Cell-count summary of the final 3-valued verdict.
    pub verdict: CegarVerdictSummary,
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
    pub counterexample: Option<CounterTraceView>,
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

/// Track I.1 (trace slice) — a reachability countertrace, viewer-shaped: the
/// ordered sequence of failing cube cells plus whether the path ends in a trap
/// (a cell whose every successor stays `False`, so the violation is locked in).
#[derive(Debug, Serialize)]
pub struct CounterTraceView {
    pub steps: Vec<WitnessCellView>,
    pub ends_in_trap: bool,
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
    pub verdict: CegarVerdictSummary,
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
}

/// A predicate spec, response-shaped.
#[derive(Debug, Serialize)]
pub struct PredicateView {
    pub name: String,
    pub register: String,
    pub value: u64,
}
