//! SystemVerilog annotation sidecar (`.mununu.json`).
//!
//! Declares which signals to preserve, how to abstract them, which
//! properties to verify, and (optionally) SMT-discovered significant
//! values. The sidecar file lives next to the `.sv` source and is the
//! single source of truth for the KMTS (`sv-yosys`) verification
//! pipeline's abstraction posture.
//!
//! # Format
//!
//! Single-module (`mununu_sv_annotation_v1`): one `"module"` field, one
//! `.sv` source, signal / input / init / memory abstraction posture.
//! Consumed by the BTOR2 bit-blaster + sidecar resolver. (The native
//! multi-module sidecar schema `mununu_sv_multi_v1` was removed in S.2b
//! along with the native parser; multi-module designs compose
//! structurally from a top module via the `sv-yosys` netlist, with one
//! single-module sidecar per submodule.)
//!
//! # Pipeline
//!
//! ```text
//! sv2v → Yosys → BTOR2 → Load .mununu.json → bit-blast (+ SMT discovery) → KMTS → Verify
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Top-level annotation loaded from `<module>.mununu.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SvAnnotation {
    /// Schema version identifier.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    /// Module name (must match the SV module declaration).
    pub module: String,

    /// Path to the `.sv` source file (relative to the sidecar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    /// Register/internal signal annotations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<SignalAnnotation>,

    /// Input port annotations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputAnnotation>,

    /// Signals explicitly marked as controllable (output ports or overrides).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controllable: Vec<String>,

    /// Properties to verify.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyAnnotation>,

    /// SMT-discovered significant values per signal.
    /// Populated by `mununu sv discover`; user may edit names.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub discovered_values: HashMap<String, DiscoveredValues>,

    /// Module parameter overrides (e.g., `{"DEPTH": 4}`).
    ///
    /// Bare name→value map. Preserved for backwards compatibility
    /// with sidecars authored before R-S1 (every existing fixture).
    /// When a parameter appears in both this map AND
    /// `parameter_concretizations`, the structured entry wins —
    /// see [`SvAnnotation::effective_parameters`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameters: HashMap<String, i64>,

    /// R-S1 (§Phase 9 §9.1, 2026-06-11) — structured parameter
    /// concretization profile. Same key space as `parameters` but
    /// each entry also carries a rationale (why this value was
    /// chosen) and an optional milestone / fixture citation.
    /// Formalises M.0's manual `N=8 → N=2` scale-down — that
    /// decision was recorded ad-hoc in the M-0-result.md ledger;
    /// R-S1 promotes it to a first-class sidecar field so future
    /// contributors can see WHY a given parameter was concretized
    /// without grepping milestone notes.
    ///
    /// Coexists with `parameters` (above) — the resolver
    /// [`SvAnnotation::effective_parameters`] folds both into a
    /// single name→value map, with structured entries winning
    /// over bare entries on name conflict.
    ///
    /// Default empty — preserves the legacy behaviour for every
    /// existing fixture.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameter_concretizations: HashMap<String, ParameterConcretization>,

    /// R-Y6 (§Phase 8) — reset-sequence-aware init. When set, the
    /// BTOR2 bit-blaster runs `hold_cycles` cycles of "reset asserted"
    /// simulation before enumerating initial states. The named input
    /// is pinned to `asserted_value` for those cycles; design logic
    /// propagates the reset through synchronizers and reset-domain
    /// flip-flops. The state after the K-cycle hold becomes the
    /// effective initial state (instead of the cycle-0 BTOR2 init
    /// values).
    ///
    /// Useful for designs where the reset signal passes through a
    /// multi-stage synchronizer before the main reset takes effect
    /// (OpenTitan `prim_reset_sync` is the canonical example: 2-cycle
    /// hold ensures the synchronizer settles before any verification
    /// cycle runs). Caliptra's `soc_ifc_boot_fsm` does not need this
    /// — the reset path is single-cycle.
    ///
    /// Default `None` — preserves the cycle-0 init behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_sequence: Option<ResetSequence>,

    /// R-S2b.5 (§Phase 9 §9.1, 2026-06-11) — Verilator-driven
    /// reset-simulation declaration. When set + Verilator
    /// discoverable, R-S2b.6's orchestration runs a short concrete
    /// simulation through Verilator, captures the post-reset
    /// valuation of the declared `observe_registers`, and feeds
    /// the result to the bit-blaster's R-S2b.4 seeding helper.
    /// Verilator-absent falls back gracefully to other Phase 9
    /// strategies (R-S5 / R-S7 / R-S3).
    ///
    /// Distinct from `reset_sequence` (R-Y6) — R-Y6 runs an
    /// _abstract_ K-cycle reset hold inside the bit-blaster
    /// (no external simulator) to settle the init state of
    /// flip-flops behind a synchronizer; R-S2b.5 runs a
    /// _concrete_ Verilator simulation to seed the abstraction
    /// with observed register values. The two compose — a sidecar
    /// may set both.
    ///
    /// Default `None` — preserves the legacy behaviour.
    /// R-S2b.6's orchestration (queued under §Phase 11 §11.1
    /// slot 3) is the consumer of this field; until that lands,
    /// declarations here are accepted by the schema but have no
    /// effect on the abstract model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulate_reset: Option<SimulateReset>,

    /// R-S6.4 (§Phase 9 §9.1, 2026-06-11) — Pre-existing VCD
    /// trace files to mine for predicate-cube seeds. Each entry
    /// declares a path (relative to the sidecar) + per-trace
    /// sampling policy. R-S6.6's orchestration walks every entry
    /// when the bit-blaster runs, parses the trace via
    /// [`crate::adapter::vcd::parse_vcd_changes`], mines per-
    /// signal value frequencies via
    /// [`crate::adapter::vcd::mine_vcd_frequencies`], and feeds
    /// the top-N heavy-hitters + boundary values to the
    /// bit-blaster's `EnumValues` discriminator lists via R-S6.5's
    /// seeding helper.
    ///
    /// Distinct from R-S2b's `simulate_reset` (which RUNS a fresh
    /// concrete simulation via Verilator): R-S6 reuses traces the
    /// project's regression suite already produced. The two
    /// compose — a sidecar may set both, in which case both
    /// strategies feed cell_domains independently and additively.
    ///
    /// Default empty (no traces) — preserves the legacy behaviour.
    /// R-S6.5 (bit-blaster seeding helper) + R-S6.6 (CLI flag +
    /// orchestration) are the consumers of this field; until R-S6.6
    /// ships, declarations here are accepted by the schema but
    /// have no effect on the abstract model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vcd_traces: Vec<VcdTraceConfig>,

    /// §Phase 10 §10.2 stage 2 — Memory-cell annotations. For each
    /// `$mem` / `$mem_v2` BTOR2 cell the user wants to abstract,
    /// declare its address-width, data-width, and abstraction strategy
    /// (UF / bit-blast / havoc / bounded-bit-blast). Optional
    /// `selected_addresses` restricts UF mode to a finite set of
    /// addresses kept concrete; the rest fall under the
    /// abstraction's catch-all behaviour.
    ///
    /// Default empty — preserves the legacy behaviour (the BTOR2
    /// lifter today silently treats `$mem` cells as regular state
    /// cells, which produces the cap explosion on any non-trivial
    /// memory). §Phase 10 §10.2 stage 1 (the lifter extension) is
    /// the consumer of this field; until that lands, declarations
    /// here are accepted by the schema but have no effect on the
    /// abstract model. See §Phase 10 §10.1 fixture-selection
    /// measurement record for the precondition fixture analysis
    /// (ibex `ibex_register_file_ff.sv` selected as the first
    /// fixture).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<MemoryAnnotation>,

    /// R.5b — Cell-instance names to **force-wrap** as uninterpreted
    /// functions in the BTOR2 lifter. Each name in the list is matched
    /// against BTOR2 cell-instance symbols (the same name space the
    /// `signals[]` array's per-signal `name` field targets) — when a
    /// match is found, the cell's output is treated as a fresh
    /// nondeterministic input rather than evaluated through its
    /// arithmetic. Sound may-side over-approximation per
    /// `docs/design/native-sv-abstraction.md` §6.10: UF only ADDS
    /// admissible may-behaviour beyond the concrete relation, so any
    /// concrete violation the design admits is preserved by the
    /// abstract. Useful for wide multipliers / dividers / hashes that
    /// blow up SMT predicate-image queries when evaluated concretely.
    ///
    /// Default empty — preserves the legacy behaviour (every cell
    /// evaluated concretely). R.5b's BTOR2 lifter integration (the
    /// consumer of this field; tracked under §Phase 11 §11.1 slot 1.e)
    /// reads this list when the design has cells that match the
    /// default UF wrapping policy (`$mul`/`$div`/`$mod`/`$pow` always;
    /// `$add`/`$sub` for width > 32); user-supplied entries here
    /// extend or override the default policy. Until R.5b's lifter
    /// integration ships, declarations here are accepted by the
    /// schema but have no effect on the abstract model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uf_wrap: Vec<String>,

    /// R.5b — Cell-instance names to **force-concretize** even if
    /// the default UF wrapping policy would have wrapped them. The
    /// inverse of [`Self::uf_wrap`]: an entry here disables UF
    /// abstraction for the named cell, falling back to concrete
    /// arithmetic evaluation. Useful when the user knows a specific
    /// multiplier / divider / hash is small enough to evaluate
    /// concretely + wants exact verdicts on it rather than the
    /// `KleeneBot` -refined-later cycle UF wrapping would produce.
    ///
    /// Default empty. R.5b lifter integration consumes this field
    /// (queued); until then, schema-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uf_unwrap: Vec<String>,

    /// IR-track P3.2 (2026-06-22) — predicate-cube declaration for the
    /// SV **verify** path. Each entry is a `{name, register, value}`
    /// register-value equality predicate (`register == value`) that
    /// bounds the abstraction — the same shape the cegar
    /// `predicate_cube_lift` / API `PredicateSpecRequest` consume.
    ///
    /// When non-empty, the verify path (IR-track **P3.3**, NOT yet
    /// wired) routes this module through `predicate_cube_lift` + the
    /// 3-valued (`KleeneDom`) evaluator instead of the bit-blast
    /// `FieldDomain` path — i.e. this is how an SV verify run opts into
    /// the predicate-cube abstraction. Until P3.3's verify-dispatch
    /// integration ships, declarations here are accepted + validated by
    /// the schema but have NO effect on the abstract model (the verify
    /// path stays bit-blast).
    ///
    /// Default empty — preserves the bit-blast behaviour for every
    /// existing fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<PredicateDeclaration>,

    /// B.1 increment 3c (2026-06-26) — boolean-composite predicates for
    /// the predicate-cube CEGAR path. Each entry is a `{name, expr}` pair
    /// whose `expr` is a boolean combination of `register == value` /
    /// relational atoms (e.g. `"cnt == 0 && en == 1"`), parsed by
    /// [`crate::adapter::btor2::predicate_expr::parse_predicate_expr`].
    ///
    /// Unlike the simple `register == value` [`PredicateDeclaration`]s
    /// above, a compound predicate's cube-bit truth is decided by the
    /// **eager SMT all-pairs** may-relation (the only compound-aware
    /// lift). When any `compound_predicates` is present the CEGAR loop
    /// forces `may_edge_inference = SmtAllPairs` + `LiftStrategy::Eager`
    /// (and warns if that overrides an explicit request) — the sampling
    /// representative inverse can't realise a conjunction, and the lazy
    /// per-cube body never consults the expr.
    ///
    /// Default empty — preserves behaviour for every existing fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compound_predicates: Vec<CompoundPredicateDecl>,

    /// H.E.2 (combinational outputs, 2026-06-28) — **derived combinational
    /// predicates**: `signal == value` atoms whose `signal` is a *combinational
    /// node* (an `eq`/`or`/… output of state, e.g. csrng's `main_sm_err_o`), NOT
    /// a cube dimension. The cube lift labels each per cube via SMT (Approach B,
    /// free-input-atoms.md §4) — KleeneT/F where the cube pins the signal,
    /// KleeneBot where it doesn't — writing the label into
    /// `state_3valued_predicates` so the evaluator binds the formula atom by
    /// name. These do NOT enlarge the cube space. Like compounds, they require
    /// the eager SmtAllPairs lift (the labeling pass is SMT-based).
    ///
    /// Default empty — preserves behaviour for every existing fixture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combinational_predicates: Vec<CombinationalPredicateDecl>,
}

/// H.E.2 (2026-06-28) — one derived combinational predicate declaration for the
/// predicate-cube path. [`crate::adapter::btor2::sidecar_combinational_predicates`]
/// reads these into [`crate::adapter::btor2::kmts_lift::PredicateCubeLiftOptions::derived_predicates`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CombinationalPredicateDecl {
    /// Predicate name = the formula atom string (the `state_3valued_predicates`
    /// key the evaluator binds by name), e.g. `"main_sm_err_o"`.
    pub name: String,
    /// The combinational node's symbol in the lifted BTOR2 (its own `Op` symbol).
    pub signal: String,
    /// Value the predicate checks: it holds iff `signal == value` (bare boolean
    /// atom ⇒ `value = 1`).
    pub value: u64,
}

/// IR-track P3.2 (2026-06-22) — one register-value equality predicate
/// for the predicate-cube verify path. Mirrors the cegar
/// [`crate::adapter::btor2::kmts_lift::PredicateSpec`] and the API
/// `PredicateSpecRequest` shape so the sidecar declaration threads
/// directly into the cube lift once P3.3 wires it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PredicateDeclaration {
    /// Human-readable predicate name (e.g. `"boot_idle"`).
    pub name: String,
    /// BTOR2/register symbol the predicate tests (the same name space
    /// the `signals[]` entries' `name` field targets).
    pub register: String,
    /// Value the predicate checks: the predicate holds iff
    /// `register == value`.
    pub value: u64,
}

/// B.1 increment 3c (2026-06-26) — one boolean-composite predicate
/// declaration for the predicate-cube CEGAR path. The
/// [`crate::adapter::btor2::sidecar_compound_predicates`] helper parses
/// `expr` into a [`crate::adapter::btor2::predicate_expr::PredicateExpr`]
/// and pairs it with a placeholder [`crate::adapter::btor2::PredicateSpec`]
/// (the expr — not `register == value` — drives the cube-bit truth via the
/// SMT seam's `PredicateLike::expr`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompoundPredicateDecl {
    /// Predicate name; the cube bit + `compound_exprs` key (e.g. `"idle"`).
    pub name: String,
    /// Boolean-expression source, e.g. `"cnt == 0 && en == 1"`. Parsed by
    /// `parse_predicate_expr`; a malformed expr is skipped with a warning.
    pub expr: String,
    /// H.F — when `true`, this compound is a DERIVED 3-valued LABEL (its operands
    /// include an input/combinational signal, so it is not a sound cube dimension;
    /// the lift labels it per cube via the SMT seam), NOT a cube dimension.
    /// `false` (the default, the all-state B.1 case) → a cube dimension.
    #[serde(default)]
    pub derived: bool,
}

/// §Phase 10 §10.2 stage 2 — Memory-cell annotation.
///
/// Declares an abstraction strategy for a single `$mem` / `$mem_v2`
/// cell in the BTOR2 source. The BTOR2 cell is named after the SV
/// signal that declared the array; e.g. an SV `logic [31:0] rf_reg
/// [0:31]` produces a BTOR2 array cell named `rf_reg` with
/// address-width 5 and data-width 32.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAnnotation {
    /// Memory-cell name (matches the BTOR2 `$mem` symbol — typically
    /// the SV array signal's name).
    pub name: String,

    /// Number of bits in the address. For a `[0:N-1]` array, this is
    /// `ceil(log2(N))`. The lifter validates this matches the BTOR2
    /// cell's actual address width.
    pub address_width: u32,

    /// Number of bits per memory word. For a `[W-1:0]` data type,
    /// this is `W`. The lifter validates this matches the BTOR2
    /// cell's data-port width.
    pub data_width: u32,

    /// Abstraction strategy.
    pub abstraction: MemoryAbstraction,

    /// Optional: addresses kept concrete under UF mode. The lifter
    /// enforces SMT predicate-image queries on these specific
    /// addresses; addresses outside this set fall under the catch-all
    /// behaviour (havoc reads, no-op writes for soundness).
    /// Default `None` (or empty) means "every address is symbolic
    /// under the chosen abstraction".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_addresses: Option<Vec<u64>>,
}

