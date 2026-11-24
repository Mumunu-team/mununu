//! Format adapter subsystem — translates external specification formats
//! into CTXDSL text via a shared intermediate representation.
//!
//! # Architecture
//!
//! ```text
//! Source file → Format Parser → Format AST → to_ir() → AdapterIR → emit() → CTXDSL text
//! ```
//!
//! Each adapter implements the [`FormatAdapter`] trait. The shared IR types
//! live in [`ir`], and the CTXDSL emitter in [`emit`].
//!
//! # Soundness Checklist for Adapter Implementors
//!
//! Every adapter must address the following before being considered complete:
//!
//! 1. **Unsupported constructs must warn.** Any source-language construct that
//!    is skipped or partially handled must emit an [`AdapterWarning`] with
//!    [`WarningKind::UnsupportedConstruct`] and note the soundness impact
//!    (over-approx or under-approx).
//!
//! 2. **State abstraction direction must be documented.** For every field or
//!    register abstracted into a finite domain, document whether the abstraction
//!    is an over-approximation (admits more behaviors) or under-approximation
//!    (admits fewer behaviors) using `// SOUNDNESS:` comments.
//!
//! 3. **Guard evaluation failures must be documented.** When `eval_expr` or
//!    guard evaluation returns `None`/default, document whether the fallback
//!    is conservative (over-approx: allows transition) or optimistic
//!    (under-approx: blocks transition).
//!
//! 4. **Controllability must be explicit.** Every label must have a clear
//!    rationale for its controllability classification. If heuristic-based,
//!    emit [`WarningKind::NeutralControllability`] so the user can override.
//!
//! 5. **Known-verdict regression test must exist.** At minimum one test with a
//!    known-safe and one known-unsafe property, verifying the adapter produces
//!    a model that gives the expected verdict.

pub mod aiger;
pub mod btor2;
pub mod btormc;
pub mod cdc_lint;
pub mod clts_to_ir;
pub mod crewai;
pub mod cvc5;
pub mod domain;
pub mod emit;
pub mod engine_isolate;
pub(crate) mod env_var;

/// mununu#981 — the tool gate of a tool-dependent test: returns `present`, unless
/// `MUNUNU_REQUIRE_TOOLS=1` is set and the tool is absent, which panics with the tool's name.
/// The nightly's in-image unit pass sets it, so a test that would skip there (the image lost a
/// tool, a probe broke) fails visibly instead of reporting a pass. Public for the integration
/// tests' probes; not a product knob.
#[doc(hidden)]
pub fn require_tool(tool: &str, present: bool) -> bool {
    if !present && env_var::read("MUNUNU_REQUIRE_TOOLS").is_some_and(|v| v.trim() == "1") {
        panic!(
            "MUNUNU_REQUIRE_TOOLS=1 but {tool} is not available here: a tool-gated test would \
             skip and report a pass (mununu#981)"
        );
    }
    present
}

/// mununu#1121 — an "unknown X" refusal prints every candidate, sorted, when the set has at most
/// this many members; a larger set prints the [`CANDIDATE_LIST_NEAREST`] nearest followed by
/// "and N more" (`docs/policies/failure-paths.md`, Rule 5).
pub const CANDIDATE_LIST_FULL: usize = 20;
/// How many candidates [`candidate_list`] shows of a set larger than [`CANDIDATE_LIST_FULL`].
pub const CANDIDATE_LIST_NEAREST: usize = 5;

/// mununu#1121 — the candidates nearest to `wanted`, at most `max`: an exact match ignoring case
/// first, then a candidate that is a prefix of `wanted` (or the reverse), then the rest by edit
/// distance; ties alphabetically. Duplicates are dropped. The ranking an "unknown X" message
/// uses when the candidate set is too large to print whole.
pub fn nearest_names<'a>(
    wanted: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    max: usize,
) -> Vec<&'a str> {
    let wanted_lc = wanted.to_lowercase();
    let mut ranked: Vec<(u8, usize, &'a str)> = candidates
        .into_iter()
        .map(|candidate| {
            let candidate_lc = candidate.to_lowercase();
            let class = if candidate_lc == wanted_lc {
                0
            } else if candidate_lc.starts_with(&wanted_lc) || wanted_lc.starts_with(&candidate_lc) {
                1
            } else {
                2
            };
            (class, edit_distance(&wanted_lc, &candidate_lc), candidate)
        })
        .collect();
    ranked.sort_unstable();
    ranked.dedup_by(|a, b| a.2 == b.2);
    ranked.truncate(max);
    ranked
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// mununu#1121 — the candidate list an "unknown X" message appends: every candidate, sorted and
/// comma-separated, when there are at most [`CANDIDATE_LIST_FULL`]; otherwise the
/// [`CANDIDATE_LIST_NEAREST`] nearest to `wanted` (see [`nearest_names`]) and "and N more".
/// An empty set reads "(none declared)", so the message still says what was looked at.
pub fn candidate_list<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    let mut all: Vec<&str> = candidates.into_iter().collect();
    all.sort_unstable();
    all.dedup();
    if all.is_empty() {
        return "(none declared)".to_string();
    }
    if all.len() <= CANDIDATE_LIST_FULL {
        return all.join(", ");
    }
    let shown = nearest_names(wanted, all.iter().copied(), CANDIDATE_LIST_NEAREST);
    format!("{} and {} more", shown.join(", "), all.len() - shown.len())
}