/// §Phase 10 §10.2 stage 2 — Memory abstraction strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAbstraction {
    /// UF (uninterpreted-function) abstraction over Z3 array theory.
    /// Reads/writes treated as EUF terms with the standard array
    /// axioms (`read(write(a, i, v), j) = if i==j then v else read(a, j)`).
    /// Sound for safety + liveness (under-approximation of address
    /// resolution); may produce `KleeneBot` verdicts that R.5 CEGAR
    /// refines by selecting specific addresses (via `selected_addresses`).
    /// **Default abstraction for any declared memory** because it's
    /// the most general while staying sound; falls back to other
    /// modes when explicitly requested.
    Uf,

    /// Full bit-blast over the memory's `address_width × data_width`
    /// state space. Exact; only viable when `address_width + data_width`
    /// fits within `MAX_STATE_BITS` after combining with other state
    /// cells (typically ≤ 6 + 6 = 12 bits in practice). Useful for
    /// small register files or tiny accumulators where the user wants
    /// exact verdicts and the state space is manageable.
    BitBlast,

    /// Havoc on all reads, no-op on writes. Reads return any
    /// admissible value of the data-width; writes are silently
    /// dropped. Sound for safety (over-approximation of read
    /// behaviour, so any property violation in the concrete is
    /// still detected); **unsound for liveness** (the concrete may
    /// reach a state the abstract cannot).
    Havoc,

    /// Bounded bit-blast: full bit-blast over a small subset of
    /// addresses (declared in `selected_addresses`), havoc on the
    /// rest. Hybrid between `BitBlast` and `Havoc` — the user picks
    /// which addresses are worth the exact verdict and accepts havoc
    /// reads on the rest. Sound for safety; unsound for liveness on
    /// the havoc'd addresses.
    BoundedBitBlast,
}

impl Default for MemoryAbstraction {
    /// UF is the default — most general + sound for safety AND
    /// liveness modulo CEGAR refinement on KleeneBot verdicts.
    fn default() -> Self {
        Self::Uf
    }
}

/// R-S2b.5 (§Phase 9 §9.1, 2026-06-11) — declaration of a
/// Verilator-driven reset simulation, used by R-S2b's
/// predicate-set seeding strategy. Mirrors
/// [`crate::adapter::verilator::ResetSimConfig`] one-for-one
/// minus the `top` field (which the sidecar already declares as
/// `SvAnnotation::module`).
///
/// When this field is set on a sidecar AND a Verilator binary is
/// discoverable, R-S2b.6's orchestration runs
/// [`crate::adapter::verilator::run_reset_simulation`], feeds the
/// resulting `Vec<RegisterValuation>` to
/// [`crate::adapter::btor2::bit_blast::apply_reset_simulation_seeding`],
/// and the seeded values land in the bit-blaster's `EnumValues`
/// variant lists exactly the way R-S2a's BTOR2-init seeding
/// already does. Verilator-absent falls back gracefully —
/// downstream Phase 9 strategies (R-S5 type-driven, R-S7
/// property-syntactic, R-S3 case-literal) still apply.
///
/// Default: omitted entirely (`Option::None`) — the simulation
/// is opt-in per sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimulateReset {
    /// SV signal name for the clock. The testbench toggles it
    /// `0 → 1 → 0` once per cycle. Typically `clk` / `clk_i` /
    /// `clk_aon`.
    pub clock_signal: String,
    /// SV signal name for the reset. Held at `reset_asserted`
    /// during the hold phase, then flipped to `1 - reset_asserted`
    /// for the settle phase.
    pub reset_signal: String,
    /// Logical value driven on `reset_signal` during the hold
    /// phase. Use `0` for active-low resets (`rst_n` / `rst_ni`)
    /// and `1` for active-high (`rst`).
    pub reset_asserted: u8,
    /// Cycles to hold the reset asserted before deasserting.
    /// Caliptra `soc_ifc_boot_fsm` needs 1; OpenTitan `uart_tx`
    /// needs ~4 (the synchronizer chain + reset-domain ff).
    pub hold_cycles: u32,
    /// Cycles to run after deasserting reset before sampling
    /// register valuations. Default `1` is sufficient for
    /// the M.0–M.4 fixtures.
    #[serde(default = "default_settle_cycles")]
    pub settle_cycles: u32,
    /// Register names (matching the SV declarations) to dump
    /// after the settle phase. Order is preserved in the output
    /// `Vec<RegisterValuation>` — `apply_reset_simulation_seeding`
    /// resolves each by name against the BTOR2 symbols map.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observe_registers: Vec<String>,
}

fn default_settle_cycles() -> u32 {
    1
}

/// R-S6.4 (§Phase 9 §9.1, 2026-06-11) — declaration of a
/// pre-existing VCD trace file to mine for predicate-cube seeds.
///
/// One entry per trace file the sidecar wants the bit-blaster to
/// consume. R-S6.6's orchestration reads the file via
/// [`crate::adapter::vcd::parse_vcd_changes`], runs the R-S6.3
/// frequency miner, and feeds the result through R-S6.5's
/// seeding helper.
///
/// Path semantics: resolved relative to the sidecar's containing
/// directory (matches the convention `SvAnnotation::source` uses
/// for the `.sv` source path). Absolute paths are used verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VcdTraceConfig {
    /// Path to the VCD file, relative to the sidecar's parent
    /// directory (or absolute). The orchestration in R-S6.6
    /// reads this path on the file system; non-existent files
    /// surface an `AdapterWarning` and the bit-blaster falls
    /// through.
    pub path: String,
    /// Maximum number of heavy-hitter values per signal to lift
    /// into `EnumValues` discriminators. Caps the seeding budget
    /// so a long-running trace with many distinct values doesn't
    /// blow up the cube space. Defaults to 4 (covers most FSM
    /// state machines + a handful of common counter values).
    #[serde(default = "default_max_heavy_hitters_per_signal")]
    pub max_heavy_hitters_per_signal: u32,
    /// When true (default), R-S6.5 also seeds the boundary
    /// values (min / max) per signal in addition to the
    /// heavy-hitter top-N. Useful for counter / saturator
    /// predicates: "did the counter ever reach its boundary?"
    /// Disable when the trace's tail values (often X-during-
    /// shutdown artefacts) would pollute the cube space.
    #[serde(default = "default_seed_boundary_values")]
    pub seed_boundary_values: bool,
    /// Whitelist of signal names (matching SV declarations) to
    /// mine. Empty (default) means mine every signal declared in
    /// the trace; R-S6.5 cross-references each mined signal
    /// against the BTOR2 symbols map and only seeds cells that
    /// match (silently skips the rest).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
}

fn default_max_heavy_hitters_per_signal() -> u32 {
    4
}

fn default_seed_boundary_values() -> bool {
    true
}

impl SimulateReset {
    /// R-S2b.5 (2026-06-11) — convert the sidecar declaration to
    /// the runner's [`crate::adapter::verilator::ResetSimConfig`].
    /// Combines the sidecar's settings with the `top` module name
    /// (which lives at the [`SvAnnotation`] top level, not on the
    /// `SimulateReset` substruct).
    ///
    /// Pure helper — no I/O. Round-trips the field set without
    /// loss. The runner then calls `config.validate()` before
    /// invoking Verilator.
    pub fn to_reset_sim_config(&self, top: String) -> crate::adapter::verilator::ResetSimConfig {
        crate::adapter::verilator::ResetSimConfig {
            top,
            clock_signal: self.clock_signal.clone(),
            reset_signal: self.reset_signal.clone(),
            reset_asserted: self.reset_asserted,
            hold_cycles: self.hold_cycles,
            settle_cycles: self.settle_cycles,
            observe_registers: self.observe_registers.clone(),
        }
    }
}

/// R-S1 (§Phase 9 §9.1, 2026-06-11) — structured parameter
/// concretization profile for one module parameter.
///
/// Generalises the bare `HashMap<String, i64>` parameters field
/// by attaching a rationale (why this value was chosen) and an
/// optional citation (which milestone / fixture justified it).
///
/// Per §Phase 9 R-S1's design: M.0 scaled `prim_arbiter_fixed`'s
/// `N=8, DW=32` to `N=2, DW=2` to fit within MAX_STATE_BITS=20.
/// That decision was recorded ad-hoc in the M-0-result.md ledger;
/// R-S1 promotes it to a first-class sidecar field so the
/// decision survives independent of the milestone notes + so
/// future contributors can see WHY a parameter was concretized
/// without grepping outside the sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParameterConcretization {
    /// The concretized parameter value.
    pub value: i64,
    /// Human-readable explanation for why this value was chosen.
    /// Examples:
    /// - "Scaled N from 8 to 2 to fit within MAX_STATE_BITS=20."
    /// - "DW reduced from 32 to 4 — property does not observe
    ///   data bits beyond the LSB."
    /// - "Common case observed in regression sweep at this
    ///   parameter level."
    pub rationale: String,
    /// Optional reference to a milestone or fixture record where
    /// this concretization was first justified. Examples:
    /// `Some(".claude/plans/milestones/M-0-result.md")`,
    /// `Some("examples/verify/sv_yosys_caliptra_rtl_150/README.md")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justified_by: Option<String>,
}

/// R-Y6 (§Phase 8) — declaration of a reset-hold sequence for the
/// bit-blaster's init-state computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetSequence {
    /// Name of the BTOR2 input signal to pin during the hold cycles
    /// (typically `rst_ni` for active-low or `rst` for active-high).
    pub reset_input: String,
    /// The value to pin the reset input to during the hold. Use 0 for
    /// active-low reset (rst_ni asserted = 0) and 1 for active-high
    /// reset. Masked to the input's bit-width.
    pub asserted_value: u64,
    /// How many clock cycles to hold the reset asserted before
    /// enumerating initial states. Typical values: 1 (single-cycle
    /// reset domain), 2 (one synchronizer stage), 3 (two synchronizer
    /// stages). Must be > 0; 0 is equivalent to omitting the field.
    pub hold_cycles: u32,
}