/// Levenshtein distance over chars; both inputs are short names, so the quadratic table is fine.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let subst = prev[j] + usize::from(ca != cb);
            cur[j + 1] = subst.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod candidate_list_tests {
    use super::*;

    #[test]
    fn x1121_nearest_names_ranks_exact_then_prefix_then_edit_distance() {
        let names = ["parity-game", "product-game", "projection", "gr1"];
        assert_eq!(
            nearest_names("Projection", names, 1),
            vec!["projection"],
            "an exact match ignoring case comes first"
        );
        assert_eq!(
            nearest_names("pro", names, 2),
            vec!["projection", "product-game"],
            "a shared prefix beats a smaller edit distance (`gr1` is closer by edits); \
             among prefixes the shorter edit distance first"
        );
        assert_eq!(
            nearest_names("parity_gam", names, 1),
            vec!["parity-game"],
            "otherwise the nearest by edit distance"
        );
        assert_eq!(nearest_names("x", ["b", "a", "a"], 5), vec!["a", "b"]);
        assert!(nearest_names("x", [], 3).is_empty());
    }

    #[test]
    fn x1121_candidate_list_prints_a_small_set_whole_and_a_large_set_by_nearness() {
        assert_eq!(candidate_list("b", ["c", "a", "b", "a"]), "a, b, c");
        assert_eq!(candidate_list("b", []), "(none declared)");
        let many: Vec<String> = (0..30).map(|i| format!("reg_{i:02}")).collect();
        let listed = candidate_list("reg_1", many.iter().map(String::as_str));
        assert!(
            listed.starts_with("reg_10, reg_11, reg_12, reg_13, reg_14"),
            "{listed}"
        );
        assert!(listed.ends_with(" and 25 more"), "{listed}");
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
pub mod extraction;
pub mod fault_adversary;
pub mod fsm_scan;
pub mod gr1_synth;
pub mod ir;
pub mod justice;
pub mod langgraph;
pub mod lift_decisions;
pub mod liveness_rescue;
pub mod memory_budget;
pub mod microcode;
/// mununu#1121 U2.10 — the owner clause of a message that reports a gap.
pub mod owner;
pub mod partial_json;
pub mod partition;
pub mod pono;
pub mod promela;
pub mod reach_portfolio;
pub mod reach_rescue;
pub mod recoverability;
/// Roadmap J3 — resume `sv verify-auto` from the breadcrumb a killed run left behind.
pub mod resume;
pub mod ric3;
/// N0.1 — the per-row facts of a finished run (`MUNUNU_VERIFY_AUTO_ROW_FACTS`), for the ledger.
pub mod row_facts;
pub mod run_budget;
pub mod sidecar;
pub mod slang;
pub mod state_enum;
pub mod sts_ir;
pub mod sv_verify;
pub mod systemverilog;
pub mod templates;
pub mod tlsf;
pub mod trap_certificate;
pub mod vcd;
pub mod verilator;
pub mod xstate;
pub mod yosys;

use std::collections::HashMap;
use std::fmt;

/// Trait implemented by each format adapter.
pub trait FormatAdapter {
    /// Detect whether the input content is in this format.
    fn detect(content: &str) -> bool;

    /// Translate source content to CTXDSL text via the shared IR.
    fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError>;
}

/// Options controlling adapter translation behavior.
#[derive(Debug, Clone, Default)]
pub struct AdapterOptions {
    /// Which input signals are controllable (for AIGER, Promela).
    pub controllable_inputs: Vec<String>,
    /// Variable bounds for Promela (overrides inferred bounds).
    pub variable_bounds: HashMap<String, (i64, i64)>,
    /// Context name for the output CTXDSL document.
    pub context_name: Option<String>,
    /// Mode for extraction adapter: "fixed" or "vulnerable".
    pub mode: Option<String>,
    /// Raw `.mununu.json` content. When present, the BTOR2 reader (and
    /// any future adapter) parses it through
    /// [`crate::adapter::sidecar::resolve_to_field_domain`] to bound
    /// per-state-cell value enumeration. The SV adapter loads its
    /// sidecar via filesystem convention (next to the .sv source); the
    /// BTOR2 reader takes the JSON in-memory because the BTOR2 source
    /// may itself live in memory (the Yosys driver case).
    pub sidecar_json: Option<String>,
    /// Direction map for top-module ports the upstream frontend
    /// captured before any flattening / inlining destroyed the
    /// module-boundary information. Used by the BTOR2 reader (when
    /// the yosys driver populates this from the pre-flatten
    /// `write_json`) to classify BTOR2 inputs by direction instead of
    /// defaulting every input to `Uncontrollable`.
    ///
    /// This is Document B task B1's plumbing: the §4 rule says
    /// "controllability follows the direction at the surrounding
    /// scope's boundary." When `port_directions` is non-empty, the
    /// reader classifies each input by looking up its name here; any
    /// input *not* in the map keeps the historical "Uncontrollable"
    /// default (the right call for BTOR2 inputs that originated as
    /// cut points from `cutpoint -blackbox`).
    pub port_directions: HashMap<String, crate::controllability::BoundaryDirection>,

    /// R-Y3 (§Phase 8) — opt-in BTOR2 init-line smart defaults for
    /// state cells without sidecar entries. When true, each
    /// unsidecared cell with a BTOR2 `Init` line is pinned to its
    /// init value (1-state abstraction) instead of falling back to
    /// full bit-blast (`2^width`). Cells without init lines retain
    /// full bit-blast.
    ///
    /// SOUNDNESS: under-approximation. Sound for liveness ("the
    /// reset state is reachable in the real design"); unsound for
    /// safety ("property violations that require deviation from the
    /// init value are silently masked"). Surfaced as an adapter
    /// warning naming the affected cells.
    ///
    /// The CLI also honours the env var
    /// `MUNUNU_BTOR2_SMART_INIT_DEFAULTS=1` as a fallback so existing
    /// `validate.sh` scripts can opt in without touching every
    /// AdapterOptions construction site. Default `false` — preserves
    /// legacy full-bit-blast behaviour for unsidecared cells.
    pub smart_init_defaults: bool,

    /// R-S2b.6 (§Phase 9 §9.1, 2026-06-11) — path to the original
    /// SystemVerilog source. When `Some` AND the sidecar declares
    /// a `simulate_reset` block AND a Verilator binary is
    /// discoverable, the BTOR2 bit-blaster runs a short concrete
    /// reset simulation via
    /// [`crate::adapter::verilator::run_reset_simulation`] and
    /// feeds the result through
    /// [`crate::adapter::btor2::bit_blast::apply_reset_simulation_seeding`].
    ///
    /// The path is populated by the caller (CLI / API / yosys
    /// driver) — the BTOR2 bit-blaster does not derive it.
    /// `None` (default) preserves the legacy "no simulation seeding"
    /// behaviour; Verilator is also skipped silently when the
    /// binary is absent (graceful fallback to other Phase 9
    /// strategies). See R-S2b.4 / R-S2b.5 for the prerequisite
    /// helpers + sidecar schema.
    pub sv_source_path: Option<std::path::PathBuf>,

    /// R-S6.6 (§Phase 9 §9.1, 2026-06-11) — filesystem path to
    /// the sidecar that produced `sidecar_json` (when known).
    /// Used by R-S6.6's orchestration to resolve VCD trace paths
    /// declared in `SvAnnotation::vcd_traces`: relative paths in
    /// the sidecar (e.g. `"regression/uart_tx.vcd"`) are resolved
    /// against this path's parent directory.
    ///
    /// When `None`, only absolute trace paths can be read; relative
    /// paths emit an `AdapterWarning` and fall through. Default
    /// `None` preserves the legacy behaviour. Populated by the CLI
    /// (`mununu sv extract --sidecar <path>`) and any API caller
    /// that has the sidecar file system path.
    pub sidecar_path: Option<std::path::PathBuf>,

    /// R-MM-4b (KMTS multi-module composition) — names of module
    /// **output ports** the BTOR2 bit-blaster should surface as per-state
    /// valuations (`<port> = T/F`), in addition to the property-referenced
    /// combinational signals it surfaces by default.
    ///
    /// Net-driving combinational outputs (e.g. a producer's `valid`, a
    /// Moore function of its register) are otherwise dropped entirely — not
    /// a label, not a valuation, not a transition observation — because
    /// they neither appear in a property formula nor name an `Op` line the
    /// default candidate scan picks up. The multi-module composition driver
    /// (R-MM-4d) needs these values to synthesise the value-encoded
    /// rendezvous labels (`net_<v>`) that let a driver instance's output
    /// rendezvous with a reader instance's input under composition.
    ///
    /// A name that is not an output port of the module being lifted is a
    /// silent no-op, so the driver can pass the union of all net-driving
    /// output names across the design. Surfaced ports flow through the
    /// same state-splitting machinery as property-referenced combinational
    /// signals, so an input-dependent (Mealy) output is split per
    /// achievable value. Default empty — the single-module path passes
    /// none, leaving the verdict-parity gate untouched.
    pub surface_output_ports: Vec<String>,

    /// R4W-2 (R.4 clustered-COI wiring) — per-property COI seed atoms
    /// harvested from the verify manifest's `[[properties]]` formulas
    /// (`(property_name, seed_atom_names)`). The verify orchestrator
    /// resolves each property, parses it, and runs
    /// [`crate::adapter::partition::coi::property_seed_atoms`] to fill
    /// this in. When non-empty, the BTOR2 bit-blaster computes a
    /// joint-vs-clustered COI comparison
    /// ([`crate::adapter::partition::coi::cluster_coi_report`]) over its
    /// own dep graph and surfaces it on
    /// [`crate::adapter::partition::PartitionSummary::cluster_coi`].
    ///
    /// Pure telemetry — does **not** change which signals the partition
    /// keeps. Default empty (the legacy intrinsic-seed-only path; no
    /// behaviour change). R4W-2 used the recommended `0.5` default at
    /// the bit-blast layer; R4W-3 threads
    /// [`Self::cluster_similarity_floor`] as the override.
    pub property_seeds: Vec<(String, Vec<String>)>,

    /// R4W-3 (R.4 clustered-COI wiring) — Jaccard similarity floor for
    /// the clustered-COI comparison (see
    /// [`crate::adapter::partition::coi::cluster_properties_by_jaccard`]).
    /// `None` → the bit-blaster uses the recommended `0.5` default
    /// (R4W-2 behaviour). Tighter floors (→ `1.0`) approach
    /// per-property COI; looser floors (→ `0.0`) collapse toward joint
    /// COI. Only consulted when [`Self::property_seeds`] is non-empty.
    /// The verify orchestrator populates this from
    /// `VerifyConfig::cluster_similarity_floor` (settable in
    /// `verify.toml`, or overridden by the CLI `--cluster-coi-floor`
    /// flag / the API request's `cluster_similarity_floor` field).
    pub cluster_similarity_floor: Option<f64>,

    /// R.4.6 (per-cluster verification) — when `Some`, restrict the BTOR2
    /// bit-blast to the cone of influence of these property atoms (one
    /// cluster's members). Every state cell **not** in the cone is pinned
    /// to `Ignored` (cut from the state space), and its width is
    /// subtracted from the `MAX_STATE_BITS` cap check — so a design whose
    /// JOINT cone busts the cap can still be verified per-cluster when
    /// each cluster's cone fits ("joint busts cap, clusters fit").
    ///
    /// SOUNDNESS: the cone of influence is exact (bisimilar) on the atom
    /// set, so cutting out-of-cone cells cannot change any verdict over
    /// those atoms (CLAUDE.md §Soundness — COI is the exact / free /
    /// sound abstraction). This is *not* an approximation. `None`
    /// (default) preserves the joint single-bit-blast behaviour. The
    /// verify orchestrator (R46-2 / R46-3) sets this per cluster from
    /// [`crate::adapter::partition::coi::PropertyCluster::members`]'
    /// resolved atoms. The cone itself is computed by
    /// [`crate::adapter::btor2::dep_graph::state_cone_nids`].
    pub cone_restrict_atoms: Option<Vec<String>>,
}

/// A sidecar file the adapter produced alongside its CTXDSL output.
///
/// Used by the contract subsystem (Document A § A5 / Document B § B.3
/// row "Black-box submodule handling"): when an adapter encounters a
/// black-box module it cannot fully extract, it emits a phase-1 contract
/// description (`BlackBoxInterface`) and a phase-1 gap-marker report
/// (`GapMarkerReport`) as side outputs. Callers (CLI / API / UI) write
/// the sidecars to disk next to the primary CTXDSL file so the rest of
/// the `mununu contract …` workflow can consume them without the user
/// hand-authoring JSON.
///
/// The adapter does **not** decide where the file lands — it returns the
/// suggested filename (relative to the primary CTXDSL output's parent
/// directory) and the rendered content. The caller is responsible for
/// the final write path and any conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSidecar {
    /// Suggested filename, relative to the primary output's parent
    /// directory. Example: `"ddr3_phy.interface.json"`.
    pub filename: String,
    /// Rendered file content (pretty-printed JSON for the contract
    /// sidecars; the caller writes it byte-for-byte).
    pub content: String,
    /// Why this sidecar was emitted. Carried through to user-facing
    /// reports so the operator can see at a glance which adapter slot
    /// produced what.
    pub origin: SidecarOrigin,
}

/// Why an `AdapterSidecar` exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarOrigin {
    /// A `BlackBoxInterface` description for a module the adapter
    /// classified as black-box (un-elaborated submodule, vendor IP,
    /// `(* blackbox *)` attribute).
    BlackBoxInterface,
    /// A `GapMarkerReport` for the same module, prefilled with the
    /// chaotic-stub default gaps.
    BlackBoxGapReport,
}

/// Output from a successful adapter translation.
#[derive(Debug, Clone)]
pub struct AdapterOutput {
    /// The generated CTXDSL text.
    pub ctxdsl: String,
    /// Warnings about unsupported constructs, neutral controllability, etc.
    pub warnings: Vec<AdapterWarning>,
    /// Metadata about the translation.
    pub source_info: SourceInfo,
    /// Sidecar files the adapter produced alongside the CTXDSL.
    /// Currently used by the contract subsystem to emit
    /// `BlackBoxInterface` + `GapMarkerReport` JSONs when the adapter
    /// encounters a black-box submodule. See [`AdapterSidecar`] for the
    /// shape.
    #[doc(hidden)] // kept hidden until B3 lands at least one producer
    pub sidecars: Vec<AdapterSidecar>,
    /// Structured state valuations from cross-product enumeration.
    /// Keyed by `automaton_name → state_name → { variable → display_value }`.
    /// Populated by adapters that enumerate states from register/field domains
    /// (SV Kripke, extraction). Used to wire structured predicate matching
    /// on the CLTS without encoding valuations in the CTXDSL text format.
    pub state_valuations: std::collections::HashMap<
        String,
        std::collections::HashMap<String, std::collections::BTreeMap<String, String>>,
    >,
    /// Per-transition signal observations, keyed by automaton name.
    /// The inner vector mirrors the order of transitions emitted in the
    /// CTXDSL text — adapters that populate it (currently the BTOR2
    /// reader, for Mealy outputs) record `signal → value` pairs that
    /// depend on the input combination of the transition.
    ///
    /// These are *display-only* metadata, never consulted by the
    /// formal evaluator. The CLI / UI trace renderer queries them when
    /// rendering counterexamples and counterstrategies so the user
    /// sees Mealy output values per cycle.
    pub transition_observations: std::collections::HashMap<String, Vec<TransitionObservation>>,
    /// Auto-partition telemetry (Phase A.3 step 3.6) — populated by
    /// adapters that run `crate::adapter::partition::classify` during
    /// translation. Surfaced in CLI / API summaries so users can see
    /// how many signals the cone-of-influence pass dropped without
    /// re-deriving the count from warnings. `None` when the adapter
    /// did not run the partition (e.g. agentic / xstate paths).
    #[doc(hidden)]
    pub partition_summary: Option<partition::PartitionSummary>,
}

/// A single per-transition observation row, emitted by adapters that
/// expose Mealy-style outputs. Used only for trace presentation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TransitionObservation {
    pub source: String,
    pub target: String,
    /// Labels on the transition, mirroring `IRTransition.labels`. The
    /// renderer matches an observation row to a CLTS transition by
    /// `(source, target, sorted-labels)`.
    pub labels: Vec<String>,
    /// `signal_name → display_value` for signals whose value depends
    /// on the input portion of this transition.
    pub observations: std::collections::BTreeMap<String, String>,
}

/// A warning produced during translation.
#[derive(Debug, Clone)]
pub struct AdapterWarning {
    pub kind: WarningKind,
    pub message: String,
    pub location: Option<SourceLocation>,
}

/// Classification of translation warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningKind {
    /// A source-language construct is not supported and was skipped.
    UnsupportedConstruct,
    /// A signal has no inherent controllability; defaulting to uncontrollable.
    NeutralControllability,
    /// The generated state space is large (>10k states).
    LargeStateSpace,
    /// A variable bound overflows the practical state-space limit.
    BoundOverflow,
    /// The translation is an approximation (e.g., Promela liveness under unfairness).
    ApproximateTranslation,
}

/// Source location for diagnostics.
#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

/// Metadata about the translated source.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub format: SourceFormat,
    pub title: Option<String>,
    pub signal_count: usize,
    pub state_count: usize,
    pub property_count: usize,
}

/// Identifies the source format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Tlsf,
    Aiger,
    Btor2,
    Promela,
    XState,
    SystemVerilog,
    Extraction,
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceFormat::Tlsf => write!(f, "TLSF"),
            SourceFormat::Aiger => write!(f, "AIGER"),
            SourceFormat::Btor2 => write!(f, "BTOR2"),
            SourceFormat::Promela => write!(f, "Promela"),
            SourceFormat::XState => write!(f, "XState"),
            SourceFormat::SystemVerilog => write!(f, "SystemVerilog"),
            SourceFormat::Extraction => write!(f, "Extraction"),
        }
    }
}

/// Error during adapter translation.
#[derive(Debug, Clone)]
pub struct AdapterError {
    pub kind: AdapterErrorKind,
    pub message: String,
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterErrorKind {
    /// Syntax error in the source format.
    ParseError,
    /// Unsupported construct that cannot be skipped.
    UnsupportedConstruct,
    /// State-space explosion (too many signals/latches).
    StateSpaceOverflow,
    /// Internal consistency error in the IR.
    IrConsistencyError,
    /// CTXDSL emission failed.
    EmitError,
    /// mununu#504 — a wall-clock or resource budget expired mid-verification. Distinct from the
    /// other kinds because it is NOT a defect in the input: the design is fine, we simply stopped.
    /// `verify_auto` maps it to `Unknown`, never `Skipped` — `ci_exit_code` does not fail on
    /// `skipped`, so a strict `--fail-on unknown` gate would otherwise go GREEN on a property
    /// that timed out.
    ResourceBudgetExceeded,
}

/// mununu#542 — classify an ENGINE error message as a budget abstention or a real defect.
///
/// The BDD / predicate-cube engines signal failure with a `String`, and every *abstention*
/// message carries an `abstained on the <NODE|ITERATION|WALL-CLOCK|BIT CAP> budget` marker (see
/// [`crate::adapter::btor2::symbolic_bitblast`]'s `oom` helper and the budget checks) or names
/// OxiDD's `OutOfMemory` directly.
///
/// Such a message is NOT a defect in the input — the design is fine, we stopped — so it must
/// reach `verify_auto` as [`AdapterErrorKind::ResourceBudgetExceeded`], which maps to `Unknown`.
/// Any other kind falls into `verify_auto`'s generic arm and becomes `Skipped`, and
/// `ci_exit_code` does not fail on `skipped` — so a strict `--fail-on unknown` gate would go
/// GREEN on a property the engine never decided. That is the same silent pass mununu#504
/// removed for the wall-clock budget; this extends it to the engine's other budgets, which were
/// still arriving as `IrConsistencyError`.
pub fn classify_engine_error(message: &str, default: AdapterErrorKind) -> AdapterErrorKind {
    /// Markers that identify an abstention rather than a defect. Kept as substrings because the
    /// engine layer's error channel is `String`; a marker change must update this list.
    const BUDGET_MARKERS: [&str; 2] = ["abstained on the", "OutOfMemory"];
    if BUDGET_MARKERS.iter().any(|m| message.contains(m)) {
        AdapterErrorKind::ResourceBudgetExceeded
    } else {
        default
    }
}

/// mununu#553 ask 1 — WHICH engine budget an abstention hit.
///
/// [`classify_engine_error`] answers *"is this an abstention or a defect?"*. This answers the
/// question a consumer asks next, and could not answer before: **which** budget, so a gate can
/// raise the right knob — or, via [`Self::is_host_dependent`], tell a reproducible abstention
/// from one that depends on the afternoon.
///
/// Kept as substring matching for the same reason as `classify_engine_error`: the engine layer's
/// error channel is a `String`. **A marker change must update [`classify_engine_budget`].**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineBudget {
    /// `MUNUNU_BDD_ITER_BUDGET` — the μ/ν pre-image step cap. Deterministic total work.
    Iteration,
    /// `MUNUNU_BDD_FIXPOINT_NODES` — the fixpoint live-node guard.
    Node,
    /// `MUNUNU_BDD_MAX_BITS` — the cone was wider than the bit cap, before any fixpoint ran.
    BitCap,
    /// The arena-safety bound: the fixpoint approached the OxiDD arena and stopped short of
    /// the uncatchable in-apply exhaustion.
    ArenaSafety,
    /// The 2-D latency bound (nodes AND iterations together).
    FixpointLatency,
    /// The OxiDD arena was exhausted outright (`OutOfMemory`).
    ArenaExhausted,
    /// ⚠️ `MUNUNU_BDD_TIME_BUDGET_MS` — the OPT-IN engine wall clock. **The one engine budget
    /// that is host-dependent**, which is why it defaults to off since mununu#553.
    WallClock,
    /// The message carried an abstention marker this classifier does not recognise. Never
    /// claim determinism for it.
    Unrecognised,
}