/// Annotation for a register or internal signal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalAnnotation {
    /// Signal name (must match an SV declaration).
    pub name: String,

    /// Whether to include this signal in the state space.
    /// Default: `true` if listed, but can be explicitly set to `false`.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub preserve: bool,

    /// Abstraction strategy.
    #[serde(default)]
    pub abstraction: SignalAbstraction,

    /// Upper bound for `bounded_counter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<i64>,

    /// Explicit enum variants (for `abstraction: "enum"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,

    /// Value map entries (for `abstraction: "enum"` with numeric mapping).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_map: Option<Vec<ValueMapEntry>>,

    /// Whether this signal is combinational (computed from `assign` / `always_comb`).
    /// Combinational signals are included in the state space but their value is
    /// computed from the combinational logic each cycle, not from sequential assignments.
    #[serde(default, skip_serializing_if = "is_false")]
    pub combinational: bool,

    /// R-Y2 (§Phase 8 §8.1) — per-signal init policy override. When
    /// not `Inherit`, this signal's undef bits are treated per the
    /// chosen policy (zero / anyconst / anyseq) regardless of the
    /// global `YosysOptions::setundef_*` flags. The Yosys script-
    /// builder emits `setattr -mod -set <attr> <val> w:<name>`
    /// between `read_verilog` and `proc` to apply the override.
    /// **Default `Inherit`** — strict additivity; legacy sidecars
    /// unchanged.
    #[serde(default, skip_serializing_if = "is_inherit_init_policy")]
    pub init_policy: InitPolicy,

    /// R-Y4 (§Phase 8) — bounded-havoc init value set. When set AND
    /// `init_policy == Anyconst`, Path 3's per-anyconst-value
    /// initial-state enumeration restricts the admissible init
    /// values for this signal to the listed values (instead of
    /// enumerating every value in the cell's abstract admissible
    /// set). Useful when the hardware spec documents a restricted
    /// reset-sample range (e.g. "reset value is one of 0..4" on a
    /// 3-bit register that admits 0..7) or when the user wants to
    /// focus the verifier on specific bug-relevant init samples.
    ///
    /// Strictly additive: when empty/None OR when `init_policy` is
    /// not `Anyconst`, Path 3's existing enumeration runs unchanged.
    /// Values outside the cell's abstract admissible set (e.g.
    /// outside the typedef's UNMATCHED-extended set after R-S5
    /// widening) are silently skipped by the cartesian product
    /// (cells.encode returns None for unrepresentable combinations).
    ///
    /// Default `None` — preserves Path 3's full-enumeration behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounded_init: Option<Vec<i64>>,

    /// R-S4 (§Phase 9 §9.1) — opt-in equivalence-class seeding. When
    /// `true` AND R-S3's `extract_case_literals` finds case-statement
    /// labels for this signal, R-S4 populates the signal's
    /// `discovered_values` with the labels as named representatives
    /// plus a single `OTHER` catch-all variant (state-space:
    /// `N + 1` instead of `2^width`). R-S3 is then skipped for this
    /// signal to avoid double-emission of per-literal discriminators.
    ///
    /// Trades verdict precision (the catch-all collapses all unmatched
    /// values into one abstract state) for state-space efficiency
    /// (manageable abstraction sizes on wide signals). Useful for
    /// parametric constants — opcodes, address-decoder bits,
    /// agent-IDs in coherence protocols — where each named literal
    /// matters individually but the unmatched values can be treated
    /// uniformly.
    ///
    /// Default `false` — preserves R-S3's individual-literal behaviour.
    #[serde(default, skip_serializing_if = "is_false")]
    pub equivalence_classes: bool,

    /// R-S8 session 2 (§Phase 9 §9.1) — under-constrained constant's
    /// admissible value set. When set, the predicate-cube lift path
    /// (R.2.5 + R-Y7) treats this signal as a config bit / parameter
    /// whose value can be ANY of the listed values across concrete
    /// instances; the lifter expands the initial-state set to all
    /// cubes whose predicate evaluation is consistent with some
    /// valid value (per the [`crate::adapter::btor2::r_s8_encoder`]
    /// helper). The GKMTS evaluator then treats this as a
    /// hyper-must initial set ("some cube in this set is the actual
    /// start"); refinement narrows on demand.
    ///
    /// Distinct from [`Self::bounded_init`] (R-Y4): `bounded_init`
    /// is the BIT-BLAST path's per-anyconst-value initial-state
    /// enumeration (`init_policy: Anyconst` only); `config_values`
    /// is the PREDICATE-CUBE path's hyper-must initial-state
    /// admission. Both fields may coexist on the same signal — the
    /// bit-blast path reads `bounded_init`; the cube path reads
    /// `config_values`.
    ///
    /// Default `None` — preserves the cube path's single-initial-
    /// cube behaviour exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_values: Option<Vec<u64>>,

    /// M.1 Path B (§Phase 11) — identify a BTOR2 state cell by a
    /// named output / wire that it drives, instead of by the cell's
    /// own (possibly stripped) symbol. When set, the BTOR2 resolver
    /// walks the operand graph backward from any node carrying the
    /// `drives` symbol and resolves the sidecar entry to the nearest
    /// reachable `State` NID.
    ///
    /// Useful on designs where Yosys's `flatten` + `async2sync` +
    /// `dffunmap` chain strips the original register names from the
    /// `state` lines and only attaches them to derived `output` /
    /// `Op` lines (the OpenTitan `uart_tx.sv` case discovered during
    /// the M.1 attempt — see
    /// `.claude/plans/milestones/M-1-blocker-2026-05-25.md`).
    ///
    /// Resolution: the resolver searches BTOR2 for nodes carrying the
    /// `drives` symbol when set, falling back to `name` when `drives`
    /// is `None`; it then BFS-walks the operand graph backward to
    /// the nearest reachable `State` NID. When the chosen symbol
    /// resolves to multiple distinct state cells at the same minimum
    /// distance (ambiguous combinational chain), the resolver returns
    /// no match — the user must disambiguate by picking a closer
    /// driver name.
    ///
    /// Default `None` — preserves the existing direct-name-only
    /// resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drives: Option<String>,

    /// R-S5 (§Phase 9 §9.1) — SV typedef name (e.g. `boot_fsm_state_e`)
    /// this signal is declared with. When set AND the signal's
    /// `abstraction` is `Discover` or `Enum` with empty `variants` /
    /// `value_map`, the loader walks SV source for the typedef
    /// declaration via [`super::typedef_extract::extract_typedef_enums`]
    /// and auto-fills the variant list + value map. Includes the
    /// `UNMATCHED_<n>` synthetic variants for encodings the typedef's
    /// bit-width admits but doesn't enumerate, which is the load-bearing
    /// CWE-1245 detection mechanism on the Caliptra fixture (the bug
    /// fires precisely on the unmatched encodings).
    ///
    /// Default `None` — opt-in per-signal; existing sidecars unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,

    /// Human-readable note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// R-Y2 — serde skip-helper: skip serialising `init_policy` when it
/// is the default `Inherit`. Keeps existing sidecars round-trip stable.
fn is_inherit_init_policy(p: &InitPolicy) -> bool {
    matches!(p, InitPolicy::Inherit)
}

impl SvAnnotation {
    /// R-S1 (§Phase 9 §9.1, 2026-06-11) — compute the effective
    /// parameter override map for this annotation.
    ///
    /// Folds two sources:
    /// 1. The legacy `parameters: HashMap<String, i64>` field
    ///    (bare name→value pairs; preserved for backwards
    ///    compatibility with sidecars authored before R-S1).
    /// 2. The structured `parameter_concretizations:
    ///    HashMap<String, ParameterConcretization>` field
    ///    (R-S1's new schema; each entry carries a value +
    ///    rationale + optional citation).
    ///
    /// **Conflict rule**: when the same parameter name appears
    /// in BOTH maps, the structured entry wins. The structured
    /// schema is the authoritative source per §Phase 9 R-S1's
    /// design — the bare map is a legacy fallback, not a
    /// preferred override channel.
    ///
    /// Pure helper — no I/O. Order is non-deterministic (the
    /// underlying HashMap iteration is hash-randomised); callers
    /// that need stable ordering should sort by key after this
    /// method returns.
    pub fn effective_parameters(&self) -> HashMap<String, i64> {
        let mut out: HashMap<String, i64> = self.parameters.clone();
        for (name, concretization) in &self.parameter_concretizations {
            out.insert(name.clone(), concretization.value);
        }
        out
    }

    /// R-Y2 (§Phase 8 §8.1) — Collect per-signal init-policy overrides
    /// from the sidecar's `signals` + `inputs` declarations. Returns
    /// `(signal_name, InitPolicy)` pairs for every signal whose policy
    /// is not `Inherit`. The Yosys script-builder consumes this to
    /// emit `setattr -mod -set <attr> <val> w:<signal>` commands.
    ///
    /// Order is deterministic (signals first, in declaration order,
    /// then inputs in declaration order) so the emitted script is
    /// stable across runs.
    pub fn init_policy_overrides(&self) -> Vec<(String, InitPolicy)> {
        let mut out: Vec<(String, InitPolicy)> = Vec::new();
        for sig in &self.signals {
            if !matches!(sig.init_policy, InitPolicy::Inherit) {
                out.push((sig.name.clone(), sig.init_policy));
            }
        }
        for inp in &self.inputs {
            if !matches!(inp.init_policy, InitPolicy::Inherit) {
                out.push((inp.name.clone(), inp.init_policy));
            }
        }
        out
    }

    /// R-S5 (§Phase 9 §9.1) — auto-fill `variants` + `value_map` on
    /// signals that declare a `type_name` and whose abstraction is
    /// `Discover` or `Enum` with empty variant info. The `typedefs`
    /// map is the output of
    /// [`super::typedef_extract::extract_typedef_enums`] over every
    /// SV source file the loader has access to (primary + sidecars).
    ///
    /// **Closes the §Phase 8 §8.2 abstraction-clipping bottleneck
    /// without manual sidecar editing.** When the typedef admits more
    /// encodings than its named variants (e.g. `boot_fsm_state_e`:
    /// width 3, 5 named variants, 3 unmatched encodings `{5,6,7}`),
    /// the auto-widening emits the named variants AND the synthetic
    /// `UNMATCHED_<n>` variants so the abstraction layer keeps
    /// transitions to the unmatched encodings in the abstract relation.
    /// On the Caliptra fixture this is exactly the manual Path 1
    /// widening reproduced automatically.
    ///
    /// Returns the list of `(signal_name, type_name, variant_count)`
    /// triples that were widened, for caller logging. Signals without
    /// a `type_name`, with `Ignored` / `Boolean` abstraction, or whose
    /// `type_name` doesn't appear in `typedefs` are silently skipped
    /// (additive — no behavior change for legacy sidecars).
    pub fn apply_type_driven_widening(
        &mut self,
        typedefs: &std::collections::HashMap<String, super::typedef_extract::TypedefEnum>,
    ) -> Vec<(String, String, usize)> {
        let mut applied = Vec::new();
        for sig in &mut self.signals {
            let Some(type_name) = sig.type_name.as_deref() else {
                continue;
            };
            let Some(td) = typedefs.get(type_name) else {
                continue;
            };
            // Only widen when the abstraction strategy can take a
            // variant list AND it isn't already supplied. Discover and
            // Enum-with-empty-variants are the targets; explicit
            // user-supplied variants are NEVER overwritten.
            let target_strategy = matches!(
                sig.abstraction,
                SignalAbstraction::Discover | SignalAbstraction::Enum
            );
            let variants_empty = sig.variants.as_ref().map(|v| v.is_empty()).unwrap_or(true);
            if !target_strategy || !variants_empty {
                continue;
            }
            // Apply the type-driven widening. Variants include the
            // synthetic UNMATCHED_<n> entries; value_map binds each
            // variant name to its numeric encoding.
            let all = td.all_encodings();
            sig.abstraction = SignalAbstraction::Enum;
            sig.variants = Some(all.iter().map(|(n, _)| n.clone()).collect());
            sig.value_map = Some(
                all.iter()
                    .map(|(n, v)| ValueMapEntry {
                        name: n.clone(),
                        value: *v as i64,
                    })
                    .collect(),
            );
            applied.push((sig.name.clone(), type_name.to_string(), all.len()));
        }
        applied
    }

    /// R-S7 (§Phase 9 §9.1) — property-syntactic seeding. Walks every
    /// property formula in the sidecar, collects predicate names of
    /// the shape `<signal>_<integer-suffix>` where `<signal>` is a
    /// declared signal in the sidecar, and adds the integer values as
    /// abstraction discriminators for that signal (synthetic variant
    /// name `<signal>_<n>` mirroring R-S5's UNMATCHED_<n> convention).
    ///
    /// **Bridges the gap between handwritten formulas and the
    /// abstraction set.** When the user writes a property referencing
    /// `count_3` or `mode_7` but the signal has no typedef for R-S5
    /// to widen against, R-S7 picks up the values from the formula
    /// text and ensures the abstraction layer keeps transitions to
    /// those specific values.
    ///
    /// Strictly additive — only inserts values not already in the
    /// signal's value_map. Signals with explicit `bit_blast` /
    /// `boolean` / `ignored` abstractions are skipped (those don't
    /// take per-value discriminators). Signals without any seeded
    /// values from the property text are untouched.
    ///
    /// Returns `(signal_name, [seeded_values])` for each signal
    /// widened, for caller logging.
    pub fn apply_property_syntactic_seeding(&mut self) -> Vec<(String, Vec<i64>)> {
        let mut harvest: std::collections::BTreeMap<String, std::collections::BTreeSet<i64>> =
            std::collections::BTreeMap::new();
        let signal_names: std::collections::HashSet<String> =
            self.signals.iter().map(|s| s.name.clone()).collect();

        for prop in &self.properties {
            let Some(formula_text) = prop.formula.as_deref() else {
                continue;
            };
            let Ok(formula) = crate::mu_calculus::parser::parse(formula_text) else {
                continue;
            };
            for node in formula.nodes() {
                let preds_in_node: Vec<String> = match node {
                    crate::mu_calculus::Node::Predicate(name) => vec![name.clone()],
                    crate::mu_calculus::Node::Modal { guard, .. } => guard
                        .current
                        .required
                        .iter()
                        .chain(guard.current.forbidden.iter())
                        .chain(guard.next.required.iter())
                        .chain(guard.next.forbidden.iter())
                        .cloned()
                        .collect(),
                    _ => Vec::new(),
                };
                for pred in preds_in_node {
                    let Some((sig, value)) = split_signal_integer_suffix(&pred, &signal_names)
                    else {
                        continue;
                    };
                    harvest.entry(sig).or_default().insert(value);
                }
            }
        }

        let mut applied = Vec::new();
        for sig in &mut self.signals {
            let Some(seeds) = harvest.get(&sig.name) else {
                continue;
            };
            let widenable = matches!(
                sig.abstraction,
                SignalAbstraction::Discover | SignalAbstraction::Enum
            );
            if !widenable {
                continue;
            }
            let mut value_map = sig.value_map.clone().unwrap_or_default();
            let existing_values: std::collections::HashSet<i64> =
                value_map.iter().map(|e| e.value).collect();
            let mut added_now = Vec::new();
            for &v in seeds {
                if existing_values.contains(&v) {
                    continue;
                }
                let variant = format!("{}_{}", sig.name, v);
                value_map.push(ValueMapEntry {
                    name: variant,
                    value: v,
                });
                added_now.push(v);
            }
            if added_now.is_empty() {
                continue;
            }
            // Refresh the variants list to mirror value_map order.
            let mut variants = sig.variants.clone().unwrap_or_default();
            for &v in &added_now {
                variants.push(format!("{}_{}", sig.name, v));
            }
            sig.abstraction = SignalAbstraction::Enum;
            sig.variants = Some(variants);
            sig.value_map = Some(value_map);
            applied.push((sig.name.clone(), added_now));
        }
        applied
    }