impl EngineBudget {
    /// Stable kebab-case tag for gates and log greps.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Iteration => "iteration",
            Self::Node => "node",
            Self::BitCap => "bit-cap",
            Self::ArenaSafety => "arena-safety",
            Self::FixpointLatency => "fixpoint-latency",
            Self::ArenaExhausted => "arena-exhausted",
            Self::WallClock => "wall-clock",
            Self::Unrecognised => "unrecognised",
        }
    }

    /// The env knob to raise, when there is one.
    ///
    /// ⚠️ [`Self::FixpointLatency`] is a **2-D** bound — it fires only when the node count AND
    /// the iteration count are both past their softs — so raising `MUNUNU_BDD_FIXPOINT_ITERS`
    /// escapes it just as well as the node knob named here. The field is singular; that is the
    /// half it names.
    pub fn knob(&self) -> Option<&'static str> {
        match self {
            Self::Iteration => Some("MUNUNU_BDD_ITER_BUDGET"),
            Self::Node | Self::FixpointLatency => Some("MUNUNU_BDD_FIXPOINT_NODES"),
            Self::BitCap => Some("MUNUNU_BDD_MAX_BITS"),
            Self::ArenaSafety | Self::ArenaExhausted => Some("MUNUNU_BDD_ARENA_NODES"),
            Self::WallClock => Some("MUNUNU_BDD_TIME_BUDGET_MS"),
            Self::Unrecognised => None,
        }
    }

    /// mununu#553 ask 2 — would the same command on the same commit abstain again?
    ///
    /// `None` = not established; never guess. `Some(true)` is reserved for the wall clock, the
    /// only engine budget whose outcome is a property of the host rather than of the problem.
    ///
    /// ⚠️ The node-shaped budgets are reproducible *to about 0.01%*, not to the byte — a live
    /// count can differ slightly between runs — so a budget set exactly at a cone's measured
    /// peak can still flip. That is a reason to leave headroom, not a reason to call them
    /// host-dependent.
    pub fn is_host_dependent(&self) -> Option<bool> {
        match self {
            Self::WallClock => Some(true),
            Self::Iteration
            | Self::Node
            | Self::BitCap
            | Self::ArenaSafety
            | Self::FixpointLatency
            | Self::ArenaExhausted => Some(false),
            Self::Unrecognised => None,
        }
    }
}

/// mununu#553 ask 1 — which budget an engine abstention names.
///
/// Substring matching against the engine's verbatim message, for the same reason
/// [`classify_engine_error`] does: the channel is a `String`. Order matters — `OPT-IN
/// WALL-CLOCK` must be tested before the bare budget words it also contains.
pub fn classify_engine_budget(message: &str) -> EngineBudget {
    // WALL-CLOCK first: its message also contains "budget", and mis-filing it as a deterministic
    // budget is the exact confusion mununu#553 exists to remove.
    if message.contains("WALL-CLOCK") {
        EngineBudget::WallClock
    } else if message.contains("OutOfMemory") || message.contains("arena exhausted") {
        EngineBudget::ArenaExhausted
    } else if message.contains("ARENA-SAFETY") {
        EngineBudget::ArenaSafety
    } else if message.contains("LATENCY") {
        EngineBudget::FixpointLatency
    } else if message.contains("ITERATION") {
        EngineBudget::Iteration
    } else if message.contains("NODE") {
        EngineBudget::Node
    } else if message.contains("BIT CAP") {
        EngineBudget::BitCap
    } else {
        EngineBudget::Unrecognised
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(loc) = &self.location {
            write!(f, "{}:{}: {}", loc.line, loc.column, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for AdapterError {}

/// AR-GO-2 — the shared "locate + version-probe a `--version` subprocess tool" body.
/// The per-tool `locate_*` wrappers (`slang`/`btormc`/`cvc5`/`verilator`) were
/// copy-paste-identical modulo the env var, default binary, adapter tag, install hint,
/// and version parser; this factors their common env-or-default → `--version` →
/// status-check body so they cannot drift. Returns `(resolved path, parsed version)`;
/// a missing binary or a failed probe is `Err(UnsupportedConstruct)` so callers degrade
/// gracefully (the tool is simply unavailable).
/// How long a tool gets to answer its discovery probe (`--version`, `--help`, `-V`) before it is
/// reported as unavailable (mununu#876). The stock tools answer in milliseconds; this is the
/// backstop for a wrapper that ignores the flag, or a binary that hangs on start (a licence
/// check, a network filesystem). A probe used to be a bare `Command::output()`, so such a tool
/// hung `mununu` before any member budget applied.
pub(crate) const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a discovery probe produced no answer.
#[derive(Debug)]
pub(crate) enum ProbeFailure {
    /// The binary could not be started (absent, not executable).
    Spawn(std::io::Error),
    /// It started and did not exit within [`PROBE_TIMEOUT`]; it was killed with its group.
    Timeout,
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeFailure::Spawn(e) => write!(f, "{e}"),
            ProbeFailure::Timeout => write!(
                f,
                "no answer within {} s (the process was killed)",
                PROBE_TIMEOUT.as_secs()
            ),
        }
    }
}

/// Run a tool's discovery probe under [`PROBE_TIMEOUT`]: `(status, stdout, stderr)` when it
/// answers, else why not. The child runs in its own process group, so a wrapper's forks die with
/// it (mununu#875).
pub(crate) fn probe_tool(
    path: &std::path::Path,
    args: &[&str],
) -> Result<(std::process::ExitStatus, String, String), ProbeFailure> {
    let mut cmd = std::process::Command::new(path);
    cmd.args(args);
    match run_with_timeout(&mut cmd, None, PROBE_TIMEOUT) {
        Ok(Some(out)) => Ok(out),
        Ok(None) => Err(ProbeFailure::Timeout),
        Err(e) => Err(ProbeFailure::Spawn(e)),
    }
}

pub(crate) fn locate_tool(
    var_name: &str,
    default_bin: &str,
    adapter_tag: &str,
    install_hint: &str,
    parse_version: impl Fn(&str) -> Option<String>,
) -> Result<(std::path::PathBuf, String), AdapterError> {
    use std::path::PathBuf;
    let path = env_var::read(var_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_bin));
    let (status, stdout, _) = probe_tool(&path, &["--version"]).map_err(|e| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!(
            "adapter/{adapter_tag}: failed to invoke `{} --version`: {e}. {install_hint}",
            path.display()
        ),
        location: None,
    })?;
    if !status.success() {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "adapter/{adapter_tag}: `{} --version` exited with status {}",
                path.display(),
                status
            ),
            location: None,
        });
    }
    let version = parse_version(&stdout).unwrap_or_else(|| "<unparseable>".to_string());
    Ok((path, version))
}

/// Run a subprocess with a **wall-clock timeout**, draining stdout/stderr on
/// separate threads (so a large witness trace can't deadlock the pipe) and
/// killing the child if it exceeds `timeout`.
///
/// Returns `Ok(None)` on timeout — the caller treats a timed-out model checker as
/// *inconclusive* (`Unknown`), never as a wrong verdict. `Ok(Some((status, stdout,
/// stderr)))` when the child completes on its own. `stdin_data`, when present, is
/// written to the child's stdin (then EOF); pass `None` for a file-arg tool.
///
/// std-only (no `wait-timeout` crate, no coreutils `timeout`) so it is portable
/// and adds no dependency: model checkers like Pono's IC3 can run unbounded, and
/// this is the portfolio's guard against a single member hanging the whole run.
/// mununu#504 — the wall-clock cap for the three lift subprocesses (yosys, slang, sv2v).
///
/// All three used a bare `Command::output()`, which blocks forever. That is the *first* way a
/// consumer loses a whole run: the lift runs BEFORE any property is attempted, so a hung tool
/// emits zero output and every property reads as absent. btormc and pono have been bounded by
/// `run_with_timeout` for a long time; this closes the gap for the lift.
///
/// Env-overridable via `MUNUNU_LIFT_TIMEOUT_MS` (one variable for all three — YAGNI over three
/// until a measurement says otherwise). `0` disables the cap, matching the escape-hatch
/// convention the other budgets use.
pub(crate) const LIFT_TIMEOUT_ENV: &str = "MUNUNU_LIFT_TIMEOUT_MS";

/// Default lift cap: 15 minutes. Deliberately generous — a large multi-file elaboration under
/// `--single-unit` legitimately takes minutes, and a cap that fires on real work is worse than
/// no cap at all (it converts a slow success into a spurious failure).
const DEFAULT_LIFT_TIMEOUT_MS: u64 = 15 * 60 * 1000;

/// The lift subprocess cap. `None` ⇒ uncapped (`MUNUNU_LIFT_TIMEOUT_MS=0`).
pub(crate) fn lift_timeout() -> Option<std::time::Duration> {
    parse_lift_timeout_ms(crate::adapter::env_var::read(LIFT_TIMEOUT_ENV))
}

/// Pure parse of `MUNUNU_LIFT_TIMEOUT_MS`, extracted so the ladder is unit-testable without
/// touching process-global environment: a positive integer → that many ms; `0` → uncapped;
/// unset or unparseable → the default.
pub(crate) fn parse_lift_timeout_ms(v: Option<String>) -> Option<std::time::Duration> {
    match v.as_deref().map(str::trim) {
        Some("0") => None,
        Some(s) => match s.parse::<u64>() {
            Ok(ms) if ms > 0 => Some(std::time::Duration::from_millis(ms)),
            _ => Some(std::time::Duration::from_millis(DEFAULT_LIFT_TIMEOUT_MS)),
        },
        None => Some(std::time::Duration::from_millis(DEFAULT_LIFT_TIMEOUT_MS)),
    }
}

/// Spawn an external solver so that killing it reaches every process it forked (mununu#875).
///
/// `Child::kill` signals the direct child only. A member whose configured path is a launcher
/// that forks the solver instead of `exec`ing it (a capture, `nice` or `env` shim; a tool that
/// forks its own workers) used to leave that solver running with no timeout and no owner, and
/// the forked process, holding the stdout / stderr pipes open, kept the reader threads, and so
/// the member, waiting past its timeout. On Unix the child now leads its own process group, and
/// [`MemberGroup::kill_and_reap`] kills the group. Because the group no longer receives the
/// terminal's Ctrl-C, a SIGINT / SIGTERM / SIGHUP handler kills every live member group before
/// the process dies; it is installed only where the disposition is still the default, so an
/// embedder's own handler is left alone.
///
/// Private to this module (mununu#1186): [`run_with_timeout_cancellable`] is the one place a
/// member child is spawned and waited on, so every member is stopped by the same four things —
/// its own exit, the wall, the caller's flag, a cancelled run budget — and has its pipes
/// drained while it runs. The two solver runners that had their own wait loop had neither the
/// run-budget stop nor the drain.
trait SpawnMember {
    fn spawn_member(&mut self) -> std::io::Result<(std::process::Child, MemberGroup)>;
}

impl SpawnMember for std::process::Command {
    fn spawn_member(&mut self) -> std::io::Result<(std::process::Child, MemberGroup)> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            self.process_group(0);
            member_group::install_signal_cleanup();
        }
        let child = self.spawn()?;
        let group = MemberGroup::register(&child);
        Ok((child, group))
    }
}

/// The process group of a spawned member; dropping it forgets the group for signal cleanup.
/// On Windows (mununu#904) it is a Job Object the member is assigned to right after spawn: every
/// process the member creates joins the job, `kill_and_reap` terminates the job, and the job is
/// created with `KILL_ON_JOB_CLOSE`, so dropping the last handle — mununu exiting, even on a
/// crash — kills the members too (the role the Unix signal handler plays). A process the member
/// forks in the few microseconds between spawn and assignment escapes the job; a launcher forks
/// after it starts, which is far later.
struct MemberGroup {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(unix)]
    slot: Option<usize>,
    /// The job's handle as an integer (`0` = no job: creation or assignment failed, and the
    /// member falls back to the direct-child kill, logged).
    #[cfg(windows)]
    job: usize,
}

impl MemberGroup {
    #[cfg(unix)]
    fn register(child: &std::process::Child) -> Self {
        // A pid beyond `i32::MAX` cannot occur on a Unix kernel; 0 disables the group kill.
        let pgid = i32::try_from(child.id()).unwrap_or(0);
        Self {
            pgid,
            slot: member_group::remember(pgid),
        }
    }

    #[cfg(windows)]
    fn register(child: &std::process::Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        let job = member_job::create_and_assign(child.as_raw_handle());
        if job == 0 {
            tracing::warn!(
                pid = child.id(),
                "could not put this solver member in a Job Object: a process it forks is not \
                 killed with it on timeout (mununu#904)"
            );
        }
        Self { job }
    }

    #[cfg(not(any(unix, windows)))]
    fn register(_child: &std::process::Child) -> Self {
        Self {}
    }

    /// Kill the member and everything in its process group, then reap the child. The group is
    /// killed before the reap, so its id cannot have been reused by an unrelated process.
    fn kill_and_reap(&self, child: &mut std::process::Child) {
        #[cfg(unix)]
        member_group::kill(self.pgid);
        #[cfg(windows)]
        member_job::terminate(self.job);
        let _ = child.kill(); // the direct child, also where the group kill is unavailable
        let _ = child.wait();
    }
}

impl Drop for MemberGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(slot) = self.slot {
            member_group::forget(slot);
        }
        // Closing the last handle of a KILL_ON_JOB_CLOSE job kills what is still in it — a
        // member that finished normally has nothing left, so this only reaches stragglers.
        #[cfg(windows)]
        member_job::close(self.job);
    }
}

#[cfg(windows)]
mod member_job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// Create a kill-on-close Job Object and assign `process` to it; the job's handle as an
    /// integer, or `0` when any step fails (the caller logs and falls back).
    pub(super) fn create_and_assign(process: std::os::windows::io::RawHandle) -> usize {
        // SAFETY: plain Win32 calls with valid arguments: a null security descriptor and name
        // create an anonymous job; `info` is a zero-initialised POD struct of the size passed;
        // `process` is the live child's handle, owned by the `Child` for this call's duration.
        unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
            );
            if set == 0 || AssignProcessToJobObject(job, process as HANDLE) == 0 {
                CloseHandle(job);
                return 0;
            }
            job as usize
        }
    }

    pub(super) fn terminate(job: usize) {
        if job != 0 {
            // SAFETY: `job` is a job handle this module created and has not closed yet.
            unsafe {
                TerminateJobObject(job as HANDLE, 1);
            }
        }
    }

    pub(super) fn close(job: usize) {
        if job != 0 {
            // SAFETY: as above; closed exactly once, from `MemberGroup::drop`.
            unsafe {
                CloseHandle(job as HANDLE);
            }
        }
    }
}

#[cfg(unix)]
mod member_group {
    use std::sync::atomic::{AtomicI32, Ordering::SeqCst};

    /// Live member groups, read by the signal handler. A fixed table of atomics, because a
    /// handler may neither lock nor allocate. When it is full a member still gets its timeout
    /// kill; only the signal-time cleanup misses it, and that is logged.
    const SLOTS: usize = 256;
    static LIVE: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];
    static INSTALL: std::sync::Once = std::sync::Once::new();

    pub(super) fn remember(pgid: i32) -> Option<usize> {
        if pgid <= 0 {
            return None;
        }
        let slot = LIVE
            .iter()
            .position(|s| s.compare_exchange(0, pgid, SeqCst, SeqCst).is_ok());
        if slot.is_none() {
            tracing::warn!(
                pgid,
                "more than {SLOTS} live solver processes: this one is not killed if mununu is interrupted"
            );
        }
        slot
    }

    pub(super) fn forget(slot: usize) {
        LIVE[slot].store(0, SeqCst);
    }

    pub(super) fn kill(pgid: i32) {
        if pgid > 0 {
            // SAFETY: `kill(2)` with a negative pid signals that process group; it has no
            // memory-safety preconditions. ESRCH (the group is already gone) is the common case.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    extern "C" fn on_terminating_signal(sig: libc::c_int) {
        for s in &LIVE {
            kill(s.load(SeqCst));
        }
        // SAFETY: `signal` and `raise` are async-signal-safe. Restoring the default action and
        // re-raising terminates the process exactly as the signal would have without the handler.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    pub(super) fn install_signal_cleanup() {
        INSTALL.call_once(|| {
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                // SAFETY: a zeroed `sigaction` is a valid out-parameter; querying with a null
                // new action changes nothing. The handler only touches atomics and calls
                // async-signal-safe functions.
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(sig, std::ptr::null(), &mut old) != 0
                        || old.sa_sigaction != libc::SIG_DFL
                    {
                        continue; // ignored, or someone else's handler: leave it alone
                    }
                    let handler: extern "C" fn(libc::c_int) = on_terminating_signal;
                    libc::signal(sig, handler as libc::sighandler_t);
                }
            }
        });
    }
}

pub(crate) fn run_with_timeout(
    command: &mut std::process::Command,
    stdin_data: Option<&[u8]>,
    timeout: std::time::Duration,
) -> std::io::Result<Option<(std::process::ExitStatus, String, String)>> {
    run_with_timeout_cancellable(command, stdin_data, timeout, None)
}

/// How long a cancelled portfolio member keeps running after it first observes the `cancel`
/// flag. The reach portfolio sets the flag the moment one member decides; a member that is
/// about to finish still lands — so it is listed in `reachable_by` / `unreachable_by` and
/// still reaches the inter-engine contradiction alarm — if it finishes inside this window
/// (the owned-only driver's straggler grace, applied to every cancellable member: the
/// btormc / Pono / rIC3 children and native k-induction's depth loop). Short on
/// purpose: past it the member's remaining budget — up to a 60 s btormc / Pono timeout, 40 depths × 2 queries × 5 s for native — is pure wall-clock the caller
/// would otherwise wait for. On a design every member decides in milliseconds, nothing is
/// cancelled and the attribution is the same as before S1.
pub(crate) const MEMBER_CANCEL_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// The portfolio's cooperative cancel with its grace, in one place (mununu#966 — it was written
/// by hand at four sites): the clock starts the first time the flag is seen set, and
/// [`past_grace`](Self::past_grace) is true once [`MEMBER_CANCEL_GRACE`] has elapsed since —
/// a member that finishes inside the window still lands. A flag that is never set, or no flag,
/// never trips it.
#[derive(Debug)]
pub(crate) struct CancelLatch {
    grace: std::time::Duration,
    seen: Option<std::time::Instant>,
}

impl CancelLatch {
    pub(crate) fn new() -> Self {
        Self::with_grace(MEMBER_CANCEL_GRACE)
    }

    /// A latch with a window other than [`MEMBER_CANCEL_GRACE`], which every member uses
    /// ([`Self::new`]). This is the seam for a test of what a member does on each side of
    /// the window (mununu#1236): with the real window, "the run finished inside it" is a
    /// fact about the host, so such a test passes a window no run outlasts, or none at all.
    pub(crate) fn with_grace(grace: std::time::Duration) -> Self {
        Self { grace, seen: None }
    }

    /// Poll the flag: `true` once it has been set for the grace. `None` = no flag.
    pub(crate) fn past_grace(&mut self, cancel: Option<&std::sync::atomic::AtomicBool>) -> bool {
        cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            && self
                .seen
                .get_or_insert_with(std::time::Instant::now)
                .elapsed()
                >= self.grace
    }
}