    /// R-S3 (§Phase 9 §9.1) — case-literal seeding. Takes a
    /// `signal_name → [literal values]` map (produced by
    /// [`super::case_literal_extract::extract_case_literals`] over
    /// every SV source the loader has access to) and, for each
    /// sidecar-declared signal that appears in the map AND has a
    /// widenable abstraction, adds the literals as discriminators
    /// (synthetic variant name `<signal>_<n>`, mirroring R-S5's
    /// UNMATCHED_<n> and R-S7's `<signal>_<n>` conventions).
    ///
    /// Bridges the gap when R-S5's typedef widening can't fire (no
    /// typedef on the signal) AND the property doesn't reference the
    /// case labels directly (R-S7 no-op). Captures the
    /// designer-intended distinctions from `case (signal)` blocks in
    /// the RTL.
    ///
    /// Strictly additive — never overwrites existing variants /
    /// value_map entries. Signals with Boolean / BitBlast / Ignored
    /// abstractions are skipped (no per-value discriminators).
    /// Signals whose name does not appear in the case-literal map
    /// are untouched.
    ///
    /// Returns `(signal_name, [seeded_values])` for each signal
    /// widened, for caller logging.
    pub fn apply_case_literal_seeding(
        &mut self,
        literals: &HashMap<String, Vec<u64>>,
    ) -> Vec<(String, Vec<i64>)> {
        let mut applied = Vec::new();
        for sig in &mut self.signals {
            let Some(seeds) = literals.get(&sig.name) else {
                continue;
            };
            if seeds.is_empty() {
                continue;
            }
            // R-S4 opt-in skips R-S3 for this signal (R-S4 emits the
            // literals as a catch-all-based Discover abstraction; if
            // R-S3 also fired we'd double-emit discriminators).
            if sig.equivalence_classes {
                continue;
            }
            let widenable = matches!(
                sig.abstraction,
                SignalAbstraction::Discover | SignalAbstraction::Enum
            );
            if !widenable {
                continue;
            }
            let mut value_map = sig.value_map.clone().unwrap_or_default();
            let existing_values: std::collections::HashSet<i64> =
                value_map.iter().map(|e| e.value).collect();
            let mut added_now = Vec::new();
            for &v in seeds {
                let v_signed = v as i64;
                if existing_values.contains(&v_signed) {
                    continue;
                }
                let variant = format!("{}_{}", sig.name, v_signed);
                value_map.push(ValueMapEntry {
                    name: variant,
                    value: v_signed,
                });
                added_now.push(v_signed);
            }
            if added_now.is_empty() {
                continue;
            }
            let mut variants = sig.variants.clone().unwrap_or_default();
            for &v in &added_now {
                variants.push(format!("{}_{}", sig.name, v));
            }
            sig.abstraction = SignalAbstraction::Enum;
            sig.variants = Some(variants);
            sig.value_map = Some(value_map);
            applied.push((sig.name.clone(), added_now));
        }
        applied
    }

    /// R-S4 (§Phase 9 §9.1) — equivalence-class seeding. For each
    /// signal with `equivalence_classes: true` AND a case-literals
    /// entry in `literals`, populates the signal's
    /// `self.discovered_values` with the literals as named
    /// representatives plus a single `OTHER` catch-all variant.
    /// Sets the signal's abstraction to `Discover` so the existing
    /// sidecar resolver picks up the discovered_values + catch_all
    /// (the `Discover` arm of `resolve_to_field_domain` already
    /// handles the catch-all variant — R-S4 leverages that
    /// infrastructure without introducing a new abstraction
    /// primitive).
    ///
    /// **State-space tradeoff:** R-S4 collapses all unmatched
    /// values into one abstract state (`OTHER`), giving `N + 1`
    /// states for a signal with `N` case labels — vs `2^width`
    /// under full bit-blast OR `N` under R-S3's per-literal
    /// emission (which lacks the catch-all). Sound for the named
    /// values; over-approximates the unmatched values into one
    /// representative state.
    ///
    /// Mutually exclusive with R-S3 per signal: R-S3 skips signals
    /// where `equivalence_classes: true` (the loader runs both
    /// methods; R-S4 fires first for opt-in signals; R-S3 then
    /// covers the rest). Strictly additive — if the user already
    /// declared `discovered_values` for the signal, R-S4 augments
    /// it (adds new literals, dedups against existing values, keeps
    /// the existing catch_all name).
    ///
    /// Returns `(signal_name, [seeded_values])` for each signal
    /// widened, for caller logging.
    pub fn apply_equivalence_class_seeding(
        &mut self,
        literals: &HashMap<String, Vec<u64>>,
    ) -> Vec<(String, Vec<i64>)> {
        let mut applied = Vec::new();
        for sig in &mut self.signals {
            if !sig.equivalence_classes {
                continue;
            }
            let Some(seeds) = literals.get(&sig.name) else {
                continue;
            };
            if seeds.is_empty() {
                continue;
            }
            // Fetch or create the discovered_values entry.
            let entry = self
                .discovered_values
                .entry(sig.name.clone())
                .or_insert_with(|| DiscoveredValues {
                    values: Vec::new(),
                    catch_all: "OTHER".to_string(),
                });
            let existing_values: std::collections::HashSet<i64> =
                entry.values.iter().map(|v| v.value).collect();
            let mut added_now = Vec::new();
            for &v in seeds {
                let v_signed = v as i64;
                if existing_values.contains(&v_signed) {
                    continue;
                }
                let variant = format!("{}_{}", sig.name, v_signed);
                entry.values.push(DiscoveredValue {
                    name: variant,
                    value: v_signed,
                    from: Some("case-literal equivalence class".to_string()),
                });
                added_now.push(v_signed);
            }
            if added_now.is_empty() {
                continue;
            }
            // Force abstraction to Discover so the sidecar resolver's
            // Discover arm picks up the discovered_values + catch_all.
            sig.abstraction = SignalAbstraction::Discover;
            applied.push((sig.name.clone(), added_now));
        }
        applied
    }
}

/// R-S7 helper — split a predicate-name string of the shape
/// `<signal>_<integer-suffix>` into `(signal, value)`. Uses the
/// declared-signals set to disambiguate (e.g. `boot_fsm_ns_5` against
/// declared `boot_fsm_ns` → splits at the last `_` boundary that
/// matches a declared signal name).
///
/// Returns `None` when the predicate name does not parse this way —
/// e.g. typedef-derived variants like `boot_fsm_ns_BOOT_IDLE` (non-
/// numeric suffix), or names where no prefix matches a declared
/// signal.
fn split_signal_integer_suffix(
    predicate: &str,
    signal_names: &std::collections::HashSet<String>,
) -> Option<(String, i64)> {
    // Try every underscore split position; prefer the longest prefix
    // that matches a declared signal (handles signal names containing
    // underscores like `boot_fsm_ns`).
    let bytes = predicate.as_bytes();
    let mut best: Option<(String, i64)> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'_' {
            continue;
        }
        let prefix = &predicate[..i];
        let suffix = &predicate[i + 1..];
        if !signal_names.contains(prefix) {
            continue;
        }
        let Ok(value) = suffix.parse::<i64>() else {
            continue;
        };
        // Prefer the longest matching prefix.
        if best
            .as_ref()
            .map(|(p, _)| prefix.len() > p.len())
            .unwrap_or(true)
        {
            best = Some((prefix.to_string(), value));
        }
    }
    best
}

/// Annotation for an input port.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputAnnotation {
    /// Port name.
    pub name: String,

    /// Whether to include this input as a label dimension.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub preserve: bool,

    /// Abstraction strategy (default: boolean for 1-bit).
    #[serde(default)]
    pub abstraction: SignalAbstraction,

    /// Upper bound for `bounded_counter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound: Option<i64>,

    /// Explicit enum variants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,

    /// Value map entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_map: Option<Vec<ValueMapEntry>>,

    /// Override the label prefix used in transitions. When set, transitions
    /// for this input use `label_name` instead of `name` in the generated
    /// labels. Used by multi-module connections to create shared labels
    /// between driving and receiving modules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_name: Option<String>,

    /// R-Y2 (§Phase 8 §8.1) — per-input init policy override. Same
    /// semantics as `SignalAnnotation::init_policy`; used when an
    /// input port is the under-constrained constant the property
    /// depends on (e.g. fuse / strap bits in the Caliptra context).
    /// **Default `Inherit`** — strict additivity.
    #[serde(default, skip_serializing_if = "is_inherit_init_policy")]
    pub init_policy: InitPolicy,
}

/// Abstraction strategy for a signal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalAbstraction {
    /// 1-bit: true/false.
    Boolean,
    /// N-bit kept as individual bits (≤4 bits only).
    BitBlast,
    /// 0..bound counter with saturation.
    BoundedCounter,
    /// Named enum variants (optionally with value mapping).
    Enum,
    /// Let SMT discover significant values from guard analysis.
    #[default]
    Discover,
    /// Exclude from state space.
    Ignored,
}

/// A named value in a value map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueMapEntry {
    pub name: String,
    pub value: i64,
}

/// R-Y2 (§Phase 8 §8.1) — Per-signal init policy. Selects which
/// Yosys `setundef`-style treatment applies to *this signal* in
/// isolation, overriding the global `setundef_zero` / `setundef_anyseq`
/// / `setundef_anyconst` policy from `YosysOptions`.
///
/// The Yosys mechanism is the `(* anyconst *)` net attribute applied
/// per signal via the `setattr -mod -set anyconst 1 w:<signal>`
/// script command between `read_verilog` and `proc` passes. This
/// gives surgical control — `anyconst` only on the bug-relevant
/// register (e.g. `boot_fsm_ns` in the Caliptra fixture) while
/// other undefs stay `zero`.
///
/// **Default**: `Inherit` — apply the global policy from
/// `YosysOptions`. Explicit per-signal opt-in is the load-bearing
/// case for the Caliptra anchor per §Phase 8 §8.2.
///
/// **Strict additivity**: legacy sidecars without this field
/// continue to load (default `Inherit`); existing fixtures'
/// verdicts unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitPolicy {
    /// Defer to the global `YosysOptions::setundef_*` flags. This
    /// is the legacy behaviour and the load-preserving default for
    /// sidecars that pre-date R-Y2.
    #[default]
    Inherit,
    /// Override: pin this signal's undefined bits to 0. Cheapest;
    /// matches the global `setundef -zero` semantics for this
    /// signal alone.
    Zero,
    /// Override: this signal's undefined bits become one
    /// nondeterministic constant input each. Solver picks any
    /// concrete value at init; value stays fixed for the run. The
    /// Caliptra anchor uses this on `boot_fsm_ns` (3 bits → 8 init
    /// choices) while other undefs stay `zero`.
    Anyconst,
    /// Override: this signal's undefined bits become free symbolic
    /// choices each cycle. Per-signal `$anyseq` cells; small
    /// per-signal cost.
    Anyseq,
}

impl InitPolicy {
    /// Returns the Yosys per-signal attribute name + value pair,
    /// or `None` for `Inherit` (the global policy applies).
    /// Used by the Yosys script-builder to emit `setattr` commands.
    pub fn yosys_attribute(self) -> Option<(&'static str, u32)> {
        match self {
            InitPolicy::Inherit => None,
            InitPolicy::Zero => Some(("init", 0)), // emitted as `setattr -set init 0`
            InitPolicy::Anyconst => Some(("anyconst", 1)),
            InitPolicy::Anyseq => Some(("anyseq", 1)),
        }
    }

    /// The sidecar spelling of the policy (`inherit`, `zero`, `anyconst`, `anyseq`), as the
    /// `init_policy` key reads it.
    pub fn as_str(self) -> &'static str {
        match self {
            InitPolicy::Inherit => "inherit",
            InitPolicy::Zero => "zero",
            InitPolicy::Anyconst => "anyconst",
            InitPolicy::Anyseq => "anyseq",
        }
    }
}

/// A property to verify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyAnnotation {
    /// Property identifier.
    pub id: String,

    /// Mu-calculus formula body.
    /// Optional when `template_ref` is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,

    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Property role: "guarantee" (default), "assumption", or "standalone".
    #[serde(default = "default_guarantee", skip_serializing_if = "is_guarantee")]
    pub role: String,

    /// Reference to a property template from the template catalog.
    /// When present, the template is instantiated to produce a mu-calculus formula.
    /// If both `formula` and `template_ref` are present, `formula` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_ref: Option<crate::adapter::templates::TemplateRef>,
}

/// SMT-discovered values for a signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredValues {
    /// Discovered significant values with provenance.
    pub values: Vec<DiscoveredValue>,

    /// Name for the catch-all variant (values not in the map).
    #[serde(default = "default_other")]
    pub catch_all: String,
}

/// A single discovered value with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredValue {
    /// The concrete numeric value.
    pub value: i64,

    /// User-editable variant name.
    pub name: String,

    /// How this value was discovered (e.g., "case label at line 38",
    /// "SMT: guard (cmd * 2 == 6) at line 45").
    #[serde(default)]
    pub from: Option<String>,
}

// Loading
// ---------------------------------------------------------------------------