#[cfg(test)]
mod cancel_latch_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// mununu#1186 — a child's output that is not UTF-8 is kept, with the bad bytes replaced.
    /// `read_to_string` gave nothing for it: on a clean exit the reply read as empty, and on a
    /// stop at the limit the stderr line that says so was lost.
    #[test]
    fn x1186_a_child_s_output_that_is_not_utf8_is_kept_lossily() {
        assert_eq!(read_lossy(&b"un\xffsat\n"[..]), "un\u{fffd}sat\n");
        assert_eq!(read_lossy(&b"cvc5 interrupted by timeout.\n"[..]).len(), 29);
        assert_eq!(read_lossy(&b""[..]), "");
    }

    /// The grace runs from the FIRST sighting of the flag, not from the latch's creation, and
    /// a latch without a flag, or with one that is never set, never trips.
    #[test]
    fn the_latch_trips_one_grace_after_the_flag_is_first_seen() {
        let flag = AtomicBool::new(false);
        let mut latch = CancelLatch::new();
        assert!(!latch.past_grace(None));
        std::thread::sleep(MEMBER_CANCEL_GRACE + std::time::Duration::from_millis(20));
        assert!(
            !latch.past_grace(Some(&flag)),
            "never set: never trips, however old"
        );
        flag.store(true, Ordering::Relaxed);
        assert!(
            !latch.past_grace(Some(&flag)),
            "the first sighting starts the clock"
        );
        std::thread::sleep(MEMBER_CANCEL_GRACE + std::time::Duration::from_millis(20));
        assert!(latch.past_grace(Some(&flag)));
        assert!(latch.past_grace(Some(&flag)), "and stays tripped");
    }

    /// mununu#1236 — the window is the latch's own: with none, a set flag trips it at the
    /// first sighting; an unset flag, or no flag, still never does.
    #[test]
    fn a_latch_with_no_window_trips_at_the_first_sighting_of_a_set_flag() {
        let flag = AtomicBool::new(false);
        let mut latch = CancelLatch::with_grace(std::time::Duration::ZERO);
        assert!(!latch.past_grace(None));
        assert!(!latch.past_grace(Some(&flag)));
        flag.store(true, Ordering::Relaxed);
        assert!(latch.past_grace(Some(&flag)));
    }
}

/// [`run_with_timeout`] with a cooperative **cancel** flag (S1 of the engine-performance
/// roadmap, category 4). Polled on the same 20 ms cadence as the timeout; once the flag is
/// set the child gets [`MEMBER_CANCEL_GRACE`] and is then killed and reaped, which the
/// caller reads exactly like a timeout — `Ok(None)`, a sound abstention. `None` for the flag
/// recovers the plain timeout behaviour.
pub(crate) fn run_with_timeout_cancellable(
    command: &mut std::process::Command,
    stdin_data: Option<&[u8]>,
    timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> std::io::Result<Option<(std::process::ExitStatus, String, String)>> {
    use std::io::Write;
    use std::process::Stdio;
    let (mut child, group) = command
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_member()?;
    // Feed stdin on its own thread (owned copy) so a large input can't deadlock
    // against a tool that interleaves reading stdin with writing stdout — the
    // writer, stdout-reader, and stderr-reader all run concurrently.
    if let Some(mut si) = child.stdin.take() {
        let data = stdin_data.unwrap_or_default().to_vec();
        std::thread::spawn(move || {
            let _ = si.write_all(&data); // drop `si` ⇒ EOF to the child
        });
    }
    // Drain both pipes concurrently so the child never blocks on a full buffer.
    let so = child.stdout.take().expect("piped stdout");
    let se = child.stderr.take().expect("piped stderr");
    let so_h = std::thread::spawn(move || read_lossy(so));
    let se_h = std::thread::spawn(move || read_lossy(se));
    let start = std::time::Instant::now();
    let mut latch = CancelLatch::new();
    enum End {
        Done(std::process::ExitStatus),
        Killed,
        Cancelled,
    }
    let end = loop {
        match child.try_wait() {
            Ok(Some(st)) => break End::Done(st),
            Ok(None) => {}
            // The child's state is unknown: it is not left running with no owner.
            Err(e) => {
                group.kill_and_reap(&mut child);
                return Err(e);
            }
        }
        // mununu#1147 — the run that started this child is gone (an HTTP request past its
        // timeout cancels its budget): kill it now, and say so rather than "timed out".
        if crate::adapter::run_budget::cancelled() {
            group.kill_and_reap(&mut child);
            break End::Cancelled;
        }
        if latch.past_grace(cancel) || start.elapsed() >= timeout {
            group.kill_and_reap(&mut child); // the group's pipes close, so the readers return
            break End::Killed;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let stdout = so_h.join().unwrap_or_default();
    let stderr = se_h.join().unwrap_or_default();
    match end {
        End::Done(st) => Ok(Some((st, stdout, stderr))),
        End::Killed => Ok(None),
        End::Cancelled => Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            RUN_CANCELLED,
        )),
    }
}

/// mununu#1196 — wait for a child to its end, as `Command::output` does, through the one helper:
/// for a tool that has no wall, or whose wall the user turned off (`MUNUNU_LIFT_TIMEOUT_MS=0`).
/// "No wall" is not "no owner": the child leads its own group, its pipes are drained while it
/// runs, and a cancelled run budget kills it ([`RUN_CANCELLED`], kind `Interrupted`) — a bare
/// `output()` left it running after the request that started it was gone. Standard input is
/// closed and both outputs are text, lossily decoded, as every caller read them.
pub(crate) fn run_to_end(
    command: &mut std::process::Command,
) -> std::io::Result<(std::process::ExitStatus, String, String)> {
    // `Duration::MAX` is never reached: the helper compares the time elapsed with it.
    match run_with_timeout_cancellable(command, None, std::time::Duration::MAX, None)? {
        Some(done) => Ok(done),
        // The wall and the flag are the only two ways to a `None`, and this wait has neither.
        None => Err(std::io::Error::other(
            "the wait for the tool ended with no wall and no cancel flag",
        )),
    }
}

/// A child's pipe read to its end, as text. Bytes that are not UTF-8 become U+FFFD:
/// `read_to_string` returns nothing at all for such a stream, so a solver's reply, or the
/// stderr line that says why it stopped, was dropped whole — the two solver runners decoded
/// lossily before they moved onto this helper (mununu#1186).
fn read_lossy(mut pipe: impl std::io::Read) -> String {
    let mut bytes = Vec::new();
    // A read error ends the stream where it failed; what arrived before it is kept.
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// mununu#1147 — the error a subprocess helper returns when the thread's run budget was
/// cancelled under it: the tool was killed because the caller is gone, not because it was slow.
pub(crate) const RUN_CANCELLED: &str =
    "the run was cancelled before the tool finished (the request that started it is gone)";

/// Detect the source format from a file extension.
pub fn detect_format_by_extension(path: &std::path::Path) -> Option<&'static str> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.ends_with(".espec") {
        return Some("extraction");
    }
    if stem.ends_with(".xstate") {
        return Some("xstate");
    }
    if stem.ends_with(".crewai") {
        return Some("crewai");
    }
    if stem.ends_with(".langgraph") {
        return Some("langgraph");
    }
    if stem.ends_with(".microcode") {
        return Some("microcode");
    }

    match path.extension().and_then(|e| e.to_str()) {
        Some("tlsf") => Some("tlsf"),
        Some("aag") | Some("aig") => Some("aiger"),
        Some("btor") | Some("btor2") => Some("btor2"),
        Some("pml") | Some("promela") => Some("promela"),
        Some("xstate") => Some("xstate"),
        Some("crewai") => Some("crewai"),
        Some("langgraph") => Some("langgraph"),
        Some("microcode") => Some("microcode"),
        Some("sv") | Some("v") => Some("systemverilog"),
        _ => None,
    }
}

/// Auto-detect the format of the given content and translate it.
pub fn auto_translate(
    content: &str,
    options: &AdapterOptions,
) -> Result<AdapterOutput, AdapterError> {
    if tlsf::TlsfAdapter::detect(content) {
        return tlsf::TlsfAdapter::translate(content, options);
    }
    if aiger::AigerAdapter::detect(content) {
        return aiger::AigerAdapter::translate(content, options);
    }
    if btor2::Btor2Adapter::detect(content) {
        return btor2::Btor2Adapter::translate(content, options);
    }
    if promela::PromelaAdapter::detect(content) {
        return promela::PromelaAdapter::translate(content, options);
    }
    if xstate::XStateAdapter::detect(content) {
        return xstate::XStateAdapter::translate(content, options);
    }
    // CrewAI / LangGraph come after XState — XState's `states` + `initial`
    // shape is distinct from CrewAI's `agents` + `tasks` and LangGraph's
    // `nodes` + `edges`, so order avoids false positives.
    if crewai::CrewaiAdapter::detect(content) {
        return crewai::CrewaiAdapter::translate(content, options);
    }
    if langgraph::LangGraphAdapter::detect(content) {
        return langgraph::LangGraphAdapter::translate(content, options);
    }
    // Microcode after LangGraph — microcode's `steps` array plus
    // `regs`/`mem`/`interrupts` resource declarations make it
    // unambiguous against LangGraph's `nodes` + `edges` shape.
    if microcode::MicrocodeAdapter::detect(content) {
        return microcode::MicrocodeAdapter::translate(content, options);
    }
    // SystemVerilog is intentionally NOT content-auto-detected: the sole
    // SV route (`sv-yosys`) runs sv2v + Yosys subprocesses over the file
    // on disk (plus any submodule sources), which the content-only
    // auto-detect path cannot supply. Drive SV explicitly via the verify
    // framework with `adapter = "sv-yosys"`.
    if extraction::ExtractionAdapter::detect(content) {
        return extraction::ExtractionAdapter::translate(content, options);
    }

    Err(AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: "Could not detect source format. Supported formats: TLSF (.tlsf), AIGER (.aag/.aig), BTOR2 (.btor/.btor2), Promela (.pml), XState (.xstate or .xstate.json), CrewAI (.crewai.json), LangGraph (.langgraph.json), Extraction (.espec.json). SystemVerilog is not auto-detected — use the verify framework with adapter = \"sv-yosys\".".into(),
        location: None,
    })
}

#[cfg(test)]
mod tests {
    /// S1 (engine-performance roadmap, category 4) — a child whose portfolio `cancel` flag
    /// flips is killed after the grace, long before its own timeout, and reads as a timeout
    /// (`None`). A flag that never flips leaves the timeout behaviour untouched.
    #[test]
    fn a_cancelled_child_is_killed_after_the_grace_not_at_its_timeout() {
        use super::{MEMBER_CANCEL_GRACE, run_with_timeout_cancellable};
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        use std::time::{Duration, Instant};

        let cancel = AtomicBool::new(false);
        let t0 = Instant::now();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(100));
                cancel.store(true, Relaxed);
            });
            let mut cmd = std::process::Command::new("sleep");
            cmd.arg("30");
            run_with_timeout_cancellable(&mut cmd, None, Duration::from_secs(30), Some(&cancel))
                .expect("spawn ok")
        });
        let took = t0.elapsed();
        assert!(
            outcome.is_none(),
            "a cancelled child reports exactly like a timeout"
        );
        assert!(
            took >= MEMBER_CANCEL_GRACE,
            "the grace is honoured before the kill, took {took:?}"
        );
        assert!(
            took < Duration::from_secs(5),
            "killed within the grace plus the poll cadence, not at the 30 s timeout: {took:?}"
        );

        // Control: an unset flag changes nothing — the child still runs to completion.
        let never = AtomicBool::new(false);
        let mut cmd = std::process::Command::new("true");
        let outcome =
            run_with_timeout_cancellable(&mut cmd, None, Duration::from_secs(30), Some(&never))
                .expect("spawn ok");
        assert!(outcome.is_some_and(|(st, _, _)| st.success()));
    }

    /// mununu#542 — an engine BUDGET abstention must be classified `ResourceBudgetExceeded`, so
    /// `verify_auto` maps it to `Unknown`.
    ///
    /// Before this, every engine error — including the NODE / ITERATION / WALL-CLOCK budget
    /// abstentions and OxiDD's `OutOfMemory` — arrived as `IrConsistencyError` and fell into
    /// `verify_auto`'s generic arm, which yields `Skipped`. `ci_exit_code` never fails on
    /// `skipped`, so a strict `--fail-on unknown` gate went GREEN on a property the engine had
    /// explicitly abstained on. mununu#504 removed that silent pass for the wall-clock budget at
    /// the `verify_auto` seam; the engine's own budgets were still arriving misclassified.
    #[test]
    fn an_engine_budget_abstention_is_a_budget_error_not_an_ir_defect() {
        use super::{AdapterErrorKind, classify_engine_error};

        // Every abstention the engine can emit (cf. `symbolic_bitblast`'s `oom` helper and the
        // NODE / ITERATION / WALL-CLOCK guards).
        for msg in [
            "symbolic bit-blaster: BDD arena exhausted (OxiDD OutOfMemory) — abstained on the \
             NODE budget; the cone does not compress to a tractable BDD",
            "abstained on the ITERATION budget",
            "abstained on the WALL-CLOCK budget",
            "abstained on the BIT CAP",
            "called `Result::unwrap()` on an `Err` value: OutOfMemory",
        ] {
            assert_eq!(
                classify_engine_error(msg, AdapterErrorKind::IrConsistencyError),
                AdapterErrorKind::ResourceBudgetExceeded,
                "abstention must reach verify_auto as a budget error, else it becomes \
                 `Skipped` and no CI gate fails on it: {msg}"
            );
        }
    }

    /// ...and the special case stays NARROW: a real defect keeps its kind, so it still maps to
    /// `Skipped` with its message. Widening this would hide genuine input defects as `unknown`.
    #[test]
    fn a_real_engine_defect_keeps_its_kind() {
        use super::{AdapterErrorKind, classify_engine_error};

        for msg in [
            "adapter/btor2: operator Read unsupported in Phase 1 bit-blaster",
            "label intern failed",
            "builder.build failed",
        ] {
            assert_eq!(
                classify_engine_error(msg, AdapterErrorKind::IrConsistencyError),
                AdapterErrorKind::IrConsistencyError,
                "a defect must NOT be laundered into a budget abstention: {msg}"
            );
        }
    }

    use super::detect_format_by_extension;
    use std::path::Path;

    #[test]
    fn detects_xstate_compound_extension() {
        assert_eq!(
            detect_format_by_extension(Path::new("support_pipeline.xstate.json")),
            Some("xstate")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("/abs/path/auth_flow.xstate.json")),
            Some("xstate")
        );
    }

    #[test]
    fn detects_extraction_compound_extension() {
        assert_eq!(
            detect_format_by_extension(Path::new("game.espec.json")),
            Some("extraction")
        );
    }

    #[test]
    fn detects_simple_extensions() {
        assert_eq!(
            detect_format_by_extension(Path::new("design.sv")),
            Some("systemverilog")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("model.tlsf")),
            Some("tlsf")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("circuit.aag")),
            Some("aiger")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("proc.pml")),
            Some("promela")
        );
    }

    #[test]
    fn detects_crewai_compound_extension() {
        assert_eq!(
            detect_format_by_extension(Path::new("research_crew.crewai.json")),
            Some("crewai")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("/abs/path/crew.crewai")),
            Some("crewai")
        );
    }

    #[test]
    fn detects_langgraph_compound_extension() {
        assert_eq!(
            detect_format_by_extension(Path::new("workflow.langgraph.json")),
            Some("langgraph")
        );
        assert_eq!(
            detect_format_by_extension(Path::new("/abs/path/graph.langgraph")),
            Some("langgraph")
        );
    }

    #[test]
    fn returns_none_for_unknown_or_plain_json() {
        assert_eq!(detect_format_by_extension(Path::new("README.md")), None);
        // A plain .json file (not .xstate.json or .espec.json) should not auto-route.
        // Content-based detection via `auto_translate` is the right path here.
        assert_eq!(detect_format_by_extension(Path::new("payload.json")), None);
    }
}

/// mununu#873 — the reason a model-checker subprocess gave for exiting with no verdict, for a
/// portfolio refusal. stderr when it has text; otherwise stdout, where pono writes its own
/// rejection (`Unsupported variable sort in IC3Bits: …`) — quoting stderr alone recorded an empty
/// reason on every HWMCC'20 array instance. Capped so a chatty tool cannot flood the report.
pub(crate) fn subprocess_failure_reason(stderr: &str, stdout: &str) -> String {
    const CAP: usize = 400;
    let text = match stderr.trim() {
        "" => stdout.trim(),
        e => e,
    };
    let reason = match text.char_indices().nth(CAP) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    };
    if reason.is_empty() {
        "(no output on stderr or stdout)".to_string()
    } else {
        reason
    }
}

/// The directory mununu creates its per-call working directories under (the lift's staging
/// directory, a Verilator build): `MUNUNU_LIFT_STAGING_ROOT` when set, else the system temp
/// directory — absolute either way, since the tools run inside the directories made here and
/// name their outputs by absolute path (mununu#1138, mununu#1176).
pub(crate) fn staging_root() -> std::io::Result<std::path::PathBuf> {
    let root = env_var::read("MUNUNU_LIFT_STAGING_ROOT")
        .filter(|v| !v.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    if root.is_absolute() {
        Ok(root)
    } else {
        Ok(std::env::current_dir()?.join(root))
    }
}

/// A source a caller NAMED, staged under `dir`: `dir/<name>`, or a refusal when the name would
/// leave `dir`.
///
/// Every SV staging path joins a request-supplied file name (`additional_sources[].name`,
/// `source_name`, `lift_overrides[].name`) onto its temp dir. `Path::join` with an absolute name
/// REPLACES the dir, and a `..` component climbs out of it, so an unchecked name lets a request
/// write a file anywhere the process can. Only plain components (and `.`) are accepted: no root,
/// no prefix, no `..`, not empty.
pub(crate) fn staged_path(
    dir: &std::path::Path,
    name: &str,
) -> Result<std::path::PathBuf, AdapterError> {
    use std::path::Component;
    let p = std::path::Path::new(name);
    let plain = p
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if name.is_empty() || !plain || !p.components().any(|c| matches!(c, Component::Normal(_))) {
        return Err(AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!(
                "source name `{name}` is not a relative file name: a staged source is named by \
                 plain path components (no leading `/`, no `..`)"
            ),
            location: None,
        });
    }
    Ok(dir.join(p))
}

/// mununu#980 — the no-verdict line of a solver member, with a signal death named as a crash. A
/// tool killed by a signal (`signal: 6` = SIGABRT: an internal assertion or error) did not refuse
/// the design — it crashed on it, and the portfolio note must let a reader tell the two apart: a
/// refusal is a construct the member does not carry, an abort is a defect to reproduce. On a
/// non-Unix host, or for an ordinary exit code, the line is the plain "exited with status" form.
pub(crate) fn no_verdict_line(
    tool: &std::path::Path,
    status: std::process::ExitStatus,
    stderr: &str,
    stdout: &str,
) -> String {
    let reason = subprocess_failure_reason(stderr, stdout);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                libc::SIGABRT => " (SIGABRT)",
                libc::SIGSEGV => " (SIGSEGV)",
                libc::SIGBUS => " (SIGBUS)",
                libc::SIGFPE => " (SIGFPE)",
                libc::SIGILL => " (SIGILL)",
                libc::SIGKILL => " (SIGKILL)",
                _ => "",
            };
            return format!(
                "`{}` ABORTED by signal {sig}{name} with no verdict — a crash inside the tool, not a \
                 refusal of the design: {reason}",
                tool.display()
            );
        }
    }
    format!(
        "`{}` exited with status {status} and no verdict: {reason}",
        tool.display()
    )
}

#[cfg(test)]
mod staged_path_tests {
    use super::staged_path;