/// Look for `<stem>.mununu.json` next to the given `.sv` file.
pub fn find_sidecar(sv_path: &Path) -> Option<std::path::PathBuf> {
    let stem = sv_path.file_stem()?.to_str()?;
    let dir = sv_path.parent()?;
    let sidecar = dir.join(format!("{stem}.mununu.json"));
    if sidecar.exists() {
        Some(sidecar)
    } else {
        None
    }
}

/// Canonical `$schema` tag for the single-module SV/BTOR2 sidecar.
pub const SV_ANNOTATION_SCHEMA: &str = "mununu_sv_annotation_v1";

/// `$schema` tags removed from the tool. The native multi-module sidecar
/// path was excised in S.2b; a sidecar still tagged with one of these is
/// stale and would load with most fields silently defaulted, so loading
/// hard-fails with a migration hint instead.
const REMOVED_SCHEMA_TAGS: &[&str] = &["mununu_sv_multi_v1"];

// Per-level known-key allowlists for the load-time lint (C0.1 / finding E3).
// Kept in sync with the structs above by `sidecar_key_allowlists_match_structs`.
const SV_TOP_KEYS: &[&str] = &[
    "$schema",
    "module",
    "source",
    "signals",
    "inputs",
    "controllable",
    "properties",
    "discovered_values",
    "parameters",
    "parameter_concretizations",
    "reset_sequence",
    "simulate_reset",
    "vcd_traces",
    "memories",
    "uf_wrap",
    "uf_unwrap",
    "predicates",
];
const SV_SIGNAL_KEYS: &[&str] = &[
    "name",
    "preserve",
    "abstraction",
    "bound",
    "variants",
    "value_map",
    "combinational",
    "init_policy",
    "bounded_init",
    "equivalence_classes",
    "config_values",
    "drives",
    "type_name",
    "note",
];
const SV_INPUT_KEYS: &[&str] = &[
    "name",
    "preserve",
    "abstraction",
    "bound",
    "variants",
    "value_map",
    "label_name",
    "init_policy",
];
const SV_PROPERTY_KEYS: &[&str] = &["id", "formula", "description", "role", "template_ref"];

/// A JSON key is a documentation comment — never a typo — when it begins
/// with `$` or `_`. Authors annotate sidecars with `$comment_*` / `_note`;
/// these are tolerated and never flagged. (`$schema` is also in the
/// top-level allowlist; either rule accepts it.)
fn is_sidecar_comment_key(k: &str) -> bool {
    k.starts_with('$') || k.starts_with('_')
}

fn collect_unknown_key_warnings(
    obj: &serde_json::Map<String, serde_json::Value>,
    allow: &[&str],
    ctx: &str,
    label: &str,
    out: &mut Vec<String>,
) {
    for k in obj.keys() {
        if !allow.contains(&k.as_str()) && !is_sidecar_comment_key(k) {
            out.push(format!(
                "sidecar `{label}`: unknown field `{k}` in {ctx} — IGNORED (typo? \
                 a `$`- or `_`-prefixed key is treated as a comment). Known {ctx} \
                 fields: {}.",
                allow.join(", ")
            ));
        }
    }
}

/// Lint a sidecar's raw JSON before it is deserialized into [`SvAnnotation`].
///
/// Returns the list of non-fatal warnings (unknown fields at the root /
/// `signals[]` / `inputs[]` / `properties[]` levels — a likely typo that
/// would otherwise silently deserialize to a serde default, e.g. a
/// mistyped `abstraction` collapsing a register to `Discover`/`bound: 3`).
/// `$`- and `_`-prefixed keys are author comments and never flagged.
///
/// Hard-fails (`Err`) only when `$schema` names a removed schema
/// ([`REMOVED_SCHEMA_TAGS`]). A parse error is left to the real
/// deserialize so its message + location are preserved.
///
/// Sidecar-audit finding E3/O3 (C0.1). Returning the warnings (rather than
/// logging inside) keeps the detection testable; callers log each via
/// `tracing::warn!`.
pub fn lint_annotation_json(content: &str, label: &str) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return Ok(warnings); // parse error surfaces in the real deserialize
    };
    let Some(top) = value.as_object() else {
        return Ok(warnings);
    };

    if let Some(schema) = top.get("$schema").and_then(|v| v.as_str()) {
        if REMOVED_SCHEMA_TAGS.contains(&schema) {
            return Err(format!(
                "sidecar `{label}`: `$schema = \"{schema}\"` is a removed schema \
                 — the native multi-module SV sidecar path was excised in S.2b. \
                 Convert to `{SV_ANNOTATION_SCHEMA}` (single-module); multi-module \
                 composition is now driven structurally from the hierarchy, not a \
                 sidecar."
            ));
        }
        if schema != SV_ANNOTATION_SCHEMA {
            warnings.push(format!(
                "sidecar `{label}`: unrecognized `$schema = \"{schema}\"` (expected \
                 `{SV_ANNOTATION_SCHEMA}`) — loading leniently."
            ));
        }
    }

    collect_unknown_key_warnings(top, SV_TOP_KEYS, "the sidecar root", label, &mut warnings);
    let nested = [
        ("signals", SV_SIGNAL_KEYS, "a signals[] entry"),
        ("inputs", SV_INPUT_KEYS, "an inputs[] entry"),
        ("properties", SV_PROPERTY_KEYS, "a properties[] entry"),
    ];
    for (key, allow, ctx) in nested {
        for entry in top
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(o) = entry.as_object() {
                collect_unknown_key_warnings(o, allow, ctx, label, &mut warnings);
            }
        }
    }
    Ok(warnings)
}

/// Load and parse a `.mununu.json` sidecar file (single-module format).
pub fn load_annotation(path: &Path) -> Result<SvAnnotation, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
    let label = path.display().to_string();
    // C0.1 (finding E3/O3): hard-fail on a removed `$schema`, warn on
    // unknown fields (typo guard) before the lenient deserialize.
    for w in lint_annotation_json(&content, &label)? {
        tracing::warn!("{w}");
    }
    serde_json::from_str(&content).map_err(|e| format!("failed to parse '{}': {e}", path.display()))
}

/// Merge newly discovered values into an existing `discovered_values` map,
/// deduplicating by value and sorting.
pub fn merge_discovered_values(
    target: &mut HashMap<String, DiscoveredValues>,
    results: HashMap<String, DiscoveredValues>,
) {
    for (signal, discovered) in results {
        let existing = target.entry(signal).or_insert_with(|| DiscoveredValues {
            values: vec![],
            catch_all: "OTHER".to_string(),
        });
        for new_val in &discovered.values {
            if !existing.values.iter().any(|v| v.value == new_val.value) {
                existing.values.push(new_val.clone());
            }
        }
        existing.values.sort_by_key(|v| v.value);
    }
}

// ---------------------------------------------------------------------------
// Serde defaults / skips shared by the sidecar data types above.
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn default_guarantee() -> String {
    "guarantee".to_string()
}

fn is_guarantee(v: &str) -> bool {
    v == "guarantee"
}