    /// mununu#994 sibling — a request-supplied source name stays under the staging dir: plain
    /// components are joined, and a root, a `..` or an empty name is refused. On the parent every
    /// staging site used `dir.join(name)`, which an absolute name REPLACES and `..` climbs out of.
    #[test]
    fn a_source_name_that_would_leave_the_staging_dir_is_refused() {
        let dir = std::path::Path::new("stage");
        assert_eq!(
            staged_path(dir, "pkg/defs.vh").expect("plain"),
            dir.join("pkg/defs.vh")
        );
        assert_eq!(
            staged_path(dir, "./a.sv").expect("curdir"),
            dir.join("a.sv")
        );
        let abs = if cfg!(windows) {
            r"C:\x\a.sv"
        } else {
            "/tmp/a.sv"
        };
        for bad in [abs, "../a.sv", "pkg/../../a.sv", "", ".", "/"] {
            let err = staged_path(dir, bad).expect_err(bad);
            assert!(
                err.message.contains("not a relative file name"),
                "{bad}: {}",
                err.message
            );
        }
        // `Path::join` is what made this a defect: an absolute name replaces the dir.
        assert_eq!(dir.join(abs), std::path::Path::new(abs));
    }
}

#[cfg(test)]
mod subprocess_failure_reason_tests {
    use super::subprocess_failure_reason;

    /// mununu#980 — a member killed by SIGABRT reads as an abort, not a refusal; an ordinary
    /// non-zero exit keeps the plain form. Runs on every OS (the OS matrix's same-tests rule):
    /// the signal half is Unix-only, as signals are.
    #[test]
    fn a_signal_death_is_named_an_abort_and_an_exit_code_is_not() {
        let tool = std::path::Path::new("rIC3");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let aborted = super::no_verdict_line(
                tool,
                std::process::ExitStatus::from_raw(libc::SIGABRT),
                "assertion failed",
                "",
            );
            assert!(
                aborted.contains("ABORTED by signal 6 (SIGABRT)")
                    && aborted.contains("not a refusal"),
                "{aborted}"
            );
            assert!(aborted.ends_with("assertion failed"), "{aborted}");
        }
        #[cfg(unix)]
        let exit_one = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(1 << 8)
        };
        #[cfg(windows)]
        let exit_one = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(1)
        };
        let exited = super::no_verdict_line(tool, exit_one, "", "parse error");
        assert!(
            exited.contains("exited with status") && !exited.contains("ABORTED"),
            "{exited}"
        );
        assert!(exited.ends_with("parse error"), "{exited}");
    }

    #[test]
    fn a_tool_that_explains_itself_on_stdout_is_quoted() {
        let stdout = "Unsupported variable sort in IC3Bits: mem:(Array (_ BitVec 2) (_ BitVec 30))\nerror\nb0\n";
        assert!(subprocess_failure_reason("", stdout).starts_with("Unsupported variable sort"));
    }

    #[test]
    fn stderr_wins_when_it_has_text_and_silence_says_so() {
        assert_eq!(
            subprocess_failure_reason("parse error\n", "sat"),
            "parse error"
        );
        assert_eq!(
            subprocess_failure_reason(" ", "\n"),
            "(no output on stderr or stdout)"
        );
        assert!(
            subprocess_failure_reason("", &"x".repeat(1000))
                .chars()
                .count()
                <= 401
        );
    }
}

#[cfg(test)]
mod run_cancel_tests {
    //! mununu#1147 — a cancelled run budget kills the child it is waiting on.

    /// REPRODUCER — the subprocess helper under a budget that is cancelled while the child
    /// runs returns at once with the cancellation, not after the timeout (fails on the parent:
    /// the child runs to the 3 s timeout and the result is the timeout's `None`).
    #[test]
    fn x1147_a_cancelled_run_budget_kills_the_child_it_waits_on() {
        use crate::adapter::run_budget::{Budget, enter};
        let budget = Budget::unbounded();
        let _g = enter(&budget);
        let flag = budget.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            flag.cancel();
        });
        let t0 = std::time::Instant::now();
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        let r = super::run_with_timeout(&mut cmd, None, std::time::Duration::from_secs(3));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(1500),
            "killed on the cancel, not at the timeout: {:?}",
            t0.elapsed()
        );
        match r {
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::Interrupted);
                assert!(e.to_string().contains("cancelled"), "{e}");
            }
            Ok(other) => panic!("a cancelled run is an error, not {other:?}"),
        }
    }
}

/// mununu#1247 — the wait a test makes before it runs a script it has just written.
///
/// A test writes a `/bin/sh` stand-in for a tool, makes it executable and runs it. While the
/// file was open for writing, a thread of another test may have forked a child of its own; that
/// child holds a copy of the descriptor until its own `exec`, and until then the kernel refuses
/// to run the script (`ETXTBSY`, "Text file busy"). Nothing can take a new copy once the write
/// has returned, so the first probe the kernel does not refuse ends the exposure.
///
/// `pub`, not `pub(crate)`: the source scans of `tests/policy` drop a test-only block whose
/// header reads `mod` or `pub mod`, and read anything else as shipped code.
#[cfg(test)]
pub mod stand_in_script {
    /// `ETXTBSY`, on Linux and on macOS.
    const TEXT_FILE_BUSY: i32 = 26;

    /// The probes the wait makes before it hands the script back as it is. The test that then
    /// runs it fails on the exec, with the kernel's own error.
    const TRIES: usize = 500;

    /// Runs `script` with `probe_arg`, 10 ms apart, until the kernel no longer refuses it as
    /// busy. `probe_arg` is an argument the script answers at once (`--version`, `--probe`);
    /// what the probe run prints or returns is not looked at.
    #[cfg(unix)]
    pub fn wait_out_a_busy_script(script: &std::path::Path, probe_arg: &str) {
        wait_out(
            || {
                std::process::Command::new(script)
                    .arg(probe_arg)
                    .output()
                    .map(drop)
            },
            || std::thread::sleep(std::time::Duration::from_millis(10)),
        );
    }

    /// The loop of `wait_out_a_busy_script` with its probe and its pause handed in. `pause`
    /// runs after each probe the kernel refused as busy; the loop ends at the first probe that
    /// is anything else, or after [`TRIES`] refusals. Returns how many probes were refused.
    fn wait_out(mut probe: impl FnMut() -> std::io::Result<()>, mut pause: impl FnMut()) -> usize {
        let mut refused = 0;
        while refused < TRIES {
            match probe() {
                Err(e) if e.raw_os_error() == Some(TEXT_FILE_BUSY) => {
                    refused += 1;
                    pause();
                }
                _ => break,
            }
        }
        refused
    }

    /// The loop alone, the same on every OS: it pauses once per refusal, stops at the first
    /// answer that is not one, does not wait on another error (the test that runs the script
    /// reports that), and gives up after [`TRIES`].
    #[test]
    fn x1247_the_wait_pauses_once_per_busy_refusal_and_ends_at_the_first_other_answer() {
        let busy = || std::io::Error::from_raw_os_error(TEXT_FILE_BUSY);

        let mut answers = vec![Ok(()), Err(busy()), Err(busy())];
        let (mut probes, mut pauses) = (0, 0);
        let refused = wait_out(
            || {
                probes += 1;
                answers.pop().expect("three answers, three probes")
            },
            || pauses += 1,
        );
        assert_eq!((refused, probes, pauses), (2, 3, 2));

        let mut probes = 0;
        let refused = wait_out(
            || {
                probes += 1;
                Err(std::io::Error::from_raw_os_error(2))
            },
            || panic!("an error that is not the busy one is not waited on"),
        );
        assert_eq!((refused, probes), (0, 1));

        let mut probes = 0;
        let refused = wait_out(
            || {
                probes += 1;
                Err(busy())
            },
            || {},
        );
        assert_eq!((refused, probes), (TRIES, TRIES));
    }

    /// The kernel's side, on the hosts that have one to ask: a script somebody still holds open
    /// for writing, which is what a fork on another test's thread leaves behind. A plain exec
    /// asks the kernel first whether it refuses such a script. Where it does, the wait sees the
    /// refusal, the pause lets the holder go, and the script then runs; where it does not, the
    /// wait has nothing to do and the script runs all the same. No clock decides either branch.
    /// The test exists on every OS (the OS matrix's same-tests rule). Its body is empty on
    /// Windows, where the stand-ins are not `/bin/sh` scripts and this wait is not used.
    #[test]
    fn x1247_a_script_still_open_for_writing_elsewhere_is_waited_out() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().expect("a temporary directory");
            let script = dir.path().join("stand-in");
            std::fs::write(&script, "#!/bin/sh\necho ran\n").expect("write the script");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            let run = || std::process::Command::new(&script).output();

            let mut holder = Some(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&script)
                    .expect("open the script for writing"),
            );
            let kernel_refuses =
                matches!(run(), Err(e) if e.raw_os_error() == Some(TEXT_FILE_BUSY));
            // The first pause closes the holder. A later one waits for a copy that a fork of
            // another test took while this test wrote the script, as the real wait does.
            let refused = wait_out(
                || run().map(drop),
                || match holder.take() {
                    Some(held) => drop(held),
                    None => std::thread::sleep(std::time::Duration::from_millis(10)),
                },
            );
            assert_eq!(
                refused > 0,
                kernel_refuses,
                "the wait saw {refused} refusal(s) of a script the kernel {} to run while it \
                 is open for writing",
                if kernel_refuses {
                    "refuses"
                } else {
                    "does not refuse"
                }
            );
            let out = run().expect("the script runs once nobody holds it open");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ran");
        }
    }
}