fn default_other() -> String {
    "OTHER".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- R-Y2 (§Phase 8 §8.1) — per-signal init policy ----

    #[test]
    fn init_policy_defaults_to_inherit() {
        assert!(matches!(InitPolicy::default(), InitPolicy::Inherit));
    }

    #[test]
    fn init_policy_yosys_attribute_inherit_is_none() {
        assert!(InitPolicy::Inherit.yosys_attribute().is_none());
    }

    #[test]
    fn init_policy_yosys_attribute_anyconst() {
        assert_eq!(
            InitPolicy::Anyconst.yosys_attribute(),
            Some(("anyconst", 1))
        );
    }

    #[test]
    fn init_policy_yosys_attribute_anyseq() {
        assert_eq!(InitPolicy::Anyseq.yosys_attribute(), Some(("anyseq", 1)));
    }

    #[test]
    fn init_policy_yosys_attribute_zero() {
        assert_eq!(InitPolicy::Zero.yosys_attribute(), Some(("init", 0)));
    }

    #[test]
    fn legacy_sidecar_without_init_policy_loads_with_inherit() {
        // R-Y2 strict additivity: a sidecar that pre-dates R-Y2
        // (no init_policy fields) must deserialise cleanly with
        // init_policy = Inherit on every signal/input.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy",
            "signals": [
                { "name": "reg_a", "abstraction": "boolean" }
            ],
            "inputs": [
                { "name": "in_b", "abstraction": "boolean" }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("legacy sidecar parses");
        assert_eq!(ann.signals.len(), 1);
        assert!(matches!(ann.signals[0].init_policy, InitPolicy::Inherit));
        assert_eq!(ann.inputs.len(), 1);
        assert!(matches!(ann.inputs[0].init_policy, InitPolicy::Inherit));
        // init_policy_overrides() should return empty for an
        // all-inherit sidecar.
        assert!(ann.init_policy_overrides().is_empty());
    }

    #[test]
    fn sidecar_with_anyconst_override_round_trips() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "caliptra",
            "signals": [
                {
                    "name": "boot_fsm_ns",
                    "abstraction": "boolean",
                    "init_policy": "anyconst"
                },
                { "name": "other_reg", "abstraction": "boolean" }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parses");
        assert!(matches!(ann.signals[0].init_policy, InitPolicy::Anyconst));
        assert!(matches!(ann.signals[1].init_policy, InitPolicy::Inherit));
        let overrides = ann.init_policy_overrides();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].0, "boot_fsm_ns");
        assert!(matches!(overrides[0].1, InitPolicy::Anyconst));
    }

    #[test]
    fn init_policy_overrides_orders_signals_then_inputs() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [
                { "name": "sig_a", "abstraction": "boolean", "init_policy": "anyconst" },
                { "name": "sig_b", "abstraction": "boolean" },
                { "name": "sig_c", "abstraction": "boolean", "init_policy": "anyseq" }
            ],
            "inputs": [
                { "name": "in_x", "abstraction": "boolean", "init_policy": "zero" },
                { "name": "in_y", "abstraction": "boolean" }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parses");
        let overrides = ann.init_policy_overrides();
        // signals first (in declaration order, skipping Inherit), then inputs.
        let names: Vec<&str> = overrides.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["sig_a", "sig_c", "in_x"]);
        // policies preserved
        assert!(matches!(overrides[0].1, InitPolicy::Anyconst));
        assert!(matches!(overrides[1].1, InitPolicy::Anyseq));
        assert!(matches!(overrides[2].1, InitPolicy::Zero));
    }

    #[test]
    fn parse_minimal_sidecar() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "fifo",
            "signals": [
                {"name": "state", "abstraction": "enum", "variants": ["IDLE", "WRITING", "READING"]},
                {"name": "fill", "abstraction": "bounded_counter", "bound": 4}
            ],
            "properties": [
                {"id": "safety", "formula": "nu X. ([] X)"}
            ]
        }"#;

        let ann: SvAnnotation = serde_json::from_str(json).unwrap();
        assert_eq!(ann.module, "fifo");
        assert_eq!(ann.signals.len(), 2);
        assert_eq!(ann.signals[0].abstraction, SignalAbstraction::Enum);
        assert_eq!(ann.signals[1].bound, Some(4));
        assert_eq!(ann.properties.len(), 1);
    }

    #[test]
    fn sidecar_with_predicate_declarations_round_trips_and_lints_clean() {
        // IR-track P3.2 — a `predicates` block deserializes into
        // PredicateDeclaration entries, is NOT flagged as an unknown
        // root field by the lint (it's in SV_TOP_KEYS), and round-trips
        // on serialize. Schema-only — no verify wiring yet (P3.3).
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "boot",
            "signals": [{ "name": "boot_fsm_ns", "abstraction": "discover" }],
            "predicates": [
                { "name": "boot_idle", "register": "boot_fsm_ns", "value": 0 },
                { "name": "boot_done", "register": "boot_fsm_ns", "value": 7 }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parses");
        assert_eq!(ann.predicates.len(), 2);
        assert_eq!(ann.predicates[0].name, "boot_idle");
        assert_eq!(ann.predicates[0].register, "boot_fsm_ns");
        assert_eq!(ann.predicates[1].value, 7);
        // `predicates` is an allowlisted root key → no unknown-field warning.
        let warnings = lint_annotation_json(json, "boot.mununu.json").expect("lints");
        assert!(
            !warnings.iter().any(|w| w.contains("predicates")),
            "predicates must not warn as unknown; got: {warnings:?}"
        );
        // Round-trips on serialize.
        let out = serde_json::to_string(&ann).expect("serializes");
        assert!(out.contains("\"predicates\""), "serialized: {out}");
    }

    #[test]
    fn sidecar_without_predicates_defaults_empty() {
        // Back-compat: legacy sidecars (no `predicates` key) load with
        // an empty predicate set — the bit-blast verify path is unchanged.
        let json = r#"{ "module": "m", "signals": [] }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parses");
        assert!(ann.predicates.is_empty());
    }

    #[test]
    fn parse_discover_with_discovered_values() {
        let json = r#"{
            "module": "alu",
            "signals": [
                {"name": "cmd", "abstraction": "discover"}
            ],
            "discovered_values": {
                "cmd": {
                    "values": [
                        {"value": 0, "name": "NOP", "from": "case label at line 38"},
                        {"value": 1, "name": "LOAD", "from": "case label at line 39"}
                    ],
                    "catch_all": "OTHER"
                }
            },
            "properties": []
        }"#;

        let ann: SvAnnotation = serde_json::from_str(json).unwrap();
        let discovered = ann.discovered_values.get("cmd").unwrap();
        assert_eq!(discovered.values.len(), 2);
        assert_eq!(discovered.values[0].name, "NOP");
        assert_eq!(discovered.values[0].value, 0);
        assert_eq!(discovered.catch_all, "OTHER");
    }

    #[test]
    fn parse_full_sidecar() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "alu",
            "source": "alu.sv",
            "signals": [
                {"name": "acc", "abstraction": "bounded_counter", "bound": 7},
                {"name": "cmd", "abstraction": "discover", "note": "opcode register"},
                {"name": "data_buf", "preserve": false}
            ],
            "inputs": [
                {"name": "start", "abstraction": "boolean"},
                {"name": "operand", "abstraction": "bounded_counter", "bound": 3},
                {"name": "data_in", "preserve": false}
            ],
            "controllable": ["acc"],
            "properties": [
                {"id": "safety", "formula": "nu X. ([] X)", "description": "No deadlock"},
                {"id": "reset_clears", "formula": "nu X. ([] X)", "role": "guarantee"}
            ],
            "parameters": {"WIDTH": 8},
            "discovered_values": {}
        }"#;

        let ann: SvAnnotation = serde_json::from_str(json).unwrap();
        assert_eq!(ann.module, "alu");
        assert_eq!(ann.signals.len(), 3);
        assert!(!ann.signals[2].preserve); // data_buf excluded
        assert_eq!(ann.inputs.len(), 3);
        assert!(!ann.inputs[2].preserve); // data_in excluded
        assert_eq!(ann.controllable, vec!["acc"]);
        assert_eq!(ann.parameters.get("WIDTH"), Some(&8));
    }

    // ---- R-S5 (§Phase 9 §9.1) — type-driven valuation auto-widening ----

    fn caliptra_typedefs()
    -> std::collections::HashMap<String, super::super::typedef_extract::TypedefEnum> {
        super::super::typedef_extract::extract_typedef_enums(
            r#"typedef enum logic [2:0] {
                BOOT_IDLE   = 3'b000,
                BOOT_FUSE   = 3'b001,
                BOOT_FW_RST = 3'b010,
                BOOT_WAIT   = 3'b011,
                BOOT_DONE   = 3'b100
            } boot_fsm_state_e;"#,
        )
    }

    fn empty_sv_annotation() -> SvAnnotation {
        SvAnnotation {
            schema: Some("mununu_sv_annotation_v1".into()),
            module: "test".into(),
            source: None,
            signals: vec![],
            inputs: vec![],
            controllable: vec![],
            properties: vec![],
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        }
    }

    fn signal_with_type(
        name: &str,
        type_name: &str,
        abstraction: SignalAbstraction,
    ) -> SignalAnnotation {
        SignalAnnotation {
            name: name.into(),
            preserve: true,
            abstraction,
            bound: None,
            variants: None,
            value_map: None,
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: Some(type_name.into()),
            note: None,
            config_values: None,
        }
    }

    #[test]
    fn r_s5_widens_discover_with_named_and_unmatched_variants() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_with_type(
            "boot_fsm_ns",
            "boot_fsm_state_e",
            SignalAbstraction::Discover,
        ));
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());

        assert_eq!(applied.len(), 1);
        assert_eq!(
            applied[0],
            ("boot_fsm_ns".into(), "boot_fsm_state_e".into(), 8)
        );

        let sig = &ann.signals[0];
        assert_eq!(sig.abstraction, SignalAbstraction::Enum);
        let variants = sig.variants.as_ref().unwrap();
        // 5 named + 3 unmatched, sorted by encoding value
        assert_eq!(variants.len(), 8);
        assert_eq!(variants[0], "BOOT_IDLE");
        assert_eq!(variants[4], "BOOT_DONE");
        assert_eq!(variants[5], "UNMATCHED_5");
        assert_eq!(variants[7], "UNMATCHED_7");

        let vm = sig.value_map.as_ref().unwrap();
        assert_eq!(vm.len(), 8);
        assert_eq!(vm[0].value, 0);
        assert_eq!(vm[7].value, 7);
    }

    #[test]
    fn r_s5_skips_signals_without_type_name() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(SignalAnnotation {
            name: "wait_count".into(),
            preserve: true,
            abstraction: SignalAbstraction::Discover,
            bound: None,
            variants: None,
            value_map: None,
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,

            config_values: None,
        });
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        assert!(applied.is_empty());
        assert_eq!(ann.signals[0].abstraction, SignalAbstraction::Discover);
    }

    #[test]
    fn r_s5_skips_signals_with_unknown_type() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_with_type(
            "unknown_signal",
            "no_such_type_t",
            SignalAbstraction::Discover,
        ));
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        assert!(applied.is_empty());
    }

    #[test]
    fn r_s5_skips_signals_with_explicit_variants() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_with_type("boot_fsm_ns", "boot_fsm_state_e", SignalAbstraction::Enum);
        sig.variants = Some(vec!["E0".into(), "E1".into()]);
        sig.value_map = Some(vec![
            ValueMapEntry {
                name: "E0".into(),
                value: 0,
            },
            ValueMapEntry {
                name: "E1".into(),
                value: 1,
            },
        ]);
        ann.signals.push(sig);
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        // User-supplied variants are NEVER overwritten.
        assert!(applied.is_empty());
        assert_eq!(ann.signals[0].variants.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn r_s5_skips_signals_with_non_widenable_abstraction() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_with_type(
            "some_flag",
            "boot_fsm_state_e",
            SignalAbstraction::Boolean,
        ));
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        assert!(applied.is_empty());
    }

    #[test]
    fn r_s5_widens_enum_with_empty_variants() {
        let mut ann = empty_sv_annotation();
        let sig = signal_with_type("boot_fsm_ns", "boot_fsm_state_e", SignalAbstraction::Enum);
        // variants and value_map are None — treated as empty, eligible for widening
        ann.signals.push(sig);
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        assert_eq!(applied.len(), 1);
        assert_eq!(ann.signals[0].variants.as_ref().unwrap().len(), 8);
    }

    // ---- R-S7 (§Phase 9 §9.1) — property-syntactic predicate seeding ----

    #[test]
    fn r_s7_split_signal_integer_suffix_basic() {
        let signals: std::collections::HashSet<String> =
            ["count".to_string(), "boot_fsm_ns".to_string()]
                .into_iter()
                .collect();
        assert_eq!(
            split_signal_integer_suffix("count_5", &signals),
            Some(("count".to_string(), 5))
        );
        assert_eq!(
            split_signal_integer_suffix("boot_fsm_ns_7", &signals),
            Some(("boot_fsm_ns".to_string(), 7))
        );
        // Longest-prefix-match wins
        assert_eq!(
            split_signal_integer_suffix("boot_fsm_ns_42", &signals),
            Some(("boot_fsm_ns".to_string(), 42))
        );
        // Non-numeric suffix returns None (typedef variants handled by R-S5)
        assert_eq!(
            split_signal_integer_suffix("boot_fsm_ns_BOOT_IDLE", &signals),
            None
        );
        // Unknown prefix returns None
        assert_eq!(split_signal_integer_suffix("unknown_5", &signals), None);
    }

    #[test]
    fn r_s7_seeds_integer_predicates_from_formula() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(SignalAnnotation {
            name: "count".into(),
            preserve: true,
            abstraction: SignalAbstraction::Discover,
            bound: None,
            variants: None,
            value_map: None,
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,

            config_values: None,
        });
        ann.properties.push(PropertyAnnotation {
            id: "p".into(),
            formula: Some("nu X. ((!count_3) && ([] X))".into()),
            description: None,
            role: "guarantee".into(),
            template_ref: None,
        });
        let applied = ann.apply_property_syntactic_seeding();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0, "count");
        assert_eq!(applied[0].1, vec![3]);
        let sig = &ann.signals[0];
        assert_eq!(sig.abstraction, SignalAbstraction::Enum);
        let vm = sig.value_map.as_ref().unwrap();
        assert_eq!(vm.len(), 1);
        assert_eq!(vm[0].name, "count_3");
        assert_eq!(vm[0].value, 3);
    }

    #[test]
    fn r_s7_skips_signals_with_explicit_value_map() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(SignalAnnotation {
            name: "count".into(),
            preserve: true,
            abstraction: SignalAbstraction::Enum,
            bound: None,
            variants: Some(vec!["ZERO".into(), "ONE".into()]),
            value_map: Some(vec![
                ValueMapEntry {
                    name: "ZERO".into(),
                    value: 0,
                },
                ValueMapEntry {
                    name: "ONE".into(),
                    value: 1,
                },
            ]),
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,

            config_values: None,
        });
        ann.properties.push(PropertyAnnotation {
            id: "p".into(),
            formula: Some("nu X. ((!count_3) && ([] X))".into()),
            description: None,
            role: "guarantee".into(),
            template_ref: None,
        });
        let applied = ann.apply_property_syntactic_seeding();
        // count_3 is new (3 isn't in {0, 1}), so it gets added —
        // additive, never overwrites existing.
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].1, vec![3]);
        assert_eq!(ann.signals[0].value_map.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn r_s7_skips_non_widenable_abstractions() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(SignalAnnotation {
            name: "flag".into(),
            preserve: true,
            abstraction: SignalAbstraction::Boolean,
            bound: None,
            variants: None,
            value_map: None,
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,

            config_values: None,
        });
        ann.properties.push(PropertyAnnotation {
            id: "p".into(),
            formula: Some("nu X. ((!flag_3) && ([] X))".into()),
            description: None,
            role: "guarantee".into(),
            template_ref: None,
        });
        let applied = ann.apply_property_syntactic_seeding();
        // Boolean abstraction can't take per-value discriminators; skipped.
        assert!(applied.is_empty());
    }

    #[test]
    fn r_s7_dedupes_existing_value_map_entries() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(SignalAnnotation {
            name: "count".into(),
            preserve: true,
            abstraction: SignalAbstraction::Enum,
            bound: None,
            variants: Some(vec!["count_3".into()]),
            value_map: Some(vec![ValueMapEntry {
                name: "count_3".into(),
                value: 3,
            }]),
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,

            config_values: None,
        });
        ann.properties.push(PropertyAnnotation {
            id: "p".into(),
            formula: Some("nu X. ((!count_3) && ([] X))".into()),
            description: None,
            role: "guarantee".into(),
            template_ref: None,
        });
        let applied = ann.apply_property_syntactic_seeding();
        // count_3 already in value_map → no-op.
        assert!(applied.is_empty());
    }

    // ---- R-S3 (§Phase 9 §9.1) — case-literal seeding ----

    fn signal_discover(name: &str) -> SignalAnnotation {
        SignalAnnotation {
            name: name.into(),
            preserve: true,
            abstraction: SignalAbstraction::Discover,
            bound: None,
            variants: None,
            value_map: None,
            combinational: false,
            init_policy: InitPolicy::Inherit,
            bounded_init: None,
            drives: None,
            equivalence_classes: false,
            type_name: None,
            note: None,
            config_values: None,
        }
    }

    #[test]
    fn r_s3_seeds_discriminators_from_case_literals() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_discover("opcode"));
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        let applied = ann.apply_case_literal_seeding(&literals);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0, "opcode");
        assert_eq!(applied[0].1, vec![1, 2, 5]);
        let sig = &ann.signals[0];
        assert_eq!(sig.abstraction, SignalAbstraction::Enum);
        let vm = sig.value_map.as_ref().unwrap();
        assert_eq!(vm.len(), 3);
        assert_eq!(vm[0].name, "opcode_1");
        assert_eq!(vm[2].name, "opcode_5");
    }

    #[test]
    fn r_s3_skips_signals_not_in_case_literal_map() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_discover("unused_signal"));
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1]);
        let applied = ann.apply_case_literal_seeding(&literals);
        assert!(applied.is_empty());
    }

    #[test]
    fn r_s3_dedupes_against_existing_value_map() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_discover("opcode");
        sig.abstraction = SignalAbstraction::Enum;
        sig.value_map = Some(vec![ValueMapEntry {
            name: "opcode_1".into(),
            value: 1,
        }]);
        sig.variants = Some(vec!["opcode_1".into()]);
        ann.signals.push(sig);
        let mut literals = HashMap::new();
        // 1 already in value_map; 2 and 5 are new.
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        let applied = ann.apply_case_literal_seeding(&literals);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].1, vec![2, 5]);
        assert_eq!(ann.signals[0].value_map.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn r_s3_skips_non_widenable_abstractions() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_discover("flag");
        sig.abstraction = SignalAbstraction::Boolean;
        ann.signals.push(sig);
        let mut literals = HashMap::new();
        literals.insert("flag".to_string(), vec![3]);
        let applied = ann.apply_case_literal_seeding(&literals);
        assert!(applied.is_empty());
    }

    #[test]
    fn r_s3_no_op_when_literals_empty() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_discover("opcode"));
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![]);
        let applied = ann.apply_case_literal_seeding(&literals);
        assert!(applied.is_empty());
    }

    // ---- R-S4 (§Phase 9 §9.1) — equivalence-class seeding ----

    #[test]
    fn r_s4_seeds_discovered_values_with_catch_all_for_opt_in_signals() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_discover("opcode");
        sig.equivalence_classes = true;
        ann.signals.push(sig);
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        let applied = ann.apply_equivalence_class_seeding(&literals);
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].0, "opcode");
        assert_eq!(applied[0].1, vec![1, 2, 5]);
        let entry = ann.discovered_values.get("opcode").unwrap();
        assert_eq!(entry.values.len(), 3);
        assert_eq!(entry.catch_all, "OTHER");
        // Provenance is set so future contributors know where the
        // values came from (e.g. for sidecar inspection / round-trip).
        assert!(
            entry.values[0]
                .from
                .as_deref()
                .unwrap_or("")
                .contains("case-literal equivalence class")
        );
        // Abstraction forced to Discover so the sidecar resolver
        // picks up the discovered_values + catch_all.
        assert_eq!(ann.signals[0].abstraction, SignalAbstraction::Discover);
    }

    #[test]
    fn r_s4_skips_signals_without_opt_in() {
        let mut ann = empty_sv_annotation();
        // Default `equivalence_classes: false`.
        ann.signals.push(signal_discover("opcode"));
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        let applied = ann.apply_equivalence_class_seeding(&literals);
        assert!(applied.is_empty());
        assert!(ann.discovered_values.is_empty());
    }

    #[test]
    fn r_s4_dedupes_against_existing_discovered_values() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_discover("opcode");
        sig.equivalence_classes = true;
        ann.signals.push(sig);
        ann.discovered_values.insert(
            "opcode".to_string(),
            DiscoveredValues {
                values: vec![DiscoveredValue {
                    name: "opcode_1".into(),
                    value: 1,
                    from: None,
                }],
                catch_all: "OTHER".into(),
            },
        );
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        let applied = ann.apply_equivalence_class_seeding(&literals);
        // 1 already in discovered_values; 2 and 5 are new.
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].1, vec![2, 5]);
        assert_eq!(ann.discovered_values.get("opcode").unwrap().values.len(), 3);
    }

    #[test]
    fn r_s3_skips_signals_with_equivalence_classes_opt_in() {
        let mut ann = empty_sv_annotation();
        let mut sig = signal_discover("opcode");
        sig.equivalence_classes = true;
        ann.signals.push(sig);
        let mut literals = HashMap::new();
        literals.insert("opcode".to_string(), vec![1, 2, 5]);
        // R-S3 should skip — R-S4 handles this signal exclusively.
        let r_s3_applied = ann.apply_case_literal_seeding(&literals);
        assert!(r_s3_applied.is_empty());
        assert_eq!(
            ann.signals[0].abstraction,
            SignalAbstraction::Discover,
            "R-S3 should not have promoted to Enum"
        );
        // R-S4 then fires.
        let r_s4_applied = ann.apply_equivalence_class_seeding(&literals);
        assert_eq!(r_s4_applied.len(), 1);
        assert_eq!(ann.discovered_values.get("opcode").unwrap().values.len(), 3);
    }

    #[test]
    fn r_s5_deterministic_order_signals_in_declaration_order() {
        let mut ann = empty_sv_annotation();
        ann.signals.push(signal_with_type(
            "sig_b",
            "boot_fsm_state_e",
            SignalAbstraction::Discover,
        ));
        ann.signals.push(signal_with_type(
            "sig_a",
            "boot_fsm_state_e",
            SignalAbstraction::Discover,
        ));
        let applied = ann.apply_type_driven_widening(&caliptra_typedefs());
        assert_eq!(applied.len(), 2);
        // Reported in sidecar declaration order, not alphabetical
        assert_eq!(applied[0].0, "sig_b");
        assert_eq!(applied[1].0, "sig_a");
    }

    // ---- §Phase 10 §10.2 stage 2 — Memory schema extension ----

    #[test]
    fn phase10_memory_abstraction_defaults_to_uf() {
        assert_eq!(MemoryAbstraction::default(), MemoryAbstraction::Uf);
    }

    #[test]
    fn phase10_memory_annotation_round_trips_through_json() {
        let json = r#"{
            "name": "rf_reg",
            "address_width": 5,
            "data_width": 32,
            "abstraction": "uf",
            "selected_addresses": [0, 1, 5, 31]
        }"#;
        let mem: MemoryAnnotation = serde_json::from_str(json).expect("parse");
        assert_eq!(mem.name, "rf_reg");
        assert_eq!(mem.address_width, 5);
        assert_eq!(mem.data_width, 32);
        assert_eq!(mem.abstraction, MemoryAbstraction::Uf);
        assert_eq!(
            mem.selected_addresses.as_deref(),
            Some(&[0u64, 1, 5, 31][..])
        );
        let back = serde_json::to_string(&mem).expect("serialize");
        let again: MemoryAnnotation = serde_json::from_str(&back).expect("re-parse");
        assert_eq!(again.name, mem.name);
        assert_eq!(again.address_width, mem.address_width);
        assert_eq!(again.abstraction, mem.abstraction);
    }

    #[test]
    fn phase10_memory_annotation_omits_selected_addresses_when_none() {
        let mem = MemoryAnnotation {
            name: "rf_reg".into(),
            address_width: 5,
            data_width: 32,
            abstraction: MemoryAbstraction::Uf,
            selected_addresses: None,
        };
        let json = serde_json::to_string(&mem).expect("serialize");
        assert!(
            !json.contains("selected_addresses"),
            "selected_addresses None should be omitted from JSON; got {json}"
        );
    }

    #[test]
    fn phase10_memory_abstraction_all_variants_parse() {
        for (s, variant) in [
            ("uf", MemoryAbstraction::Uf),
            ("bit_blast", MemoryAbstraction::BitBlast),
            ("havoc", MemoryAbstraction::Havoc),
            ("bounded_bit_blast", MemoryAbstraction::BoundedBitBlast),
        ] {
            let json =
                format!(r#"{{"name":"m","address_width":4,"data_width":8,"abstraction":"{s}"}}"#);
            let mem: MemoryAnnotation =
                serde_json::from_str(&json).unwrap_or_else(|e| panic!("parse {s}: {e}"));
            assert_eq!(mem.abstraction, variant, "variant {s} round-trips");
        }
    }

    #[test]
    fn phase10_sv_annotation_with_memories_field_loads() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "ibex_register_file_ff",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 32,
                    "abstraction": "uf",
                    "selected_addresses": [0, 1, 5, 31]
                }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        assert_eq!(ann.memories.len(), 1);
        assert_eq!(ann.memories[0].name, "rf_reg");
        assert_eq!(ann.memories[0].abstraction, MemoryAbstraction::Uf);
    }

    #[test]
    fn phase10_legacy_sidecar_without_memories_field_loads_with_empty_vec() {
        // Strict additivity: existing fixtures' sidecars continue to work.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy"
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse legacy");
        assert!(ann.memories.is_empty());
    }

    #[test]
    fn phase10_empty_memories_omitted_on_serialize() {
        // skip_serializing_if = "Vec::is_empty" → field omitted when empty
        let ann = SvAnnotation {
            schema: Some("mununu_sv_annotation_v1".into()),
            module: "test".into(),
            source: None,
            signals: vec![],
            inputs: vec![],
            controllable: vec![],
            properties: vec![],
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        assert!(
            !json.contains("memories"),
            "empty memories should be omitted from JSON; got {json}"
        );
    }

    // ---- R.5b — uf_wrap + uf_unwrap sidecar API surface ----

    #[test]
    fn r5b_uf_wrap_uf_unwrap_round_trip_through_json() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["wide_mul_inst", "sha_round_compress"],
            "uf_unwrap": ["small_mul_inst"]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse uf_wrap fields");
        assert_eq!(ann.uf_wrap, vec!["wide_mul_inst", "sha_round_compress"]);
        assert_eq!(ann.uf_unwrap, vec!["small_mul_inst"]);

        let back = serde_json::to_string(&ann).expect("serialize");
        let again: SvAnnotation = serde_json::from_str(&back).expect("re-parse");
        assert_eq!(again.uf_wrap, ann.uf_wrap);
        assert_eq!(again.uf_unwrap, ann.uf_unwrap);
    }

    #[test]
    fn r5b_legacy_sidecar_without_uf_fields_loads_with_empty_vecs() {
        // Strict additivity: existing fixtures' sidecars continue to work
        // without uf_wrap / uf_unwrap declarations.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy"
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse legacy");
        assert!(
            ann.uf_wrap.is_empty(),
            "legacy sidecar must default uf_wrap to empty"
        );
        assert!(
            ann.uf_unwrap.is_empty(),
            "legacy sidecar must default uf_unwrap to empty"
        );
    }

    #[test]
    fn r5b_empty_uf_fields_omitted_on_serialize() {
        // skip_serializing_if = "Vec::is_empty" → fields omitted when empty.
        let ann = SvAnnotation {
            schema: Some("mununu_sv_annotation_v1".into()),
            module: "test".into(),
            source: None,
            signals: vec![],
            inputs: vec![],
            controllable: vec![],
            properties: vec![],
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        assert!(
            !json.contains("uf_wrap"),
            "empty uf_wrap must be omitted from JSON; got {json}"
        );
        assert!(
            !json.contains("uf_unwrap"),
            "empty uf_unwrap must be omitted from JSON; got {json}"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.5 (§Phase 9 §9.1) — SimulateReset serde + converter
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r_s2b_5_simulate_reset_round_trips_through_json() {
        let json = r#"{
            "clock_signal": "clk_i",
            "reset_signal": "rst_ni",
            "reset_asserted": 0,
            "hold_cycles": 4,
            "settle_cycles": 2,
            "observe_registers": ["boot_fsm_ns", "wait_count"]
        }"#;
        let sim: SimulateReset = serde_json::from_str(json).expect("parse");
        assert_eq!(sim.clock_signal, "clk_i");
        assert_eq!(sim.reset_signal, "rst_ni");
        assert_eq!(sim.reset_asserted, 0);
        assert_eq!(sim.hold_cycles, 4);
        assert_eq!(sim.settle_cycles, 2);
        assert_eq!(
            sim.observe_registers,
            vec!["boot_fsm_ns".to_string(), "wait_count".to_string()]
        );
        let back = serde_json::to_string(&sim).expect("serialize");
        let again: SimulateReset = serde_json::from_str(&back).expect("re-parse");
        assert_eq!(again, sim);
    }

    #[test]
    fn r_s2b_5_simulate_reset_settle_cycles_defaults_to_1_when_omitted() {
        // R-S2b.3a's ResetSimConfig defaults settle_cycles to 1
        // (sufficient for M.0–M.4 fixtures). The sidecar's serde
        // default mirrors that.
        let json = r#"{
            "clock_signal": "clk",
            "reset_signal": "rst",
            "reset_asserted": 1,
            "hold_cycles": 1,
            "observe_registers": ["q"]
        }"#;
        let sim: SimulateReset = serde_json::from_str(json).expect("parse");
        assert_eq!(sim.settle_cycles, 1);
    }

    #[test]
    fn r_s2b_5_simulate_reset_observe_registers_defaults_to_empty_when_omitted() {
        // observe_registers may legitimately be omitted in a draft
        // sidecar (the user will fill it in later). The runner
        // (R-S2b.3b) validates non-emptiness before invoking
        // Verilator.
        let json = r#"{
            "clock_signal": "clk",
            "reset_signal": "rst",
            "reset_asserted": 1,
            "hold_cycles": 1
        }"#;
        let sim: SimulateReset = serde_json::from_str(json).expect("parse");
        assert!(sim.observe_registers.is_empty());
    }

    #[test]
    fn r_s2b_5_simulate_reset_omits_empty_observe_registers_on_serialize() {
        let sim = SimulateReset {
            clock_signal: "clk".into(),
            reset_signal: "rst".into(),
            reset_asserted: 1,
            hold_cycles: 1,
            settle_cycles: 1,
            observe_registers: Vec::new(),
        };
        let json = serde_json::to_string(&sim).expect("serialize");
        assert!(
            !json.contains("observe_registers"),
            "empty observe_registers must be omitted from JSON; got {json}"
        );
    }

    #[test]
    fn r_s2b_5_sv_annotation_with_simulate_reset_loads() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "soc_ifc_boot_fsm",
            "simulate_reset": {
                "clock_signal": "clk_i",
                "reset_signal": "rst_ni",
                "reset_asserted": 0,
                "hold_cycles": 1,
                "settle_cycles": 1,
                "observe_registers": ["boot_fsm_ns", "wait_count"]
            }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        let sim = ann.simulate_reset.as_ref().expect("simulate_reset present");
        assert_eq!(sim.clock_signal, "clk_i");
        assert_eq!(sim.hold_cycles, 1);
        assert_eq!(sim.observe_registers.len(), 2);
    }

    #[test]
    fn r_s2b_5_legacy_sidecar_without_simulate_reset_loads_with_none() {
        // Strict additivity — existing fixtures' sidecars continue
        // to work, with simulate_reset defaulting to None.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy"
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse legacy");
        assert!(ann.simulate_reset.is_none());
    }

    #[test]
    fn r_s2b_5_simulate_reset_omitted_on_serialize_when_none() {
        let ann = SvAnnotation {
            schema: None,
            module: "test".into(),
            source: None,
            signals: Vec::new(),
            inputs: Vec::new(),
            controllable: Vec::new(),
            properties: Vec::new(),
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        assert!(
            !json.contains("simulate_reset"),
            "None simulate_reset must be omitted from JSON; got {json}"
        );
    }

    #[test]
    fn r_s2b_5_to_reset_sim_config_carries_all_fields() {
        let sim = SimulateReset {
            clock_signal: "clk_i".into(),
            reset_signal: "rst_ni".into(),
            reset_asserted: 0,
            hold_cycles: 3,
            settle_cycles: 2,
            observe_registers: vec!["a".into(), "b".into()],
        };
        let cfg = sim.to_reset_sim_config("soc_ifc_boot_fsm".into());
        assert_eq!(cfg.top, "soc_ifc_boot_fsm");
        assert_eq!(cfg.clock_signal, "clk_i");
        assert_eq!(cfg.reset_signal, "rst_ni");
        assert_eq!(cfg.reset_asserted, 0);
        assert_eq!(cfg.hold_cycles, 3);
        assert_eq!(cfg.settle_cycles, 2);
        assert_eq!(
            cfg.observe_registers,
            vec!["a".to_string(), "b".to_string()]
        );
        // The resulting config validates cleanly (sanity check that
        // a well-formed sidecar produces a well-formed runner
        // input — round-trips into R-S2b.3a's validator).
        assert!(cfg.validate().is_ok());
    }

    // ─────────────────────────────────────────────────────────────
    // R-S6.4 (§Phase 9 §9.1) — VcdTraceConfig serde + defaults
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r_s6_4_vcd_trace_config_round_trips_through_json() {
        let json = r#"{
            "path": "regression/uart_tx.vcd",
            "max_heavy_hitters_per_signal": 6,
            "seed_boundary_values": false,
            "signals": ["count", "state"]
        }"#;
        let cfg: VcdTraceConfig = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.path, "regression/uart_tx.vcd");
        assert_eq!(cfg.max_heavy_hitters_per_signal, 6);
        assert!(!cfg.seed_boundary_values);
        assert_eq!(cfg.signals, vec!["count".to_string(), "state".to_string()]);
        let back = serde_json::to_string(&cfg).expect("serialize");
        let again: VcdTraceConfig = serde_json::from_str(&back).expect("re-parse");
        assert_eq!(again, cfg);
    }

    #[test]
    fn r_s6_4_max_heavy_hitters_defaults_to_4() {
        let json = r#"{ "path": "trace.vcd" }"#;
        let cfg: VcdTraceConfig = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.max_heavy_hitters_per_signal, 4);
    }

    #[test]
    fn r_s6_4_seed_boundary_values_defaults_to_true() {
        let json = r#"{ "path": "trace.vcd" }"#;
        let cfg: VcdTraceConfig = serde_json::from_str(json).expect("parse");
        assert!(cfg.seed_boundary_values);
    }

    #[test]
    fn r_s6_4_signals_defaults_to_empty() {
        // Empty signals means "mine every signal in the trace"
        // — explicit per-trace allowlisting is opt-in.
        let json = r#"{ "path": "trace.vcd" }"#;
        let cfg: VcdTraceConfig = serde_json::from_str(json).expect("parse");
        assert!(cfg.signals.is_empty());
    }

    #[test]
    fn r_s6_4_empty_signals_omitted_on_serialize() {
        let cfg = VcdTraceConfig {
            path: "trace.vcd".into(),
            max_heavy_hitters_per_signal: 4,
            seed_boundary_values: true,
            signals: Vec::new(),
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(
            !json.contains("signals"),
            "empty signals must be omitted from JSON; got {json}"
        );
    }

    #[test]
    fn r_s6_4_sv_annotation_with_vcd_traces_loads() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "uart_tx",
            "vcd_traces": [
                {
                    "path": "regression/uart_tx.vcd",
                    "signals": ["count"]
                },
                {
                    "path": "regression/uart_tx_corner.vcd",
                    "max_heavy_hitters_per_signal": 8
                }
            ]
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        assert_eq!(ann.vcd_traces.len(), 2);
        assert_eq!(ann.vcd_traces[0].path, "regression/uart_tx.vcd");
        assert_eq!(ann.vcd_traces[0].max_heavy_hitters_per_signal, 4); // default
        assert_eq!(ann.vcd_traces[0].signals, vec!["count".to_string()]);
        assert_eq!(ann.vcd_traces[1].max_heavy_hitters_per_signal, 8);
        assert!(ann.vcd_traces[1].signals.is_empty());
    }

    #[test]
    fn r_s6_4_legacy_sidecar_without_vcd_traces_loads_with_empty_vec() {
        // Strict additivity — existing fixtures' sidecars continue
        // to work, vcd_traces defaults to empty Vec.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy"
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse legacy");
        assert!(ann.vcd_traces.is_empty());
    }

    #[test]
    fn r_s6_4_empty_vcd_traces_omitted_on_serialize() {
        let ann = SvAnnotation {
            schema: None,
            module: "test".into(),
            source: None,
            signals: Vec::new(),
            inputs: Vec::new(),
            controllable: Vec::new(),
            properties: Vec::new(),
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        assert!(
            !json.contains("vcd_traces"),
            "empty vcd_traces must be omitted from JSON; got {json}"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R-S1 (§Phase 9 §9.1) — ParameterConcretization + resolver
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r_s1_parameter_concretization_round_trips_through_json() {
        let json = r#"{
            "value": 2,
            "rationale": "Scaled N from 8 to 2 to fit MAX_STATE_BITS=20.",
            "justified_by": ".claude/plans/milestones/M-0-result.md"
        }"#;
        let pc: ParameterConcretization = serde_json::from_str(json).expect("parse");
        assert_eq!(pc.value, 2);
        assert!(pc.rationale.contains("Scaled"));
        assert_eq!(
            pc.justified_by.as_deref(),
            Some(".claude/plans/milestones/M-0-result.md")
        );
        let back = serde_json::to_string(&pc).expect("serialize");
        let again: ParameterConcretization = serde_json::from_str(&back).expect("re-parse");
        assert_eq!(again, pc);
    }

    #[test]
    fn r_s1_parameter_concretization_omits_justified_by_when_none() {
        let pc = ParameterConcretization {
            value: 4,
            rationale: "Default fit".into(),
            justified_by: None,
        };
        let json = serde_json::to_string(&pc).expect("serialize");
        assert!(
            !json.contains("justified_by"),
            "None justified_by must be omitted from JSON; got {json}"
        );
    }

    #[test]
    fn r_s1_sv_annotation_with_parameter_concretizations_loads() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "prim_arbiter_fixed",
            "parameter_concretizations": {
                "N": {
                    "value": 2,
                    "rationale": "Scaled from 8 to 2 to fit MAX_STATE_BITS=20."
                },
                "DW": {
                    "value": 2,
                    "rationale": "Reduced from 32 to 2 — property only observes the LSB."
                }
            }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        assert_eq!(ann.parameter_concretizations.len(), 2);
        assert_eq!(ann.parameter_concretizations.get("N").unwrap().value, 2);
        assert_eq!(ann.parameter_concretizations.get("DW").unwrap().value, 2);
    }

    #[test]
    fn r_s1_legacy_sidecar_without_parameter_concretizations_loads() {
        // Strict additivity — sidecars authored before R-S1
        // continue to work; field defaults to empty HashMap.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "legacy"
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse legacy");
        assert!(ann.parameter_concretizations.is_empty());
    }

    #[test]
    fn r_s1_empty_parameter_concretizations_omitted_on_serialize() {
        let ann = SvAnnotation {
            schema: None,
            module: "test".into(),
            source: None,
            signals: Vec::new(),
            inputs: Vec::new(),
            controllable: Vec::new(),
            properties: Vec::new(),
            discovered_values: HashMap::new(),
            parameters: HashMap::new(),
            parameter_concretizations: HashMap::new(),
            reset_sequence: None,
            simulate_reset: None,
            vcd_traces: Vec::new(),
            memories: Vec::new(),
            uf_wrap: Vec::new(),
            uf_unwrap: Vec::new(),
            predicates: Vec::new(),
            compound_predicates: Vec::new(),
            combinational_predicates: Vec::new(),
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        assert!(
            !json.contains("parameter_concretizations"),
            "empty parameter_concretizations must be omitted; got {json}"
        );
    }

    #[test]
    fn r_s1_effective_parameters_returns_legacy_when_concretizations_empty() {
        // Sidecar only uses the legacy `parameters` map; resolver
        // returns its contents verbatim.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "parameters": { "DEPTH": 4, "WIDTH": 8 }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        let effective = ann.effective_parameters();
        assert_eq!(effective.len(), 2);
        assert_eq!(effective.get("DEPTH").copied(), Some(4));
        assert_eq!(effective.get("WIDTH").copied(), Some(8));
    }

    #[test]
    fn r_s1_effective_parameters_returns_concretizations_when_legacy_empty() {
        // Sidecar only uses the structured concretizations field.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "parameter_concretizations": {
                "N": { "value": 2, "rationale": "Scaled for M.0." }
            }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        let effective = ann.effective_parameters();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective.get("N").copied(), Some(2));
    }

    #[test]
    fn r_s1_effective_parameters_structured_wins_on_name_conflict() {
        // Same parameter declared in both maps with different
        // values — the structured concretization wins (the bare
        // map is the legacy fallback).
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "parameters": { "N": 8 },
            "parameter_concretizations": {
                "N": { "value": 2, "rationale": "Scaled for M.0." }
            }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        let effective = ann.effective_parameters();
        assert_eq!(effective.len(), 1);
        assert_eq!(
            effective.get("N").copied(),
            Some(2),
            "structured concretization must win over bare value; got {effective:?}"
        );
    }

    #[test]
    fn r_s1_effective_parameters_merges_disjoint_keys() {
        // Different parameter names in each map; resolver returns
        // their union.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "parameters": { "DEPTH": 4 },
            "parameter_concretizations": {
                "N": { "value": 2, "rationale": "Scaled for M.0." }
            }
        }"#;
        let ann: SvAnnotation = serde_json::from_str(json).expect("parse");
        let effective = ann.effective_parameters();
        assert_eq!(effective.len(), 2);
        assert_eq!(effective.get("DEPTH").copied(), Some(4));
        assert_eq!(effective.get("N").copied(), Some(2));
    }

    // ---- C0.1 sidecar load-time lint (finding E3/O3) -------------------

    #[test]
    fn lint_clean_sidecar_has_no_warnings() {
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "m",
            "signals": [ { "name": "cnt", "abstraction": "bounded_counter", "bound": 7 } ],
            "properties": [ { "id": "p", "formula": "nu X. ([] X)" } ]
        }"#;
        let w = lint_annotation_json(json, "clean").expect("no hard fail");
        assert!(w.is_empty(), "clean sidecar should not warn; got {w:?}");
    }

    #[test]
    fn lint_flags_unknown_signal_field_typo() {
        // `abstration` (typo of `abstraction`) + `bonud` (typo of `bound`)
        // would otherwise silently deserialize to serde defaults.
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "module": "m",
            "signals": [ { "name": "cnt", "abstration": "bounded_counter", "bonud": 7 } ]
        }"#;
        let w = lint_annotation_json(json, "typo").expect("no hard fail");
        assert!(
            w.iter().any(|m| m.contains("abstration")) && w.iter().any(|m| m.contains("bonud")),
            "both field typos should be flagged; got {w:?}"
        );
    }

    #[test]
    fn lint_flags_unknown_root_field() {
        let json = r#"{ "$schema": "mununu_sv_annotation_v1", "module": "m", "signls": [] }"#;
        let w = lint_annotation_json(json, "root").expect("no hard fail");
        assert!(
            w.iter()
                .any(|m| m.contains("signls") && m.contains("sidecar root")),
            "root-level typo should be flagged; got {w:?}"
        );
    }

    #[test]
    fn lint_tolerates_comment_keys() {
        // `$comment_*` and `_note` are author comments — never flagged.
        // (Both shapes appear in the shipped fixture corpus.)
        let json = r#"{
            "$schema": "mununu_sv_annotation_v1",
            "$comment_distinguishing_property": "doc",
            "_note": "doc",
            "module": "m",
            "signals": [ { "name": "x", "abstraction": "boolean", "_why": "doc" } ]
        }"#;
        let w = lint_annotation_json(json, "comments").expect("no hard fail");
        assert!(w.is_empty(), "comment keys must not warn; got {w:?}");
    }

    #[test]
    fn lint_hard_fails_on_removed_multi_schema() {
        let json = r#"{ "$schema": "mununu_sv_multi_v1", "module": "m" }"#;
        let err = lint_annotation_json(json, "stale").expect_err("removed schema must hard-fail");
        assert!(
            err.contains("removed schema") && err.contains(SV_ANNOTATION_SCHEMA),
            "error should name the removal + the migration target; got {err}"
        );
    }

    #[test]
    fn lint_warns_on_unrecognized_schema() {
        let json = r#"{ "$schema": "mununu_sv_v99", "module": "m" }"#;
        let w = lint_annotation_json(json, "future").expect("unrecognized schema warns, not fails");
        assert!(
            w.iter()
                .any(|m| m.contains("unrecognized") && m.contains("mununu_sv_v99")),
            "unrecognized schema should warn; got {w:?}"
        );
    }

    /// Drift guard, two ways: the exhaustive struct literals below fail to
    /// COMPILE if a field is added to `SignalAnnotation` / `PropertyAnnotation`
    /// without updating this test, and the runtime assertions fail if such a
    /// new field serializes to a key not in the corresponding lint allowlist.
    /// Together they keep the C0.1 allowlists in sync with the structs, so the
    /// lint never flags a real (new) field as an unknown typo.
    #[test]
    fn sidecar_key_allowlists_match_structs() {
        fn keys(v: &serde_json::Value) -> Vec<String> {
            v.as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default()
        }
        // Populate every field so each serializes (skip_serializing_if'd
        // Options would otherwise vanish), giving the runtime check coverage.
        let sig = SignalAnnotation {
            name: "s".into(),
            preserve: true,
            abstraction: SignalAbstraction::BoundedCounter,
            bound: Some(1),
            variants: Some(vec!["A".into()]),
            value_map: Some(vec![ValueMapEntry {
                name: "A".into(),
                value: 0,
            }]),
            combinational: true,
            init_policy: InitPolicy::Zero,
            bounded_init: Some(vec![0]),
            equivalence_classes: true,
            config_values: Some(vec![0u64]),
            drives: Some("o".into()),
            type_name: Some("t".into()),
            note: Some("n".into()),
        };
        for k in keys(&serde_json::to_value(&sig).expect("serialize signal")) {
            assert!(
                SV_SIGNAL_KEYS.contains(&k.as_str()),
                "SignalAnnotation field `{k}` missing from SV_SIGNAL_KEYS allowlist"
            );
        }
        let prop = PropertyAnnotation {
            id: "p".into(),
            formula: Some("nu X. ([] X)".into()),
            description: Some("d".into()),
            role: "standalone".into(),
            template_ref: None,
        };
        for k in keys(&serde_json::to_value(&prop).expect("serialize property")) {
            assert!(
                SV_PROPERTY_KEYS.contains(&k.as_str()),
                "PropertyAnnotation field `{k}` missing from SV_PROPERTY_KEYS allowlist"
            );
        }
    }

    /// mununu#1121 — the policy's name is the sidecar spelling: it reads back through the
    /// `init_policy` key.
    #[test]
    fn x1121_the_init_policy_name_is_the_sidecar_spelling() {
        for p in [
            InitPolicy::Inherit,
            InitPolicy::Zero,
            InitPolicy::Anyconst,
            InitPolicy::Anyseq,
        ] {
            let back: InitPolicy = serde_json::from_str(&format!("\"{}\"", p.as_str()))
                .expect("the sidecar key accepts the name");
            assert_eq!(back, p);
        }
    }
}
