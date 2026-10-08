//! BTOR2 → AdapterIR bit-blaster.
//!
//! The bit-blaster enumerates every (state-valuation, input-valuation) pair
//! within user-bounded register widths and produces an explicit automaton
//! suitable for mununu's explicit-state μ-calculus engine.
//!
//! # Soundness (per CLAUDE.md soundness rules)
//!
//! - **Bit-blasting is exact** for the operators marked `is_blastable()` in
//!   [`super::ast::Op`]. No approximation is introduced for those.
//! - **Arbitrary width:** concrete values use [`super::bv::Bv`] (native `u128`
//!   for width ≤ 128, `BigUint` above), so intermediate and wide-datapath
//!   values are exact — no `u128` truncation.
//! - **State-space bound:** total reachable states are capped at
//!   [`MAX_STATE_BITS`]; the bit-blaster errors before enumeration if the
//!   bound is exceeded. This is an *under*-approximation in scope (the
//!   design is rejected, not silently truncated) — sound by construction.
//! - **Unsupported operators** (sdiv/udiv/srem/smod/urem/saddo/...): the
//!   bit-blaster emits [`AdapterErrorKind::UnsupportedConstruct`] with the
//!   offending NID. Handing the BTOR2 to an external symbolic engine
//!   (Phase 3 hand-off) is the documented escape hatch.

use super::ast::*;
use super::bv::Bv;
use super::parser;
use crate::adapter::ir::*;
use crate::adapter::{
    AdapterError, AdapterErrorKind, AdapterOptions, AdapterOutput, AdapterWarning, SourceFormat,
    SourceInfo, SourceLocation, WarningKind,
};

/// Maximum total bits across all state declarations.
/// 2^20 = ~1 M explicit states — the explicit-state engine's practical
/// upper bound. Raised from 16 → 20 on 2026-05-18 to admit small
/// industrial RTL modules (e.g. the Caliptra-RTL boot FSM bundles a
/// 3-bit state enum with an 8-bit wait counter + reset-window logic
/// totalling 19 state bits, which the previous cap rejected just past
/// the boundary). Designs above this still error rather than truncate.
pub const MAX_STATE_BITS: u32 = 20;

/// MIG-2 (§S-track migration, 2026-06-13) — the out-of-bounds sink
/// state. When a transition's next-state escapes the sidecar-declared
/// abstraction (a bounded counter overflows, an enum index leaves its
/// value set, an `Ignored`/`Dropped` cell leaves its pinned value),
/// the transition is routed to this absorbing sink instead of being
/// dropped. The sink carries the valuation `{"__mununu_oob__": "true"}`
/// — the marker [`crate::mu_calculus::evaluator`]'s `compute_oob_bits`
/// masks out of every formula's satisfying set (OOB-as-bottom). This
/// turns the abstraction from an **under-approximation** (drop = miss
/// behaviours, unsound for safety) into a sound **over-approximation**
/// (escape ⇒ "anything could happen" ⇒ falsifies safety), matching the
/// native `kripke.rs` `OOB_STATE_KEY` mechanism. The state NAME and the
/// marker valuation KEY are deliberately the same string; only the
/// valuation entry is load-bearing (the evaluator keys on it).
const OOB_SINK_KEY: &str = "__mununu_oob__";

/// Maximum total bits across all input declarations (per-step combinations).
/// 2^10 = 1024 input combos per state → up to ~1 G transitions at the
/// raised state cap (MAX_STATE_BITS = 20). Lifting the input cap further
/// quickly becomes intractable: 2^20 × 2^16 ≈ 6.8e10 transitions takes
/// hours to enumerate concretely. Designs above this cap must prune
/// unused inputs via a `.mununu.json` sidecar (see
/// [`crate::adapter::sidecar`]) declaring `Ignored` / `Boolean` /
/// `Symbols` abstractions per input.
pub const MAX_INPUT_BITS: u32 = 10;

// The concrete bit-vector value type is now [`crate::adapter::btor2::bv::Bv`]
// (arbitrary width via a Small(u128)/Wide(BigUint) split); the previous
// `u128`-ceiling `Bv` was retired in AR-S2.

/// Bit-blaster output structure for a single BTOR2 file.
struct BlastOutput {
    automaton: AutomatonSpec,
    signals: Vec<Signal>,
    properties: Vec<PropertySpec>,
    /// Phase A.3 step 3.6 — auto-partition telemetry populated by
    /// `enumerate_and_blast` after the COI pass runs. `None` when
    /// the partition step was skipped (empty seeds, or a future
    /// `--no-partition` opt-out).
    partition_summary: Option<crate::adapter::partition::PartitionSummary>,
}

/// R.2.5 predicate-image MVP — simulate one clock step of the BTOR2
/// design.
///
/// **Inputs.**
/// - `file`: parsed BTOR2.
/// - `register_values`: map from state-cell symbol to its current
///   value (as a `u128` masked to the cell's width). Cells absent
///   from the map default to zero.
/// - `input_values`: map from input symbol to its value for this
///   step. Inputs absent from the map default to zero (mirrors the
///   `setundef -zero` discipline the upstream Yosys script enforces).
///
/// **Output.** Map from state-cell symbol to its NEXT value after
/// the step. Cells without a `Next` line in the BTOR2 retain their
/// `register_values` entry (BTOR2 convention).
///
/// **Semantics.** Internally runs `evaluate_pure(file, env, honor_init=false)`
/// to compute every NID's value from the current state + inputs,
/// then `apply_next(...)` to commit the new state values. Returns
/// the post-step state values keyed by symbol for downstream
/// consumption (e.g. R.2.5's predicate-image edge construction).
///
/// **Error cases.** Returns `Err` only when `evaluate_pure` or
/// `apply_next` fails — typically on malformed BTOR2 with dangling
/// references (already validated at parse time, so this path is
/// rarely hit in practice).
pub fn simulate_one_step(
    file: &Btor2File,
    register_values: &std::collections::HashMap<String, u128>,
    input_values: &std::collections::HashMap<String, u128>,
) -> Result<std::collections::HashMap<String, u128>, AdapterError> {
    // Resolve symbols → NIDs for every state + input.
    let symbols = parser::collect_symbols(file);
    let mut symbol_to_state_nid: std::collections::HashMap<String, (Nid, u32)> =
        std::collections::HashMap::new();
    let mut symbol_to_input_nid: std::collections::HashMap<String, (Nid, u32)> =
        std::collections::HashMap::new();
    let mut state_meta: Vec<StateMeta> = Vec::new();

    for line in &file.lines {
        match &line.node {
            Node::State { sort, .. } => {
                let width = parser::bv_width(file, *sort).unwrap_or(0);
                let symbol = symbols
                    .get(&line.nid)
                    .cloned()
                    .unwrap_or_else(|| format!("st_n{}", line.nid));
                symbol_to_state_nid.insert(symbol.clone(), (line.nid, width));
                state_meta.push(StateMeta {
                    nid: line.nid,
                    width,
                    symbol,
                });
            }
            Node::Input { sort, .. } => {
                let width = parser::bv_width(file, *sort).unwrap_or(0);
                let symbol = symbols
                    .get(&line.nid)
                    .cloned()
                    .unwrap_or_else(|| format!("in_n{}", line.nid));
                symbol_to_input_nid.insert(symbol, (line.nid, width));
            }
            _ => {}
        }
    }

    // Seed the Env with the given register + input values (cells
    // without an entry default to zero per the `setundef -zero`
    // convention).
    let mut env = Env::default();
    for sm in &state_meta {
        let bits = register_values.get(&sm.symbol).copied().unwrap_or(0);
        env.values
            .insert(sm.nid, Bv::from_u128_width(bits, sm.width));
    }
    for (symbol, &(nid, width)) in &symbol_to_input_nid {
        let bits = input_values.get(symbol).copied().unwrap_or(0);
        env.values.insert(nid, Bv::from_u128_width(bits, width));
    }

    // Step the design: evaluate all derived NIDs, then commit
    // `next` updates to the state cells.
    evaluate_pure(file, &mut env, false)?;
    apply_next(file, &mut env, &state_meta)?;

    // Extract post-step state values keyed by symbol.
    let mut out: std::collections::HashMap<String, u128> = std::collections::HashMap::new();
    for sm in &state_meta {
        let bits = env.values.get(&sm.nid).map(|v| v.low128()).unwrap_or(0);
        out.insert(sm.symbol.clone(), bits);
    }
    Ok(out)
}

/// P1 #4 Phase 1 (IR-unification track, Q2 = B) — the observability-rich
/// one-step primitive behind
/// [`crate::adapter::sts_ir::StepEval::step_observe`].
///
/// Same concrete `(state, inputs) ↦ next_state` semantics as
/// [`simulate_one_step`] (absent keys default to 0, the `setundef -zero`
/// convention), but additionally reports, from the post-`evaluate_pure` /
/// pre-`apply_next` env (the current-cycle values):
///
/// - `observed`: the current value of each requested signal name, resolved
///   the SAME way the Enumerate strategy resolves combinational signals so
///   a future Pass-1 reroute is value-faithful — an `Op` node carrying the
///   symbol → its own computed value (mirrors `combinational_signal_nids`);
///   else an `Output` node → its referenced signal's value (mirrors
///   `output_port_nids`); else a `State` / `Input` → its own value.
///   Precedence Op > Output > State/Input. Names that resolve to no
///   symbol-bearing node are omitted from the map.
/// - `admissible`: whether every BTOR2 `constraint` line holds under this
///   `(state, inputs)` assignment. [`simulate_one_step`] returns the
///   next-state regardless; this variant surfaces the verdict so the
///   Enumerate strategy can drop inadmissible input combos *without*
///   re-opening the BTOR2 design — the Q2 = B "observability-rich
///   contract, clamping stays policy" decision.
///
/// Returns a [`crate::adapter::sts_ir::StepOutcome`] (`next_state` +
/// `observed` + `admissible`). The domain-encoding / OOB-sink /
/// state-splitting policy stays in the caller (`enumerate_and_blast`);
/// this primitive never mentions `FieldDomain`.
pub fn simulate_one_step_observe(
    file: &Btor2File,
    register_values: &std::collections::HashMap<String, u128>,
    input_values: &std::collections::HashMap<String, u128>,
    observe: &[String],
) -> Result<crate::adapter::sts_ir::StepOutcome, AdapterError> {
    // Build-once-step-once over the prepared evaluator. Hot callers that
    // step the same design many times (the Enumerate strategy) build a
    // `PreparedStep` once and call `.step` per (state, input) instead.
    PreparedStep::new(file).step(register_values, input_values, observe)
}

/// P1 #4 Phase 2b (IR-unification track, Q2 = B) — a prepared one-step
/// evaluator. Builds the per-design setup ONCE — the state / input lists
/// (each keyed by its *step name*: the [`parser::collect_symbols`] symbol,
/// or a deterministic `st_n<nid>` / `in_n<nid>` fallback) and the
/// observe-resolution maps — so the Enumerate strategy can step many
/// `(state, inputs)` assignments without re-running `collect_symbols` per
/// step (the cost the original Pass-1 cached once via `make_step_env` over
/// a shared `state_meta`).
///
/// `step` has the exact semantics of [`simulate_one_step_observe`] (which
/// is now a build-once-step-once wrapper over this).
struct PreparedStep<'a> {
    file: &'a Btor2File,
    /// `file.states()` order; `StateMeta.symbol` holds the step name.
    states: Vec<StateMeta>,
    /// `file.inputs()` order; `StateMeta.symbol` holds the step name.
    inputs: Vec<StateMeta>,
    /// Observe resolution (Phase 2a semantics), built once: Op symbol →
    /// own NID; Output symbol → referenced signal NID; State/Input → own.
    op_nid: std::collections::HashMap<String, Nid>,
    out_sig_nid: std::collections::HashMap<String, Nid>,
    si_nid: std::collections::HashMap<String, Nid>,
}

impl<'a> PreparedStep<'a> {
    fn new(file: &'a Btor2File) -> Self {
        let symbols = parser::collect_symbols(file);
        let mut states: Vec<StateMeta> = Vec::new();
        let mut inputs: Vec<StateMeta> = Vec::new();
        for line in &file.lines {
            match &line.node {
                Node::State { sort, .. } => {
                    let width = parser::bv_width(file, *sort).unwrap_or(0);
                    let symbol = symbols
                        .get(&line.nid)
                        .cloned()
                        .unwrap_or_else(|| format!("st_n{}", line.nid));
                    states.push(StateMeta {
                        nid: line.nid,
                        width,
                        symbol,
                    });
                }
                Node::Input { sort, .. } => {
                    let width = parser::bv_width(file, *sort).unwrap_or(0);
                    let symbol = symbols
                        .get(&line.nid)
                        .cloned()
                        .unwrap_or_else(|| format!("in_n{}", line.nid));
                    inputs.push(StateMeta {
                        nid: line.nid,
                        width,
                        symbol,
                    });
                }
                _ => {}
            }
        }
        let mut op_nid: std::collections::HashMap<String, Nid> = std::collections::HashMap::new();
        let mut out_sig_nid: std::collections::HashMap<String, Nid> =
            std::collections::HashMap::new();
        let mut si_nid: std::collections::HashMap<String, Nid> = std::collections::HashMap::new();
        for line in &file.lines {
            match &line.node {
                Node::Op {
                    symbol: Some(s), ..
                } => {
                    op_nid.entry(s.clone()).or_insert(line.nid);
                }
                Node::Output {
                    symbol: Some(s),
                    signal,
                } => {
                    out_sig_nid.entry(s.clone()).or_insert(signal.nid());
                }
                Node::State {
                    symbol: Some(s), ..
                }
                | Node::Input {
                    symbol: Some(s), ..
                } => {
                    si_nid.entry(s.clone()).or_insert(line.nid);
                }
                _ => {}
            }
        }
        Self {
            file,
            states,
            inputs,
            op_nid,
            out_sig_nid,
            si_nid,
        }
    }

    /// State cells in `file.states()` order; each `StateMeta.symbol` is the
    /// step name the caller keys `register_values` / reads `next_state` by.
    /// The Enumerate strategy's `state_meta` is the same order (both
    /// `file.states()`), so position `i` refers to the same cell.
    fn states(&self) -> &[StateMeta] {
        &self.states
    }

    /// Inputs in `file.inputs()` order; `StateMeta.symbol` is the step name.
    fn inputs(&self) -> &[StateMeta] {
        &self.inputs
    }

    fn step(
        &self,
        register_values: &std::collections::HashMap<String, u128>,
        input_values: &std::collections::HashMap<String, u128>,
        observe: &[String],
    ) -> Result<crate::adapter::sts_ir::StepOutcome, AdapterError> {
        let mut env = Env::default();
        for sv in &self.states {
            let bits = register_values.get(&sv.symbol).copied().unwrap_or(0);
            env.values
                .insert(sv.nid, Bv::from_u128_width(bits, sv.width));
        }
        for sv in &self.inputs {
            let bits = input_values.get(&sv.symbol).copied().unwrap_or(0);
            env.values
                .insert(sv.nid, Bv::from_u128_width(bits, sv.width));
        }

        // Evaluate the combinational cone; observability + admissibility
        // are read here (current cycle), BEFORE `apply_next` commits.
        evaluate_pure(self.file, &mut env, false)?;
        let admissible = constraints_hold(self.file, &env)?;

        let mut observed: std::collections::HashMap<String, u128> =
            std::collections::HashMap::new();
        for name in observe {
            // Phase 2a resolution: Op > Output(signal) > State/Input.
            let v = self
                .op_nid
                .get(name.as_str())
                .or_else(|| self.out_sig_nid.get(name.as_str()))
                .or_else(|| self.si_nid.get(name.as_str()))
                .and_then(|nid| env.values.get(nid))
                .map(|bv| bv.low128());
            if let Some(v) = v {
                observed.insert(name.clone(), v);
            }
        }

        // `apply_next` mutates `env` in place — no full-env clone (the
        // original Pass-1 cloned the evaluated env per step).
        apply_next(self.file, &mut env, &self.states)?;
        let mut next_state: std::collections::HashMap<String, u128> =
            std::collections::HashMap::new();
        for sv in &self.states {
            let bits = env.values.get(&sv.nid).map(|v| v.low128()).unwrap_or(0);
            next_state.insert(sv.symbol.clone(), bits);
        }

        Ok(crate::adapter::sts_ir::StepOutcome {
            next_state,
            observed,
            admissible,
        })
    }
}

/// R.5b lifter integration MVP — variant of [`simulate_one_step`]
/// that substitutes a UF zero-value for each Op NID in
/// `uf_wrapped_nids` during the per-step evaluation. Use
/// [`collect_uf_wrapped_nids`] to derive the NID set from a
/// sidecar's `uf_wrap` / `uf_unwrap` declarations.
///
/// The wrapped Ops' results propagate through downstream evaluations
/// (they're inserted into `env` like any other Op result), so any
/// computation that transitively reads a wrapped Op sees the
/// UF-stand-in. Sound may-side over-approximation per the schema
/// docs on `SvAnnotation::uf_wrap`.
pub fn simulate_one_step_with_uf(
    file: &Btor2File,
    register_values: &std::collections::HashMap<String, u128>,
    input_values: &std::collections::HashMap<String, u128>,
    uf_wrapped_nids: &std::collections::HashSet<Nid>,
) -> Result<std::collections::HashMap<String, u128>, AdapterError> {
    simulate_one_step_with_uf_rep(
        file,
        register_values,
        input_values,
        uf_wrapped_nids,
        UfRepresentative::Zero,
    )
}

/// R.5b multi-value enumeration variant — like
/// [`simulate_one_step_with_uf`] but lets the caller pick the
/// [`UfRepresentative`] each wrapped Op's output substitutes to.
/// Used by `predicate_cube_lift` to enumerate multiple UF
/// representatives per (cube, input combo), generating multiple
/// may-edges per starting cube — a tighter may-side approximation
/// than the zero-only MVP.
///
/// Returns the per-symbol next register values under the chosen
/// representative. Callers needing all representatives should
/// invoke this fn once per [`UfRepresentative`] variant.
pub fn simulate_one_step_with_uf_rep(
    file: &Btor2File,
    register_values: &std::collections::HashMap<String, u128>,
    input_values: &std::collections::HashMap<String, u128>,
    uf_wrapped_nids: &std::collections::HashSet<Nid>,
    rep: UfRepresentative,
) -> Result<std::collections::HashMap<String, u128>, AdapterError> {
    // Symbol-resolution mirror of simulate_one_step.
    let symbols = parser::collect_symbols(file);
    let mut symbol_to_input_nid: std::collections::HashMap<String, (Nid, u32)> =
        std::collections::HashMap::new();
    let mut state_meta: Vec<StateMeta> = Vec::new();

    for line in &file.lines {
        match &line.node {
            Node::State { sort, .. } => {
                let width = parser::bv_width(file, *sort).unwrap_or(0);
                let symbol = symbols
                    .get(&line.nid)
                    .cloned()
                    .unwrap_or_else(|| format!("st_n{}", line.nid));
                state_meta.push(StateMeta {
                    nid: line.nid,
                    width,
                    symbol,
                });
            }
            Node::Input { sort, .. } => {
                let width = parser::bv_width(file, *sort).unwrap_or(0);
                let symbol = symbols
                    .get(&line.nid)
                    .cloned()
                    .unwrap_or_else(|| format!("in_n{}", line.nid));
                symbol_to_input_nid.insert(symbol, (line.nid, width));
            }
            _ => {}
        }
    }

    let mut env = Env::default();
    for sm in &state_meta {
        let bits = register_values.get(&sm.symbol).copied().unwrap_or(0);
        env.values
            .insert(sm.nid, Bv::from_u128_width(bits, sm.width));
    }
    for (symbol, &(nid, width)) in &symbol_to_input_nid {
        let bits = input_values.get(symbol).copied().unwrap_or(0);
        env.values.insert(nid, Bv::from_u128_width(bits, width));
    }

    evaluate_pure_with_uf_rep(file, &mut env, false, Some(uf_wrapped_nids), rep)?;
    apply_next(file, &mut env, &state_meta)?;

    let mut out: std::collections::HashMap<String, u128> = std::collections::HashMap::new();
    for sm in &state_meta {
        let bits = env.values.get(&sm.nid).map(|v| v.low128()).unwrap_or(0);
        out.insert(sm.symbol.clone(), bits);
    }
    Ok(out)
}

/// R.5b lifter integration — collect the BTOR2 Op NIDs that should
/// be substituted with UF zero-values during evaluation, per:
///
/// 1. The **default UF policy** (per `docs/design/native-sv-abstraction.md`
///    §6.10): every `Op::Mul` regardless of width; every `Op::Add` /
///    `Op::Sub` whose result width is greater than [`UF_WIDE_ADD_SUB_THRESHOLD`]
///    (= 32 bits). Other heavy operators (`$div`, `$mod`, `$pow`)
///    are not in the Phase 1 bit-blaster's blastable set and would
///    have errored earlier; default-policy wrapping doesn't try to
///    handle them.
/// 2. The sidecar's `uf_wrap` (force-wrap) declarations — adds the
///    cell's NID to the wrap set even when the default policy
///    doesn't fire.
/// 3. The sidecar's `uf_unwrap` (force-concretize) declarations —
///    REMOVES a cell from the wrap set even when the default policy
///    or `uf_wrap` would have wrapped it. Identification is by
///    `Node::Op.symbol` (the Yosys alias name).
///
/// **Symbol resolution.** Both `uf_wrap` and `uf_unwrap` match on
/// `Op::Op.symbol` (the Yosys `uext _ _ 0 NAME` alias pattern). An
/// Op without a symbol can still be wrapped by the default policy
/// (its NID enters the set unconditionally) but cannot be referenced
/// from the sidecar.
///
/// **Returns** an empty set when no Op triggers wrapping AND the
/// sidecar's `uf_wrap` list is empty (covers the no-sidecar case
/// too — absent / malformed sidecar yields an empty annotation).
pub fn collect_uf_wrapped_nids(
    file: &Btor2File,
    options: &AdapterOptions,
) -> std::collections::HashSet<Nid> {
    use crate::adapter::btor2::ast::Op;
    use crate::adapter::systemverilog::annotation::SvAnnotation;

    let (wrap_names, unwrap_names): (
        std::collections::HashSet<String>,
        std::collections::HashSet<String>,
    ) = if let Some(json) = &options.sidecar_json
        && let Ok(ann) = serde_json::from_str::<SvAnnotation>(json)
    {
        (
            ann.uf_wrap.iter().cloned().collect(),
            ann.uf_unwrap.iter().cloned().collect(),
        )
    } else {
        (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        )
    };

    let mut out = std::collections::HashSet::new();
    for line in &file.lines {
        let Node::Op {
            sort, op, symbol, ..
        } = &line.node
        else {
            continue;
        };

        // uf_unwrap always wins — skip outright when the symbol is
        // in the unwrap set.
        if let Some(name) = symbol
            && unwrap_names.contains(name.as_str())
        {
            continue;
        }

        // Explicit uf_wrap declaration → always include.
        if let Some(name) = symbol
            && wrap_names.contains(name.as_str())
        {
            out.insert(line.nid);
            continue;
        }

        // Default policy (per §6.10):
        //   - Op::Mul always wrap.
        //   - Op::Add / Op::Sub wrap when result width > UF_WIDE_ADD_SUB_THRESHOLD.
        let width = parser::bv_width(file, *sort).unwrap_or(0);
        let default_policy_match = match op {
            Op::Mul => true,
            Op::Add | Op::Sub => width > UF_WIDE_ADD_SUB_THRESHOLD,
            _ => false,
        };
        if default_policy_match {
            out.insert(line.nid);
        }
    }
    out
}

/// R.5b default UF policy — width threshold above which `Op::Add` /
/// `Op::Sub` cells get auto-wrapped as uninterpreted functions. Per
/// `docs/design/native-sv-abstraction.md` §6.10. Below this width,
/// add/sub are cheap enough to evaluate concretely; above it, the
/// SMT predicate-image cost (when R.5b/R.5 eventually wire SMT)
/// starts to dominate.
pub const UF_WIDE_ADD_SUB_THRESHOLD: u32 = 32;

/// Top-level entry: convert a parsed BTOR2 file + options to an AdapterIR.
///
/// Returns a tuple of (IR, warnings, partition_summary). The third
/// element is `Some` whenever `enumerate_and_blast` ran the auto-COI
/// pass; the BTOR2 adapter forwards it onto `AdapterOutput`.
pub fn to_ir(
    file: &Btor2File,
    options: &AdapterOptions,
) -> Result<
    (
        AdapterIR,
        Vec<AdapterWarning>,
        Option<crate::adapter::partition::PartitionSummary>,
    ),
    AdapterError,
> {
    let mut warnings = Vec::new();

    // C1.3 (sidecar Phase 1) — warn for sidecar `signals[]` entries whose
    // name (or `drives` override) matches no state cell. Runs HERE, against
    // the original full `file` BEFORE the cone-slice rebinding below, so a
    // name that resolves in the whole circuit but is later sliced out of a
    // per-cluster cone does NOT warn — only genuine non-matches do.
    warn_unmatched_sidecar_signals(file, options, &mut warnings);

    // R46-1/R46-2 (R.4.6) — cone restriction by SLICING. When the caller
    // (or the per-cluster fallback) requests a cone restriction, replace
    // the design with the exact sub-circuit its property atoms depend on:
    // every out-of-cone state cell, its next/init, and any out-of-cone
    // input or output is removed from the BTOR2 entirely. The rest of
    // `to_ir` then bit-blasts a strictly smaller design. This SUPERSEDES
    // the earlier pin-to-`Ignored` mechanism (which was sound only for
    // safety and, because transitions are synchronous, dropped in-cone
    // updates whenever an out-of-cone cell changed). On a synchronous
    // system a cone closed over BOTH data-flow and constraint/fairness
    // co-occurrence is a strong bisimulation on the atom set, so slicing is
    // exact — sound for the full mu-calculus (`cone_slice` enforces the
    // constraint/fairness half of that closure).
    let cone_sliced;
    let file: &Btor2File = match &options.cone_restrict_atoms {
        Some(atoms) if !atoms.is_empty() => {
            cone_sliced = cone_slice(file, atoms);
            &cone_sliced
        }
        _ => file,
    };

    // §Phase 10 §10.2 stage 1 — detect array-typed state cells
    // (BTOR2 `state` lines whose sort is `Sort::Array`) and validate
    // them against the sidecar's `memories: [...]` declarations.
    // The detection runs BEFORE the is_blastable check so that
    // memory-bearing fixtures get an actionable error pointing at
    // the sidecar template they need to add, instead of a generic
    // "operator not supported" error from the downstream
    // Read/Write check.
    let memory_cells = detect_btor2_memories(file);

    // §Phase 10 §10.2 stage 1b — havoc-mode BTOR2 rewriting. When
    // the sidecar declares a memory with `abstraction: havoc`, we
    // rewrite the BTOR2 in-place before the is_blastable check:
    // - drop the array `State` line for the memory cell;
    // - drop Init/Next lines whose `state` operand is the memory;
    // - drop `Op::Write` lines whose first operand is the memory;
    // - rewrite each `Op::Read` from the memory to a fresh `Input`
    //   of the data-port width (keeping the same NID so downstream
    //   references resolve unchanged).
    //
    // The result is a memory-free BTOR2 the stage-1 bit-blaster can
    // already handle. Reads return nondeterministic values on every
    // cycle (the standard havoc abstraction); writes are silently
    // dropped (no abstract memory to update). Soundness: over-
    // approximates read behaviour ⇒ sound for safety, unsound for
    // liveness on the memory contents.
    //
    // Non-havoc memory abstractions (`uf`, `bit_blast`,
    // `bounded_bit_blast`) are left for stage 3/4 — the rewriter
    // simply passes the file through unchanged in those cases, and
    // the is_blastable check below produces the existing
    // "stage 3/4 not yet shipped" error.
    // §Phase 10 §10.2 stage 3.a (2026-06-12) — UF-mode recognition.
    // Computed once up-front so the is_blastable error path below
    // can prepend a stage-3.a-specific hint when the Read/Write
    // bail-out fires on a memory the sidecar declared with
    // `abstraction: uf`. Stage 3.a ships the recognition layer
    // only; the actual Z3 Array `select`/`store` encoding (stage
    // 3.b) and predicate-image query integration (stage 3.c) are
    // queued. The hint narrows the generic "stage 3/4 not yet
    // shipped" error to "your specific UF declaration is recognised;
    // stages 3.b/c track the actual lift."
    let uf_memory_names: std::collections::HashMap<Nid, String> = if memory_cells.is_empty() {
        std::collections::HashMap::new()
    } else {
        let uf_nids = sidecar_uf_memory_nids(&memory_cells, options);
        memory_cells
            .iter()
            .filter(|m| uf_nids.contains(&m.nid))
            .map(|m| (m.nid, m.name.clone()))
            .collect()
    };

    let mut rewritten_holder: Option<Btor2File> = None;
    if !memory_cells.is_empty() {
        validate_sidecar_memories(file, &memory_cells, options)?;
        let havoc_nids = sidecar_havoc_memory_nids(&memory_cells, options);
        if !havoc_nids.is_empty() {
            let havoc_names: Vec<&str> = memory_cells
                .iter()
                .filter(|m| havoc_nids.contains(&m.nid))
                .map(|m| m.name.as_str())
                .collect();
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "§Phase 10 §10.2 stage 1b: {} memor{} havoc-abstracted (reads return \
                     fresh nondeterministic values; writes/inits/nexts dropped). SOUND for \
                     safety (over-approximation of read behaviour); UNSOUND for liveness on \
                     memory contents (the concrete may reach states the abstract cannot). \
                     Memories: {}",
                    havoc_names.len(),
                    if havoc_names.len() == 1 { "y" } else { "ies" },
                    havoc_names.join(", "),
                ),
                location: None,
            });
            rewritten_holder = Some(havoc_rewrite_memories(file, &havoc_nids)?);
        }
    }
    let file: &Btor2File = rewritten_holder.as_ref().unwrap_or(file);

    // Reject unsupported operators up-front so users get a clear error
    // pointing to the BTOR2 line, not a confusing run-time panic.
    // §Phase 10 §10.2 stage 1 nuance: Read/Write operators on
    // memory cells will still trip this check until stage 3 (UF)
    // or stage 4 (bounded bit-blast) ships. The error message
    // includes the §Phase 10 hint so the user knows to wait for
    // (or contribute) the stage 3/4 implementation.
    for line in &file.lines {
        if let Node::Op { op, args, .. } = &line.node
            && !op.is_blastable()
        {
            // §Phase 10 §10.2 stage 3.a (2026-06-12) — when the
            // bail-out fires on a Read/Write whose array operand
            // (args[0]) was declared with `abstraction: uf`,
            // prepend a UF-specific hint that narrows the generic
            // stage-3/4 message to "your specific declaration is
            // recognised; stages 3.b/c track the actual lift."
            let uf_memory_hint = if matches!(op, Op::Read | Op::Write)
                && let Some(arr_operand) = args.first()
                && let Some(name) = uf_memory_names.get(&arr_operand.nid())
            {
                format!(
                    " (§Phase 10 §10.2 stage 3.a: memory `{name}` was declared with \
                     `abstraction: uf`; the recognition layer is shipped, but the \
                     Z3 Array `select`/`store` encoding [stage 3.b] and the \
                     predicate-image query integration [stage 3.c] are queued. \
                     Until stages 3.b + 3.c ship, switch to `abstraction: havoc` \
                     for a sound over-approximating safety verdict.)"
                )
            } else if matches!(op, Op::Read | Op::Write) {
                " (§Phase 10 §10.2 stage 1 has detected memory cells in this BTOR2 \
                 and validated the sidecar; the actual lift will succeed once \
                 stage 3 (UF mode) or stage 4 (bounded bit-blast) ships.)"
                    .to_string()
            } else {
                String::new()
            };
            return Err(AdapterError {
                kind: AdapterErrorKind::UnsupportedConstruct,
                message: format!(
                    "BTOR2 operator '{op:?}' at NID {} is not supported by the Phase 1 bit-blaster.{uf_memory_hint} \
                     Hand the BTOR2 to an external symbolic engine (Phase 3 hand-off) instead.",
                    line.nid
                ),
                location: Some(SourceLocation {
                    line: line.source_line,
                    column: 0,
                }),
            });
        }
    }

    let states: Vec<&Line> = file.states().collect();
    let inputs: Vec<&Line> = file.inputs().collect();

    // M.1 (§Phase 11 priority 2) — abstraction-aware bit accounting.
    // Cells declared `Ignored` in the sidecar contribute zero bits
    // to the state-space cap check (they collapse to a single
    // pinned value at lift time). Discovered while attempting M.1
    // on OpenTitan `uart_tx.sv`: an 11-bit shift register the
    // property doesn't reference would otherwise push the design
    // past `MAX_STATE_BITS = 20` despite the user's intent to
    // ignore it.
    //
    // The pre-existing `sum_widths` runs over raw BTOR2 widths
    // because cell_domains isn't populated yet at this point. We
    // run a lightweight sidecar pre-scan here to subtract Ignored
    // cells' widths before checking the cap. Strict additivity:
    // when no sidecar is provided OR no cells are Ignored, the
    // accounting is identical to the legacy behaviour.
    // Note: `file` is already the cone slice when a restriction is active
    // (see the top of `to_ir`), so the cap is naturally measured against
    // the restricted design. Per-cell EFFECTIVE bits (GAP-2): a
    // sidecar-concretized wide field counts ceil(log2(value-set)) bits,
    // not its raw width, so a property over a wide-but-concretized
    // register fits the cap (see `sidecar_effective_state_bits`).
    let total_state_bits = sidecar_effective_state_bits(file, &states, options)?;
    let total_input_bits = sum_widths(file, &inputs)?;

    if total_state_bits > MAX_STATE_BITS {
        // R46-2 (R.4.6 per-cluster verification) — the joint design busts
        // the cap. If the manifest threaded per-property COI seeds AND we
        // are not already inside a single-cone slice, try to partition the
        // properties by cone overlap and bit-blast each cluster sliced to
        // its own cone. When every cluster fits and there is more than
        // one, this yields K automata + a routing map ("joint busts cap,
        // clusters fit"). Otherwise fall through to the joint cap error.
        if options.cone_restrict_atoms.is_none()
            && let Some((ir, summary)) = try_per_cluster_blast(file, options, &mut warnings)?
        {
            return Ok((ir, warnings, summary));
        }
        return Err(AdapterError {
            kind: AdapterErrorKind::StateSpaceOverflow,
            message: format!(
                "BTOR2 design has {total_state_bits} state bits → 2^{total_state_bits} = {} states (max supported: 2^{MAX_STATE_BITS} = {}). \
                 Compose-and-decompose (Phase 3) or hand-off to an external symbolic engine.",
                1u64 << total_state_bits.min(63),
                1u64 << MAX_STATE_BITS,
            ),
            location: None,
        });
    }
    // Input-bit cap is checked AFTER sidecar resolution inside
    // `enumerate_and_blast` (per-input `FieldDomain` abstractions may
    // collapse a wide raw input into a 1-of-N value set — e.g. an
    // `Ignored` input contributes zero effective bits even when its
    // raw BTOR2 width is large). The raw-bit number is kept for
    // diagnostics only.
    let _ = total_input_bits;

    if total_state_bits >= 12 {
        warnings.push(AdapterWarning {
            kind: WarningKind::LargeStateSpace,
            message: format!(
                "BTOR2 design produces 2^{total_state_bits} = {} explicit states",
                1u64 << total_state_bits.min(63),
            ),
            location: None,
        });
    }

    let blasted = enumerate_and_blast(file, &states, &inputs, options, &mut warnings)?;

    let context_name = options
        .context_name
        .clone()
        .unwrap_or_else(|| "btor2_design".into());

    Ok((
        AdapterIR {
            metadata: Metadata {
                title: context_name,
                source_format: SourceFormat::Btor2,
                description: None,
                game_semantics: None,
                known_status: None,
            },
            signals: blasted.signals,
            automata: vec![blasted.automaton],
            compositions: vec![],
            properties: blasted.properties,
            controller: None,
        },
        warnings,
        blasted.partition_summary,
    ))
}

/// R46-2 (R.4.6 per-cluster verification) — the "joint busts cap,
/// clusters fit" fallback. Called from [`to_ir`] when the joint design
/// exceeds [`MAX_STATE_BITS`] AND the manifest threaded per-property COI
/// seeds (`AdapterOptions::property_seeds`).
///
/// Partitions the properties by cone overlap
/// ([`coi::cluster_properties_by_jaccard`], reusing the same atom→terminal
/// resolution as the clustered-COI telemetry), then bit-blasts each
/// cluster restricted to its own cone (R46-1's `cone_restrict_atoms`,
/// applied internally). Returns:
///
/// - `Some((ir, summary))` with one automaton per cluster (named
///   `Circuit__cl{k}`) and a property→automaton routing map on the
///   summary, when there is **more than one** cluster AND **every**
///   cluster fits the cap. This is the case the report's clustered-COI
///   reduction predicted.
/// - `None` when per-cluster cannot help — no seeds, a single cluster
///   (equivalent to the joint design), or some cluster still busts the
///   cap even restricted (it carries a wide field; see GAP-2 /
///   param-concretization). The caller then emits the joint cap error.
///
/// SOUNDNESS: each cluster's model is the joint design restricted (via
/// [`cone_slice`]) to that cluster's exact cone of influence. On the
/// synchronous BTOR2 system that cone is closed over both data-flow and
/// `constraint`/`fair`/`justice` coupling (`cone_slice` enforces the
/// latter), making the restriction a strong bisimulation on the cluster's
/// atoms: cutting out-of-cone cells cannot change any verdict over the
/// cluster's properties (CLAUDE.md §Soundness — COI is exact). Each
/// property lands in exactly one cluster (the clustering invariant), so
/// routing it to its cluster's model is sound and complete.
#[allow(clippy::type_complexity)]
fn try_per_cluster_blast(
    file: &Btor2File,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<
    Option<(
        AdapterIR,
        Option<crate::adapter::partition::PartitionSummary>,
    )>,
    AdapterError,
> {
    use crate::adapter::partition::{DepGraphBuilder, Partition, PartitionSummary, coi};
    use std::collections::{BTreeMap, HashSet};

    if options.property_seeds.is_empty() {
        return Ok(None);
    }

    let deps = file.build();
    const DEFAULT_CLUSTER_SIMILARITY_FLOOR: f64 = 0.5;
    let floor = options
        .cluster_similarity_floor
        .unwrap_or(DEFAULT_CLUSTER_SIMILARITY_FLOOR);

    // Resolve each property's raw atoms to its terminal symbols before
    // clustering (same resolution the cluster_coi telemetry uses — a
    // bare output atom is not a dep-graph key).
    let resolved: Vec<(String, HashSet<String>)> = options
        .property_seeds
        .iter()
        .map(|(name, atoms)| {
            let mut seeds = HashSet::new();
            for atom in atoms {
                match super::dep_graph::resolve_atom_to_terminals(file, atom) {
                    Some(terminals) => seeds.extend(terminals),
                    None => {
                        seeds.insert(atom.clone());
                    }
                }
            }
            (name.clone(), seeds)
        })
        .collect();

    let clusters = coi::cluster_properties_by_jaccard(&resolved, &deps, floor);
    if clusters.len() < 2 {
        // A single cluster spans every property → its cone is the joint
        // cone → no benefit over the joint design. Fall through.
        return Ok(None);
    }

    // property name → its raw atoms (the cone_restrict_atoms seed).
    let raw_by_name: std::collections::HashMap<&str, &Vec<String>> = options
        .property_seeds
        .iter()
        .map(|(n, a)| (n.as_str(), a))
        .collect();

    let mut automata = Vec::with_capacity(clusters.len());
    let mut signals: Vec<crate::adapter::ir::Signal> = Vec::new();
    let mut seen_signals: HashSet<String> = HashSet::new();
    let mut properties: Vec<crate::adapter::ir::PropertySpec> = Vec::new();
    let mut routing: BTreeMap<String, String> = BTreeMap::new();

    for (k, cluster) in clusters.iter().enumerate() {
        let aut_name = format!("Circuit__cl{k}");

        // This cluster's cone atoms = union of its members' raw property
        // atoms; `cone_slice` resolves them to terminals internally.
        let mut cone_atoms: Vec<String> = Vec::new();
        for member in &cluster.members {
            if let Some(atoms) = raw_by_name.get(member.as_str()) {
                cone_atoms.extend(atoms.iter().cloned());
            }
            routing.insert(member.clone(), aut_name.clone());
        }

        // Slice the BTOR2 to this cluster's cone — a sound, exact
        // sub-circuit (out-of-cone state cells AND the inputs that only
        // feed them are removed). The per-cluster blast calls
        // `enumerate_and_blast` directly on the slice (not `to_ir`), so it
        // cannot recurse into another per-cluster fallback.
        let sliced = cone_slice(file, &cone_atoms);
        let sliced_states: Vec<&Line> = sliced.states().collect();
        let sliced_inputs: Vec<&Line> = sliced.inputs().collect();

        // Cap-check this cluster's sliced state bits, EFFECTIVE (GAP-2):
        // a wide field inside the cluster that the sidecar concretizes
        // counts ceil(log2(value-set)) bits, so a cluster with a
        // param-concretized wide register fits. A cluster that STILL busts
        // the cap (an un-concretized wide field) returns None → the joint
        // error stands, so the user gets one clear diagnostic.
        let cluster_bits = sidecar_effective_state_bits(&sliced, &sliced_states, options)?;
        if cluster_bits > MAX_STATE_BITS {
            return Ok(None);
        }

        let mut blast =
            enumerate_and_blast(&sliced, &sliced_states, &sliced_inputs, options, warnings)?;
        blast.automaton.name = aut_name;
        automata.push(blast.automaton);
        for sig in blast.signals {
            if seen_signals.insert(sig.name.clone()) {
                signals.push(sig);
            }
        }
        properties.extend(blast.properties);
    }

    warnings.push(AdapterWarning {
        kind: WarningKind::LargeStateSpace,
        message: format!(
            "R.4.6 per-cluster verification: joint design exceeded the state-bit cap; \
             partitioned into {} clusters and bit-blasted each restricted to its own cone",
            automata.len()
        ),
        location: None,
    });

    // Telemetry + routing on the partition summary. The summary's
    // kept/dropped counts are not meaningful in per-cluster mode (there
    // are K partitions, not one), so they default to zero; the
    // cluster_coi report carries the cone sizes and cluster_routing
    // drives the orchestrator's per-property routing.
    let mut summary = PartitionSummary::from_partition(&Partition::default(), None);
    summary.cluster_coi = Some(coi::cluster_coi_report(&resolved, &deps, floor));
    summary.cluster_routing = Some(routing);

    let context_name = options
        .context_name
        .clone()
        .unwrap_or_else(|| "btor2_design".into());

    let ir = AdapterIR {
        metadata: Metadata {
            title: context_name,
            source_format: SourceFormat::Btor2,
            description: None,
            game_semantics: None,
            known_status: None,
        },
        signals,
        automata,
        compositions: vec![],
        properties,
        controller: None,
    };

    Ok(Some((ir, Some(summary))))
}

/// R46-1/R46-2 (R.4.6) — slice a BTOR2 file to the cone of influence of
/// `atoms`: keep only the lines transitively reachable from the atoms'
/// defining expressions, the state cells they read, those cells'
/// init/next, and so on. Out-of-cone state cells, their init/next, the
/// inputs that only feed them, and the other clusters' outputs are
/// removed from the IR entirely.
///
/// The result is the exact sub-circuit the cluster's properties depend
/// on. On a **synchronous** transition system (BTOR2) a correctly closed
/// cone is a strong bisimulation on the atom set, so every mu-calculus
/// verdict over `atoms` — including the single-step `[]` / `<>` modalities
/// and nested fixpoints — agrees between the slice and the full design,
/// sound at every alternation depth, not just for safety (this is why
/// slicing supersedes the pin-to-`Ignored` mechanism).
///
/// "Correctly closed" is the load-bearing precondition: the cone must be
/// closed not only under the data-flow (`next` / `init`) dependency
/// relation but also under `constraint` / `fair` / `justice` co-occurrence
/// — a line that couples an in-cone signal to an out-of-cone one keeps the
/// latter in the cone. The closure loop below enforces both; without the
/// constraint/fairness half the slice silently drops assumptions the joint
/// bit-blaster enforces and degrades to an unsound over-approximation.
///
/// Sort lines are always kept (cheap, shared); NIDs are preserved so
/// surviving references stay valid without renumbering.
fn cone_slice(file: &Btor2File, atoms: &[String]) -> Btor2File {
    use std::collections::HashSet;

    let mut keep: HashSet<Nid> = HashSet::new();
    let mut work: Vec<Nid> = Vec::new();

    // Seed from each atom's defining line (output / op / state / input
    // whose symbol matches the atom).
    for line in &file.lines {
        let sym = match &line.node {
            Node::Output {
                symbol: Some(s), ..
            }
            | Node::Op {
                symbol: Some(s), ..
            }
            | Node::State {
                symbol: Some(s), ..
            }
            | Node::Input {
                symbol: Some(s), ..
            } => Some(s.as_str()),
            _ => None,
        };
        if let Some(s) = sym
            && atoms.iter().any(|a| a == s)
        {
            work.push(line.nid);
        }
    }

    // Transitive closure over operands, interleaved with constraint /
    // fairness pullback, iterated to a joint fixpoint.
    //
    // The operand closure keeps everything that FEEDS the cone: keeping a
    // State cell pulls in its Init/Next lines (whose value fan-in is then
    // closed over) — that is how the cone walks backwards through the
    // transition relation.
    //
    // But the data-flow closure alone is NOT the full cone of influence.
    // BTOR2 `constraint` / `fair` / `justice` lines couple signals into the
    // cone that no `next` reads: a `constraint` mentioning an in-cone signal
    // RESTRICTS the reachable state space (the joint bit-blaster enforces it
    // via `constraints_hold`), and a `fair` / `justice` line referencing an
    // in-cone signal shapes which infinite paths count. Their other operands
    // are therefore in the true cone of influence and MUST be retained.
    // Dropping such a line silently removes an assumption / fairness
    // obligation, turning the "exact" slice into an over-approximation — a
    // spurious counterexample for safety, an unsound verdict for liveness.
    // This pullback is load-bearing for the "exact / bisimilar / full
    // mu-calculus" guarantee in this function's doc comment; removing it
    // reintroduces the soundness bug.
    //
    // We therefore loop: drain the operand-closure frontier, then pull in
    // every constraint / fairness line whose operand DAG touches the current
    // cone (and all signals it references), until neither step grows `keep`.
    // `keep` only ever grows and is bounded by the line count, so this
    // terminates. Constraints/fairness disjoint from the cone are correctly
    // left out — they cannot restrict any in-cone signal.
    loop {
        while let Some(nid) = work.pop() {
            if !keep.insert(nid) {
                continue;
            }
            let Some(line) = file.lookup(nid) else {
                continue;
            };
            push_operand_nids(&line.node, &mut work);
            if matches!(line.node, Node::State { .. }) {
                for l in &file.lines {
                    match &l.node {
                        Node::Init { state, .. } | Node::Next { state, .. } if *state == nid => {
                            work.push(l.nid);
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut grew = false;
        for line in &file.lines {
            if keep.contains(&line.nid) {
                continue;
            }
            let operands: Vec<Nid> = match &line.node {
                Node::Constraint { signal } | Node::Fair { signal } => vec![signal.nid()],
                Node::Justice { signals } => signals.iter().map(|s| s.nid()).collect(),
                _ => continue,
            };
            if operands.iter().any(|&op| dag_touches_keep(file, op, &keep)) {
                work.push(line.nid);
                work.extend(operands);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    // Forward-observation pass (fixpoint). The backward closure above keeps
    // what FEEDS the cone, but the bit-blaster also needs the named
    // combinational OBSERVATIONS computed FROM the cone: Yosys emits a
    // register's name on a `uext` alias line (`<nid> uext W <reg> 0 cnt_x`)
    // that `collect_symbols` traces to attach the symbol to the state cell
    // — without it the cell stays anonymous and emits no predicate. We
    // therefore additionally keep any symbol-bearing `Op` and any
    // property/output line whose operands are ALL already in-cone, iterating
    // to a fixpoint so chained aliases survive. This keeps the other
    // clusters' outputs out (their operands are out-of-cone) so no dangling
    // reference can result.
    loop {
        let mut added = false;
        for line in &file.lines {
            if keep.contains(&line.nid) {
                continue;
            }
            let refs: Vec<Nid> = match &line.node {
                Node::Op {
                    symbol: Some(_),
                    args,
                    ..
                } => args.iter().map(|a| a.nid()).collect(),
                Node::Output { signal, .. }
                | Node::Bad { signal }
                | Node::Constraint { signal }
                | Node::Fair { signal } => vec![signal.nid()],
                Node::Justice { signals } => signals.iter().map(|s| s.nid()).collect(),
                _ => continue,
            };
            if !refs.is_empty() && refs.iter().all(|n| keep.contains(n)) {
                keep.insert(line.nid);
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    let new_lines: Vec<Line> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::Sort { .. }) || keep.contains(&l.nid))
        .cloned()
        .collect();
    let by_nid = new_lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.nid, i))
        .collect();
    Btor2File {
        lines: new_lines,
        by_nid,
    }
}

/// Push the operand NIDs a node references onto `work` (for the
/// [`cone_slice`] transitive closure). Sort references are omitted —
/// `cone_slice` keeps every Sort line unconditionally.
fn push_operand_nids(node: &Node, work: &mut Vec<Nid>) {
    match node {
        Node::Op { args, .. } => work.extend(args.iter().map(|a| a.nid())),
        Node::Init { state, value, .. } | Node::Next { state, value, .. } => {
            work.push(*state);
            work.push(value.nid());
        }
        Node::Bad { signal }
        | Node::Constraint { signal }
        | Node::Fair { signal }
        | Node::Output { signal, .. } => work.push(signal.nid()),
        Node::Justice { signals } => work.extend(signals.iter().map(|s| s.nid())),
        Node::State { .. } | Node::Input { .. } | Node::Const { .. } | Node::Sort { .. } => {}
    }
}

/// Does the operand DAG rooted at `start` reach any NID already in `keep`?
///
/// Used by [`cone_slice`]'s constraint / fairness pullback to decide
/// whether a `constraint` / `fair` / `justice` line couples the current
/// cone to additional signals (and so must be retained, pulling its
/// operands in). Walks operands only; membership of *any* reached node —
/// terminal or intermediate — short-circuits to `true`. A `visited` set
/// guards against cycles (BTOR2 is acyclic, but the guard is cheap).
fn dag_touches_keep(file: &Btor2File, start: Nid, keep: &std::collections::HashSet<Nid>) -> bool {
    let mut stack = vec![start];
    let mut visited = std::collections::HashSet::new();
    while let Some(nid) = stack.pop() {
        if !visited.insert(nid) {
            continue;
        }
        if keep.contains(&nid) {
            return true;
        }
        if let Some(line) = file.lookup(nid) {
            push_operand_nids(&line.node, &mut stack);
        }
    }
    false
}

fn sum_widths(file: &Btor2File, lines: &[&Line]) -> Result<u32, AdapterError> {
    let mut total: u32 = 0;
    for line in lines {
        let sort_nid = match &line.node {
            Node::Input { sort, .. } | Node::State { sort, .. } => *sort,
            _ => continue,
        };
        // §Phase 10 §10.2 stage 1: skip array-sorted state cells in
        // the bit-width accounting. Memory abstraction (UF / Havoc /
        // bounded bit-blast) handles them separately via the
        // sidecar's `memories` declarations. Stage 1 today still
        // errors at the Read/Write op check (the actual lift fails),
        // but the state-bit accounting must not double-error here.
        if matches!(
            sort_of_arc(file, sort_nid),
            Some(crate::adapter::btor2::ast::Sort::Array { .. })
        ) {
            continue;
        }
        let width = parser::bv_width(file, sort_nid).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "input/state NID {} references non-bitvec sort {sort_nid} (arrays not yet supported)",
                line.nid
            ),
            location: Some(SourceLocation {
                line: line.source_line,
                column: 0,
            }),
        })?;
        total = total.checked_add(width).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::StateSpaceOverflow,
            message: "total bit-width exceeds u32 range".into(),
            location: None,
        })?;
    }
    Ok(total)
}

/// M.1 (§Phase 11) — sum the raw bit-widths of state cells whose
/// sidecar entry declares `abstraction: ignored`. Subtracted from
/// the cap-check total so wide registers the user explicitly opted
/// to ignore don't push the design past `MAX_STATE_BITS`.
///
/// Returns 0 when no sidecar is provided, the sidecar fails to
/// parse, or no cells are declared Ignored. Strict additivity:
/// legacy behaviour is preserved when this helper returns 0.
///
/// **M.1 Path B (§Phase 11):** resolves each ignored signal via the
/// `drives` override (falling back to the sidecar `name`) using
/// [`parser::resolve_state_by_symbol`], so sidecar entries match
/// state cells even when Yosys strips the original register symbol
/// from the `state` line and only attaches it via an Op-alias or
/// `output` port symbol.
/// R46-6 / GAP-2 — effective state-bit count for the cap check. A state
/// cell the sidecar param-concretizes (`Boolean` / `BoundedCounter` /
/// `EnumValues` / `Ignored`) contributes `ceil(log2(value-set size))`
/// bits — the number of distinct values the bit-blaster actually
/// enumerates for it — instead of its full register width. Cells with no
/// sidecar entry contribute their raw width (the legacy behaviour, so
/// no-sidecar designs are unchanged).
///
/// This is what lets a property over a WIDE field (a 32-bit timer, a
/// 64-bit data word) fit the cap once that field is concretized to a
/// small value set, instead of being rejected on raw width before
/// enumeration ever runs — the GAP-2 enabler for per-cluster verification
/// of real RTL whose clusters carry wide datapath/counters.
///
/// Subsumes the old `sidecar_ignored_state_bits`: an `Ignored` cell has a
/// one-element value set → 0 effective bits.
///
/// SOUNDNESS: the bit-blaster enumerates exactly the abstraction's value
/// set (see `CellEnumeration::values_for_field_domain`), so
/// `ceil(log2(|values|))` is the true per-cell contribution to the
/// explicit state space; the cap then bounds the actual enumerated size,
/// not an over-conservative raw-width proxy. Strictly more permissive
/// than the raw-width cap and never accepts a design whose enumeration
/// would exceed `2^MAX_STATE_BITS`.
fn sidecar_effective_state_bits(
    file: &Btor2File,
    states: &[&Line],
    options: &AdapterOptions,
) -> Result<u32, AdapterError> {
    let value_counts = sidecar_state_value_counts(file, options);
    let mut total: u32 = 0;
    for line in states {
        let sort_nid = match &line.node {
            Node::State { sort, .. } => *sort,
            _ => continue,
        };
        // §Phase 10 — array-sorted state cells are handled by memory
        // abstraction, not the bit cap; skip (mirrors `sum_widths`).
        if matches!(
            sort_of_arc(file, sort_nid),
            Some(crate::adapter::btor2::ast::Sort::Array { .. })
        ) {
            continue;
        }
        let raw_width = parser::bv_width(file, sort_nid).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "state NID {} references non-bitvec sort {sort_nid}",
                line.nid
            ),
            location: None,
        })?;
        let eff = match value_counts.get(&line.nid) {
            Some(count) => effective_bits(*count),
            None => raw_width,
        };
        total = total.checked_add(eff).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::StateSpaceOverflow,
            message: "total effective bit-width exceeds u32 range".into(),
            location: None,
        })?;
    }
    Ok(total)
}

/// Per-state-cell enumerated value count from the sidecar's declared
/// abstraction (NID → `|value set|`). Only sidecar-declared cells appear;
/// the caller treats absent cells as full-width bit-blast. Each signal is
/// resolved via the `drives` override (falling back to `name`) like the
/// other sidecar helpers, then through the canonical
/// [`crate::adapter::sidecar::resolve_to_field_domain`] resolver.
fn sidecar_state_value_counts(
    file: &Btor2File,
    options: &AdapterOptions,
) -> std::collections::HashMap<Nid, usize> {
    let mut out = std::collections::HashMap::new();
    let Some(json) = &options.sidecar_json else {
        return out;
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return out;
    };
    for sig in &ann.signals {
        let target = sig.drives.as_deref().unwrap_or(sig.name.as_str());
        if let Some(nid) = parser::resolve_state_by_symbol(file, target) {
            let (fd, _vm) = crate::adapter::sidecar::resolve_to_field_domain(sig, &ann);
            // `values()` is empty for `Ignored` → one pinned value.
            out.insert(nid, fd.values().len().max(1));
        }
    }
    out
}

/// `ceil(log2(value_count))` — the bits needed to index a
/// `value_count`-element value set. 0 for a singleton (a pinned /
/// `Ignored` cell).
fn effective_bits(value_count: usize) -> u32 {
    if value_count <= 1 {
        0
    } else {
        (value_count as u64).next_power_of_two().trailing_zeros()
    }
}

/// M.1 Path B (§Phase 11) — build an augmented NID→sidecar-name map
/// for the four downstream helpers that today look up sidecar names
/// via `symbols.get(nid)`. For each sidecar signal entry, resolve
/// its driver (`drives` override > sidecar `name`) via
/// [`parser::resolve_state_by_symbol`]; when that finds a state NID,
/// override the entry in the map with the sidecar's `name` (so the
/// downstream consumers see the user's name regardless of which
/// symbol Yosys happens to have attached to the state cell).
///
/// **Strict additivity.** Cells with no sidecar entry retain
/// whatever Yosys-attached symbol was already in `base_symbols`.
/// Cells whose sidecar entry resolves to the same NID Yosys already
/// named identically see no change. Only cells where the sidecar's
/// `drives` (or `name`, when `drives` is None) resolves the cell to
/// a sidecar entry whose `name` differs from Yosys's symbol — that
/// is the Path B-shaped overlap — get their entry rewritten.
fn enrich_symbols_with_sidecar_drives(
    file: &Btor2File,
    base_symbols: &std::collections::HashMap<Nid, String>,
    options: &AdapterOptions,
) -> std::collections::HashMap<Nid, String> {
    let mut out = base_symbols.clone();
    let Some(json) = &options.sidecar_json else {
        return out;
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return out;
    };
    for sig in &ann.signals {
        let target = sig.drives.as_deref().unwrap_or(sig.name.as_str());
        if let Some(state_nid) = parser::resolve_state_by_symbol(file, target) {
            out.insert(state_nid, sig.name.clone());
        }
    }
    out
}

/// C1.3 (sidecar Phase 1) — warn for each sidecar `signals[]` entry
/// whose resolved target (the `drives` override, falling back to
/// `name`) matches no state cell in the BTOR2.
///
/// Such an entry is a silent no-op today:
/// [`enrich_symbols_with_sidecar_drives`],
/// [`sidecar_state_value_counts`], and the `sidecar_effective_state_bits`
/// accounting all skip names that [`parser::resolve_state_by_symbol`]
/// cannot resolve. So a mistyped dotted name — the common error when
/// hand-editing a `mununu sv discover` skeleton — declares an
/// abstraction that never takes effect, and the lift proceeds as if the
/// entry were absent. The warning names the offending entry and points
/// the user back at `mununu sv discover` for the real post-flatten cell
/// names.
///
/// Absent / unparseable sidecar JSON is skipped silently here — the
/// load-time lint ([`crate::adapter::systemverilog::annotation::lint_annotation_json`])
/// owns malformed-sidecar diagnostics; this pass only catches
/// well-formed entries that point at nothing.
fn warn_unmatched_sidecar_signals(
    file: &Btor2File,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) {
    let Some(json) = &options.sidecar_json else {
        return;
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return;
    };
    for sig in &ann.signals {
        let target = sig.drives.as_deref().unwrap_or(sig.name.as_str());
        if parser::resolve_state_by_symbol(file, target).is_some() {
            continue;
        }
        let drives_note = if sig.drives.is_some() {
            format!(" (drives = \"{target}\")")
        } else {
            String::new()
        };
        warnings.push(AdapterWarning {
            kind: WarningKind::UnsupportedConstruct,
            message: format!(
                "sidecar signal \"{}\"{} matched no state cell in the BTOR2 — its declared \
                 abstraction has no effect. Check the name against the design; \
                 `mununu sv discover` prints the real post-flatten cell names (dotted per \
                 instance, e.g. `u_inst.reg_q`).",
                sig.name, drives_note
            ),
            location: None,
        });
    }
}

/// §Phase 10 §10.2 stage 1 — metadata for one memory state cell
/// detected in the BTOR2 source. The `name` is the user-visible
/// symbol (from `parser::collect_symbols`), used to cross-reference
/// against the sidecar's `memories[]` declarations.
#[derive(Debug, Clone)]
pub(crate) struct MemoryCellMeta {
    /// BTOR2 NID of the `state` line. Read by the exact-symbolic array/$mem
    /// lift (`symbolic_bitblast::exact_symbolic_verdict_with_witness`) to test
    /// each memory's cone membership before auto-havocing out-of-cone memories,
    /// and by the §Phase 10 §10.2 sidecar lifter for predicate-image queries.
    pub(crate) nid: Nid,
    name: String,
    address_width: u32,
    data_width: u32,
    source_line: usize,
}

/// §Phase 10 §10.2 stage 1 — walk the BTOR2 file for `state` lines
/// whose sort is `Sort::Array`, resolve their address/data widths
/// via the array sort's index/element references, and return one
/// [`MemoryCellMeta`] per detected memory.
///
/// Returns an empty vec when no array state cells exist (the common
/// case for FSM-only fixtures). Memory cells without a resolvable
/// symbol get a synthetic `nid_<n>` name so the sidecar validator
/// can still emit an actionable error pointing at the BTOR2 line.
pub(crate) fn detect_btor2_memories(file: &Btor2File) -> Vec<MemoryCellMeta> {
    let symbols = parser::collect_symbols(file);
    let mut out = Vec::new();
    for line in &file.lines {
        let Node::State { sort, .. } = &line.node else {
            continue;
        };
        let Some(crate::adapter::btor2::ast::Sort::Array { index, element }) =
            sort_of_arc(file, *sort)
        else {
            continue;
        };
        let Some(address_width) = parser::bv_width(file, index) else {
            continue;
        };
        let Some(data_width) = parser::bv_width(file, element) else {
            continue;
        };
        let name = symbols
            .get(&line.nid)
            .cloned()
            .unwrap_or_else(|| format!("nid_{}", line.nid));
        out.push(MemoryCellMeta {
            nid: line.nid,
            name,
            address_width,
            data_width,
            source_line: line.source_line,
        });
    }
    out
}

/// F1 — metadata for a state register with a provably SMALL set of reachable
/// CONSTANT values (an ENUM). A candidate for enum re-encoding: `W` bits →
/// `⌈log₂K⌉` bits, a bijection on the reachable states ⇒ verdict-preserving.
/// One-hot is the special case where every value has popcount ≤ 1; the detection
/// and re-encode are value-set-agnostic (the induction below never used one-hot-ness),
/// so a binary-encoded FSM (`{0, 3, 5}`) is compressed the same way. Consumed by
/// [`enum_reencode`]; the detection itself is read-only.
#[derive(Debug, Clone)]
pub(crate) struct EnumStateMeta {
    /// BTOR2 NID of the `state` line.
    pub nid: Nid,
    /// Bit-vector width of the register — the bits an enum re-encode compresses to
    /// `⌈log₂ values.len()⌉`.
    pub width: u32,
    /// The distinct constant values the register can hold (its enum domain),
    /// ascending; always includes the init value.
    pub values: Vec<u64>,
}

/// The `init` value operand of a state cell (`None` if the state has no `init` line).
fn init_value_of(file: &Btor2File, state_nid: Nid) -> Option<crate::adapter::btor2::ast::Operand> {
    file.lines.iter().find_map(|l| match &l.node {
        Node::Init { state, value, .. } if *state == state_nid => Some(*value),
        _ => None,
    })
}

/// F1 detection — state registers provably restricted to a SMALL set of CONSTANT
/// values (an ENUM). SOUND syntactic sufficient condition, by induction on the run:
/// `init` resolves to a constant (base case), and every value the `next` logic can
/// WRITE is a constant or a hold. We walk only the next-state VALUE path (the `ite`
/// then/else chain, NOT the conditions) — every value node must be a same-width `Const`,
/// the register itself (a hold), or another `ite`. Anything else on the value path
/// (an `add`/`or`/`shift`, or a copy of a different register) could yield an
/// out-of-set value, so the register is conservatively EXCLUDED. Guards may freely
/// reference other registers (they are not on the value path). Since a `next` that is
/// a mux-tree of {constants, hold} can only ever select a value already in the set, the
/// register stays in `values` forever ⇒ re-encoding `values → 0..K-1` is a bijection on
/// reachable states. One-hot is the special case where every value has popcount ≤ 1; the
/// proof and re-encode never depend on that, so binary-encoded FSMs are handled the same.
/// Only registers where the enum re-encode actually SAVES bits (`⌈log₂K⌉ < W`) are
/// reported. Read-only; the re-encode consumes this.
pub(crate) fn detect_enum_states(file: &Btor2File) -> Vec<EnumStateMeta> {
    use crate::adapter::btor2::ast::Op;
    let mut out = Vec::new();
    for line in &file.lines {
        let Node::State { sort, .. } = &line.node else {
            continue;
        };
        // bitvec sort only, and wide enough that an enum re-encode saves bits.
        let Some(width) = parser::bv_width(file, *sort) else {
            continue;
        };
        if width < 3 {
            continue;
        }
        let state_nid = line.nid;
        // init must resolve to a constant (an un-init / non-constant init could power
        // up an out-of-set value — conservatively skip). Its popcount is irrelevant:
        // any constant is a valid enum member.
        let Some(init_v) =
            init_value_of(file, state_nid).and_then(|op| resolve_btor2_constant(file, op.nid()))
        else {
            continue;
        };
        let Some(next) = parser::find_next_value_operand(file, state_nid) else {
            continue;
        };
        let mut values: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        values.insert(init_v);
        let mut ok = true;
        let mut visited: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        let mut stack = vec![next.nid()];
        while let Some(nid) = stack.pop() {
            if !visited.insert(nid) {
                continue;
            }
            let Some(n) = file.lookup(nid) else {
                ok = false;
                break;
            };
            match &n.node {
                // A constant leaf on the value path — any value, any popcount, is a
                // valid enum member.
                Node::Const { sort, .. } if parser::bv_width(file, *sort) == Some(width) => {
                    let Some(v) = resolve_btor2_constant(file, nid) else {
                        ok = false;
                        break;
                    };
                    values.insert(v);
                }
                // A hold (the register feeding its own next).
                Node::State { .. } if nid == state_nid => {}
                // A mux selecting among values — recurse into THEN/ELSE only (args[1],
                // args[2]); the condition (args[0]) is guard logic, not a written value.
                Node::Op { op, args, sort, .. }
                    if parser::bv_width(file, *sort) == Some(width)
                        && *op == Op::Ite
                        && args.len() >= 3 =>
                {
                    stack.push(args[1].nid());
                    stack.push(args[2].nid());
                }
                // Any other same-width value on the path ⇒ not provably a fixed enum set.
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        // Only report when the re-encode SAVES bits: ⌈log₂K⌉ < W. (For one-hot this is
        // always true; for a general enum a register with 2^W distinct values would not
        // compress, so gate on it explicitly.)
        let k = values.len() as u32;
        let enc_bits = (u32::BITS - (k - 1).leading_zeros()).max(1);
        if ok && k >= 2 && enc_bits < width {
            out.push(EnumStateMeta {
                nid: state_nid,
                width,
                values: values.into_iter().collect(),
            });
        }
    }
    out
}

/// Does `node` reference `target` as an operand?
fn node_refs_nid(node: &Node, target: Nid) -> bool {
    match node {
        Node::Op { args, .. } => args.iter().any(|a| a.nid() == target),
        Node::Init { state, value, .. } | Node::Next { state, value, .. } => {
            *state == target || value.nid() == target
        }
        Node::Bad { signal }
        | Node::Constraint { signal }
        | Node::Fair { signal }
        | Node::Output { signal, .. } => signal.nid() == target,
        Node::Justice { signals } => signals.iter().any(|s| s.nid() == target),
        _ => false,
    }
}

/// F1 re-encode — replace an enum register `meta` (`W` bits, `K` reachable constant
/// values) with an ENUM register (`E = ⌈log₂K⌉` bits) that maps each value `v_i → i`.
/// A BIJECTION on the reachable states ⇒ VERDICT-PRESERVING. Value-set-agnostic: it
/// never assumes the values are one-hot. Rewrites: the state's sort; its init
/// (value → index); its next-state VALUE PATH (const leaves → index consts, `ite`
/// sorts → `E`); and every READ — `eq/neq(S, v)` → `eq/neq(S, index v)`, and
/// `redor(S)` (is-any-bit-set = `S != 0`) → `neq(S, index-of-value-0)` — which is
/// correct for any enum whose set contains 0, and ABSTAINS when it does not (via
/// `const_for(0)?`, since `redor` would then be a constant that is not re-expressible as
/// an index compare). ABSTAINS (`None`) on ANY other use of `S` (slice, arithmetic,
/// output, a compare to a value not in the set) so a re-encode is only ever applied when
/// provably sound. The register keeps its NID; only new sort/const nodes are added (fresh
/// NIDs), so the result composes with re-encoding other registers.
pub(crate) fn enum_reencode(file: &Btor2File, meta: &EnumStateMeta) -> Option<Btor2File> {
    use crate::adapter::btor2::ast::{ConstValue, Op, Sort};
    let s = meta.nid;
    let k = meta.values.len() as u32;
    let e: u32 = (u32::BITS - (k - 1).leading_zeros()).max(1);
    let index_of =
        |v: u64| -> Option<u64> { meta.values.iter().position(|&x| x == v).map(|p| p as u64) };

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut prelude: Vec<Line> = Vec::new();
    // E-bit sort (reuse an existing bitvec-E sort, else create one).
    let e_sort = file
        .lines
        .iter()
        .find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width },
            } if *width == e => Some(l.nid),
            _ => None,
        })
        .unwrap_or_else(|| {
            let n = next_nid;
            next_nid += 1;
            prelude.push(Line {
                nid: n,
                node: Node::Sort {
                    sort: Sort::BitVec { width: e },
                },
                immediates: Vec::new(),
                source_line: 0,
            });
            n
        });
    // One index const per value (E-bit).
    let mut idx_const: std::collections::HashMap<u64, Nid> = std::collections::HashMap::new();
    for i in 0..meta.values.len() as u64 {
        let n = next_nid;
        next_nid += 1;
        prelude.push(Line {
            nid: n,
            node: Node::Const {
                sort: e_sort,
                value: ConstValue::Dec(i as i128),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        idx_const.insert(i, n);
    }
    let const_for = |v: u64| -> Option<Operand> {
        index_of(v)
            .and_then(|i| idx_const.get(&i))
            .map(|n| Operand(*n))
    };

    // The value-path node set (the `ite`/const nodes feeding S's next).
    let next_op = parser::find_next_value_operand(file, s)?;
    let mut value_path: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    {
        let mut vis: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        let mut st = vec![next_op.nid()];
        while let Some(nid) = st.pop() {
            if !vis.insert(nid) {
                continue;
            }
            let n = file.lookup(nid)?;
            match &n.node {
                Node::Const { sort, .. } if parser::bv_width(file, *sort) == Some(meta.width) => {
                    value_path.insert(nid);
                }
                Node::State { .. } if nid == s => {}
                Node::Op {
                    op: Op::Ite,
                    args,
                    sort,
                    ..
                } if parser::bv_width(file, *sort) == Some(meta.width) => {
                    value_path.insert(nid);
                    if args.len() >= 3 {
                        st.push(args[1].nid());
                        st.push(args[2].nid());
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
    }

    // Rewrite pass.
    let mut out: Vec<Line> = Vec::with_capacity(file.lines.len() + prelude.len());
    out.extend(prelude);
    for line in &file.lines {
        let nid = line.nid;
        let node = match &line.node {
            Node::State { symbol, .. } if nid == s => Node::State {
                sort: e_sort,
                symbol: symbol.clone(),
            },
            Node::Init { state, value, .. } if *state == s => Node::Init {
                sort: e_sort,
                state: s,
                value: const_for(resolve_btor2_constant(file, value.nid())?)?,
            },
            Node::Next { state, value, .. } if *state == s => Node::Next {
                sort: e_sort,
                state: s,
                value: *value,
            },
            // A value-path `ite`: sort → E, const operands → index consts (a hold or a
            // nested value-path ite keeps its NID — resolved to None by const-lookup).
            Node::Op {
                op: Op::Ite,
                args,
                symbol,
                ..
            } if value_path.contains(&nid) => {
                if args.len() < 3 {
                    return None;
                }
                let rewire = |o: &Operand| -> Option<Operand> {
                    match resolve_btor2_constant(file, o.nid()) {
                        Some(v) => const_for(v),
                        None => Some(*o),
                    }
                };
                Node::Op {
                    op: Op::Ite,
                    sort: e_sort,
                    args: vec![args[0], rewire(&args[1])?, rewire(&args[2])?],
                    symbol: symbol.clone(),
                }
            }
            // A read `eq/neq(S, onehot const)` → `eq/neq(S, index const)`.
            Node::Op {
                op: op @ (Op::Eq | Op::Neq),
                args,
                symbol,
                sort,
            } if args.iter().any(|a| a.nid() == s) => {
                if args.len() != 2 {
                    return None;
                }
                let (spos, other) = if args[0].nid() == s {
                    (0, args[1])
                } else {
                    (1, args[0])
                };
                let idx = const_for(resolve_btor2_constant(file, other.nid())?)?;
                let new_args = if spos == 0 {
                    vec![Operand(s), idx]
                } else {
                    vec![idx, Operand(s)]
                };
                Node::Op {
                    op: *op,
                    sort: *sort,
                    args: new_args,
                    symbol: symbol.clone(),
                }
            }
            // `redor(S)` (is any bit set) → `neq(S, index 0)` (is non-idle).
            Node::Op {
                op: Op::Redor,
                args,
                symbol,
                sort,
            } if args.len() == 1 && args[0].nid() == s => Node::Op {
                op: Op::Neq,
                sort: *sort,
                args: vec![Operand(s), const_for(0)?],
                symbol: symbol.clone(),
            },
            // Any OTHER reference to S (not a value-path node) ⇒ abstain.
            other if node_refs_nid(other, s) => return None,
            other => other.clone(),
        };
        out.push(Line {
            nid,
            node,
            immediates: line.immediates.clone(),
            source_line: line.source_line,
        });
    }
    let by_nid = out.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    Some(Btor2File { lines: out, by_nid })
}

/// b1 — metadata for a counter register the cube path can collapse to a single
/// `cnt == threshold` bit via a **may-abstraction** (over-approximating the count,
/// SOUND for the outer ν / AG-safety; the inner-μ must-obligation is discharged
/// separately by b2's ranking). Unlike [`EnumStateMeta`] this abstraction is
/// LOSSY — it must feed the 3-valued / cube engine, never the exact oracle.
#[derive(Debug, Clone)]
pub(crate) struct DownCounterMeta {
    /// BTOR2 NID of the `state` line.
    pub nid: Nid,
    /// Bit-vector width of the counter (collapses to 1 bit).
    pub width: u32,
    /// The single constant the counter is compared against externally; the
    /// abstraction predicate is `cnt == threshold` (usually 0 — expiry).
    pub threshold: u64,
}

/// mununu#602 — narrow every leaf cell (`state` / `input`) whose [`super::dep_graph::bit_cone`]
/// mask is PARTIAL to its kept bits, so the exact engine bit-blasts a 32-bit field of a 736-bit
/// crossed vector as 32 variables, not 736.
///
/// For a leaf `L` of width `W` with kept bits `K` (`0 < |K| < W`), the rewrite keeps `L`'s NID
/// and symbol on a **narrow** leaf of width `|K|` (so `init` / `next` lines, the symbol-keyed
/// next-state table and a two-player `controllable` match stay valid), appends right after it a
/// **reconstruction** `R` of width `W` — the kept bits in their original positions, zero
/// everywhere else — and redirects every reader of `L` to `R`. `next L` becomes the kept bits of
/// the old next value (one `slice` per run, `concat`ed high-to-low); a constant `init` is
/// narrowed the same way; a non-constant `init` keeps the leaf whole.
///
/// **Soundness.** The bit cone is closed under `next` bit by bit and over-approximates every
/// operator, so no kept bit's next value and no property atom reads a dropped bit; reading the
/// dropped bits as `0` through `R` therefore changes nothing observable — it is exactly what the
/// bit-blaster already does to a whole out-of-cone leaf, one bit at a time. A leaf with NO kept
/// bit is left in place and excluded from the returned keep-set (the blaster pins it whole);
/// a leaf with every bit kept is untouched.
///
/// Returns the rewritten file and the keep-set to hand to `BddBitBlaster::build_with_keep`.
pub(crate) fn narrow_leaves(
    file: &Btor2File,
    cone: &std::collections::HashMap<Nid, Vec<bool>>,
) -> (Btor2File, std::collections::HashSet<Nid>) {
    use crate::adapter::btor2::ast::{ConstValue, Op, Sort};
    let mut keep: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let init_value: std::collections::HashMap<Nid, Operand> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, value, .. } => Some((*state, *value)),
            _ => None,
        })
        .collect();
    // Which leaves narrow, with their kept masks.
    let mut partial: std::collections::HashMap<Nid, Vec<bool>> = std::collections::HashMap::new();
    for line in &file.lines {
        if !matches!(line.node, Node::State { .. } | Node::Input { .. }) {
            continue;
        }
        let Some(mask) = cone.get(&line.nid) else {
            continue; // no kept bit: pinned whole by the blaster
        };
        let kept = mask.iter().filter(|b| **b).count();
        if kept == 0 {
            continue;
        }
        keep.insert(line.nid);
        if kept == mask.len() {
            continue;
        }
        // A non-constant init cannot be narrowed by this rewrite: keep the leaf whole.
        if let Some(v) = init_value.get(&line.nid)
            && !matches!(
                file.lookup(v.nid()).map(|l| &l.node),
                Some(Node::Const { .. })
            )
        {
            continue;
        }
        partial.insert(line.nid, mask.clone());
    }
    if partial.is_empty() {
        return (file.clone(), keep);
    }

    /// The line emitter: fresh NIDs, sorts by width (declared in a prelude), the output.
    struct Emit {
        next_nid: Nid,
        sorts: std::collections::HashMap<u32, Nid>,
        prelude: Vec<Line>,
        out: Vec<Line>,
    }
    impl Emit {
        fn line(nid: Nid, node: Node, immediates: Vec<u32>) -> Line {
            Line {
                nid,
                node,
                immediates,
                source_line: 0,
            }
        }
        fn fresh(&mut self) -> Nid {
            let n = self.next_nid;
            self.next_nid += 1;
            n
        }
        fn sort_for(&mut self, w: u32) -> Nid {
            if let Some(n) = self.sorts.get(&w) {
                return *n;
            }
            let n = self.fresh();
            self.prelude.push(Self::line(
                n,
                Node::Sort {
                    sort: Sort::BitVec { width: w },
                },
                Vec::new(),
            ));
            self.sorts.insert(w, n);
            n
        }
        fn op(&mut self, op: Op, w: u32, args: Vec<Operand>, immediates: Vec<u32>) -> Nid {
            let s = self.sort_for(w);
            let n = self.fresh();
            self.out.push(Self::line(
                n,
                Node::Op {
                    op,
                    sort: s,
                    args,
                    symbol: None,
                },
                immediates,
            ));
            n
        }
        fn zero(&mut self, w: u32) -> Nid {
            let s = self.sort_for(w);
            let n = self.fresh();
            self.out.push(Self::line(
                n,
                Node::Const {
                    sort: s,
                    value: ConstValue::Zero,
                },
                Vec::new(),
            ));
            n
        }
        /// `concat` a high piece onto an accumulated low value.
        fn stack(&mut self, acc: Option<(Nid, u32)>, piece: Nid, w: u32) -> Option<(Nid, u32)> {
            Some(match acc {
                None => (piece, w),
                Some((lo, lo_w)) => (
                    self.op(
                        Op::Concat,
                        lo_w + w,
                        vec![Operand(piece), Operand(lo)],
                        Vec::new(),
                    ),
                    lo_w + w,
                ),
            })
        }
        /// The slices of `src` at `runs`, concatenated high-to-low; the source itself when one
        /// run covers it.
        fn gather(&mut self, src: Nid, w_src: u32, runs: &[(u32, u32)]) -> Nid {
            let mut acc: Option<(Nid, u32)> = None;
            for (lo, hi) in runs {
                let w = hi - lo + 1;
                let piece = if w == w_src {
                    src
                } else {
                    self.op(Op::Slice, w, vec![Operand(src)], vec![*hi, *lo])
                };
                acc = self.stack(acc, piece, w);
            }
            acc.map_or(src, |(n, _)| n)
        }
    }
    let mut e = Emit {
        next_nid: file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1,
        sorts: file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Sort {
                    sort: Sort::BitVec { width },
                } => Some((*width, l.nid)),
                _ => None,
            })
            .collect(),
        prelude: Vec::new(),
        out: Vec::with_capacity(file.lines.len() + 4 * partial.len()),
    };
    // The kept bits as maximal runs `(lo, hi)`, low to high.
    fn runs(mask: &[bool]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < mask.len() {
            if mask[i] {
                let lo = i;
                while i + 1 < mask.len() && mask[i + 1] {
                    i += 1;
                }
                out.push((lo as u32, i as u32));
            }
            i += 1;
        }
        out
    }
    fn narrow_width(rs: &[(u32, u32)]) -> u32 {
        rs.iter().map(|(lo, hi)| hi - lo + 1).sum()
    }
    // The width-`w` constant's bits, low first; `None` when the spelling cannot carry `w` bits.
    fn const_bits(v: &ConstValue, w: usize) -> Option<Vec<bool>> {
        let mut bits = vec![false; w];
        match v {
            ConstValue::Zero => {}
            ConstValue::One => {
                if w > 0 {
                    bits[0] = true;
                }
            }
            ConstValue::Ones => bits.iter_mut().for_each(|b| *b = true),
            ConstValue::Dec(d) => {
                if w > 127 && *d != 0 {
                    return None;
                }
                for (i, b) in bits.iter_mut().enumerate() {
                    *b = i < 127 && (*d >> i) & 1 == 1;
                }
            }
            ConstValue::Bin(s) => {
                for (i, ch) in s.chars().rev().enumerate() {
                    if i < w {
                        bits[i] = ch == '1';
                    }
                }
            }
            ConstValue::Hex(s) => {
                for (k, ch) in s.trim_start_matches("0x").chars().rev().enumerate() {
                    let d = ch.to_digit(16)?;
                    for j in 0..4 {
                        let i = k * 4 + j;
                        if i < w {
                            bits[i] = (d >> j) & 1 == 1;
                        }
                    }
                }
            }
        }
        Some(bits)
    }

    // Per narrowed leaf: its reconstruction NID (readers are redirected to it).
    let mut recon: std::collections::HashMap<Nid, Nid> = std::collections::HashMap::new();
    let redirect = |o: &Operand, recon: &std::collections::HashMap<Nid, Nid>| -> Operand {
        match recon.get(&o.nid()) {
            Some(r) => Operand(if o.is_negated() { -*r } else { *r }),
            None => *o,
        }
    };

    for line in &file.lines {
        let nid = line.nid;
        match &line.node {
            Node::State { symbol, .. } | Node::Input { symbol, .. }
                if partial.contains_key(&nid) =>
            {
                let mask = &partial[&nid];
                let w_full = mask.len() as u32;
                let rs = runs(mask);
                let w_narrow = narrow_width(&rs);
                let ns = e.sort_for(w_narrow);
                let node = if matches!(line.node, Node::State { .. }) {
                    Node::State {
                        sort: ns,
                        symbol: symbol.clone(),
                    }
                } else {
                    Node::Input {
                        sort: ns,
                        symbol: symbol.clone(),
                    }
                };
                e.out.push(Emit::line(nid, node, Vec::new()));
                // Reconstruction: zeros between the runs, the narrow leaf's slices inside them.
                let mut pos = 0u32;
                let mut off = 0u32;
                let mut acc: Option<(Nid, u32)> = None;
                for (lo, hi) in &rs {
                    if *lo > pos {
                        let z = e.zero(lo - pos);
                        acc = e.stack(acc, z, lo - pos);
                    }
                    let w = hi - lo + 1;
                    let piece = if w == w_narrow {
                        nid
                    } else {
                        e.op(Op::Slice, w, vec![Operand(nid)], vec![off + w - 1, off])
                    };
                    acc = e.stack(acc, piece, w);
                    off += w;
                    pos = hi + 1;
                }
                if pos < w_full {
                    let z = e.zero(w_full - pos);
                    acc = e.stack(acc, z, w_full - pos);
                }
                let (r, _) = acc.expect("a partial mask has at least one run");
                recon.insert(nid, r);
            }
            Node::Init { state, value, .. } if partial.contains_key(state) => {
                let mask = &partial[state];
                let rs = runs(mask);
                let w_narrow = narrow_width(&rs);
                let ns = e.sort_for(w_narrow);
                let Some(Node::Const { value: cv, .. }) = file.lookup(value.nid()).map(|l| &l.node)
                else {
                    unreachable!("a non-constant init keeps the leaf whole");
                };
                let v = match const_bits(cv, mask.len()) {
                    Some(bits) => {
                        let narrow: String = mask
                            .iter()
                            .zip(bits.iter())
                            .filter(|(k, _)| **k)
                            .map(|(_, b)| if *b { '1' } else { '0' })
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        let c = e.fresh();
                        e.out.push(Emit::line(
                            c,
                            Node::Const {
                                sort: ns,
                                value: ConstValue::Bin(narrow),
                            },
                            Vec::new(),
                        ));
                        c
                    }
                    // Cannot spell the constant's bits: gather them from the constant itself.
                    None => e.gather(value.nid(), mask.len() as u32, &rs),
                };
                e.out.push(Emit::line(
                    nid,
                    Node::Init {
                        sort: ns,
                        state: *state,
                        value: Operand(v),
                    },
                    Vec::new(),
                ));
            }
            Node::Next { state, value, .. } if partial.contains_key(state) => {
                let mask = &partial[state];
                let rs = runs(mask);
                let ns = e.sort_for(narrow_width(&rs));
                let v = redirect(value, &recon);
                let g = e.gather(v.nid(), mask.len() as u32, &rs);
                e.out.push(Emit::line(
                    nid,
                    Node::Next {
                        sort: ns,
                        state: *state,
                        value: Operand(if v.is_negated() { -g } else { g }),
                    },
                    Vec::new(),
                ));
            }
            Node::Op {
                op,
                sort,
                args,
                symbol,
            } => e.out.push(Emit::line(
                nid,
                Node::Op {
                    op: *op,
                    sort: *sort,
                    args: args.iter().map(|a| redirect(a, &recon)).collect(),
                    symbol: symbol.clone(),
                },
                line.immediates.clone(),
            )),
            Node::Init { sort, state, value } => e.out.push(Emit::line(
                nid,
                Node::Init {
                    sort: *sort,
                    state: *state,
                    value: redirect(value, &recon),
                },
                Vec::new(),
            )),
            Node::Next { sort, state, value } => e.out.push(Emit::line(
                nid,
                Node::Next {
                    sort: *sort,
                    state: *state,
                    value: redirect(value, &recon),
                },
                Vec::new(),
            )),
            Node::Bad { signal } => e.out.push(Emit::line(
                nid,
                Node::Bad {
                    signal: redirect(signal, &recon),
                },
                Vec::new(),
            )),
            Node::Constraint { signal } => e.out.push(Emit::line(
                nid,
                Node::Constraint {
                    signal: redirect(signal, &recon),
                },
                Vec::new(),
            )),
            Node::Fair { signal } => e.out.push(Emit::line(
                nid,
                Node::Fair {
                    signal: redirect(signal, &recon),
                },
                Vec::new(),
            )),
            Node::Output { signal, symbol } => e.out.push(Emit::line(
                nid,
                Node::Output {
                    signal: redirect(signal, &recon),
                    symbol: symbol.clone(),
                },
                Vec::new(),
            )),
            Node::Justice { signals } => e.out.push(Emit::line(
                nid,
                Node::Justice {
                    signals: signals.iter().map(|s| redirect(s, &recon)).collect(),
                },
                Vec::new(),
            )),
            _ => e.out.push(line.clone()),
        }
    }
    let Emit { prelude, out, .. } = e;
    let mut lines = prelude;
    lines.extend(out);
    let by_nid = lines.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    (Btor2File { lines, by_nid }, keep)
}

/// Every NID used as an operand anywhere (Op args, next/init state+value, bad/output/
/// constraint/fair/justice signals). A NID absent from this set is DEAD — no node reads
/// it and it drives no property — so it cannot affect any verdict (the cone-of-influence
/// would drop it). Used to ignore yosys's named observable copies (`uext(cnt, 0)` named
/// `…cnt`) that would otherwise trip the counter's external-use guard.
fn collect_referenced(file: &Btor2File) -> std::collections::HashSet<Nid> {
    let mut work = Vec::new();
    for l in &file.lines {
        push_operand_nids(&l.node, &mut work);
    }
    work.into_iter().collect()
}

/// The reset-mux aliases of a counter state `cnt`: `cnt` itself plus every wire
/// `ite(cond, cnt, const)` / `ite(cond, const, cnt)`. Yosys lowers an async-reset
/// register so its value reaches arithmetic / comparisons through a mux forcing the
/// reset constant on one branch (`sub(ite(arst_n, cnt, 0), 1)`, `redor(ite(arst_n,
/// cnt, 0))`). Such a wire behaves as `cnt` for the counter's stride and threshold
/// comparisons, and — being a value-path `ite` — abstracts to `ite(cond, cnt_z,
/// const==T)`, a faithful `(alias == T)` bit. One level of mux (the async-reset
/// idiom); deeper nests fall back to the raw-nid pattern.
fn reset_mux_aliases(file: &Btor2File, cnt: Nid) -> std::collections::HashSet<Nid> {
    use crate::adapter::btor2::ast::Op;
    let mut aliases = std::collections::HashSet::new();
    aliases.insert(cnt);
    for l in &file.lines {
        if let Node::Op {
            op: Op::Ite, args, ..
        } = &l.node
            && args.len() >= 3
        {
            let (then, els) = (args[1].nid(), args[2].nid());
            let one_is_cnt = then == cnt || els == cnt;
            let other = if then == cnt { els } else { then };
            if one_is_cnt && resolve_btor2_constant(file, other).is_some() {
                aliases.insert(l.nid);
            }
        }
    }
    aliases
}

/// b1 detection — counter registers whose only EXTERNAL use is a comparison of `cnt`
/// against a single threshold `T` (`eq/neq(cnt, T)`, or `redor(cnt)` = `~|cnt` for the
/// `T = 0` expiry idiom), and whose `next` value path is built ONLY from arithmetic
/// strides (`add`/`sub` of `cnt` and a constant), holds (`cnt`), reload constants,
/// opaque reload values that do NOT reference `cnt` (a prescale input, `clk_cnt >> 2`),
/// and `ite` muxes. SOUND syntactic sufficient condition:
/// - a stride leaf (`cnt ± k`) or an opaque reload can produce ANY value ⇒ under the
///   may-abstraction `cnt == T` becomes a free (nondeterministic) bit — a sound
///   over-approximation;
/// - requiring ≥1 stride distinguishes a genuine counter from a one-hot (F1) or a
///   frozen register (a future fold);
/// - requiring every external use to be a single-threshold comparison guarantees the
///   1-bit `{cnt==T}` abstraction PRESERVES those uses (multi-threshold / arithmetic
///   external uses ⇒ abstain, the value would be needed).
///
/// Guards/conditions are not walked (like F1). Read-only; `counter_may_abstract`
/// consumes this.
pub(crate) fn detect_down_counter(file: &Btor2File) -> Vec<DownCounterMeta> {
    use crate::adapter::btor2::ast::Op;
    let referenced = collect_referenced(file);
    let mut out = Vec::new();
    'states: for line in &file.lines {
        let Node::State { sort, .. } = &line.node else {
            continue;
        };
        let Some(width) = parser::bv_width(file, *sort) else {
            continue;
        };
        if width < 2 {
            continue;
        }
        let cnt = line.nid;
        let Some(next) = parser::find_next_value_operand(file, cnt) else {
            continue;
        };
        // Reset-mux aliases: `cnt` plus every wire `ite(cond, cnt, const)` — yosys's
        // async-reset lowering, where the counter's value reaches its stride and expiry
        // comparison through a mux that forces a constant (the reset value) on one
        // branch. A stride / comparison on such an alias is a stride / comparison on cnt.
        let aliases = reset_mux_aliases(file, cnt);
        let is_alias = |o: Nid| aliases.contains(&o);

        // Walk the next-state VALUE path; classify every leaf. Collect the node
        // set so external-use analysis can exclude the counter's own logic.
        let mut value_path: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        let mut has_stride = false;
        let mut visited: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        let mut stack = vec![next.nid()];
        while let Some(nid) = stack.pop() {
            if !visited.insert(nid) {
                continue;
            }
            value_path.insert(nid);
            let Some(n) = file.lookup(nid) else {
                continue 'states;
            };
            match &n.node {
                // Reload / floor constant.
                Node::Const { .. } => {}
                // Hold — the counter feeding its own next.
                Node::State { .. } if nid == cnt => {}
                // A mux — recurse into THEN/ELSE only (not the condition).
                Node::Op {
                    op, args, sort: s, ..
                } if parser::bv_width(file, *s) == Some(width)
                    && *op == Op::Ite
                    && args.len() >= 3 =>
                {
                    stack.push(args[1].nid());
                    stack.push(args[2].nid());
                }
                // A stride: `alias ± k` (k a constant), where `alias` is `cnt` or a
                // reset-mux wire of `cnt`. Operands are NOT pushed — the count itself is
                // abstracted away, so its value is irrelevant.
                Node::Op {
                    op, args, sort: s, ..
                } if parser::bv_width(file, *s) == Some(width)
                    && (*op == Op::Add || *op == Op::Sub)
                    && args.len() == 2 =>
                {
                    let (a, b) = (args[0].nid(), args[1].nid());
                    let one_is_cnt = is_alias(a) || is_alias(b);
                    let other = if is_alias(a) { b } else { a };
                    if one_is_cnt && resolve_btor2_constant(file, other).is_some() {
                        has_stride = true;
                    } else {
                        continue 'states; // stride over another register ⇒ not a simple counter
                    }
                }
                // An OPAQUE reload value that does NOT reference the counter — a prescale
                // input (`clk_cnt`), a derived reload (`clk_cnt >> 2`), a floor register.
                // Accepted as a value-path leaf (NOT recursed) and abstracted to a free
                // may-bit; sound because a free bit over-approximates any reload value.
                // A node that DOES reference `cnt` but is not a recognised stride/hold ⇒
                // abstain (it could hide a use we cannot cleanly collapse).
                other if !node_refs_nid(other, cnt) => {}
                _ => continue 'states,
            }
        }
        if !has_stride {
            continue;
        }

        // External uses — every node referencing `cnt` OR a reset-mux alias that is not
        // part of the counter's own logic (value path, its `next`/`init` lines, the
        // state line, the alias reset-mux wires) must be a single-threshold comparison:
        // `eq/neq(·, T)`, or `redor(·)` (the `~|cnt` / `|cnt` idiom = `· != 0`,
        // threshold 0 — the DOMINANT "counter expired" check in RTL).
        let mut threshold: Option<u64> = None;
        for l in &file.lines {
            if is_alias(l.nid) || value_path.contains(&l.nid) {
                continue;
            }
            // A DEAD value node (an `Op` nothing reads) cannot affect the verdict — skip
            // it. Ignores yosys's named observable copies of the counter. Structural
            // nodes (Next/Init/Bad/Output) are never operands but ARE load-bearing, so
            // they are NOT skipped here — a Next feeding cnt's value into another register
            // or a Bad reading it must still trip the abstain below.
            if matches!(l.node, Node::Op { .. }) && !referenced.contains(&l.nid) {
                continue;
            }
            match &l.node {
                Node::Next { state, .. } | Node::Init { state, .. } if *state == cnt => continue,
                _ => {}
            }
            if !aliases.iter().any(|&a| node_refs_nid(&l.node, a)) {
                continue;
            }
            let Node::Op { op, args, .. } = &l.node else {
                continue 'states;
            };
            let t = match op {
                Op::Redor if args.len() == 1 && is_alias(args[0].nid()) => 0u64,
                Op::Eq | Op::Neq if args.len() == 2 => {
                    let (a, b) = (args[0].nid(), args[1].nid());
                    let other = if is_alias(a) {
                        b
                    } else if is_alias(b) {
                        a
                    } else {
                        continue 'states;
                    };
                    let Some(t) = resolve_btor2_constant(file, other) else {
                        continue 'states; // compared to a non-constant (another register) ⇒ abstain
                    };
                    t
                }
                _ => continue 'states,
            };
            match threshold {
                None => threshold = Some(t),
                Some(prev) if prev == t => {}
                Some(_) => continue 'states, // ≥2 distinct thresholds ⇒ abstain
            }
        }
        if let Some(threshold) = threshold {
            out.push(DownCounterMeta {
                nid: cnt,
                width,
                threshold,
            });
        }
    }
    out
}

/// b1 — the may-abstraction rewrite. Collapse the W-bit counter `meta.nid` to a
/// 1-bit `cnt_z` (`⟺ cnt == threshold`), replacing each arithmetic-stride branch of
/// its next-state with a FRESH INPUT (a nondeterministic bit ⇒ over-approximating
/// the count). Holds map to `cnt_z`; reload constants `C` map to the bit `(C == T)`;
/// external `eq/neq(cnt, T)` reads become `eq/neq(cnt_z, 1)`. Abstains (returns None)
/// on any unhandled reference to `cnt` — never a partial rewrite.
///
/// LOSSY (over-approximation): the result must feed the 3-valued / cube engine, whose
/// `smt_must_edge` construction reads the free-input stride as a **may-edge**. SOUND for
/// the outer ν (AG-safety) — holds transfers, a would-be violation on the may-edge falls
/// to ⊥. NEVER feed to the exact oracle (its 2-valued `Violated` would be unsound).
pub(crate) fn counter_may_abstract(file: &Btor2File, meta: &DownCounterMeta) -> Option<Btor2File> {
    use crate::adapter::btor2::ast::{ConstValue, Op, Sort};
    let cnt = meta.nid;
    let t = meta.threshold;
    // The reset-mux aliases (`cnt` + `ite(cond, cnt, const)` wires). Comparisons on an
    // alias are rewritten like comparisons on `cnt`; the alias reset-mux ites are
    // themselves value-path ites rewritten to `ite(cond, cnt_z, const==T)`.
    let aliases = reset_mux_aliases(file, cnt);
    let is_alias = |o: Nid| aliases.contains(&o);
    let referenced = collect_referenced(file);

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut prelude: Vec<Line> = Vec::new();
    // 1-bit sort (reuse or create).
    let bit_sort = file
        .lines
        .iter()
        .find_map(|l| match &l.node {
            Node::Sort {
                sort: Sort::BitVec { width },
            } if *width == 1 => Some(l.nid),
            _ => None,
        })
        .unwrap_or_else(|| {
            let n = next_nid;
            next_nid += 1;
            prelude.push(Line {
                nid: n,
                node: Node::Sort {
                    sort: Sort::BitVec { width: 1 },
                },
                immediates: Vec::new(),
                source_line: 0,
            });
            n
        });
    let mut mk_const = |v: i128| -> Nid {
        let n = next_nid;
        next_nid += 1;
        prelude.push(Line {
            nid: n,
            node: Node::Const {
                sort: bit_sort,
                value: ConstValue::Dec(v),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        n
    };
    let const_0 = mk_const(0);
    let const_1 = mk_const(1);
    let bit_for = |is_t: bool| Operand(if is_t { const_1 } else { const_0 });

    // Classify the value-path nodes. `may_input` maps a value leaf whose exact value
    // is abstracted away — a stride (`cnt ± k`) OR an opaque reload (a prescale input) —
    // to a fresh 1-bit may-bit. `dropped` are the STRIDE nodes only: they reference
    // `cnt` (soon 1-bit) so must be removed; opaque reloads may be shared and are kept.
    let next_op = parser::find_next_value_operand(file, cnt)?;
    let mut may_input: std::collections::HashMap<Nid, Nid> = std::collections::HashMap::new();
    let mut dropped: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let mut ite_nodes: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    let fresh_may = |next_nid: &mut Nid, prelude: &mut Vec<Line>| -> Nid {
        let inp = *next_nid;
        *next_nid += 1;
        prelude.push(Line {
            nid: inp,
            node: Node::Input {
                sort: bit_sort,
                symbol: Some(format!("__cnt{cnt}_may{inp}")),
            },
            immediates: Vec::new(),
            source_line: 0,
        });
        inp
    };
    {
        let mut vis: std::collections::HashSet<Nid> = std::collections::HashSet::new();
        let mut st = vec![next_op.nid()];
        while let Some(nid) = st.pop() {
            if !vis.insert(nid) {
                continue;
            }
            let n = file.lookup(nid)?;
            match &n.node {
                Node::Const { .. } => {}
                Node::State { .. } if nid == cnt => {}
                Node::Op { op, args, sort, .. }
                    if parser::bv_width(file, *sort) == Some(meta.width)
                        && *op == Op::Ite
                        && args.len() >= 3 =>
                {
                    ite_nodes.insert(nid);
                    st.push(args[1].nid());
                    st.push(args[2].nid());
                }
                Node::Op { op, args, sort, .. }
                    if parser::bv_width(file, *sort) == Some(meta.width)
                        && (*op == Op::Add || *op == Op::Sub)
                        && args.len() == 2 =>
                {
                    // A stride ⇒ a fresh may-bit; the node references cnt ⇒ drop it.
                    let inp = fresh_may(&mut next_nid, &mut prelude);
                    may_input.insert(nid, inp);
                    dropped.insert(nid);
                }
                other if !node_refs_nid(other, cnt) => {
                    // An opaque reload value (prescale input, floor register) ⇒ a fresh
                    // may-bit. KEEP the node (it may be shared with other logic).
                    let inp = fresh_may(&mut next_nid, &mut prelude);
                    may_input.insert(nid, inp);
                }
                _ => return None,
            }
        }
    }

    // Map a value operand to its 1-bit abstraction.
    let rewire_val = |o: &Operand| -> Option<Operand> {
        let n = o.nid();
        if n == cnt {
            Some(Operand(cnt)) // hold
        } else if let Some(&inp) = may_input.get(&n) {
            Some(Operand(inp)) // stride / opaque reload → may-bit
        } else if ite_nodes.contains(&n) {
            Some(Operand(n)) // nested ite (rewritten in place)
        } else {
            // reload constant → (C == T); anything else is unhandled.
            resolve_btor2_constant(file, n).map(|c| bit_for(c == t))
        }
    };

    let mut out: Vec<Line> = Vec::with_capacity(file.lines.len() + prelude.len());
    out.extend(prelude);
    for line in &file.lines {
        let nid = line.nid;
        // Drop the stride nodes — replaced by fresh may-bits.
        if dropped.contains(&nid) {
            continue;
        }
        // Drop a DEAD value node that reads cnt/an alias (a yosys observable copy like
        // `uext(cnt, 0)` named `…cnt`): nothing reads it, so removing it is sound and
        // avoids re-typing a now-1-bit operand it would otherwise carry at full width.
        // Only `Op` value nodes qualify — Next/Init/Bad/Output are structural (never
        // referenced as operands, but load-bearing) and must NOT be dropped.
        if matches!(line.node, Node::Op { .. })
            && !referenced.contains(&nid)
            && aliases.iter().any(|&a| node_refs_nid(&line.node, a))
        {
            continue;
        }
        let node = match &line.node {
            Node::State { symbol, .. } if nid == cnt => Node::State {
                sort: bit_sort,
                symbol: symbol.clone(),
            },
            Node::Init { state, value, .. } if *state == cnt => {
                let c = resolve_btor2_constant(file, value.nid())?;
                Node::Init {
                    sort: bit_sort,
                    state: cnt,
                    value: bit_for(c == t),
                }
            }
            Node::Next { state, value, .. } if *state == cnt => Node::Next {
                sort: bit_sort,
                state: cnt,
                value: rewire_val(value)?,
            },
            // A value-path `ite` OR a reset-mux alias wire `ite(cond, cnt, const)`: sort → 1,
            // operands remapped (condition unchanged). Covering aliases here (even when not
            // on the value path — e.g. used only by the expiry gate) keeps them well-typed
            // and off the abstain guard.
            Node::Op {
                op: Op::Ite,
                args,
                symbol,
                ..
            } if ite_nodes.contains(&nid) || (is_alias(nid) && nid != cnt) => {
                if args.len() < 3 {
                    return None;
                }
                Node::Op {
                    op: Op::Ite,
                    sort: bit_sort,
                    args: vec![args[0], rewire_val(&args[1])?, rewire_val(&args[2])?],
                    symbol: symbol.clone(),
                }
            }
            // External `redor(alias)` (= `alias != 0`, the `~|cnt` gate) → `not(alias)`
            // (= `!(alias==T)`; valid because `redor` fixes the threshold at 0, and the
            // alias node holds the `(alias == T)` bit after abstraction).
            Node::Op {
                op: Op::Redor,
                args,
                symbol,
                sort,
            } if args.len() == 1 && is_alias(args[0].nid()) => {
                if t != 0 {
                    return None;
                }
                Node::Op {
                    op: Op::Not,
                    sort: *sort,
                    args: vec![args[0]],
                    symbol: symbol.clone(),
                }
            }
            // External `eq/neq(alias, T)` → `eq/neq(alias, 1)` (the alias is now the `==T` bit).
            Node::Op {
                op: op @ (Op::Eq | Op::Neq),
                args,
                symbol,
                sort,
            } if args.iter().any(|a| is_alias(a.nid())) => {
                if args.len() != 2 {
                    return None;
                }
                let new_args = if is_alias(args[0].nid()) {
                    vec![args[0], Operand(const_1)]
                } else {
                    vec![Operand(const_1), args[1]]
                };
                Node::Op {
                    op: *op,
                    sort: *sort,
                    args: new_args,
                    symbol: symbol.clone(),
                }
            }
            // Any OTHER reference to cnt or an alias ⇒ abstain.
            other if aliases.iter().any(|&a| node_refs_nid(other, a)) => return None,
            other => other.clone(),
        };
        out.push(Line {
            nid,
            node,
            immediates: line.immediates.clone(),
            source_line: line.source_line,
        });
    }
    let by_nid = out.iter().enumerate().map(|(i, l)| (l.nid, i)).collect();
    Some(Btor2File { lines: out, by_nid })
}

/// §Phase 10 §10.2 stage 1 — validate detected memory cells against
/// the sidecar's `memories[]` declarations. Returns Ok when every
/// detected memory has a matching sidecar entry with matching
/// `address_width` + `data_width`. Returns an actionable error
/// otherwise — the error message includes a copy-paste-ready sidecar
/// template the user can drop into their `.mununu.json` to declare
/// the missing memories.
///
/// When the sidecar JSON is absent OR the schema fails to parse, the
/// error still fires (memory cells exist in the BTOR2 and we have
/// no declarations to validate against). The user sees the same
/// template; this is the right outcome — silent acceptance of
/// undeclared memories would let the downstream lift produce
/// unsound verdicts.
fn validate_sidecar_memories(
    _file: &Btor2File,
    memory_cells: &[MemoryCellMeta],
    options: &AdapterOptions,
) -> Result<(), AdapterError> {
    use crate::adapter::systemverilog::annotation::SvAnnotation;

    // Try to parse sidecar; missing/unparseable JSON ⇒ no declarations.
    let declared: std::collections::HashMap<String, (u32, u32)> = options
        .sidecar_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<SvAnnotation>(json).ok())
        .map(|ann| {
            ann.memories
                .into_iter()
                .map(|m| (m.name, (m.address_width, m.data_width)))
                .collect()
        })
        .unwrap_or_default();

    let mut missing: Vec<&MemoryCellMeta> = Vec::new();
    let mut mismatched: Vec<(&MemoryCellMeta, u32, u32)> = Vec::new();

    for cell in memory_cells {
        match declared.get(&cell.name) {
            None => missing.push(cell),
            Some(&(declared_aw, declared_dw)) => {
                if declared_aw != cell.address_width || declared_dw != cell.data_width {
                    mismatched.push((cell, declared_aw, declared_dw));
                }
            }
        }
    }

    if missing.is_empty() && mismatched.is_empty() {
        return Ok(());
    }

    let mut message = String::from(
        "§Phase 10 §10.2 stage 1 — the BTOR2 source contains memory cells that need \
         sidecar declarations before lifting can proceed. Add the following entries to your \
         `.mununu.json` sidecar's `memories` field:\n\n",
    );
    message.push_str("  \"memories\": [\n");
    for cell in &missing {
        message.push_str(&format!(
            "    {{\"name\":\"{}\", \"address_width\":{}, \"data_width\":{}, \"abstraction\":\"havoc\"}},\n",
            cell.name, cell.address_width, cell.data_width
        ));
    }
    for (cell, declared_aw, declared_dw) in &mismatched {
        message.push_str(&format!(
            "    // mismatch on `{}`: BTOR2 has address_width={}, data_width={}; \
             sidecar declared address_width={}, data_width={}. Fix:\n",
            cell.name, cell.address_width, cell.data_width, declared_aw, declared_dw
        ));
        message.push_str(&format!(
            "    {{\"name\":\"{}\", \"address_width\":{}, \"data_width\":{}, \"abstraction\":\"havoc\"}},\n",
            cell.name, cell.address_width, cell.data_width
        ));
    }
    message.push_str("  ]\n\n");
    message.push_str(
        "Currently supported abstractions: `havoc` (over-approximation; sound for safety, \
         unsound for liveness). UF mode (stage 3) and bounded bit-blast (stage 4) are queued \
         — track the §Phase 10 plan for shipping status.",
    );

    let first_line = missing
        .first()
        .map(|c| c.source_line)
        .or_else(|| mismatched.first().map(|(c, _, _)| c.source_line))
        .unwrap_or(0);

    Err(AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message,
        location: Some(SourceLocation {
            line: first_line,
            column: 0,
        }),
    })
}

/// §Phase 10 §10.2 stage 1b — collect the NIDs of memory cells the
/// sidecar declared with `abstraction: havoc`. Used by
/// [`havoc_rewrite_memories`] to know which cells to abstract.
///
/// Returns the empty set when no memories are declared havoc (the
/// stage 1a-only path; the is_blastable check will then error out
/// on any remaining Read/Write op pointing the user at stage 3/4).
fn sidecar_havoc_memory_nids(
    memory_cells: &[MemoryCellMeta],
    options: &AdapterOptions,
) -> std::collections::HashSet<Nid> {
    use crate::adapter::systemverilog::annotation::{MemoryAbstraction, SvAnnotation};

    let Some(json) = &options.sidecar_json else {
        return std::collections::HashSet::new();
    };
    let Ok(ann) = serde_json::from_str::<SvAnnotation>(json) else {
        return std::collections::HashSet::new();
    };
    let havoc_names: std::collections::HashSet<&str> = ann
        .memories
        .iter()
        .filter(|m| matches!(m.abstraction, MemoryAbstraction::Havoc))
        .map(|m| m.name.as_str())
        .collect();
    memory_cells
        .iter()
        .filter(|m| havoc_names.contains(m.name.as_str()))
        .map(|m| m.nid)
        .collect()
}

/// §Phase 10 §10.2 stage 3.a (2026-06-12) — collect the NIDs of
/// memory cells the sidecar declared with `abstraction: uf`.
///
/// Mirrors [`sidecar_havoc_memory_nids`] for the UF abstraction
/// mode. Stage 3.a ships ONLY the recognition layer; the actual
/// UF rewriting (rewriting `Op::Read` to a Z3 Array `select`
/// expression and `Op::Write` to a `store` expression in the
/// downstream SMT-query layer) is stage 3.b + 3.c work. The
/// translate() integration path uses the result of this helper to
/// emit a distinct `AdapterWarning` that tells the user the
/// recognition layer is shipped while the actual lift is still
/// queued — finer-grained than the generic
/// "operator not blastable" error users get today for any
/// UF-declared memory.
///
/// **Why the staged shipping**: matches the §Phase 11 §11.4
/// multi-session sub-item discipline (the R-S2b / R-S6 arcs
/// followed the same pattern). Stage 3.a is a small unblock-
/// for-stage-3.b commit; stage 3.b adds the SMT-side encoding;
/// stage 3.c wires the predicate-image queries.
///
/// **Pure**: no I/O; testable in isolation against a synthetic
/// `MemoryCellMeta` + sidecar JSON pair.
pub(crate) fn sidecar_uf_memory_nids(
    memory_cells: &[MemoryCellMeta],
    options: &AdapterOptions,
) -> std::collections::HashSet<Nid> {
    use crate::adapter::systemverilog::annotation::{MemoryAbstraction, SvAnnotation};

    let Some(json) = &options.sidecar_json else {
        return std::collections::HashSet::new();
    };
    let Ok(ann) = serde_json::from_str::<SvAnnotation>(json) else {
        return std::collections::HashSet::new();
    };
    let uf_names: std::collections::HashSet<&str> = ann
        .memories
        .iter()
        .filter(|m| matches!(m.abstraction, MemoryAbstraction::Uf))
        .map(|m| m.name.as_str())
        .collect();
    memory_cells
        .iter()
        .filter(|m| uf_names.contains(m.name.as_str()))
        .map(|m| m.nid)
        .collect()
}

/// §Phase 10 §10.2 stage 1b — rewrite a BTOR2 file to abstract out
/// the memory cells listed in `havoc_nids` under the havoc-mode
/// semantics:
///
/// 1. Drop each memory's `State` line (the array cell goes away).
/// 2. Drop every `Init` / `Next` line whose `state` operand is one
///    of the havoc'd memories.
/// 3. Drop every `Op::Write` whose first operand is one of the
///    havoc'd memories.
/// 4. Replace every `Op::Read` whose first operand is one of the
///    havoc'd memories with an `Input` of the read's data sort,
///    preserving the NID so downstream references resolve. The new
///    input carries a synthetic symbol `__havoc_read_<nid>` for
///    traceability in `valuations { ... }` blocks.
///
/// **Soundness.** Each Read becomes an independent nondeterministic
/// input — re-reading the same address at different cycles can
/// return different values. This over-approximates the concrete
/// read behaviour: any value the concrete memory might hold is
/// admissible. Writes are silently dropped because there is no
/// abstract memory to update. Sound for safety; unsound for liveness
/// on memory contents.
///
/// **Errors.** Returns `Err` when dropping memory operators would
/// leave a dangling reference (i.e. a node OTHER than a Next/Write
/// references a dropped Write's NID). This shouldn't happen on
/// well-formed Yosys-emitted BTOR2 for stage 1b's target fixtures
/// (register files, mailboxes); the error message asks the user to
/// file a bug with the BTOR2 dump so we can extend the rewriter.
pub(crate) fn havoc_rewrite_memories(
    file: &Btor2File,
    havoc_nids: &std::collections::HashSet<Nid>,
) -> Result<Btor2File, AdapterError> {
    use crate::adapter::btor2::ast::{Op, Sort};

    // Helper — does this sort NID resolve to an Array sort?
    let is_array_sort =
        |sort_nid: Nid| -> bool { matches!(sort_of_arc(file, sort_nid), Some(Sort::Array { .. })) };

    // First pass — compute the closed set of NIDs to drop. The havoc
    // semantics demand that every node carrying array-typed data
    // disappear from the IR; downstream consumers either (a) had
    // their array operand replaced by an Input (Reads), or
    // (b) referenced a Next line we're dropping (chained Writes,
    // ITE-on-array selectors).
    //
    // We iterate the line list once and collect every node whose
    // RESULT sort is Array — that includes the State cells themselves,
    // intermediate `Op { sort = array, ... }` lines (ITE-on-array,
    // Write, etc.), and the Init/Next lines targeting array states.
    // Yosys-emitted BTOR2 only uses array sorts for memory data flow,
    // so dropping every array-sorted node is safe; we explicitly
    // check this assumption by verifying every array-sorted State is
    // a havoc'd memory (mixed havoc/non-havoc memories would require
    // a per-memory closure which stage 1b does not yet implement).
    let mut dropped: std::collections::HashSet<Nid> = havoc_nids.clone();
    for line in &file.lines {
        match &line.node {
            Node::State { sort, .. } if is_array_sort(*sort) => {
                if !havoc_nids.contains(&line.nid) {
                    return Err(AdapterError {
                        kind: AdapterErrorKind::UnsupportedConstruct,
                        message: format!(
                            "§Phase 10 §10.2 stage 1b: BTOR2 has array-typed state at NID {} \
                             that the sidecar did not declare with `abstraction: havoc`. \
                             Stage 1b only handles mixed havoc + bitvec-only fixtures; \
                             non-havoc memories must wait for stage 3 (UF) or stage 4 \
                             (bounded bit-blast).",
                            line.nid,
                        ),
                        location: Some(SourceLocation {
                            line: line.source_line,
                            column: 0,
                        }),
                    });
                }
                dropped.insert(line.nid);
            }
            Node::Op { sort, .. } if is_array_sort(*sort) => {
                dropped.insert(line.nid);
            }
            Node::Init { sort, .. } | Node::Next { sort, .. } if is_array_sort(*sort) => {
                dropped.insert(line.nid);
            }
            _ => {}
        }
    }

    // Second pass — produce the rewritten line list. Each Read whose
    // array operand is dropped becomes a fresh Input at the same NID
    // (downstream references resolve unchanged). Every other dropped
    // line is removed. Non-dropped lines pass through cloned.
    let mut new_lines: Vec<Line> = Vec::with_capacity(file.lines.len());
    for line in &file.lines {
        if dropped.contains(&line.nid) {
            continue;
        }
        let rewritten = match &line.node {
            Node::Op {
                op: Op::Read,
                sort,
                args,
                ..
            } if args
                .first()
                .map(|a| dropped.contains(&a.nid()))
                .unwrap_or(false) =>
            {
                Line {
                    nid: line.nid,
                    node: Node::Input {
                        sort: *sort,
                        symbol: Some(format!("__havoc_read_{}", line.nid)),
                    },
                    immediates: Vec::new(),
                    source_line: line.source_line,
                }
            }
            _ => line.clone(),
        };
        new_lines.push(rewritten);
    }

    // Third pass — verify no dangling references survived. A live
    // reference to a dropped NID would mean some consumer is using an
    // array-tainted result the rewriter expected to be self-contained
    // within the dropped subgraph.
    let live: std::collections::HashSet<Nid> = new_lines.iter().map(|l| l.nid).collect();
    for line in &new_lines {
        let refs: Vec<Nid> = match &line.node {
            Node::Op { args, .. } => args.iter().map(|a| a.nid()).collect(),
            Node::Init { state, value, .. } | Node::Next { state, value, .. } => {
                vec![*state, value.nid()]
            }
            Node::Bad { signal }
            | Node::Constraint { signal }
            | Node::Fair { signal }
            | Node::Output { signal, .. } => vec![signal.nid()],
            Node::Justice { signals } => signals.iter().map(|s| s.nid()).collect(),
            _ => Vec::new(),
        };
        for r in refs {
            if !live.contains(&r) && dropped.contains(&r) {
                return Err(AdapterError {
                    kind: AdapterErrorKind::UnsupportedConstruct,
                    message: format!(
                        "§Phase 10 §10.2 stage 1b: havoc rewrite would leave NID {} \
                         (a {:?}) referencing dropped NID {} (an array-tainted node). \
                         The stage 1b rewriter assumes array data flow is self-contained \
                         within the memory subgraph; please file an issue with the BTOR2 \
                         dump so we can extend the rewriter.",
                        line.nid, line.node, r,
                    ),
                    location: Some(SourceLocation {
                        line: line.source_line,
                        column: 0,
                    }),
                });
            }
        }
    }

    // Fourth pass — rebuild the by_nid index.
    let by_nid: std::collections::HashMap<Nid, usize> = new_lines
        .iter()
        .enumerate()
        .map(|(i, l)| (l.nid, i))
        .collect();

    Ok(Btor2File {
        lines: new_lines,
        by_nid,
    })
}

/// §Phase 10 §10.2 stage 1 helper — resolve a sort NID to its `Sort`.
fn sort_of_arc(file: &Btor2File, sort_nid: Nid) -> Option<crate::adapter::btor2::ast::Sort> {
    for line in &file.lines {
        if line.nid == sort_nid
            && let Node::Sort { sort } = &line.node
        {
            return Some(sort.clone());
        }
    }
    None
}

fn enumerate_and_blast(
    file: &Btor2File,
    states: &[&Line],
    inputs: &[&Line],
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<BlastOutput, AdapterError> {
    // M.1 Path B (§Phase 11) — start from Yosys's collect_symbols
    // output, then let the sidecar override per-NID names for
    // entries whose `drives` (or fallback `name`) resolves to a
    // state cell whose Yosys symbol is some other alias (`sreg_d`,
    // `bit_cnt_d`, etc.). Strict additivity: no override fires
    // when the sidecar's name already matches Yosys's symbol.
    let symbols = enrich_symbols_with_sidecar_drives(file, &parser::collect_symbols(file), options);

    // Per-state-line metadata.
    let state_meta: Vec<StateMeta> = states
        .iter()
        .enumerate()
        .map(|(idx, l)| StateMeta {
            nid: l.nid,
            width: parser::bv_width(file, sort_of(&l.node)).expect("validated above"),
            symbol: symbols
                .get(&l.nid)
                .cloned()
                .unwrap_or_else(|| format!("st{idx}_n{}", l.nid)),
        })
        .collect();

    // F1.0 — async-reset detection: the sidecar `reset_sequence` signal
    // (if any) overrides the name heuristic.
    let sidecar_reset = sidecar_reset_signal(options);
    let input_meta: Vec<InputMeta> = inputs
        .iter()
        .enumerate()
        .map(|(idx, l)| {
            let symbol = symbols
                .get(&l.nid)
                .cloned()
                .unwrap_or_else(|| format!("in{idx}_n{}", l.nid));
            let is_clock = looks_like_clock(&symbol);
            // F1.0 — a clock is never also a reset; sidecar wins over the
            // name heuristic.
            let is_reset = if is_clock {
                None
            } else if let Some((rname, active_high)) = &sidecar_reset {
                if rname == &symbol {
                    Some(*active_high)
                } else {
                    looks_like_reset(&symbol)
                }
            } else {
                looks_like_reset(&symbol)
            };
            InputMeta {
                nid: l.nid,
                width: parser::bv_width(file, sort_of(&l.node)).expect("validated above"),
                symbol,
                controllable: false, // resolved below
                is_clock,
                is_reset,
            }
        })
        .collect();

    // Phase 1 scope: single-clock posedge designs only.
    let clock_count = input_meta.iter().filter(|im| im.is_clock).count();
    if clock_count > 1 {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "BTOR2 design declares {clock_count} clock-shaped inputs (multi-clock is out of scope for Phase 1)."
            ),
            location: None,
        });
    }

    // Two-stage controllability classification (Document B task B1):
    //
    //   1. If the upstream frontend captured port directions before any
    //      flattening / inlining (typically the yosys driver populating
    //      `options.port_directions` from the pre-flatten `write_json`
    //      hierarchy snapshot), apply the §4 rule — input direction →
    //      Uncontrollable, output direction → Controllable. Inputs not
    //      named in the map keep the historical "Uncontrollable" default
    //      (the right call for BTOR2 inputs that originated as cut points
    //      from `cutpoint -blackbox`).
    //   2. `--controllable-inputs` remains the escape hatch and runs
    //      *after* (1), so an explicit list always wins.
    //
    // Clock inputs are never controllable by construction — the controller
    // does not schedule the clock edge, the world does.
    use crate::controllability::BoundaryDirection;
    let mut input_meta = input_meta;
    let derived_from_directions = !options.port_directions.is_empty();
    for im in input_meta.iter_mut() {
        // Clocks and resets are never controllable — the world drives the
        // clock edge and the reset, not the controller. (F1: reset, like
        // clock, is excluded from the controllable signal set.)
        if im.is_clock || im.is_reset.is_some() {
            continue;
        }
        if let Some(dir) = options.port_directions.get(&im.symbol) {
            im.controllable = matches!(dir, BoundaryDirection::Output);
        }
        if options.controllable_inputs.iter().any(|c| c == &im.symbol) {
            im.controllable = true;
        }
    }
    if !input_meta.is_empty() && options.controllable_inputs.is_empty() && !derived_from_directions
    {
        warnings.push(AdapterWarning {
            kind: WarningKind::NeutralControllability,
            message:
                "All BTOR2 inputs treated as uncontrollable (no --controllable-inputs specified)"
                    .into(),
            location: None,
        });
    }

    // Phase A.3 step 3.5 — automatic partition (live).
    //
    // Compute the partition from the BTOR2 dep-graph and the seed set
    // extracted from intrinsic `bad` / `constraint` / `justice` / `fair`
    // lines. The injection into `cell_domains` / `input_domains` happens
    // **after** the sidecar resolver runs (below), so the user-wins-on-
    // collision rule is enforced naturally — anything the user listed
    // appears as a key in those maps; anything they didn't list is
    // absent, and the partition's `Dropped` verdict is what fills the
    // gap.
    //
    // SOUNDNESS: see `crate::adapter::partition` module docs. Auto-COI
    // pins a `Dropped` signal to a single value via
    // `AbstractionType::Ignored`; the abstract model admits more
    // behaviours than the concrete one (sound for safety + over-
    // approximation). This over-approximation is realized by MIG-2: a
    // pinned cell whose next-state escapes its single value routes to
    // the [`OOB_SINK_KEY`] sink (evaluator-masked), so "escape ⇒
    // anything could happen" rather than the transition being dropped.
    let partition = {
        use crate::adapter::partition::{self, PartitionOptions};
        let seeds = super::dep_graph::extract_property_seeds(file);
        partition::classify(file, &seeds, &PartitionOptions::default())
    };

    // Phase 1 sub-deliverable 2: when the caller passes a `.mununu.json`
    // sidecar, load it and resolve each named state cell to a
    // [`FieldDomain`]. Cells with a sidecar entry get bounded per the
    // declared abstraction (BoundedCounter, EnumValues, …); cells
    // without an entry fall back to full bit-blast over their width.
    let mut cell_domains = build_cell_domains(&state_meta, &symbols, options, warnings)?;
    // Phase A.3 step 3.5 — apply partition to state cells. Each NID
    // not already keyed in `cell_domains` (i.e. not user-listed in the
    // sidecar) AND classified `Dropped` by the partition gets pinned
    // to `AbstractionType::Ignored` here.
    apply_partition_drops(
        &mut cell_domains,
        state_meta.iter().map(|m| (m.nid, m.symbol.as_str())),
        &partition,
        warnings,
        "state cell",
    );

    // R.4.6 cone restriction is applied UPSTREAM by slicing the BTOR2 in
    // `to_ir` (out-of-cone cells are removed from the design before this
    // point), so `enumerate_and_blast` needs no cone-specific handling.

    // R-Y3 (§Phase 8) — BTOR2 init-line smart defaults for cells
    // WITHOUT sidecar entries. Opt-in via `MUNUNU_BTOR2_SMART_INIT_DEFAULTS=1`.
    // Default OFF — full bit-blast remains the cap-safe legacy
    // behaviour. When enabled, each state cell with NO sidecar entry
    // AND a BTOR2 `Init` line gets a synthetic `EnumValues` entry
    // pinning it to its init value (1-state abstraction instead of
    // `2^width`).
    //
    // SOUNDNESS: collapsing an unsidecared cell to its init value is
    // an **under-approximation** of the design — sound for liveness
    // ("if reset state alone reaches the property, the property is
    // reachable in the real design too"), **unsound for safety**
    // ("property violations that depend on the cell deviating from
    // its init value are silently masked"). The opt-in surfaces this
    // tradeoff explicitly; users enable it when they want to ignore
    // cells they haven't bothered to declare AND accept the
    // soundness implication. Mirrors R-Y1's env-var pattern.
    apply_btor2_init_smart_defaults(
        file,
        &state_meta,
        &symbols,
        &mut cell_domains,
        warnings,
        options,
    );

    // R-S2a (§Phase 9 §9.1) — BTOR2 init-line seeding. For every
    // state cell with an `Init` line in the BTOR2 source AND a
    // sidecar-declared abstraction that takes per-value discriminators
    // (`EnumValues` with a value_map), inject the init value as an
    // additional discriminator if not already present. Bridges the
    // gap when R-S5 / R-S3 / R-S7 don't cover the init value of a
    // sidecar-declared signal (rare on typedef-typed Caliptra-style
    // designs; common on hand-written non-typedef RTL where the
    // reset value is the only "interesting" value before any input
    // arrives). Strictly additive — never replaces existing entries.
    apply_btor2_init_seeding(file, &state_meta, &symbols, &mut cell_domains);

    // R-S2b.6 (§Phase 9 §9.1) — Verilator reset-simulation seeding.
    // Fires when the sidecar declares a `simulate_reset` block AND
    // the caller provided `AdapterOptions::sv_source_path` AND a
    // Verilator binary is discoverable. On any precondition failing
    // (no sidecar, no SV path, sidecar parse error, no simulate_reset
    // block), the labeled-break exits the block cheaply — falls
    // through silently. See `apply_simulate_reset_seeding` for the
    // full graceful-fallback contract once the simulation does run.
    'simulate_reset_seed: {
        let Some(json) = &options.sidecar_json else {
            break 'simulate_reset_seed;
        };
        let Some(sv_path) = &options.sv_source_path else {
            break 'simulate_reset_seed;
        };
        let Ok(ann) =
            serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
        else {
            break 'simulate_reset_seed;
        };
        let Some(sim_decl) = &ann.simulate_reset else {
            break 'simulate_reset_seed;
        };
        let sim_config = sim_decl.to_reset_sim_config(ann.module.clone());
        let nid_widths: Vec<(Nid, u32)> = state_meta.iter().map(|sm| (sm.nid, sm.width)).collect();
        apply_simulate_reset_seeding(
            &sim_config,
            sv_path,
            &nid_widths,
            &symbols,
            &mut cell_domains,
            warnings,
        );
    }

    // R-S6.6 (§Phase 9 §9.1) — VCD trace-mining seeding. Walks
    // every `SvAnnotation::vcd_traces` entry; for each, reads the
    // declared VCD file (path resolved against
    // `AdapterOptions::sidecar_path`'s parent dir for relative
    // paths) and mines per-signal heavy-hitter + boundary values
    // into cell_domains via R-S6.5's seeding helper. See
    // `apply_vcd_trace_seeding` for the per-trace graceful-
    // fallback contract.
    'vcd_trace_seed: {
        let Some(json) = &options.sidecar_json else {
            break 'vcd_trace_seed;
        };
        let Ok(ann) =
            serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
        else {
            break 'vcd_trace_seed;
        };
        if ann.vcd_traces.is_empty() {
            break 'vcd_trace_seed;
        }
        let sidecar_dir = options.sidecar_path.as_deref().and_then(|p| p.parent());
        let nid_widths: Vec<(Nid, u32)> = state_meta.iter().map(|sm| (sm.nid, sm.width)).collect();
        for trace in &ann.vcd_traces {
            apply_vcd_trace_seeding(
                trace,
                sidecar_dir,
                &nid_widths,
                &symbols,
                &mut cell_domains,
                warnings,
            );
        }
    }

    let cells = CellEnumeration::build(&state_meta, &cell_domains);

    // Phase 1.6: per-input sidecar resolution. The sidecar already
    // carries `inputs: [...]` entries via `SvAnnotation::inputs` (see
    // `crate::adapter::sidecar::btor2_resolver::build_input_field_domains`).
    // Without this, every input enumerates over its full bit-vector
    // width, which blocks real designs at the input-bit cap. With it,
    // an input declared `Ignored` / `Boolean` / `EnumValues` / etc.
    // collapses to a 1-of-N value list and the enumerator sees only
    // the meaningful combinations.
    let mut input_domains = build_input_domains(&input_meta, &symbols, options, warnings)?;
    // Clock inputs are auto-pinned by the bit-blaster regardless; the
    // partition's Dropped classification on a clock would be redundant
    // but harmless. We filter clocks out here only to avoid an
    // unnecessary "auto-COI dropped 'clk'" warning. F1: reset inputs are
    // likewise pinned (inactive) regardless, so exclude them too.
    apply_partition_drops(
        &mut input_domains,
        input_meta
            .iter()
            .filter(|m| !m.is_clock && m.is_reset.is_none())
            .map(|m| (m.nid, m.symbol.as_str())),
        &partition,
        warnings,
        "input port",
    );
    let input_cells = InputCellEnumeration::build(&input_meta, &input_domains);

    // Effective input-bit cap. The previous cap (`MAX_INPUT_BITS`)
    // tested raw bit width and ran before sidecars resolved; that
    // rejected designs where most of the input width is masked off by
    // an abstraction. Now the cap tests the enumerated cardinality.
    let total_input_combos = input_cells.total_combos();
    let cap_combos: usize = 1usize << MAX_INPUT_BITS;
    if total_input_combos > cap_combos {
        return Err(AdapterError {
            kind: AdapterErrorKind::StateSpaceOverflow,
            message: format!(
                "BTOR2 design enumerates {total_input_combos} input combinations per step \
                 (max supported: 2^{MAX_INPUT_BITS} = {cap_combos}). Add `.mununu.json` \
                 `inputs[]` entries declaring `Ignored` / `Boolean` / `EnumValues` per \
                 non-essential input to prune the enumeration."
            ),
            location: None,
        });
    }
    let total_state_combos = cells.total_combos();

    let state_names = enumerate_state_names(total_state_combos);

    // Initial state(s) derived from `init` lines.
    //
    // Path 3 / Option A (§Phase 8 §8.2 residual closure): when a state
    // cell has NO `Node::Init` line (which is how Yosys's
    // `(* anyconst *)` attribute survives lowering to BTOR2), its
    // initial value is **nondeterministic** — under R-Y2 + R-S5, every
    // value in the cell's declared abstraction is an admissible init
    // sample. The bit-blaster used to silently default such cells to
    // zero, producing a single abstract initial state that hid the
    // anyconst nondeterminism from the verdict (the residual the Path 3
    // design note flagged on the Caliptra fixture).
    //
    // Stage 1: detect nondeterministic cells (no `Init` line in the
    // BTOR2 file).
    // Stage 2: enumerate the cartesian product of {deterministic init
    // value for each cell WITH init} × {all admissible values for each
    // cell WITHOUT init}, encode each combination as a state index.
    //
    // To bound state explosion when many anyconst cells coexist, the
    // cartesian product is capped at `MAX_INITIAL_STATES = 256`. On
    // cap exceedance, fall back to the legacy single-init behaviour
    // and emit a soundness warning so the user can narrow the
    // abstraction set or limit the per-signal init policy.
    const MAX_INITIAL_STATES: usize = 256;
    // Path 3 — restrict the nondeterministic-init set to the cells
    // the user EXPLICITLY marked `init_policy: anyconst` via R-Y2
    // sidecar. Yosys emits many cells without `Init` lines (reset
    // synchronizers, FSM intermediates, undef bits under
    // `setundef -anyseq/-anyconst` global policy); enumerating all
    // of them as initial blows up immediately. Surgical anyconst
    // declarations are the user's signal of intent.
    let anyconst_symbols = sidecar_anyconst_symbols(options);
    let nondet_init_nids =
        nondeterministic_init_cells(file, &state_meta, &symbols, &anyconst_symbols);

    // SOUNDNESS (§Phase 8 §8.2 — Path 3 follow-up): warn when state
    // cells silently default to zero. A cell without an `Init` line in
    // the BTOR2 source AND without a sidecar `init_policy: anyconst`
    // declaration is being deterministically pinned to zero by the
    // legacy init path. This is sound iff Yosys ran with
    // `setundef -zero` (the default) which explicitly zeroes undef
    // bits. Under `setundef -anyseq/-anyconst` (R-Y1 global flag or
    // R-Y2 per-signal attribute on a different cell) the cell's
    // initial value is nondeterministic, and defaulting to zero hides
    // a class of reset samples from the verdict.
    //
    // We cannot tell from BTOR2 alone which policy was used (Yosys
    // does not annotate the `state` lines with provenance), so this
    // warning is conservative: it fires whenever the situation
    // exists, and the user decides whether their flow makes it safe.
    let uncovered_uninit_signals: Vec<String> =
        uncovered_uninit_cells(file, &state_meta, &symbols, &anyconst_symbols);
    if !uncovered_uninit_signals.is_empty() {
        // Cap the listed names so the warning stays readable on large designs.
        let shown: Vec<&str> = uncovered_uninit_signals
            .iter()
            .take(5)
            .map(String::as_str)
            .collect();
        let suffix = if uncovered_uninit_signals.len() > shown.len() {
            format!(
                " (and {} more)",
                uncovered_uninit_signals.len() - shown.len()
            )
        } else {
            String::new()
        };
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: format!(
                "{n} state cell(s) without explicit `Init` line default to zero: \
                 {names}{suffix}. SOUND only if upstream Yosys ran with \
                 `setundef -zero` (the default). Under `setundef -anyseq/-anyconst` \
                 these cells' initial values are nondeterministic; declare them in \
                 the sidecar with `init_policy: anyconst` so Path 3 (§Phase 8 §8.2) \
                 enumerates their admissible initial values.",
                n = uncovered_uninit_signals.len(),
                names = shown.join(", "),
                suffix = suffix
            ),
            location: None,
        });
    }

    let init_state_indices: Vec<usize> = {
        let mut init_env = make_initial_env(file, &state_meta, &input_meta, true);
        evaluate_pure(file, &mut init_env, /*honor_init=*/ true)?;
        // R-Y6 (§Phase 8) — reset-sequence-aware init. When the
        // sidecar declares `reset_sequence: {...}`, run K cycles of
        // reset-asserted simulation before enumerating initial states.
        // The state after the K-cycle hold becomes the effective
        // initial state. No-op when the sidecar lacks the field.
        apply_reset_sequence(
            file,
            &mut init_env,
            &state_meta,
            &input_meta,
            &symbols,
            options,
        )?;
        // F1.1 — auto-reset initial state for async-reset designs with no
        // BTOR2 Init line (no-op when reset_sequence already handled init,
        // no reset is detected, or any cell has an Init line).
        apply_auto_reset(file, &mut init_env, &state_meta, &input_meta, options)?;
        // R-Y4 — bounded-init overrides from the sidecar.
        let bounded_init_overrides = sidecar_bounded_init_overrides(options, &state_meta, &symbols);
        let combos = enumerate_initial_combos(
            &init_env,
            &state_meta,
            &cells,
            &nondet_init_nids,
            &bounded_init_overrides,
        );
        if combos.len() > MAX_INITIAL_STATES {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "anyconst-driven initial state set has {n} entries (cap {cap}); \
                     falling back to single-init state — verdict at init is unsound \
                     for the nondeterministic init samples beyond the first.",
                    n = combos.len(),
                    cap = MAX_INITIAL_STATES
                ),
                location: None,
            });
            let single = encode_state(&init_env, &state_meta, &cells).unwrap_or(0);
            vec![single]
        } else if combos.is_empty() {
            // Fallback to legacy: no nondet cells, single init state.
            let single = encode_state(&init_env, &state_meta, &cells).unwrap_or(0);
            vec![single]
        } else {
            combos
        }
    };
    // State-splitting over property-referenced combinational signals
    // (S-track KMTS-fidelity). Each register-state is split into one
    // variant per JOINTLY-achievable assignment of the property-referenced
    // combinational signals (over admissible inputs), so a joint property
    // like `!(sel_a_T && sel_b_T)` reasons over the per-input joint
    // (sel_a, sel_b) — which a per-signal ∃ labeling cannot express. With
    // k=0 combinational signals the split is a no-op (one variant per
    // register-state) — byte-identical to the register-only model.
    //
    // SOUNDNESS (MIG-2): a next-state outside the enumerated abstraction
    // routes to the [`OOB_SINK_KEY`] sink (absorbing, evaluator-masked) —
    // a sound over-approximation (escape ⇒ falsifies safety), matching
    // native `kripke.rs`; previously such transitions were dropped (an
    // unsound under-approximation).
    let comb_candidates = property_combinational_candidate_names(options);
    let mut comb_nids = combinational_signal_nids(&comb_candidates, file, &state_meta, &input_meta);
    // R-MM-4b: also surface explicitly-requested net-driving output ports
    // (their value-node from the BTOR2 `output` line). Union into the
    // combinational-signal set (dedup by name) so the existing aggregation +
    // state-splitting + `build_state_valuations` surface them per state — the
    // values the multi-module driver turns into `net_<v>` rendezvous labels.
    for (nid, name) in output_port_nids(&options.surface_output_ports, file) {
        if !comb_nids.iter().any(|(_, n)| n == &name) {
            comb_nids.push((nid, name));
        }
    }
    // Cap the split factor at 2^COMB_SPLIT_CAP variants per register-state.
    const COMB_SPLIT_CAP: usize = 8;
    if comb_nids.len() > COMB_SPLIT_CAP {
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: format!(
                "{n} property-referenced combinational signals exceed the \
                 state-splitting cap ({cap}); only the first {cap} are split \
                 (joint) — joint/`_F_` properties over the excess may be \
                 unsound. Reduce the property's combinational signal count.",
                n = comb_nids.len(),
                cap = COMB_SPLIT_CAP
            ),
            location: None,
        });
        comb_nids.truncate(COMB_SPLIT_CAP);
    }
    let comb_signal_names: Vec<String> = comb_nids.iter().map(|(_, n)| n.clone()).collect();
    let base_total = state_names.len();
    let mut oob_escapes: usize = 0;

    // Pass 1 — per (register-state, input): admissibility, the joint
    // combinational mask, and the next register-state (or OOB). One
    // evaluate_pure per pair; results reused by Pass 2 (no re-eval).
    struct StepInfo {
        cv: u32,
        target: Option<usize>,
    }
    let mut step: Vec<Vec<Option<StepInfo>>> = Vec::with_capacity(base_total);
    let mut achievable: Vec<std::collections::BTreeSet<u32>> = Vec::with_capacity(base_total);

    // P1 #4 Phase 2c (Q2 = B) — route the concrete step through the STS-IR
    // `StepEval` primitive (prepared ONCE here so the hot loop carries no
    // per-step `collect_symbols` cost). The prepared stepper's state /
    // input lists are `file.states()` / `file.inputs()` order — the SAME
    // order as `state_meta` / `input_meta` — so position `i` refers to the
    // same cell and the concrete values come from `cells` / `input_cells`
    // by position, keyed for the step by the stepper's step-names. The
    // abstraction policy (domain-encoding via `cells.encode`, OOB sink on
    // escape, combinational state-splitting via `cv`) stays here in the
    // enumerator — the seam never sees `FieldDomain`.
    let prep = PreparedStep::new(file);
    debug_assert_eq!(
        prep.states().len(),
        state_meta.len(),
        "PreparedStep / state_meta state-count mismatch"
    );
    debug_assert_eq!(
        prep.inputs().len(),
        input_meta.len(),
        "PreparedStep / input_meta input-count mismatch"
    );
    for base in 0..base_total {
        let mut row: Vec<Option<StepInfo>> = Vec::with_capacity(total_input_combos);
        let mut ach: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        // Concrete register valuation for this cube (by step-name); constant
        // across input combos for this base.
        let register_values: std::collections::HashMap<String, u128> = prep
            .states()
            .iter()
            .enumerate()
            .map(|(i, sv)| (sv.symbol.clone(), cells.value_at(base, i)))
            .collect();
        for input_idx in 0..total_input_combos {
            let input_values: std::collections::HashMap<String, u128> = prep
                .inputs()
                .iter()
                .enumerate()
                .map(|(i, sv)| (sv.symbol.clone(), input_cells.value_at(input_idx, i)))
                .collect();
            let outcome = prep.step(&register_values, &input_values, &comb_signal_names)?;
            if !outcome.admissible {
                row.push(None);
                continue;
            }
            let mut cv: u32 = 0;
            for (j, name) in comb_signal_names.iter().enumerate() {
                if outcome.observed.get(name).is_some_and(|&v| v != 0) {
                    cv |= 1u32 << j;
                }
            }
            ach.insert(cv);
            // Encode the next register-state into the abstract cube (OOB →
            // None). Values in `state_meta` / `prep.states()` position order.
            let next_values: Vec<u128> = prep
                .states()
                .iter()
                .map(|sv| outcome.next_state.get(&sv.symbol).copied().unwrap_or(0))
                .collect();
            let target = cells.encode(&next_values);
            if target.is_none() {
                oob_escapes += 1;
            }
            row.push(Some(StepInfo { cv, target }));
        }
        // Every register-state needs ≥1 variant (a base with no admissible
        // input is still a valid transition target / deadlock state).
        if ach.is_empty() {
            ach.insert(0);
        }
        step.push(row);
        achievable.push(ach);
    }

    // Split-state enumeration (base-major). For k=0, achievable[base]={0}
    // → one variant per base → split_idx == base → names s0..s{n-1}
    // (identical to the register-only model).
    let mut split_of: Vec<std::collections::BTreeMap<u32, usize>> = (0..base_total)
        .map(|_| std::collections::BTreeMap::new())
        .collect();
    let mut split_base: Vec<usize> = Vec::new();
    let mut split_cv: Vec<u32> = Vec::new();
    for (base, set) in achievable.iter().enumerate() {
        for &cv in set {
            let idx = split_base.len();
            split_of[base].insert(cv, idx);
            split_base.push(base);
            split_cv.push(cv);
        }
    }
    let split_total = split_base.len();
    let split_names: Vec<String> = (0..split_total).map(|i| format!("s{i}")).collect();

    // Initial split-states: each initial register-state splits into all of
    // its achievable combinational variants (the initial combinational
    // value is set by the initial input, which is free).
    let init_split_set: std::collections::HashSet<usize> = init_state_indices
        .iter()
        .flat_map(|&base| achievable[base].iter().map(move |cv| (base, *cv)))
        .filter_map(|(base, cv)| split_of[base].get(&cv).copied())
        .collect();

    // Pass 2 — transitions over split-states. From variant (base, cv) on
    // an input `i` whose joint combinational mask equals `cv` (source
    // consistency), go to register-state `base'` and fan out to EACH
    // achievable variant of `base'` (the target's combinational value is
    // set by the next cycle's free input). OOB targets route to the sink.
    let mut transitions: Vec<TransitionSpec> = Vec::new();
    for split_idx in 0..split_total {
        let base = split_base[split_idx];
        let cv = split_cv[split_idx];
        for (input_idx, info_opt) in step[base].iter().enumerate() {
            let Some(info) = info_opt else {
                continue;
            };
            if info.cv != cv {
                continue; // input inconsistent with this variant's comb value
            }
            let labels = signal_labels_for_input(input_idx, &input_meta, &input_cells);
            match info.target {
                None => transitions.push(TransitionSpec {
                    source: split_names[split_idx].clone(),
                    target: OOB_SINK_KEY.to_string(),
                    labels,
                    modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,
                    additional_targets: Vec::new(),
                }),
                Some(tbase) => {
                    for &cv2 in &achievable[tbase] {
                        let tsplit = split_of[tbase][&cv2];
                        transitions.push(TransitionSpec {
                            source: split_names[split_idx].clone(),
                            target: split_names[tsplit].clone(),
                            labels: labels.clone(),
                            modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,
                            additional_targets: Vec::new(),
                        });
                    }
                }
            }
        }
    }
    // If any transition escaped the abstraction, add the absorbing OOB
    // sink: a self-loop on every input combination (so it is reachable,
    // non-deadlocking, and absorbing). The sink's `__mununu_oob__`
    // valuation is added to `states_vec` below.
    if oob_escapes > 0 {
        for input_idx in 0..total_input_combos {
            transitions.push(TransitionSpec {
                source: OOB_SINK_KEY.to_string(),
                target: OOB_SINK_KEY.to_string(),
                labels: signal_labels_for_input(input_idx, &input_meta, &input_cells),
                modality: crate::context_dsl::ast::TransitionModalitySpec::Sharp,
                additional_targets: Vec::new(),
            });
        }
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: format!(
                "{oob_escapes} transitions led to a state outside the \
                 sidecar-declared abstraction and were routed to the OOB sink \
                 (over-approximation: sound for safety, the sink falsifies \
                 safety formulas). Widen bounds or run `mununu sv discover` to \
                 enlarge declared value sets and tighten the model."
            ),
            location: None,
        });
    }

    // Build signal list — inputs are labels, states are state-vars.
    // Clock inputs are excluded: each CLTS step is a clock edge, so the
    // clock is not a signal mununu reasons over. Including it would put
    // a useless `clk_0`/`clk_1` pair in the alphabet.
    //
    // F1: reset inputs are excluded for the same reason — they are
    // modeled as init-only (pinned inactive at runtime), matching the
    // native pipeline's alphabet (no `rst` label).
    let mut signals: Vec<Signal> = Vec::new();
    for im in input_meta
        .iter()
        .filter(|im| !im.is_clock && im.is_reset.is_none())
    {
        signals.push(Signal {
            name: im.symbol.clone(),
            kind: if im.controllable {
                SignalKind::Output
            } else {
                SignalKind::Input
            },
            domain: if im.width == 1 {
                SignalDomain::Boolean
            } else {
                SignalDomain::BoundedInt {
                    lower: 0,
                    upper: (1i64 << im.width.min(31)) - 1,
                }
            },
            role: SignalRole::Label,
        });
    }
    for sm in &state_meta {
        signals.push(Signal {
            name: sm.symbol.clone(),
            kind: SignalKind::Neutral,
            domain: if sm.width == 1 {
                SignalDomain::Boolean
            } else {
                SignalDomain::BoundedInt {
                    lower: 0,
                    upper: (1i64 << sm.width.min(31)) - 1,
                }
            },
            role: SignalRole::State,
        });
    }

    let k = comb_signal_names.len();
    let mut states_vec: Vec<StateSpec> = (0..split_total)
        .map(|i| {
            let base = split_base[i];
            let cv = split_cv[i];
            let cv_bits: Vec<bool> = (0..k).map(|j| (cv >> j) & 1 == 1).collect();
            let vals = build_state_valuations(
                base,
                &state_meta,
                &cells,
                &cell_domains,
                &comb_signal_names,
                &cv_bits,
            );
            StateSpec {
                name: split_names[i].clone(),
                is_initial: init_split_set.contains(&i),
                valuations: if vals.is_empty() { None } else { Some(vals) },
                three_valued: None,
            }
        })
        .collect();
    // MIG-2 — declare the absorbing OOB sink (only when reached). Its
    // `__mununu_oob__ → "true"` valuation is the marker
    // `crate::mu_calculus::evaluator::compute_oob_bits` keys on to mask
    // it out of every formula (OOB-as-bottom → sound over-approximation).
    if oob_escapes > 0 {
        let mut oob_val = std::collections::BTreeMap::new();
        oob_val.insert(OOB_SINK_KEY.to_string(), "true".to_string());
        states_vec.push(StateSpec {
            name: OOB_SINK_KEY.to_string(),
            is_initial: false,
            valuations: Some(oob_val),
            three_valued: None,
        });
    }

    // With per-signal labels, controllability becomes per-signal-value:
    // every `<signal>_<value>` label belonging to a controllable input is
    // controllable; every label belonging to an uncontrollable input is
    // not. The compound-label era required marking the entire compound
    // controllable when any input was; the multi-label encoding makes
    // this finer-grained naturally.
    let controllable_labels: Vec<String> = input_meta
        .iter()
        .filter(|im| im.controllable)
        .flat_map(|im| (0..(1u128 << im.width.min(31))).map(move |v| format!("{}_{v}", im.symbol)))
        .collect();

    let automaton = AutomatonSpec {
        name: "Circuit".into(),
        states: states_vec,
        transitions,
        controllable_labels,
        internal_labels: vec![],
    };

    let mut properties = build_properties(
        file,
        &state_meta,
        &input_meta,
        &cells,
        &input_cells,
        &state_names,
    )?;
    // Append sidecar-declared mu-calculus properties. The custom SV
    // adapter (which constructs its own SvAnnotation flow) already
    // honours `SvAnnotation::properties`; this mirrors the same shape
    // on the BTOR2 / sv-yosys path so a user's `.mununu.json` can
    // carry hand-authored property formulas alongside abstractions.
    properties.extend(sidecar_properties(options));

    // Phase A.3 step 3.6 — partition summary. Pair the partition's
    // per-signal classification with the bit-blaster's known widths
    // so the user can read `state_bits_before` / `state_bits_after`
    // as observable evidence of COI's reduction effect.
    let widths: std::collections::HashMap<String, usize> = state_meta
        .iter()
        .map(|m| (m.symbol.clone(), m.width as usize))
        .chain(
            input_meta
                .iter()
                .map(|m| (m.symbol.clone(), m.width as usize)),
        )
        .collect();
    let mut summary =
        crate::adapter::partition::PartitionSummary::from_partition(&partition, Some(&widths));

    // R4W-2 (R.4 clustered-COI wiring) — when the caller threaded the
    // manifest's per-property COI seeds, compute the joint-vs-clustered
    // cone comparison over *this* module's BTOR2 dep graph and surface
    // it on the summary. Pure telemetry: it reports what a per-cluster
    // bit-blast could save vs the naive joint COI (the M.3 reduction
    // metric); it does not change which signals the partition keeps.
    // Skipped entirely when `property_seeds` is empty (the legacy
    // intrinsic-seed-only path), so there is no behaviour change for
    // existing single-property / no-manifest runs.
    if !options.property_seeds.is_empty() {
        use crate::adapter::partition::{DepGraphBuilder, coi};
        // R4W-3 — honour the caller's Jaccard floor
        // (`AdapterOptions::cluster_similarity_floor`, threaded from
        // `VerifyConfig` / the CLI flag / the API request field). `None`
        // falls back to the recommended 0.5 default (R4W-2 behaviour;
        // see `coi::cluster_coi_report` docs).
        const DEFAULT_CLUSTER_SIMILARITY_FLOOR: f64 = 0.5;
        let floor = options
            .cluster_similarity_floor
            .unwrap_or(DEFAULT_CLUSTER_SIMILARITY_FLOOR);
        let deps = file.build();
        // R4W-3.5b — resolve each property atom to the state/input
        // terminals in its combinational fan-in before seeding the COI.
        // A property typically names a combinational *output* (e.g.
        // `main_sm_err_o`), which is neither a dep-graph key nor reached
        // by any `next` — seeding the cone with the bare atom yields a
        // degenerate size-1 cone. `resolve_atom_to_terminals` walks the
        // atom's defining expression down to the registers/inputs it
        // depends on (the same symbols `build()` keys on), giving the
        // cone a real foothold. Atoms the BTOR2 doesn't name fall back
        // to the bare-atom seed (pre-R4W-3.5b behaviour).
        let props: Vec<(String, std::collections::HashSet<String>)> = options
            .property_seeds
            .iter()
            .map(|(name, atoms)| {
                let mut seeds = std::collections::HashSet::new();
                for atom in atoms {
                    match super::dep_graph::resolve_atom_to_terminals(file, atom) {
                        Some(terminals) => seeds.extend(terminals),
                        None => {
                            seeds.insert(atom.clone());
                        }
                    }
                }
                (name.clone(), seeds)
            })
            .collect();
        summary.cluster_coi = Some(coi::cluster_coi_report(&props, &deps, floor));
    }
    let partition_summary = Some(summary);

    Ok(BlastOutput {
        signals,
        properties,
        automaton,
        partition_summary,
    })
}

/// Parse the sidecar's `properties[]` array (if any) into the
/// adapter-IR `PropertySpec` shape. Malformed entries are logged as
/// adapter warnings — same permissive policy as `build_cell_domains`.
fn sidecar_properties(options: &AdapterOptions) -> Vec<PropertySpec> {
    let Some(json) = &options.sidecar_json else {
        return vec![];
    };
    let annotation =
        match serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
        {
            Ok(a) => a,
            Err(_) => return vec![],
        };
    annotation
        .properties
        .into_iter()
        .filter_map(|p| {
            // Prefer an inline formula; template_ref support is the
            // custom SV adapter's responsibility (not the BTOR2 path).
            let body = p.formula?;
            Some(PropertySpec {
                name: p.id,
                kind: PropertyKind::Safety,
                formula: PropertyFormula::MuCalculus(body),
                role: match p.role.as_str() {
                    "assumption" => PropertyRole::Assumption,
                    "standalone" => PropertyRole::Standalone,
                    _ => PropertyRole::Guarantee,
                },
                over: None,
                description: p.description,
            })
        })
        .collect()
}

fn build_properties(
    file: &Btor2File,
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
    cells: &CellEnumeration,
    input_cells: &InputCellEnumeration,
    state_names: &[String],
) -> Result<Vec<PropertySpec>, AdapterError> {
    let mut out = Vec::new();
    let total_input_combos = input_cells.total_combos();

    // Bad properties → safety: nu X. (!(bad-states) && [] X)
    for (i, line) in file.bads().enumerate() {
        let signal = match &line.node {
            Node::Bad { signal } => *signal,
            _ => unreachable!(),
        };
        let mut hit = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            let mut found = false;
            for input_idx in 0..total_input_combos {
                let mut env = make_step_env(
                    state_meta,
                    input_meta,
                    cells,
                    input_cells,
                    state_idx,
                    input_idx,
                );
                evaluate_pure(file, &mut env, /*honor_init=*/ false)?;
                if !constraints_hold(file, &env)? {
                    continue;
                }
                if read_operand(&env, signal)
                    .map(|v| v.to_bool())
                    .unwrap_or(false)
                {
                    found = true;
                    break;
                }
            }
            if found {
                hit.push(state_name.clone());
            }
        }
        if !hit.is_empty() {
            let pred = hit.join(" || ");
            out.push(PropertySpec {
                name: format!("safety_bad_{i}"),
                kind: PropertyKind::Safety,
                formula: PropertyFormula::MuCalculus(format!("nu X. ((!({pred})) && ([] X))")),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    // Justice properties → liveness: nu Y. ((mu X. (pred || <> X)) && ([] Y))
    for (i, line) in file.justices().enumerate() {
        let signals = match &line.node {
            Node::Justice { signals } => signals.clone(),
            _ => unreachable!(),
        };
        let mut hit = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            let mut found = false;
            for input_idx in 0..total_input_combos {
                let mut env = make_step_env(
                    state_meta,
                    input_meta,
                    cells,
                    input_cells,
                    state_idx,
                    input_idx,
                );
                evaluate_pure(file, &mut env, /*honor_init=*/ false)?;
                if !constraints_hold(file, &env)? {
                    continue;
                }
                let all_true = signals
                    .iter()
                    .all(|s| read_operand(&env, *s).map(|v| v.to_bool()).unwrap_or(false));
                if all_true {
                    found = true;
                    break;
                }
            }
            if found {
                hit.push(state_name.clone());
            }
        }
        if !hit.is_empty() {
            let pred = hit.join(" || ");
            out.push(PropertySpec {
                name: format!("justice_{i}"),
                kind: PropertyKind::Fairness,
                formula: PropertyFormula::MuCalculus(format!(
                    "nu Y. ((mu X. (({pred}) || (<> X))) && ([] Y))"
                )),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    // Fair → similar to a single-element justice set.
    for (i, line) in file.fairs().enumerate() {
        let signal = match &line.node {
            Node::Fair { signal } => *signal,
            _ => unreachable!(),
        };
        let mut hit = Vec::new();
        for (state_idx, state_name) in state_names.iter().enumerate() {
            let mut found = false;
            for input_idx in 0..total_input_combos {
                let mut env = make_step_env(
                    state_meta,
                    input_meta,
                    cells,
                    input_cells,
                    state_idx,
                    input_idx,
                );
                evaluate_pure(file, &mut env, /*honor_init=*/ false)?;
                if !constraints_hold(file, &env)? {
                    continue;
                }
                if read_operand(&env, signal)
                    .map(|v| v.to_bool())
                    .unwrap_or(false)
                {
                    found = true;
                    break;
                }
            }
            if found {
                hit.push(state_name.clone());
            }
        }
        if !hit.is_empty() {
            let pred = hit.join(" || ");
            out.push(PropertySpec {
                name: format!("fairness_{i}"),
                kind: PropertyKind::Fairness,
                formula: PropertyFormula::MuCalculus(format!(
                    "nu Y. ((mu X. (({pred}) || (<> X))) && ([] Y))"
                )),
                role: PropertyRole::Standalone,
                over: None,
                description: None,
            });
        }
    }

    Ok(out)
}

fn constraints_hold(file: &Btor2File, env: &Env) -> Result<bool, AdapterError> {
    for line in file.constraints() {
        let signal = match &line.node {
            Node::Constraint { signal } => *signal,
            _ => unreachable!(),
        };
        let v = read_operand(env, signal).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::IrConsistencyError,
            message: format!(
                "constraint NID {} references unevaluated signal {}",
                line.nid,
                signal.nid()
            ),
            location: Some(SourceLocation {
                line: line.source_line,
                column: 0,
            }),
        })?;
        if !v.to_bool() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_next(
    file: &Btor2File,
    env: &mut Env,
    state_meta: &[StateMeta],
) -> Result<(), AdapterError> {
    // First gather (state_nid, next-value) pairs to avoid mutating env mid-iteration.
    let mut updates: Vec<(Nid, Bv)> = Vec::new();
    for line in &file.lines {
        if let Node::Next { state, value, .. } = &line.node {
            let v = read_operand(env, *value).ok_or_else(|| AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: format!("next NID {} references unevaluated signal", line.nid),
                location: Some(SourceLocation {
                    line: line.source_line,
                    column: 0,
                }),
            })?;
            updates.push((*state, v));
        }
    }
    for (state_nid, v) in updates {
        env.values.insert(state_nid, v);
    }
    // States without `next` keep their current value (BTOR2 convention).
    let _ = state_meta;
    Ok(())
}

// =====================================================================
// Environment + evaluator
// =====================================================================

#[derive(Debug, Clone, Default)]
pub(crate) struct Env {
    values: std::collections::HashMap<Nid, Bv>,
}

#[derive(Debug, Clone)]
struct StateMeta {
    nid: Nid,
    width: u32,
    symbol: String,
}

#[derive(Debug, Clone)]
struct InputMeta {
    nid: Nid,
    width: u32,
    symbol: String,
    controllable: bool,
    /// True when this input is a clock signal. Clock inputs are NOT
    /// enumerated in transitions — each CLTS step already represents one
    /// clock edge. The bit-blaster injects `value=1` (posedge active)
    /// during next-state evaluation so `clk2fflogic`-introduced edge
    /// detectors fire on every CLTS transition exactly once.
    is_clock: bool,
    /// F1 (S-track KMTS-fidelity, 2026-06-14) — `Some(active_high)` when
    /// this input is the design's async reset. Reset inputs are modeled
    /// like the native pipeline does: as *init-only*, NOT a free runtime
    /// transition. Concretely they are (a) pinned to their INACTIVE level
    /// for every runtime transition (like clocks are pinned, but to
    /// inactive rather than 1) and (b) excluded from the transition label
    /// alphabet. The reset-active behaviour is captured exactly once, as
    /// the initial state (see `apply_auto_reset`). Without this, Yosys's
    /// lowering of `posedge rst` makes `rst` a free input whose asserted
    /// edge returns every state to init — vacuously satisfying
    /// liveness/recovery properties and masking stuck-state bugs.
    is_reset: Option<bool>,
}

/// Parse `options.sidecar_json` (when present) and resolve each
/// state-cell symbol against its `signals[]` entries via the shared
/// resolver in [`crate::adapter::sidecar`]. Returns a NID-keyed map
/// from BTOR2 state-cell IDs to their declared [`FieldDomain`] plus
/// value-name map (for [`AbstractionType::EnumValues`] domains).
///
/// Sidecar parse errors are reported as adapter warnings, not hard
/// failures — a malformed sidecar should not break the CLI's auto-load
/// path. Cells without a sidecar entry simply do not appear in the
/// returned map; callers fall back to full-width bit-blast for those.
fn build_cell_domains(
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<CellDomainMap, AdapterError> {
    let Some(json) = &options.sidecar_json else {
        return Ok(std::collections::HashMap::new());
    };

    // Permissive parse: a malformed sidecar is a warning, not a fatal
    // adapter error — the caller can still bit-blast the design.
    let annotation = match serde_json::from_str::<
        crate::adapter::systemverilog::annotation::SvAnnotation,
    >(json)
    {
        Ok(a) => a,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::UnsupportedConstruct,
                message: format!(
                    "adapter/btor2: failed to parse .mununu.json sidecar ({e}); falling back to full bit-blast"
                ),
                location: None,
            });
            return Ok(std::collections::HashMap::new());
        }
    };

    // Restrict to state-cell symbols (don't pollute with input names).
    let state_symbols: std::collections::HashMap<i64, String> = state_meta
        .iter()
        .filter_map(|sm| symbols.get(&sm.nid).map(|s| (sm.nid, s.clone())))
        .collect();

    Ok(
        crate::adapter::sidecar::btor2_resolver::build_field_domains_for_btor2(
            &annotation,
            &state_symbols,
        ),
    )
}

/// Phase A.3 step 3.5 — inject `AbstractionType::Ignored` for signals
/// the partition classified `Dropped` *and* the user did not list in
/// the sidecar.
///
/// User-listed signals are identified by membership of their NID in
/// `domains` (the sidecar resolver only inserts entries for names that
/// appear in the sidecar's `signals[]` / `inputs[]` arrays). The
/// partition's verdict is keyed by symbol; we look up the symbol for
/// each NID via `iter` and skip anonymous signals (no symbol available).
///
/// SOUNDNESS: see `crate::adapter::partition` module docs. The injected
/// `Ignored` domain pins the signal to its initial value; the abstract
/// model thereby admits more behaviours than the concrete model — sound
/// for safety + over-approximation.
fn apply_partition_drops<'a>(
    domains: &mut CellDomainMap,
    iter: impl Iterator<Item = (i64, &'a str)>,
    partition: &crate::adapter::partition::Partition,
    warnings: &mut Vec<AdapterWarning>,
    kind: &str,
) {
    use crate::adapter::domain::{AbstractValue, AbstractionType, FieldDomain};
    use crate::adapter::partition::PartitionClass;

    for (nid, symbol) in iter {
        // User wins: an entry already keyed by NID in `domains` means
        // the sidecar listed this signal — trust the user.
        if domains.contains_key(&nid) {
            continue;
        }
        match partition.classes.get(symbol) {
            Some(PartitionClass::Dropped { reason }) => {
                warnings.push(AdapterWarning {
                    kind: WarningKind::ApproximateTranslation,
                    message: format!(
                        "auto-partition: {kind} '{symbol}' dropped ({reason}); add an explicit sidecar entry to override"
                    ),
                    location: None,
                });
                domains.insert(
                    nid,
                    (
                        FieldDomain {
                            name: symbol.to_string(),
                            abstraction: AbstractionType::Ignored,
                            bound: None,
                            lower_bound: None,
                            variants: None,
                            initial: AbstractValue::Counter(0),
                        },
                        Vec::new(),
                    ),
                );
            }
            _ => {
                // Kept (or absent for symbols outside the partition's
                // signal universe — clocks, anonymous synthesisers).
                // Leave `domains` untouched; the existing fall-through
                // (full bit-blast) applies.
            }
        }
    }
}

/// Recognize clock signals by name (case-insensitive). Single-clock
/// posedge designs are Phase 1's whole scope; multi-clock and negedge
/// will surface in Phase 3 / 4 when compositional decomposition lands.
fn looks_like_clock(symbol: &str) -> bool {
    let lower = symbol.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "clk" | "clock" | "ck" | "i_clk" | "clk_i" | "iclk" | "clki"
    )
}

/// F1.0 (S-track KMTS-fidelity, 2026-06-14) — heuristic async-reset
/// detection, mirroring [`looks_like_clock`]. Returns `Some(active_high)`
/// when `symbol` names a reset, with the active level inferred from the
/// conventional negation marker; `None` for non-reset names.
///
/// Why a name heuristic (like the clock one) rather than structural
/// BTOR2 analysis: the native pipeline knows the reset from the SV
/// `always_ff @(posedge clk or posedge rst)` syntax; Yosys lowers that
/// into an `ite` reset-mux, losing the syntactic marker. The codebase
/// already recovers the clock by name; reset follows the same precedent.
/// A sidecar `reset_sequence.reset_input` overrides this (see
/// [`sidecar_reset_signal`]). The allow-list is conservative to avoid
/// false-positives (mis-pinning a real input would drop behaviour);
/// a missed reset is safe (the fix simply doesn't apply).
fn looks_like_reset(symbol: &str) -> Option<bool> {
    let lower = symbol.to_ascii_lowercase();
    let core = lower
        .strip_prefix("i_")
        .or_else(|| lower.strip_prefix("o_"))
        .unwrap_or(lower.as_str());
    let core = core
        .strip_suffix("_i")
        .or_else(|| core.strip_suffix("_o"))
        .unwrap_or(core);
    let active_low = matches!(
        core,
        "rst_n" | "resetn" | "rstn" | "reset_n" | "arstn" | "arst_n" | "nrst" | "nreset" | "rst_ni"
    );
    let active_high = matches!(
        core,
        "rst" | "reset" | "arst" | "srst" | "rst_i" | "reset_i"
    );
    if active_low {
        Some(false)
    } else if active_high {
        Some(true)
    } else {
        None
    }
}

/// F1.0 — the sidecar-declared reset signal, if any: returns
/// `(reset_input_name, active_high)` from a `reset_sequence` block. This
/// overrides the [`looks_like_reset`] name heuristic. `active_high` is
/// derived from `asserted_value` (non-zero ⇒ active-high).
fn sidecar_reset_signal(options: &AdapterOptions) -> Option<(String, bool)> {
    let json = options.sidecar_json.as_ref()?;
    let ann = serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
        .ok()?;
    let seq = ann.reset_sequence.as_ref()?;
    Some((seq.reset_input.clone(), seq.asserted_value != 0))
}

fn sort_of(node: &Node) -> Nid {
    match node {
        Node::Input { sort, .. } | Node::State { sort, .. } => *sort,
        _ => panic!("sort_of called on non-input/state"),
    }
}

/// Per-state-cell sidecar resolution: NID → (declared abstraction,
/// integer→variant-name map). Built by [`build_cell_domains`] and
/// consumed by [`CellEnumeration::build`] / [`build_state_valuations`].
type CellDomainMap =
    std::collections::HashMap<i64, (crate::adapter::domain::FieldDomain, Vec<(String, i64)>)>;

/// Per-state-cell enumeration plan. Drives state-space size, decode
/// (combo → per-cell concrete bit-vector values), and encode (per-cell
/// values → combo).
///
/// Each cell carries a `Vec<u128>` of allowed concrete values:
/// - **No sidecar entry**: full bit-blast — `0..(1 << cell.width)`.
/// - **Sidecar `BoundedCounter` (bound N)**: `0..=N`.
/// - **Sidecar `Boolean`**: `[0, 1]`.
/// - **Sidecar `EnumValues` / `Discover`**: the `value_map`'s integer
///   values, plus `0` as the catch-all when no exact match.
/// - **Sidecar `Ignored`**: a single value `0` (the cell is pinned).
///
/// The total state space is the **product** of per-cell value-set sizes
/// — mixed-radix encoding, not bit-packed. Replaces the prior flat
/// `0..(1 << total_state_bits)` enumeration which inflated to
/// `MAX_STATE_BITS = 16` for any width-3 register.
struct CellEnumeration {
    /// Per state cell, the concrete bit-vector values to enumerate.
    /// Indexed by state-cell index in `state_meta` order.
    per_cell: Vec<Vec<u128>>,
    /// Per state cell, cached `per_cell[i].len()` (the radix base).
    radices: Vec<usize>,
    /// Per state cell, the position within `per_cell[i]` of the enum's
    /// **catch-all variant** (the first declared variant with no
    /// `value_map` entry), or `None` when the cell has no catch-all.
    /// When `encode` sees an `EnumValues` value outside the declared
    /// set, it clamps to this position instead of returning `None`
    /// (OOB). Mirrors native `kripke.rs` `clamp_to_domain`'s
    /// "value not in value_map → first unmapped variant" rule —
    /// S-track clamp-everywhere (2026-06-14), increment B.
    catch_all: Vec<Option<usize>>,
}

impl CellEnumeration {
    fn build(state_meta: &[StateMeta], cell_domains: &CellDomainMap) -> Self {
        let per_cell: Vec<Vec<u128>> = state_meta
            .iter()
            .map(|sm| {
                if let Some((fd, vm)) = cell_domains.get(&sm.nid) {
                    Self::values_for_field_domain(fd, vm, sm.width)
                } else {
                    // Fallback: full bit-blast over the cell's BTOR2 width.
                    let cap = sm.width.min(31); // u128 safety
                    (0..(1u128 << cap)).collect()
                }
            })
            .collect();
        let radices: Vec<usize> = per_cell.iter().map(|v| v.len().max(1)).collect();
        // S-track clamp-everywhere (2026-06-14), increment B — per-cell
        // catch-all position. For an `EnumValues` cell with a catch-all
        // variant (a declared variant with no `value_map` entry), record
        // the position of that variant's value in `per_cell`; `encode`
        // clamps out-of-set values to it instead of routing to OOB.
        // `None` for non-enum cells, value_map-absent enums (native OOBs
        // those), and fully-mapped enums (no catch-all) — those keep the
        // OOB convention, matching native `kripke.rs` `clamp_to_domain`.
        let catch_all: Vec<Option<usize>> = state_meta
            .iter()
            .enumerate()
            .map(|(i, sm)| {
                cell_domains.get(&sm.nid).and_then(|(fd, vm)| {
                    Self::catch_all_value(fd, vm, sm.width)
                        .and_then(|cav| per_cell[i].iter().position(|x| *x == cav))
                })
            })
            .collect();
        CellEnumeration {
            per_cell,
            radices,
            catch_all,
        }
    }

    /// The concrete value of the enum's catch-all variant — the first
    /// declared variant with NO `value_map` entry — or `None` when the
    /// cell is not an `EnumValues` abstraction, has no `value_map`
    /// (native routes out-of-set values to OOB in that case), or maps
    /// every variant (no catch-all). Mirrors native `kripke.rs`
    /// `clamp_to_domain`. The catch-all's value follows increment A's
    /// default-by-index rule (an unmapped variant takes its declaration
    /// index as its concrete enum value), so it matches the value
    /// `values_for_field_domain` placed in `per_cell` for that variant.
    fn catch_all_value(
        fd: &crate::adapter::domain::FieldDomain,
        vm: &[(String, i64)],
        width: u32,
    ) -> Option<u128> {
        use crate::adapter::domain::{AbstractValue, AbstractionType};
        if fd.abstraction != AbstractionType::EnumValues || vm.is_empty() {
            return None;
        }
        let mask = if width >= 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let mapped: std::collections::HashSet<&str> = vm.iter().map(|(n, _)| n.as_str()).collect();
        fd.values()
            .iter()
            .enumerate()
            .find_map(|(idx, av)| match av {
                AbstractValue::Variant(name) if !mapped.contains(name.as_str()) => {
                    Some((idx as u128) & mask)
                }
                _ => None,
            })
    }

    fn values_for_field_domain(
        fd: &crate::adapter::domain::FieldDomain,
        vm: &[(String, i64)],
        width: u32,
    ) -> Vec<u128> {
        use crate::adapter::domain::{AbstractValue, AbstractionType};
        let mask = if width >= 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let to_concrete = |av: &AbstractValue| -> Option<u128> {
            match av {
                AbstractValue::Bool(b) | AbstractValue::Present(b) => Some(if *b { 1 } else { 0 }),
                AbstractValue::Counter(c) => {
                    if *c < 0 {
                        None
                    } else {
                        Some((*c as u128) & mask)
                    }
                }
                AbstractValue::Variant(name) => vm
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| (*v as u128) & mask),
            }
        };
        match fd.abstraction {
            AbstractionType::Ignored => vec![0u128],
            _ => {
                // S-track clamp-everywhere (2026-06-14), increment A —
                // default-by-index for unmapped enum variants. When a
                // sidecar enum variant has no `value_map` entry, assign
                // it the SV *default* enum value = its declaration index
                // (variant 0 → 0, variant 1 → 1, …). This mirrors the
                // native parser, which reads the encoding straight from
                // the typedef; the KMTS path otherwise can't (BTOR2
                // drops the variant names), so an absent value_map
                // collapsed the value set to `{0}` → the design's real
                // states escaped to the OOB sink → spurious `[]`-formula
                // failures (the S-track verdict divergence). Variants
                // WITH a value_map entry keep their explicit value; the
                // remaining unmapped variant in an otherwise-mapped enum
                // (the catch-all, e.g. cwe1245's UNDEF) is handled by the
                // encode()-time clamp (increment B).
                let mut values: Vec<u128> =
                    fd.values()
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, av)| {
                            to_concrete(av).or(matches!(av, AbstractValue::Variant(_))
                                .then_some((idx as u128) & mask))
                        })
                        .collect();
                if values.is_empty() {
                    values.push(0u128);
                }
                values.sort_unstable();
                values.dedup();
                values
            }
        }
    }

    fn total_combos(&self) -> usize {
        self.radices
            .iter()
            .fold(1usize, |a, r| a.saturating_mul(*r))
    }

    /// Decode a linear combo index into per-cell concrete values.
    /// Cell 0 is the lowest digit (changes fastest).
    #[allow(dead_code)]
    fn decode(&self, combo: usize) -> Vec<u128> {
        let mut rem = combo;
        let mut out = Vec::with_capacity(self.per_cell.len());
        for (i, cell_values) in self.per_cell.iter().enumerate() {
            let radix = self.radices[i];
            let pick = rem % radix;
            rem /= radix;
            out.push(*cell_values.get(pick).unwrap_or(&0));
        }
        out
    }

    /// Encode per-cell concrete values back to a linear combo index.
    /// Returns `None` if any value is not in its cell's allowed set
    /// AND the cell does not declare saturating semantics — this
    /// happens when the design's transition function lands a state
    /// outside the sidecar-declared abstraction (e.g., an `EnumValues`
    /// signal landing outside its variant set). The caller treats
    /// this as out-of-bounds and routes the transition to the OOB sink.
    ///
    /// The ONE exception is the enum **catch-all** clamp (S-track
    /// increment B): when the cell declares a catch-all variant (a
    /// declared variant with no `value_map` entry, e.g. cwe1245's
    /// `UNDEF`), an out-of-set value maps to that variant's position
    /// rather than OOB — mirroring native `kripke.rs` `clamp_to_domain`.
    /// This is sound because the catch-all variant + its `default:`
    /// handler over-approximate every invalid encoding uniformly.
    ///
    /// `BoundedCounter` overflow is NOT clamped (no saturate): an
    /// overflow has no sound in-model representative (saturating to the
    /// bound would falsely satisfy `count <= bound`), so it returns
    /// `None` → OOB, matching native's uniformly-sound OOB convention.
    /// (An S-track saturate-everywhere exploration was reverted
    /// 2026-06-14 — see `kripke.rs` `clamp_to_domain`'s SOUNDNESS note.)
    fn encode(&self, values: &[u128]) -> Option<usize> {
        let mut combo = 0usize;
        let mut multiplier = 1usize;
        for (i, &v) in values.iter().enumerate() {
            let radix = self.radices[i];
            let cell_values = &self.per_cell[i];
            // In-set → its position; else the enum catch-all position
            // (increment B) if the cell has one; else None → OOB.
            let idx = cell_values
                .iter()
                .position(|x| *x == v)
                .or(self.catch_all[i])?;
            combo = combo.checked_add(idx.checked_mul(multiplier)?)?;
            multiplier = multiplier.checked_mul(radix)?;
        }
        Some(combo)
    }

    /// Per-cell concrete value at this combo — for callers that need a
    /// single cell's value (e.g., env construction, valuations).
    fn value_at(&self, combo: usize, cell_idx: usize) -> u128 {
        let mut rem = combo;
        for (i, radix) in self.radices.iter().enumerate() {
            let pick = rem % radix;
            if i == cell_idx {
                return *self.per_cell[i].get(pick).unwrap_or(&0);
            }
            rem /= radix;
        }
        0
    }
}

/// Per-input enumeration plan — sibling to [`CellEnumeration`].
///
/// Each non-clock input carries a `Vec<u128>` of allowed concrete
/// values, sourced from the sidecar's `inputs[]` `FieldDomain`. Inputs
/// without a sidecar entry fall back to full bit-blast over their
/// declared BTOR2 width. **Clock inputs are pinned to a single value
/// (1, posedge active)** — they are not part of the enumerated input
/// space, matching the prior `combinations_of_inputs` semantics.
///
/// The total input-combination count is the product of per-input
/// value-set sizes. The bit-blaster's transition loops iterate
/// `0..total_combos()` and call [`Self::value_at`] per input nid.
struct InputCellEnumeration {
    per_input: Vec<Vec<u128>>,
    radices: Vec<usize>,
}

impl InputCellEnumeration {
    fn build(input_meta: &[InputMeta], input_domains: &CellDomainMap) -> Self {
        let per_input: Vec<Vec<u128>> = input_meta
            .iter()
            .map(|im| {
                if im.is_clock {
                    // Implicit clock: pinned to 1 (posedge active).
                    return vec![1u128];
                }
                // F1: a reset input is pinned to its INACTIVE level for
                // runtime transitions (active-high → 0, active-low → all
                // ones). The reset-active edge is captured only as the
                // initial state (apply_auto_reset), never as a runtime
                // recovery transition — matching the native pipeline and
                // keeping liveness/recovery verdicts sound.
                if let Some(active_high) = im.is_reset {
                    let mask = if im.width >= 128 {
                        u128::MAX
                    } else {
                        (1u128 << im.width) - 1
                    };
                    let inactive = if active_high { 0u128 } else { mask };
                    return vec![inactive];
                }
                if let Some((fd, vm)) = input_domains.get(&im.nid) {
                    CellEnumeration::values_for_field_domain(fd, vm, im.width)
                } else {
                    let cap = im.width.min(31);
                    (0..(1u128 << cap)).collect()
                }
            })
            .collect();
        let radices: Vec<usize> = per_input.iter().map(|v| v.len().max(1)).collect();
        InputCellEnumeration { per_input, radices }
    }

    fn total_combos(&self) -> usize {
        self.radices
            .iter()
            .fold(1usize, |a, r| a.saturating_mul(*r))
    }

    /// Per-input value at this combo for the input at `input_idx`.
    fn value_at(&self, combo: usize, input_idx: usize) -> u128 {
        let mut rem = combo;
        for (i, radix) in self.radices.iter().enumerate() {
            let pick = rem % radix;
            if i == input_idx {
                return *self.per_input[i].get(pick).unwrap_or(&0);
            }
            rem /= radix;
        }
        0
    }
}

/// Sidecar resolution for inputs — mirrors [`build_cell_domains`] but
/// scopes the symbol filter to input NIDs and delegates to
/// [`build_input_field_domains`](crate::adapter::sidecar::btor2_resolver::build_input_field_domains).
fn build_input_domains(
    input_meta: &[InputMeta],
    symbols: &std::collections::HashMap<i64, String>,
    options: &AdapterOptions,
    warnings: &mut Vec<AdapterWarning>,
) -> Result<CellDomainMap, AdapterError> {
    let Some(json) = &options.sidecar_json else {
        return Ok(std::collections::HashMap::new());
    };
    let annotation = match serde_json::from_str::<
        crate::adapter::systemverilog::annotation::SvAnnotation,
    >(json)
    {
        Ok(a) => a,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::UnsupportedConstruct,
                message: format!(
                    "adapter/btor2: failed to parse .mununu.json sidecar for inputs ({e}); falling back to full bit-blast"
                ),
                location: None,
            });
            return Ok(std::collections::HashMap::new());
        }
    };
    let input_symbols: std::collections::HashMap<i64, String> = input_meta
        .iter()
        .filter_map(|im| symbols.get(&im.nid).map(|s| (im.nid, s.clone())))
        .collect();
    Ok(
        crate::adapter::sidecar::btor2_resolver::build_input_field_domains(
            &annotation,
            &input_symbols,
        ),
    )
}

/// Enumerate state names as `s0`, `s1`, ..., `s{total-1}`.
///
/// Per-state register valuations are carried via [`StateSpec.valuations`]
/// (populated by [`build_state_valuations`]) so user-written formulas like
/// `state == 0` resolve via the predicate-via-valuations path in the
/// evaluator. The compound `s_<sym1>_<v1>__<sym2>_<v2>__...` form was
/// pure file-size cost — unique state names are all that's needed.
fn enumerate_state_names(total: usize) -> Vec<String> {
    if total == 0 {
        return vec!["s0".into()];
    }
    (0..total).map(|combo| format!("s{combo}")).collect()
}

/// For state-name index `combo`, build the valuations map keyed by the
/// user-named state cell symbol, with the cell's bit-vector value as a
/// decimal string. Synthetic state cells (no symbol annotation) are
/// dropped — the `chformal -lower` property-tracking latches are
/// uninteresting for properties the user would write.
///
/// The resulting BTreeMap is what mununu's evaluator queries via the
/// on-demand expression-evaluation path: `predicate_bits("state == 0")`
/// → parse as a guard → evaluate against valuations across all states →
/// return a bitset.
/// F2.1 (S-track KMTS-fidelity, 2026-06-14) — inverse of
/// [`CellEnumeration::values_for_field_domain`]'s value assignment: map a
/// concrete enum value back to its variant NAME. A variant's value is its
/// `value_map` entry, or — when unmapped (increment A default-by-index) —
/// its declaration index. `None` for non-`EnumValues` cells or values
/// with no matching variant.
///
/// Emitting the variant NAME (`state = ADDR_WAIT`) instead of the raw
/// integer (`state = 1`) is what lets a compound predicate
/// `bvalid_r_T_state_ADDR_WAIT` resolve against the valuation via
/// [`crate::context_dsl::state_matching`]'s compound matcher — restoring
/// the binding the native pipeline carries in its cross-product state
/// names. Crucially this works even when the sidecar has NO `value_map`
/// (variants-only, e.g. axilite/cwe1260/cwe1262): default-by-index gives
/// `state = 1 → ADDR_WAIT`.
fn variant_name_for_value(
    fd: &crate::adapter::domain::FieldDomain,
    vm: &[(String, i64)],
    width: u32,
    v: u128,
) -> Option<String> {
    use crate::adapter::domain::{AbstractValue, AbstractionType};
    if fd.abstraction != AbstractionType::EnumValues {
        return None;
    }
    let mask = if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    fd.values().iter().enumerate().find_map(|(idx, av)| {
        let AbstractValue::Variant(name) = av else {
            return None;
        };
        let assigned = vm
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, val)| (*val as u128) & mask)
            .unwrap_or((idx as u128) & mask);
        (assigned == v).then(|| name.clone())
    })
}

/// F2 combinational-output support (S-track KMTS-fidelity, 2026-06-14) —
/// candidate combinational-signal names referenced by sidecar property
/// formulas. A compound predicate `<sig>_T_state_VARIANT` (or `_F_`)
/// names a boolean signal `<sig>` whose value the property cares about;
/// we collect the `<sig>` prefixes so the bit-blaster can label states
/// with their achievable values. Registers and inputs among them are
/// filtered out by the caller — only genuine combinational nodes (e.g.
/// cwe1260's `overlap = uart_sel && aes_sel`, cwe1262's `bypass`) get
/// labeled. Returns the prefix before the first `_T_` / `_F_` marker of
/// each property-formula token that contains one.
fn property_combinational_candidate_names(
    options: &AdapterOptions,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Some(json) = &options.sidecar_json else {
        return out;
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return out;
    };
    for prop in &ann.properties {
        let Some(f) = &prop.formula else {
            continue;
        };
        for token in f.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let idx = match (token.find("_T_"), token.find("_F_")) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some(i) = idx
                && i > 0
            {
                out.insert(token[..i].to_string());
            }
        }
    }
    out
}

/// F2 combinational-output support — resolve the BTOR2 node NIDs for the
/// property-referenced combinational signals: a candidate name that maps
/// to a symbol on a node that is NOT a state cell and NOT an input (i.e.
/// a genuine combinational signal that survives the no-`opt` Yosys script
/// as a named node). These are labeled into the state valuations with
/// their ∃-over-inputs achievable value (see the transition loop's
/// aggregation + [`build_state_valuations`]).
fn combinational_signal_nids(
    candidates: &std::collections::HashSet<String>,
    file: &Btor2File,
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
) -> Vec<(Nid, String)> {
    // Combinational signals carry their name on an `Op` line (Yosys emits
    // `<nid> uext 1 <src> 0 <name>` for a 1-bit `assign` output). They are
    // NOT in `collect_symbols` (whose Pass 2 traces such aliases back to
    // *state* cells, not the Op node itself), so scan the BTOR2 directly.
    let state_nids: std::collections::HashSet<Nid> = state_meta.iter().map(|s| s.nid).collect();
    let input_nids: std::collections::HashSet<Nid> = input_meta.iter().map(|i| i.nid).collect();
    let mut out: Vec<(Nid, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::Op {
            symbol: Some(name), ..
        } = &line.node
            && candidates.contains(name.as_str())
            && !state_nids.contains(&line.nid)
            && !input_nids.contains(&line.nid)
            && seen.insert(name.clone())
        {
            out.push((line.nid, name.clone()));
        }
    }
    out
}

/// R-MM-4b — Resolve the BTOR2 value-node NID for each requested OUTPUT
/// PORT name. An output port carries its name on a `Node::Output` line
/// (`<nid> output <signal> <name>`); the value to surface is the
/// referenced `signal` node (e.g. a producer's `valid` output references
/// the `eq(state, …)` node). Returns `(value_nid, name)` pairs for the
/// requested names that exist as output ports of this module; names that
/// are not output ports here are silently skipped, so the multi-module
/// driver can pass the union of all net-driving output names across the
/// design and each module's lift picks up only its own.
fn output_port_nids(surface_names: &[String], file: &Btor2File) -> Vec<(Nid, String)> {
    if surface_names.is_empty() {
        return Vec::new();
    }
    let wanted: std::collections::HashSet<&str> =
        surface_names.iter().map(String::as_str).collect();
    let mut out: Vec<(Nid, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::Output {
            signal,
            symbol: Some(name),
        } = &line.node
            && wanted.contains(name.as_str())
            && seen.insert(name.clone())
        {
            out.push((signal.nid(), name.clone()));
        }
    }
    out
}

fn build_state_valuations(
    combo: usize,
    meta: &[StateMeta],
    cells: &CellEnumeration,
    cell_domains: &CellDomainMap,
    comb_signal_names: &[String],
    cv_bits: &[bool],
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for (i, sm) in meta.iter().enumerate() {
        // Only emit user-named cells. Synthetic cells from `chformal
        // -lower` start with a digit (e.g. `_n_…`) or contain `$`; they
        // don't appear in the user's source so they shouldn't appear in
        // their formulas.
        if sm.symbol.starts_with("st") && sm.symbol.contains("_n") {
            continue;
        }
        let v = cells.value_at(combo, i);
        // For enum/discover cells, emit the variant NAME (matched by value
        // via `value_map` OR increment-A default-by-index — see
        // `variant_name_for_value`); otherwise the raw integer. The named
        // form lets a compound `state == VARIANT` predicate resolve
        // definitely (F2.1), matching native. (Was: value_map-only lookup,
        // which fell back to the raw integer when the map was absent.)
        let display = cell_domains
            .get(&sm.nid)
            .and_then(|(fd, vm)| variant_name_for_value(fd, vm, sm.width, v))
            .unwrap_or_else(|| v.to_string());
        out.insert(sm.symbol.clone(), display);
    }
    // State-splitting: label each property-referenced combinational signal
    // with THIS split-variant's value (`T`/`F`) from the joint assignment
    // `cv_bits`. Because the variant is one jointly-achievable assignment
    // (computed per input), a joint property `!(sig_a_T && sig_b_T)`
    // resolves correctly — the per-signal ∃-priority limitation is gone.
    for (j, sig) in comb_signal_names.iter().enumerate() {
        let val = cv_bits.get(j).copied().unwrap_or(false);
        out.insert(sig.clone(), if val { "T" } else { "F" }.to_string());
    }
    out
}

/// Multi-label set for one input combination — one `<signal>_<value>`
/// label per non-clock input. Returned `Vec<String>` is what
/// `IRTransition.labels` carries; `emit_explicit` joins it with `,` and
/// the DSL parser stores them as `additional_labels` on the transition
/// declaration. Properties can then refer to individual signals via
/// `[(rst_1)] φ` rather than enumerating compound `in_…` strings.
///
/// When the design has no inputs, the transition still needs a label;
/// `step` is the conventional fallback (matches AIGER's behavior).
fn signal_labels_for_input(
    combo: usize,
    meta: &[InputMeta],
    input_cells: &InputCellEnumeration,
) -> Vec<String> {
    let labels: Vec<String> = meta
        .iter()
        .enumerate()
        // F1: reset inputs are excluded from labels (pinned inactive,
        // init-only) alongside clocks — matching the native alphabet.
        .filter(|(_, im)| !im.is_clock && im.is_reset.is_none())
        .map(|(i, im)| {
            let v = input_cells.value_at(combo, i);
            format!("{}_{}", im.symbol, v)
        })
        .collect();
    if labels.is_empty() {
        // No non-clock/non-reset inputs: every CLTS step is just a clock
        // tick. `step` is the conventional fallback (matches AIGER).
        vec!["step".into()]
    } else {
        labels
    }
}

fn make_step_env(
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
    cells: &CellEnumeration,
    input_cells: &InputCellEnumeration,
    state_idx: usize,
    input_idx: usize,
) -> Env {
    let mut env = Env::default();
    for (i, sm) in state_meta.iter().enumerate() {
        let bits = cells.value_at(state_idx, i);
        env.values
            .insert(sm.nid, Bv::from_u128_width(bits, sm.width));
    }
    for (i, im) in input_meta.iter().enumerate() {
        let bits = input_cells.value_at(input_idx, i);
        env.values
            .insert(im.nid, Bv::from_u128_width(bits, im.width));
    }
    env
}

/// Path 3 / Option A (§Phase 8 §8.2 residual closure) — return the
/// NIDs of state cells the user EXPLICITLY marked `init_policy: anyconst`
/// in the sidecar.
///
/// Restricted to user-declared anyconst (not every BTOR2 cell without
/// an `Init` line) because Yosys emits many cells without init lines
/// for non-anyconst reasons (reset synchronizers, undef bits under
/// the global `setundef` policy, FSM intermediates). Enumerating all
/// of them as initial blows up immediately; the explicit per-signal
/// anyconst declaration is the user's signal of intent.
///
/// `anyconst_symbols` is the set of signal names from the sidecar's
/// `signals[].init_policy == anyconst` entries (see
/// [`sidecar_anyconst_symbols`]). Empty set ⇒ no Path 3 enumeration;
/// falls back to the legacy single-init behaviour.
/// R-S2a (§Phase 9 §9.1) — BTOR2 init-line seeding. Walks every
/// `Node::Init { state, value }` line in the BTOR2 file; for each
/// state cell with a sidecar-declared `EnumValues` abstraction whose
/// value_map does NOT already contain the resolved init value, adds
/// a synthetic discriminator `(<signal>_<value>, value)` to the
/// cell's value_map AND appends `<signal>_<value>` to its variants
/// list.
///
/// Resolves the init value by reading the `value` operand's NID and
/// looking up its constant value (`Node::Const` / `Node::Op::Zero` /
/// `Node::Op::One` / `Node::Op::Ones`). Init lines whose value is a
/// non-constant expression are silently skipped — those require the
/// full `evaluate_pure` pass which runs later for the init-state
/// computation; rare in practice (Yosys typically emits constant
/// init values).
///
/// Bridges the gap when R-S5 / R-S3 / R-S7 don't cover the signal's
/// init value. Strictly additive — never replaces existing entries.
/// R-Y3 (§Phase 8) — BTOR2 init-line smart defaults for cells
/// WITHOUT sidecar entries. Opt-in via `MUNUNU_BTOR2_SMART_INIT_DEFAULTS=1`.
///
/// For each state cell with NO entry in `cell_domains` AND a BTOR2
/// `Init` line resolvable to a constant, inserts a synthetic
/// `EnumValues` `FieldDomain` pinning the cell to its init value.
/// The synthetic variant name is `<signal>_<init_value>`.
///
/// Effect: `CellEnumeration::build` sees the cell as if the user had
/// declared `{abstraction: enum, variants: [<signal>_<init>],
/// value_map: [{<signal>_<init>: <init>}]}` in the sidecar. State
/// space for that cell collapses from `2^width` (full bit-blast) to 1.
///
/// SOUNDNESS: see the call-site comment in `enumerate_and_blast`.
/// Under-approximation; sound for liveness, unsound for safety. Emits
/// an adapter warning naming the affected cells so users see what
/// was pinned.
///
/// No-op when the env var is unset OR when the design has no
/// unsidecared init-line cells.
fn apply_btor2_init_smart_defaults(
    file: &Btor2File,
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
    warnings: &mut Vec<AdapterWarning>,
    options: &AdapterOptions,
) {
    let enabled = options.smart_init_defaults
        || std::env::var("MUNUNU_BTOR2_SMART_INIT_DEFAULTS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
    if !enabled {
        return;
    }
    use crate::adapter::domain::{AbstractValue, AbstractionType, FieldDomain};

    // Build a NID → width lookup.
    let nid_to_width: std::collections::HashMap<Nid, u32> =
        state_meta.iter().map(|sm| (sm.nid, sm.width)).collect();

    // Walk Init lines; build (state_nid, init_value) pairs.
    let mut pinned: Vec<(Nid, u64)> = Vec::new();
    for line in &file.lines {
        let Node::Init { state, value, .. } = &line.node else {
            continue;
        };
        // Only consider cells without sidecar entries.
        if cell_domains.contains_key(state) {
            continue;
        }
        let Some(init_value) = resolve_btor2_constant(file, value.nid()) else {
            continue;
        };
        let Some(width) = nid_to_width.get(state) else {
            continue;
        };
        let masked = if *width >= 64 {
            init_value
        } else {
            init_value & ((1u64 << *width) - 1)
        };
        pinned.push((*state, masked));
    }

    if pinned.is_empty() {
        return;
    }

    // Inject synthetic sidecar entries.
    let mut pinned_names: Vec<String> = Vec::new();
    for (nid, val) in &pinned {
        let signal_name = symbols
            .get(nid)
            .cloned()
            .unwrap_or_else(|| format!("nid_{nid}"));
        let variant_name = format!("{}_{}", signal_name, *val);
        let fd = FieldDomain {
            name: signal_name.clone(),
            abstraction: AbstractionType::EnumValues,
            bound: None,
            lower_bound: None,
            variants: Some(vec![variant_name.clone()]),
            initial: AbstractValue::Variant(variant_name.clone()),
        };
        let value_map = vec![(variant_name, *val as i64)];
        cell_domains.insert(*nid, (fd, value_map));
        pinned_names.push(signal_name);
    }
    pinned_names.sort();
    pinned_names.dedup();

    let shown: Vec<&str> = pinned_names.iter().take(5).map(String::as_str).collect();
    let suffix = if pinned_names.len() > shown.len() {
        format!(" (and {} more)", pinned_names.len() - shown.len())
    } else {
        String::new()
    };
    warnings.push(AdapterWarning {
        kind: WarningKind::ApproximateTranslation,
        message: format!(
            "R-Y3: pinned {n} unsidecared state cell(s) to their BTOR2 init values \
             (smart-init-defaults policy enabled): {names}{suffix}. SOUNDNESS: \
             under-approximation — sound for liveness, unsound for safety. \
             Property violations that require these cells to deviate from their \
             init values are silently masked. Disable via unsetting \
             MUNUNU_BTOR2_SMART_INIT_DEFAULTS or declare the affected signals in \
             the sidecar to override the pinning.",
            n = pinned_names.len(),
            names = shown.join(", "),
            suffix = suffix
        ),
        location: None,
    });
}

fn apply_btor2_init_seeding(
    file: &Btor2File,
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
) {
    // Build a NID → state_meta index for fast lookup of widths.
    let nid_to_width: std::collections::HashMap<Nid, u32> =
        state_meta.iter().map(|sm| (sm.nid, sm.width)).collect();

    for line in &file.lines {
        let Node::Init { state, value, .. } = &line.node else {
            continue;
        };
        // Only seed cells that have a sidecar entry. Cells without
        // one fall back to full bit-blast over their width — init
        // value is already admissible.
        let Some((fd, value_map)) = cell_domains.get_mut(state) else {
            continue;
        };
        // Only widen `EnumValues` abstractions; `BoundedCounter` and
        // friends use a different value-set construction that does
        // not benefit from per-value variant names.
        if !matches!(
            fd.abstraction,
            crate::adapter::domain::AbstractionType::EnumValues
        ) {
            continue;
        }
        // Resolve the init value's constant via the value operand's NID.
        let Some(init_value) = resolve_btor2_constant(file, value.nid()) else {
            continue;
        };
        // Mask to the cell's bit-width.
        let Some(width) = nid_to_width.get(state).copied() else {
            continue;
        };
        let mask: u64 = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        // Resolve the signal name for the variant name; fall back
        // to a synthetic `nid_<state>` label when the BTOR2 symbol
        // table doesn't carry one.
        let signal_name = symbols
            .get(state)
            .cloned()
            .unwrap_or_else(|| format!("nid_{state}"));
        // Q2 (§Phase 11 slot-3 close follow-up): delegate the
        // dedupe + append + variants-list update to R-S6.5's
        // `try_append_value` helper. The helper supersedes the
        // inlined logic these helpers carried at their original
        // (R-S2a / R-S2b.4) shipping commits.
        try_append_value(&signal_name, init_value & mask, fd, value_map);
    }
}

/// R-S2b.4 (2026-06-11) — apply the reset-simulation seeding strategy
/// to a `CellDomainMap`. Walks every [`RegisterValuation`] produced by
/// [`crate::adapter::verilator::run_reset_simulation`]; for cells whose
/// signal name matches a valuation AND whose sidecar abstraction is
/// `EnumValues`, appends the observed value to the value_map +
/// variants if not already present.
///
/// Mirrors [`apply_btor2_init_seeding`] in shape: same `EnumValues`
/// gate, same `(variant_name, value)` augmentation, same
/// non-destructive "skip if already known" check. The only difference
/// is the value source — instead of resolving a BTOR2 `Init` line's
/// constant, R-S2b.4 reads the value from a `RegisterValuation` the
/// caller obtained from a Verilator reset simulation.
///
/// **Signal-name matching**: reverse-lookup via `symbols` (NID →
/// name). Valuations whose name doesn't match any state cell are
/// silently skipped — common cause is a Yosys rename
/// (`reg_a` → `reg_a_q`) the user can fix in the sidecar.
/// R-S2b.6's CLI flag will surface a diagnostic warning naming
/// unmatched valuations.
///
/// **Width clamping**: each valuation's `u64 value` is masked to the
/// matched cell's BTOR2 width via `state_meta` lookup. Bits above the
/// cell width are discarded silently (Verilator may sign-extend on
/// the printf cast).
///
/// **Soundness**: same posture as R-S2a. The seeded value is a
/// witness — it lies inside the cell's full admissible set by
/// construction (the simulation ran the actual gate-level model).
/// Adding it as a named `EnumValues` variant only refines the
/// downstream predicate-cube partition; it cannot remove behaviour.
/// No `// SOUNDNESS:` annotation needed beyond R-S2a's existing one.
///
/// R-S2b.6 (2026-06-11) — `apply_simulate_reset_seeding` (below)
/// is the production call site; the staged `#[allow(dead_code)]`
/// from R-S2b.4's first shipping commit was removed in R-S2b.6.
pub(crate) fn apply_reset_simulation_seeding(
    valuations: &[crate::adapter::verilator::RegisterValuation],
    nid_widths: &[(Nid, u32)],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
) {
    // NID → bit-width, for the value-masking step. The caller passes
    // a `&[(Nid, u32)]` slice (not the private `StateMeta` struct)
    // so the helper's signature does not leak module-private types.
    let nid_to_width: std::collections::HashMap<Nid, u32> = nid_widths.iter().copied().collect();
    // signal-name → NID, for the valuation-to-cell lookup.
    let name_to_nid: std::collections::HashMap<&str, i64> = symbols
        .iter()
        .map(|(nid, name)| (name.as_str(), *nid))
        .collect();

    for valuation in valuations {
        let Some(&nid) = name_to_nid.get(valuation.name.as_str()) else {
            continue;
        };
        let Some((fd, value_map)) = cell_domains.get_mut(&nid) else {
            continue;
        };
        if !matches!(
            fd.abstraction,
            crate::adapter::domain::AbstractionType::EnumValues
        ) {
            continue;
        }
        let Some(width) = nid_to_width.get(&nid).copied() else {
            continue;
        };
        let mask: u64 = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        // Q2 (§Phase 11 slot-3 close follow-up): delegate the
        // dedupe + append + variants-list update to R-S6.5's
        // `try_append_value` helper. The helper supersedes the
        // inlined logic this helper carried at its original
        // (R-S2b.4) shipping commit.
        try_append_value(&valuation.name, valuation.value & mask, fd, value_map);
    }
}

/// R-S2b.6 (2026-06-11) — orchestrates the Verilator reset
/// simulation + bit-blaster seeding. Production call site for
/// R-S2b.4's [`apply_reset_simulation_seeding`].
///
/// **Inputs**:
/// - `sim_config`: from `SvAnnotation::simulate_reset.as_ref().map(|s| s.to_reset_sim_config(module.clone()))`.
/// - `sv_source_path`: from `AdapterOptions::sv_source_path`.
/// - `state_meta`, `symbols`, `cell_domains`: as built upstream
///   in `translate()`.
/// - `warnings`: receives soundness / fallback notices.
///
/// **Behaviour**:
/// 1. Calls [`crate::adapter::verilator::locate_verilator`]. On
///    `Err` (binary absent), pushes an informational
///    `AdapterWarning` listing the fallback strategies and
///    returns without seeding. This is the **graceful-fallback**
///    path — Verilator is optional at runtime; missing it must
///    never break the build.
/// 2. On `Ok`, allocates a per-call [`crate::adapter::verilator::VerilatorTempDir`],
///    calls [`crate::adapter::verilator::run_reset_simulation`],
///    and on success feeds the resulting `Vec<RegisterValuation>`
///    to `apply_reset_simulation_seeding`. The tempdir drops at
///    the end of this function (unless `MUNUNU_KEEP_VERILATOR_TMP=1`).
/// 3. On `run_reset_simulation` `Err` (validation / compile /
///    run / parse / missing-observed-register), pushes an
///    `AdapterWarning` with the error message and returns. The
///    bit-blaster continues with the cell_domains it had before
///    the simulation attempt — no half-seeded state.
///
/// **Soundness**: same posture as `apply_reset_simulation_seeding`.
/// The seeded value is a witness — it lies inside the cell's full
/// admissible set by construction. Adding a new EnumValues
/// variant only refines the downstream predicate-cube partition;
/// it cannot remove behaviour. Verilator-absent is sound by
/// fall-through (other Phase 9 strategies still apply).
pub(crate) fn apply_simulate_reset_seeding(
    sim_config: &crate::adapter::verilator::ResetSimConfig,
    sv_source_path: &std::path::Path,
    nid_widths: &[(Nid, u32)],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
    warnings: &mut Vec<AdapterWarning>,
) {
    let _bin = match crate::adapter::verilator::locate_verilator() {
        Ok(b) => b,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S2b.6: sidecar declared `simulate_reset` but Verilator is not \
                     discoverable ({}); skipping reset-simulation seeding. Other Phase 9 \
                     strategies (R-S5 type-driven, R-S7 property-syntactic, R-S3 case-literal) \
                     still apply. Install Verilator (`brew install verilator` / \
                     `apt install verilator`) or set MUNUNU_VERILATOR_PATH to enable.",
                    e.message
                ),
                location: None,
            });
            return;
        }
    };

    let tmp = match crate::adapter::verilator::VerilatorTempDir::new() {
        Ok(d) => d,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S2b.6: failed to create Verilator tempdir: {}; skipping \
                     reset-simulation seeding.",
                    e.message
                ),
                location: None,
            });
            return;
        }
    };

    let base_opts = crate::adapter::verilator::VerilatorOptions::default();
    match crate::adapter::verilator::run_reset_simulation(
        &_bin.path,
        &base_opts,
        sv_source_path,
        sim_config,
        tmp.path(),
    ) {
        Ok(valuations) => {
            let before = cell_domains_value_count(cell_domains);
            apply_reset_simulation_seeding(&valuations, nid_widths, symbols, cell_domains);
            let after = cell_domains_value_count(cell_domains);
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S2b.6: reset-simulation seeded {observed} register valuation(s); \
                     added {added} new EnumValues discriminator(s) to cell_domains \
                     (before={before}, after={after}).",
                    observed = valuations.len(),
                    added = after.saturating_sub(before),
                ),
                location: None,
            });
        }
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S2b.6: reset-simulation failed: {}; skipping seeding. \
                     Other Phase 9 strategies still apply.",
                    e.message
                ),
                location: None,
            });
        }
    }
}

/// R-S2b.6 helper — count the total number of value_map entries
/// across every cell. Used to summarize how many discriminators
/// the seeding step added (before vs after the apply call).
fn cell_domains_value_count(cell_domains: &CellDomainMap) -> usize {
    cell_domains.values().map(|(_, vm)| vm.len()).sum()
}

/// R-S6.5 (§Phase 9 §9.1, 2026-06-11) — apply the VCD-trace
/// mining seeding strategy to a `CellDomainMap`. Companion to
/// R-S2b.4's [`apply_reset_simulation_seeding`]; mirrors its
/// shape one-for-one — same `EnumValues` gate, same
/// `(variant_name, value)` augmentation, same non-destructive
/// "skip if already known" check.
///
/// The only differences from R-S2b.4 are the **value source**
/// and the **multi-value-per-signal budget**:
///
/// - **Value source**: instead of a single
///   `RegisterValuation { value }` (R-S2b's "observe one steady
///   state"), this helper consumes a [`VcdValueStats`] entry
///   carrying a heavy-hitter list + boundary values mined from
///   pre-existing regression traces (R-S6.3's miner output).
/// - **Multi-value budget**: each signal contributes up to
///   `max_heavy_hitters_per_signal` heavy-hitter values + (when
///   `seed_boundary_values`) the min and max. R-S2b.4 contributes
///   one value per signal.
///
/// **Signal-name matching**: each `VcdValueStats::id` field is
/// expected to carry the **signal name** (NOT the VCD signal-id
/// the raw miner output uses). R-S6.6's orchestration rewrites
/// the `id` field by joining mining output with the
/// `VcdHeader::signals` lookup before invoking this helper —
/// keeps R-S6.5's signature aligned with R-S2b.4's
/// signal-name-keyed contract.
///
/// **Width clamping**: each value is masked to the matched
/// cell's BTOR2 bit-width via `nid_widths` (same as R-S2b.4).
///
/// **Soundness**: same posture as R-S2a / R-S2b.4. Every seeded
/// value is a witness observed during real simulation — it lies
/// inside the cell's full admissible set by construction. Adding
/// it as a named `EnumValues` variant only refines the
/// downstream predicate-cube partition; it cannot remove
/// behaviour. No new `// SOUNDNESS:` annotation needed.
///
/// R-S6.6 (2026-06-11) — `apply_vcd_trace_seeding` (below) is
/// the production call site; the staged `#[allow(dead_code)]`
/// from R-S6.5's first shipping commit was removed in R-S6.6.
pub(crate) fn apply_vcd_seeding(
    signal_stats: &[crate::adapter::vcd::VcdValueStats],
    max_heavy_hitters_per_signal: u32,
    seed_boundary_values: bool,
    nid_widths: &[(Nid, u32)],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
) {
    let nid_to_width: std::collections::HashMap<Nid, u32> = nid_widths.iter().copied().collect();
    let name_to_nid: std::collections::HashMap<&str, i64> = symbols
        .iter()
        .map(|(nid, name)| (name.as_str(), *nid))
        .collect();

    for stats in signal_stats {
        let Some(&nid) = name_to_nid.get(stats.id.as_str()) else {
            continue;
        };
        let Some((fd, value_map)) = cell_domains.get_mut(&nid) else {
            continue;
        };
        if !matches!(
            fd.abstraction,
            crate::adapter::domain::AbstractionType::EnumValues
        ) {
            continue;
        }
        let Some(width) = nid_to_width.get(&nid).copied() else {
            continue;
        };
        let mask: u64 = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };

        // Top-N heavy-hitters (in count-descending order; R-S6.3
        // sorted the list deterministically).
        let take_n = max_heavy_hitters_per_signal as usize;
        for (raw_value, _count) in stats.heavy_hitters.iter().take(take_n) {
            try_append_value(&stats.id, *raw_value & mask, fd, value_map);
        }

        // Boundary values: min and max.
        if seed_boundary_values {
            if let Some(min_v) = stats.min {
                try_append_value(&stats.id, min_v & mask, fd, value_map);
            }
            if let Some(max_v) = stats.max {
                try_append_value(&stats.id, max_v & mask, fd, value_map);
            }
        }
    }
}

/// R-S6.5 helper — append a `(variant_name, value)` to a cell's
/// `value_map` and `variants` if the value isn't already there.
/// Variant name format matches R-S2b.4: `<signal_name>_<value>`.
fn try_append_value(
    signal_name: &str,
    masked: u64,
    fd: &mut crate::adapter::domain::FieldDomain,
    value_map: &mut Vec<(String, i64)>,
) {
    let masked_signed = masked as i64;
    if value_map.iter().any(|(_, v)| *v == masked_signed) {
        return;
    }
    let variant_name = format!("{signal_name}_{masked_signed}");
    value_map.push((variant_name.clone(), masked_signed));
    match &mut fd.variants {
        Some(v) => v.push(variant_name),
        None => fd.variants = Some(vec![variant_name]),
    }
}

/// R-S6.6 (2026-06-11) — orchestrates the VCD trace mining +
/// bit-blaster seeding for a single
/// [`crate::adapter::systemverilog::annotation::VcdTraceConfig`].
/// Production call site for R-S6.5's [`apply_vcd_seeding`].
///
/// **Inputs**:
/// - `trace_config`: one entry from `SvAnnotation::vcd_traces`.
/// - `sidecar_dir`: parent directory of the sidecar (from
///   `AdapterOptions::sidecar_path.parent()`). Used to resolve
///   relative paths in `trace_config.path`. May be `None` when
///   the sidecar path wasn't supplied — only absolute trace
///   paths can be read in that case.
/// - `nid_widths`, `symbols`, `cell_domains`: as built upstream.
/// - `warnings`: receives soundness / fallback notices.
///
/// **Behaviour**:
/// 1. Resolves the trace path: absolute → use as-is; relative →
///    join with `sidecar_dir`. If the result is unreadable
///    (file missing, no read permission), pushes an
///    `AdapterWarning` and returns.
/// 2. Parses header + value changes via
///    [`crate::adapter::vcd::parse_vcd_header`] +
///    [`crate::adapter::vcd::parse_vcd_changes`]. Parse failures
///    surface as `AdapterWarning` (graceful — the bit-blaster
///    continues with its pre-mining cell_domains).
/// 3. Mines per-signal frequencies via
///    [`crate::adapter::vcd::mine_vcd_frequencies`].
/// 4. Rewrites each stats entry's `id` from the raw VCD signal-id
///    to the SV signal name (using the header's
///    `signals` lookup, leaf-name only — the BTOR2 symbols map
///    uses leaf names).
/// 5. Applies the per-trace allowlist if `trace_config.signals`
///    is non-empty.
/// 6. Calls [`apply_vcd_seeding`] with the rewritten stats.
/// 7. Pushes a summary `AdapterWarning` recording the trace
///    consumed + value count before/after.
///
/// **Soundness**: same posture as R-S6.5. Every seeded value is a
/// witness observed during real simulation — it lies inside the
/// cell's full admissible set by construction.
pub(crate) fn apply_vcd_trace_seeding(
    trace_config: &crate::adapter::systemverilog::annotation::VcdTraceConfig,
    sidecar_dir: Option<&std::path::Path>,
    nid_widths: &[(Nid, u32)],
    symbols: &std::collections::HashMap<i64, String>,
    cell_domains: &mut CellDomainMap,
    warnings: &mut Vec<AdapterWarning>,
) {
    let path_obj = std::path::Path::new(&trace_config.path);
    let resolved = if path_obj.is_absolute() {
        path_obj.to_path_buf()
    } else if let Some(dir) = sidecar_dir {
        dir.join(path_obj)
    } else {
        warnings.push(AdapterWarning {
            kind: WarningKind::ApproximateTranslation,
            message: format!(
                "R-S6.6: relative VCD trace path `{}` cannot be resolved without \
                 AdapterOptions::sidecar_path; skipping. Set the sidecar path or use \
                 an absolute trace path.",
                trace_config.path
            ),
            location: None,
        });
        return;
    };

    let content = match std::fs::read(&resolved) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S6.6: failed to read VCD trace at `{}`: {e}; skipping.",
                    resolved.display()
                ),
                location: None,
            });
            return;
        }
    };

    let header = match crate::adapter::vcd::parse_vcd_header(&content) {
        Ok(h) => h,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S6.6: VCD header parse failed for `{}`: {}; skipping.",
                    resolved.display(),
                    e.message
                ),
                location: None,
            });
            return;
        }
    };

    let changes = match crate::adapter::vcd::parse_vcd_changes(&content) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(AdapterWarning {
                kind: WarningKind::ApproximateTranslation,
                message: format!(
                    "R-S6.6: VCD changes parse failed for `{}`: {}; skipping.",
                    resolved.display(),
                    e.message
                ),
                location: None,
            });
            return;
        }
    };

    let mut stats = crate::adapter::vcd::mine_vcd_frequencies(&changes);

    // Build VCD signal-id → SV leaf name map (R-S6.5 expects the
    // stats id field to carry the SV signal name).
    let id_to_name: std::collections::HashMap<&str, &str> = header
        .signals
        .iter()
        .map(|s| (s.id.as_str(), s.name.as_str()))
        .collect();

    // Rewrite + (optionally) filter by allowlist.
    let allowlist: Option<std::collections::HashSet<&str>> = if trace_config.signals.is_empty() {
        None
    } else {
        Some(trace_config.signals.iter().map(String::as_str).collect())
    };

    let mut rewritten = Vec::with_capacity(stats.len());
    for s in stats.drain(..) {
        let Some(name) = id_to_name.get(s.id.as_str()) else {
            continue;
        };
        if let Some(allow) = &allowlist
            && !allow.contains(name)
        {
            continue;
        }
        rewritten.push(crate::adapter::vcd::VcdValueStats {
            id: (*name).to_string(),
            ..s
        });
    }

    let before = cell_domains_value_count(cell_domains);
    apply_vcd_seeding(
        &rewritten,
        trace_config.max_heavy_hitters_per_signal,
        trace_config.seed_boundary_values,
        nid_widths,
        symbols,
        cell_domains,
    );
    let after = cell_domains_value_count(cell_domains);

    warnings.push(AdapterWarning {
        kind: WarningKind::ApproximateTranslation,
        message: format!(
            "R-S6.6: VCD trace `{}` mined {signals} signals; added {added} new EnumValues \
             discriminator(s) (before={before}, after={after}).",
            resolved.display(),
            signals = rewritten.len(),
            added = after.saturating_sub(before),
        ),
        location: None,
    });
}

/// R-S2a helper — resolve a BTOR2 NID to its constant `u64` value if
/// it's one of the simple constant nodes mununu's bit-blaster
/// recognises: `Node::Const { value: Binary/Decimal/Hex }`, or one
/// of the `Op::Zero` / `Op::One` / `Op::Ones` shortcuts that Yosys
/// emits frequently for init lines.
///
/// Returns `None` for any non-constant NID (operations on other
/// signals, references to inputs, etc.).
pub(crate) fn resolve_btor2_constant(file: &Btor2File, nid: Nid) -> Option<u64> {
    for line in &file.lines {
        if line.nid != nid {
            continue;
        }
        let Node::Const { value, sort } = &line.node else {
            return None;
        };
        let width = parser::bv_width(file, *sort)?;
        let mask = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        return match value {
            ConstValue::Zero => Some(0),
            ConstValue::One => Some(1 & mask),
            ConstValue::Ones => Some(mask),
            ConstValue::Bin(s) => u64::from_str_radix(s, 2).ok().map(|v| v & mask),
            ConstValue::Dec(n) => {
                let raw = if *n < 0 {
                    (mask.wrapping_sub((-*n) as u64).wrapping_add(1)) & mask
                } else {
                    (*n as u64) & mask
                };
                Some(raw)
            }
            ConstValue::Hex(s) => u64::from_str_radix(s, 16).ok().map(|v| v & mask),
        };
    }
    None
}

/// R.5 WP literal extraction — collect every distinct constant value
/// appearing in the BTOR2 file's `Node::Const` lines. Used by the
/// CEGAR loop's `weakest_precondition_predicates` heuristic to
/// propose richer separating predicates (e.g. `register == 5`
/// alongside `register == 0` / `register == 1`).
///
/// Returns a sorted, deduped Vec. Const widths are masked to their
/// declared sort widths before deduplication (so `4'b0000` and
/// `8'b00000000` both contribute `0`). Const values that fail to
/// parse (malformed bin/hex literals) are silently skipped.
///
/// **Why "all consts" and not "consts compared against a register".**
/// The full WP picture would walk every `Op::Eq` / `Op::Neq` whose
/// operand is a register-output and extract the comparison constant.
/// That cone walk is queued as a separate R.5 follow-up; this
/// helper is the simpler "every literal somewhere in the design"
/// MVP. Per-iteration cap on CEGAR refinement (default 2 predicates)
/// bounds the runaway risk.
pub fn collect_btor2_constants(file: &Btor2File) -> Vec<u64> {
    let mut out: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    for line in &file.lines {
        if let Some(v) = resolve_btor2_constant(file, line.nid) {
            out.insert(v);
        }
    }
    out.into_iter().collect()
}

fn nondeterministic_init_cells(
    file: &Btor2File,
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
    anyconst_symbols: &std::collections::HashSet<String>,
) -> std::collections::HashSet<Nid> {
    if anyconst_symbols.is_empty() {
        return std::collections::HashSet::new();
    }
    let mut has_init: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::Init { state, .. } = &line.node {
            has_init.insert(*state);
        }
    }
    state_meta
        .iter()
        .filter(|sm| !has_init.contains(&sm.nid))
        .filter(|sm| {
            symbols
                .get(&sm.nid)
                .map(|name| anyconst_symbols.contains(name))
                .unwrap_or(false)
        })
        .map(|sm| sm.nid)
        .collect()
}

/// Soundness companion to [`nondeterministic_init_cells`] — return
/// the signal names of state cells that have NO `Init` line AND are
/// NOT in `anyconst_symbols`. These cells silently default to zero
/// in the legacy init path; the caller emits a SOUNDNESS warning so
/// the user knows whether their upstream `setundef` policy makes
/// that safe (sound under `-zero`; unsound under `-anyseq/-anyconst`).
///
/// Cells without a recoverable symbol name (Yosys synthetic state
/// cells without user-visible names) are silently skipped — they
/// don't appear in the user-facing warning either way, and Yosys
/// almost always emits Init lines for synthetic intermediate cells
/// it created itself.
fn uncovered_uninit_cells(
    file: &Btor2File,
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
    anyconst_symbols: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut has_init: std::collections::HashSet<Nid> = std::collections::HashSet::new();
    for line in &file.lines {
        if let Node::Init { state, .. } = &line.node {
            has_init.insert(*state);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for sm in state_meta {
        if has_init.contains(&sm.nid) {
            continue;
        }
        let Some(name) = symbols.get(&sm.nid) else {
            continue;
        };
        if anyconst_symbols.contains(name) {
            continue;
        }
        out.push(name.clone());
    }
    out.sort();
    out.dedup();
    out
}

/// Path 3 — collect the set of signal names declared with
/// `init_policy: anyconst` in the sidecar. Used by
/// [`nondeterministic_init_cells`] to restrict init enumeration to
/// user-declared anyconst cells (avoiding the state explosion on
/// every Yosys-emitted cell-without-init).
fn sidecar_anyconst_symbols(options: &AdapterOptions) -> std::collections::HashSet<String> {
    let Some(json) = &options.sidecar_json else {
        return std::collections::HashSet::new();
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return std::collections::HashSet::new();
    };
    ann.init_policy_overrides()
        .into_iter()
        .filter(|(_, p)| {
            matches!(
                p,
                crate::adapter::systemverilog::annotation::InitPolicy::Anyconst
            )
        })
        .map(|(name, _)| name)
        .collect()
}

/// Path 3 / Option A — enumerate the cartesian product of admissible
/// initial-state combinations. Cells WITH an init line contribute
/// their single pinned value (from the already-evaluated `init_env`);
/// cells WITHOUT an init line contribute every value in their
/// `CellEnumeration` per-cell admissible set.
///
/// Returns an empty vec when there are no nondeterministic cells —
/// signals the caller to use the legacy single-init path. Cells
/// whose pinned init value isn't in their declared abstraction are
/// silently dropped (cannot encode); a deterministic init outside
/// the abstraction was already a soundness issue pre-Path 3.
fn enumerate_initial_combos(
    init_env: &Env,
    state_meta: &[StateMeta],
    cells: &CellEnumeration,
    nondet_nids: &std::collections::HashSet<Nid>,
    bounded_init_overrides: &std::collections::HashMap<Nid, Vec<u128>>,
) -> Vec<usize> {
    if nondet_nids.is_empty() {
        return Vec::new();
    }
    // Build per-cell admissible-init-value lists.
    let per_cell_init: Vec<Vec<u128>> = state_meta
        .iter()
        .enumerate()
        .map(|(i, sm)| {
            if nondet_nids.contains(&sm.nid) {
                // R-Y4 (§Phase 8) — if the sidecar declared
                // `bounded_init: [...]` for this signal, restrict
                // the per-cell init set to those values. The
                // bounded-init values must already be in the cell's
                // abstract admissible set (`cells.per_cell[i]`);
                // out-of-set values are silently dropped by the
                // cartesian-product encoder (cells.encode returns
                // None for unrepresentable combinations).
                if let Some(bounded) = bounded_init_overrides.get(&sm.nid) {
                    bounded.clone()
                } else {
                    // Anyconst-style: every value in the declared abstraction.
                    cells.per_cell[i].clone()
                }
            } else {
                // Deterministic: use the pinned init value.
                let bits = init_env
                    .values
                    .get(&sm.nid)
                    .map(|v| v.low128())
                    .unwrap_or(0);
                vec![bits]
            }
        })
        .collect();
    // Cartesian product → encode each combination.
    let mut combos: Vec<usize> = Vec::new();
    let mut current: Vec<u128> = vec![0; per_cell_init.len()];
    cartesian_collect_combos(&per_cell_init, 0, &mut current, &mut combos, cells);
    combos.sort_unstable();
    combos.dedup();
    combos
}

/// R-Y6 (§Phase 8) — apply the sidecar-declared reset-hold sequence
/// to the init env. Pins the named input signal to its asserted value
/// and runs `hold_cycles` cycles of `evaluate_pure` + `apply_next`,
/// mutating `init_env` to reflect the state AFTER the reset hold.
///
/// No-op when:
/// - The sidecar JSON is absent / unparseable.
/// - No `reset_sequence` field is declared in the SvAnnotation.
/// - `hold_cycles == 0`.
/// - The named reset input does not resolve to a BTOR2 input symbol
///   (silently skipped to keep the precondition behaviour additive).
///
/// SOUNDNESS: this is an exact transformation of the init env per the
/// design's own reset semantics — the K-cycle hold is what the
/// downstream verification engine would do anyway if it could
/// observe time before cycle 0. No abstraction tradeoff; the init
/// state set after R-Y6 represents "what the design's state is after
/// the reset hold settles", which is the intended initial state for
/// most real designs with multi-stage reset synchronisers (e.g.
/// OpenTitan's `prim_reset_sync`).
fn apply_reset_sequence(
    file: &Btor2File,
    init_env: &mut Env,
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
    symbols: &std::collections::HashMap<i64, String>,
    options: &AdapterOptions,
) -> Result<(), AdapterError> {
    let Some(json) = &options.sidecar_json else {
        return Ok(());
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return Ok(());
    };
    let Some(seq) = ann.reset_sequence.as_ref() else {
        return Ok(());
    };
    if seq.hold_cycles == 0 {
        return Ok(());
    }
    // Resolve reset input name to BTOR2 input NID + width.
    let reset_nid_width: Option<(Nid, u32)> = input_meta
        .iter()
        .filter_map(|im| {
            symbols.get(&im.nid).and_then(|name| {
                if name == &seq.reset_input {
                    Some((im.nid, im.width))
                } else {
                    None
                }
            })
        })
        .next();
    let Some((reset_nid, width)) = reset_nid_width else {
        // Silently skip — the reset_input name didn't match any BTOR2
        // input. Additive: leaves init_env unchanged.
        return Ok(());
    };
    let mask = if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    let asserted_bv = Bv::from_u128_width(seq.asserted_value as u128 & mask, width);
    apply_reset_hold(
        file,
        init_env,
        state_meta,
        reset_nid,
        asserted_bv,
        seq.hold_cycles,
    )
}

/// Core reset-hold loop shared by [`apply_reset_sequence`] (sidecar) and
/// [`apply_auto_reset`] (auto-detected). For each held cycle: pin the
/// reset input to its asserted value, re-evaluate combinational logic
/// (`honor_init=false`, past cycle 0), then advance state cells via their
/// next-state functions. The post-loop `init_env` state is the effective
/// initial state.
fn apply_reset_hold(
    file: &Btor2File,
    init_env: &mut Env,
    state_meta: &[StateMeta],
    reset_nid: Nid,
    asserted_bv: Bv,
    hold_cycles: u32,
) -> Result<(), AdapterError> {
    for _ in 0..hold_cycles {
        init_env.values.insert(reset_nid, asserted_bv.clone());
        evaluate_pure(file, init_env, /*honor_init=*/ false)?;
        apply_next(file, init_env, state_meta)?;
    }
    Ok(())
}

/// F1.1 (S-track KMTS-fidelity, 2026-06-14) — auto-reset initial state.
///
/// When the design has an auto-detected async reset (an `InputMeta` with
/// `is_reset = Some(_)`) and **no** state cell carries a BTOR2 `Init`
/// line — the async-reset shape, where *reset* (not init) establishes the
/// start state — assert the reset for one cycle to derive the post-reset
/// initial state. Without this, the uninitialised power-on cube (all
/// state cells default 0) becomes the initial state; once the enum
/// catch-all clamp runs, that lands in a catch-all variant (e.g.
/// cwe1245's `UNDEF`) rather than the design's real reset state (`IDLE`).
/// Mirrors the native pipeline, which starts in the reset state.
///
/// No-op when: a sidecar `reset_sequence` (hold>0) already established
/// init (handled by [`apply_reset_sequence`]); no reset is detected; or
/// any state cell has an `Init` line (then the BTOR2 init is
/// authoritative and we must not advance past it).
fn apply_auto_reset(
    file: &Btor2File,
    init_env: &mut Env,
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
    options: &AdapterOptions,
) -> Result<(), AdapterError> {
    // Skip if a sidecar reset_sequence with a real hold already ran.
    if let Some(json) = &options.sidecar_json
        && let Ok(ann) =
            serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
        && ann
            .reset_sequence
            .as_ref()
            .is_some_and(|s| s.hold_cycles > 0)
    {
        return Ok(());
    }
    // The auto-detected reset input (first is_reset; single-reset scope).
    let Some(reset_im) = input_meta.iter().find(|im| im.is_reset.is_some()) else {
        return Ok(());
    };
    let active_high = reset_im.is_reset == Some(true);
    // Guard: only the pure-async-reset shape (no Init line on any state
    // cell). If any cell has init, the BTOR2 init is authoritative.
    let any_init = file
        .lines
        .iter()
        .any(|l| matches!(&l.node, Node::Init { state, .. } if state_meta.iter().any(|sm| sm.nid == *state)));
    if any_init {
        return Ok(());
    }
    let mask = if reset_im.width >= 128 {
        u128::MAX
    } else {
        (1u128 << reset_im.width) - 1
    };
    let asserted = if active_high { 1u128 & mask } else { 0u128 };
    let asserted_bv = Bv::from_u128_width(asserted, reset_im.width);
    apply_reset_hold(file, init_env, state_meta, reset_im.nid, asserted_bv, 1)
}

/// R-Y4 (§Phase 8) — collect per-signal bounded-init overrides from
/// the sidecar. For each `signals[]` entry with both
/// `init_policy: anyconst` AND a non-empty `bounded_init: [...]`,
/// returns `(NID, [values])` keyed by the BTOR2 state-cell NID.
///
/// Values are masked to the cell's bit-width to avoid surprising
/// encode failures on values that span more bits than the cell admits.
/// Cells whose symbol does not resolve to a state-meta entry are
/// silently skipped (defensive — typically can't happen if the
/// sidecar names match the SV source).
fn sidecar_bounded_init_overrides(
    options: &AdapterOptions,
    state_meta: &[StateMeta],
    symbols: &std::collections::HashMap<i64, String>,
) -> std::collections::HashMap<Nid, Vec<u128>> {
    let Some(json) = &options.sidecar_json else {
        return std::collections::HashMap::new();
    };
    let Ok(ann) =
        serde_json::from_str::<crate::adapter::systemverilog::annotation::SvAnnotation>(json)
    else {
        return std::collections::HashMap::new();
    };
    let name_to_nid_width: std::collections::HashMap<&str, (Nid, u32)> = state_meta
        .iter()
        .filter_map(|sm| {
            symbols
                .get(&sm.nid)
                .map(|n| (n.as_str(), (sm.nid, sm.width)))
        })
        .collect();
    let mut out = std::collections::HashMap::new();
    for sig in &ann.signals {
        if !matches!(
            sig.init_policy,
            crate::adapter::systemverilog::annotation::InitPolicy::Anyconst
        ) {
            continue;
        }
        let Some(bounded) = sig.bounded_init.as_ref() else {
            continue;
        };
        if bounded.is_empty() {
            continue;
        }
        let Some((nid, width)) = name_to_nid_width.get(sig.name.as_str()) else {
            continue;
        };
        let mask = if *width >= 128 {
            u128::MAX
        } else {
            (1u128 << *width) - 1
        };
        let values: Vec<u128> = bounded.iter().map(|v| (*v as u128) & mask).collect();
        out.insert(*nid, values);
    }
    out
}

/// Recursive helper for `enumerate_initial_combos` — fills the
/// `combos` vec with every encoding of every value-vector in the
/// cartesian product of `per_cell_init`. Encodings that fall outside
/// the cell domain are silently dropped (cannot happen under R-S5's
/// invariant that the typedef-derived abstraction set is complete).
fn cartesian_collect_combos(
    per_cell: &[Vec<u128>],
    depth: usize,
    current: &mut Vec<u128>,
    out: &mut Vec<usize>,
    cells: &CellEnumeration,
) {
    if depth == per_cell.len() {
        if let Some(idx) = cells.encode(current) {
            out.push(idx);
        }
        return;
    }
    for &v in &per_cell[depth] {
        current[depth] = v;
        cartesian_collect_combos(per_cell, depth + 1, current, out, cells);
    }
}

/// Build the initial environment used to compute init values.
/// `with_inputs_zero=true` zeroes inputs (init usually doesn't depend on them).
fn make_initial_env(
    file: &Btor2File,
    state_meta: &[StateMeta],
    input_meta: &[InputMeta],
    with_inputs_zero: bool,
) -> Env {
    let mut env = Env::default();
    // For init evaluation, states default to zero unless an init line is processed
    // (handled by `evaluate_pure` → `Init` propagating the init value).
    for sm in state_meta {
        env.values.insert(sm.nid, Bv::zero(sm.width));
    }
    if with_inputs_zero {
        for im in input_meta {
            env.values.insert(im.nid, Bv::zero(im.width));
        }
    }
    let _ = file;
    env
}

/// Encode an evaluator [`Env`] back into a linear state-combo index via
/// the [`CellEnumeration`]'s mixed-radix scheme. Returns `None` when
/// the env's per-cell values are not all in their declared abstraction
/// — caller decides whether to drop the transition (under-approx) or
/// route to an OOB sink (over-approx). Stage 2B of the BTOR2 sidecar
/// integration uses the drop semantics; OOB sink is a follow-up.
fn encode_state(env: &Env, state_meta: &[StateMeta], cells: &CellEnumeration) -> Option<usize> {
    let values: Vec<u128> = state_meta
        .iter()
        .map(|sm| env.values.get(&sm.nid).map(|v| v.low128()).unwrap_or(0))
        .collect();
    cells.encode(&values)
}

fn read_operand(env: &Env, op: Operand) -> Option<Bv> {
    let v = env.values.get(&op.nid())?;
    if op.is_negated() {
        // BTOR2 negative-NID shorthand = bitwise NOT.
        Some(v.not())
    } else {
        Some(v.clone())
    }
}

/// Evaluate every node in declaration order, populating `env` with the
/// computed value for each NID.
///
/// `honor_init = true` only when computing the initial-state assignment;
/// during step (transition) evaluation it must be `false` so `init` lines
/// do not stomp the current state value passed in via `make_step_env`.
fn evaluate_pure(file: &Btor2File, env: &mut Env, honor_init: bool) -> Result<(), AdapterError> {
    evaluate_pure_with_uf(file, env, honor_init, None)
}

/// R.5b multi-value UF representative — which substitution value
/// the wrapped Op gets per evaluation pass. Used by
/// [`evaluate_pure_with_uf`] (zero only, backward compat) and
/// [`evaluate_pure_with_uf_rep`] (multi-value enumeration).
///
/// Semantics: each variant maps to a deterministic value of the
/// wrapped Op's result width. The full may-side UF semantics admit
/// every admissible value of the data sort; enumerating multiple
/// representatives ([`UfRepresentative::Zero`] +
/// [`UfRepresentative::Ones`]) gives a tighter may-side
/// approximation than zero-only at the cost of K-fold edge
/// duplication per cube/input combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UfRepresentative {
    /// Substitute `Bv::zero(width)`. The R.5b lifter MVP default.
    Zero,
    /// Substitute `Bv::ones(width)` (all bits set). Combined
    /// with `Zero` covers the boundary values of the wrapped Op's
    /// result space; together they detect Boolean-controlled
    /// downstream paths that diverge on extremes.
    Ones,
}

impl UfRepresentative {
    /// Materialise the substituted value at the given Op width.
    fn to_value(self, width: u32) -> Bv {
        match self {
            UfRepresentative::Zero => Bv::zero(width),
            UfRepresentative::Ones => Bv::ones(width),
        }
    }
}

/// R.5b lifter integration MVP — variant of [`evaluate_pure`] that
/// substitutes `Bv::zero(width)` for every Op NID in
/// `uf_wrapped_nids`, skipping the actual arithmetic evaluation. The
/// substituted value propagates through downstream Op evaluations
/// (since they read the env), so any computation that transitively
/// depends on a wrapped Op sees the UF-stand-in.
///
/// `None` for `uf_wrapped_nids` reproduces `evaluate_pure` exactly
/// (no UF substitution). `Some(empty)` is also a no-op.
///
/// **Multi-value enumeration** — see [`evaluate_pure_with_uf_rep`]
/// for the explicit-representative variant. This helper hard-codes
/// `UfRepresentative::Zero` for backward compatibility with the
/// R.5b lifter MVP.
fn evaluate_pure_with_uf(
    file: &Btor2File,
    env: &mut Env,
    honor_init: bool,
    uf_wrapped_nids: Option<&std::collections::HashSet<Nid>>,
) -> Result<(), AdapterError> {
    evaluate_pure_with_uf_rep(
        file,
        env,
        honor_init,
        uf_wrapped_nids,
        UfRepresentative::Zero,
    )
}

/// R.5b multi-value enumeration helper — like [`evaluate_pure_with_uf`]
/// but lets the caller pick which [`UfRepresentative`] each wrapped
/// Op's output substitutes to. Enables the caller to run K passes
/// (K = |UfRepresentative| variants) over the same (env, file) to
/// generate K downstream effects per UF wrapping, which the lift
/// consumes as K may-edges per cube.
fn evaluate_pure_with_uf_rep(
    file: &Btor2File,
    env: &mut Env,
    honor_init: bool,
    uf_wrapped_nids: Option<&std::collections::HashSet<Nid>>,
    rep: UfRepresentative,
) -> Result<(), AdapterError> {
    use crate::adapter::btor2::term_backend::{WalkError, walk_design};

    // §Phase 10 Option-4 step 1b (2026-06-12) — cutover. The bespoke
    // node loop is retired; `evaluate_pure_with_uf_rep` now drives
    // the unified `walk_design` over a `ConcreteBackend`. The
    // ConcreteBackend delegates operator + constant evaluation to the
    // same `eval_op` / `eval_const_value` the bespoke loop used, so
    // the computed env is bit-identical — proven by the step-1a
    // equivalence tests (`option4_backend_matches_bespoke_*`).
    //
    // The backend owns its env; we move the caller's env in, walk,
    // and move it back (restoring it even on error so partial
    // results remain inspectable, matching the bespoke in-place
    // mutation contract).
    let seeded = std::mem::take(env);
    let mut backend = ConcreteBackend::new(seeded, honor_init, uf_wrapped_nids, rep);
    let walk_result = walk_design(file, &mut backend);
    *env = backend.into_env();

    walk_result.map_err(|e| {
        // Map the structural / backend error back to the AdapterError
        // shape callers expect (IrConsistencyError + source location
        // looked up by NID), preserving the pre-cutover diagnostics.
        let lookup_loc = |nid: Nid| {
            file.lookup(nid).map(|l| SourceLocation {
                line: l.source_line,
                column: 0,
            })
        };
        match e {
            WalkError::NonBitvecSort(nid) => AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: format!("NID {nid}: node references non-bitvec sort"),
                location: lookup_loc(nid),
            },
            WalkError::Unevaluated(nid) => AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: format!("NID {nid}: init value not yet evaluated"),
                location: lookup_loc(nid),
            },
            WalkError::Backend(msg) => AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: format!("operator evaluation failed: {msg}"),
                location: None,
            },
        }
    })
}

fn eval_op(
    op: Op,
    immediates: &[u32],
    args: &[Operand],
    width: u32,
    env: &Env,
) -> Result<Bv, String> {
    let read = |i: usize| -> Result<Bv, String> {
        read_operand(env, args[i]).ok_or_else(|| format!("operand {} unevaluated", args[i].nid()))
    };
    // Shift amount, matching the pre-Bv semantics exactly: Sll/Srl/Sra mask the
    // amount to `& (width-1).max(1)`; Rol/Ror take it `% width`. `a` supplies
    // the width, `b` the raw amount (low bits).
    let shl_amount = |a: &Bv, b: &Bv| (b.low128() as u32) & (a.width().saturating_sub(1).max(1));
    let rot_amount = |a: &Bv, b: &Bv| (b.low128() as u32) % a.width().max(1);

    match op {
        Op::Not => Ok(read(0)?.not()),
        Op::Inc => {
            let v = read(0)?;
            let w = v.width();
            Ok(v.add(&Bv::one(w)))
        }
        Op::Dec => {
            let v = read(0)?;
            let w = v.width();
            Ok(v.sub(&Bv::one(w)))
        }
        Op::Neg => Ok(read(0)?.neg()),
        Op::Redand => {
            let v = read(0)?;
            let w = v.width();
            Ok(Bv::from_bool(v == Bv::ones(w)))
        }
        Op::Redor => Ok(Bv::from_bool(read(0)?.is_nonzero())),
        Op::Redxor => Ok(Bv::from_bool(read(0)?.count_ones() & 1 == 1)),
        Op::Iff | Op::Eq => Ok(read(0)?.eq_bv(&read(1)?)),
        Op::Implies => {
            let a = read(0)?;
            let b = read(1)?;
            Ok(Bv::from_bool(!a.to_bool() || b.to_bool()))
        }
        Op::Neq => Ok(read(0)?.ne_bv(&read(1)?)),
        Op::And => Ok(read(0)?.and(&read(1)?)),
        Op::Or => Ok(read(0)?.or(&read(1)?)),
        Op::Xor => Ok(read(0)?.xor(&read(1)?)),
        Op::Nand => Ok(read(0)?.and(&read(1)?).not()),
        Op::Nor => Ok(read(0)?.or(&read(1)?).not()),
        Op::Xnor => Ok(read(0)?.xor(&read(1)?).not()),
        Op::Add => Ok(read(0)?.add(&read(1)?)),
        Op::Sub => Ok(read(0)?.sub(&read(1)?)),
        Op::Mul => Ok(read(0)?.mul(&read(1)?)),
        Op::Ult => Ok(read(0)?.ult(&read(1)?)),
        Op::Ulte => Ok(read(0)?.ulte(&read(1)?)),
        Op::Ugt => Ok(read(0)?.ugt(&read(1)?)),
        Op::Ugte => Ok(read(0)?.ugte(&read(1)?)),
        Op::Slt => Ok(read(0)?.slt(&read(1)?)),
        Op::Slte => Ok(read(0)?.slte(&read(1)?)),
        Op::Sgt => Ok(read(0)?.sgt(&read(1)?)),
        Op::Sgte => Ok(read(0)?.sgte(&read(1)?)),
        Op::Sll => {
            let a = read(0)?;
            let b = read(1)?;
            let s = shl_amount(&a, &b);
            Ok(a.shl(s))
        }
        Op::Srl => {
            let a = read(0)?;
            let b = read(1)?;
            let s = shl_amount(&a, &b);
            Ok(a.shr(s))
        }
        Op::Sra => {
            let a = read(0)?;
            let b = read(1)?;
            let s = shl_amount(&a, &b);
            Ok(a.sra(s))
        }
        Op::Rol => {
            let a = read(0)?;
            let b = read(1)?;
            let s = rot_amount(&a, &b);
            Ok(a.rol(s))
        }
        Op::Ror => {
            let a = read(0)?;
            let b = read(1)?;
            let s = rot_amount(&a, &b);
            Ok(a.ror(s))
        }
        Op::Concat => Ok(read(0)?.concat(&read(1)?)),
        Op::Slice => {
            let a = read(0)?;
            let upper = immediates
                .first()
                .copied()
                .ok_or_else(|| "slice missing upper".to_string())?;
            let lower = immediates
                .get(1)
                .copied()
                .ok_or_else(|| "slice missing lower".to_string())?;
            Ok(a.slice(upper, lower))
        }
        Op::Uext => {
            let a = read(0)?;
            let by = width - a.width();
            Ok(a.uext(by))
        }
        Op::Sext => {
            let a = read(0)?;
            let by = width - a.width();
            Ok(a.sext(by))
        }
        Op::Ite => {
            let c = read(0)?;
            let t = read(1)?;
            let e = read(2)?;
            Ok(if c.to_bool() { t } else { e })
        }
        _ => Err(format!(
            "operator {op:?} unsupported in Phase 1 bit-blaster"
        )),
    }
}

/// §Phase 10 Option-4 step 1a (2026-06-12) — shared constant-node
/// evaluation. Extracted from `evaluate_pure_with_uf_rep`'s inline
/// `Node::Const` match so the bespoke loop AND the new
/// `ConcreteBackend` (the [`crate::adapter::btor2::term_backend::BvTermBackend`]
/// impl) compute constants from ONE source — no duplication, no
/// drift. Returns `Err(message)` on a malformed literal.
pub(crate) fn eval_const_value(value: &ConstValue, width: u32) -> Result<Bv, String> {
    use num_bigint::BigUint;
    let bv = match value {
        ConstValue::Zero => Bv::zero(width),
        ConstValue::One => Bv::one(width),
        ConstValue::Ones => Bv::ones(width),
        // AR-S2 — arbitrary-width literals: parse the full magnitude (no u128
        // ceiling) and let `Bv::from_biguint` mask + pick the Small/Wide storage.
        ConstValue::Bin(s) => {
            let mag = BigUint::parse_bytes(s.as_bytes(), 2)
                .ok_or_else(|| "bad binary literal".to_string())?;
            Bv::from_biguint(mag, width)
        }
        ConstValue::Dec(d) => {
            if *d >= 0 {
                Bv::from_biguint(BigUint::from(*d as u128), width)
            } else {
                // Two's complement of a negative literal: (2^width - |d|) mod 2^width.
                let modulus = BigUint::from(1u8) << width;
                let abs = BigUint::from((-d) as u128) % &modulus;
                Bv::from_biguint((&modulus - abs) % &modulus, width)
            }
        }
        ConstValue::Hex(s) => {
            let mag = BigUint::parse_bytes(s.as_bytes(), 16)
                .ok_or_else(|| "bad hex literal".to_string())?;
            Bv::from_biguint(mag, width)
        }
    };
    Ok(bv)
}

/// §Phase 10 Option-4 step 1a (2026-06-12) — the concrete
/// instantiation of [`crate::adapter::btor2::term_backend::BvTermBackend`].
///
/// `Value = Bv`. Delegates operator evaluation to the existing
/// [`eval_op`] free function so the arithmetic is bit-identical to
/// the production `evaluate_pure` path — this backend is a faithful
/// re-expression of the concrete evaluator behind the unified seam,
/// not a reimplementation. Constants go through the shared
/// [`eval_const_value`]; the UF substitution mirrors
/// `evaluate_pure_with_uf_rep`'s representative logic.
///
/// Holds its own [`Env`] (the `Nid → Bv` store), seeded by the
/// caller with the input + state bindings (the same env
/// `make_*_env` builds). As of step 1b this IS the production
/// concrete evaluator: `evaluate_pure_with_uf_rep` drives
/// `walk_design::<ConcreteBackend>`.
pub(crate) struct ConcreteBackend<'a> {
    env: Env,
    honor_init: bool,
    uf_wrapped_nids: Option<&'a std::collections::HashSet<Nid>>,
    rep: UfRepresentative,
}

impl<'a> ConcreteBackend<'a> {
    /// Build a concrete backend over a pre-seeded env (input + state
    /// bindings). `uf_wrapped_nids` + `rep` mirror
    /// `evaluate_pure_with_uf_rep`'s UF-substitution parameters;
    /// pass `None` + `UfRepresentative::Zero` for the no-UF path.
    pub(crate) fn new(
        env: Env,
        honor_init: bool,
        uf_wrapped_nids: Option<&'a std::collections::HashSet<Nid>>,
        rep: UfRepresentative,
    ) -> Self {
        Self {
            env,
            honor_init,
            uf_wrapped_nids,
            rep,
        }
    }

    /// Consume the backend, returning the populated env.
    pub(crate) fn into_env(self) -> Env {
        self.env
    }
}

impl crate::adapter::btor2::term_backend::BvTermBackend for ConcreteBackend<'_> {
    type Value = Bv;
    type Error = String;

    fn eval_const(&mut self, value: &ConstValue, width: u32) -> Result<Bv, String> {
        eval_const_value(value, width)
    }

    fn eval_op(
        &mut self,
        _nid: Nid,
        op: Op,
        immediates: &[u32],
        args: &[Operand],
        width: u32,
    ) -> Result<Bv, String> {
        // The concrete free-fn `eval_op` derives its own error
        // context from the operand NIDs; the node nid is unused here.
        eval_op(op, immediates, args, width, &self.env)
    }

    fn bind(&mut self, nid: Nid, value: Bv) {
        self.env.values.insert(nid, value);
    }

    fn honor_init(&self) -> bool {
        self.honor_init
    }

    fn read_operand(&self, op: Operand) -> Option<Bv> {
        read_operand(&self.env, op)
    }

    fn uf_substitute(&mut self, nid: Nid, width: u32) -> Option<Bv> {
        if self.uf_wrapped_nids.is_some_and(|s| s.contains(&nid)) {
            Some(self.rep.to_value(width))
        } else {
            None
        }
    }
}

/// High-level entry: parse + bit-blast + emit.
pub fn translate(content: &str, options: &AdapterOptions) -> Result<AdapterOutput, AdapterError> {
    let file = super::parser::parse(content)?;
    let (ir, warnings, partition_summary) = to_ir(&file, options)?;

    let state_count = ir.automata.first().map(|a| a.states.len()).unwrap_or(0);
    let signal_count = ir.signals.len();
    let property_count = ir.properties.len();

    let emit_result = crate::adapter::emit::emit(&ir)?;

    // Extract state valuations from the IR and stash them in the output's
    // side-channel map. Same pattern as the SV adapter: the CTXDSL text
    // does not encode valuations, so the realize pipeline picks them up
    // from `AdapterOutput.state_valuations` and registers them on the
    // CLTS for the on-demand predicate evaluator.
    let mut state_valuations: std::collections::HashMap<
        String,
        std::collections::HashMap<String, std::collections::BTreeMap<String, String>>,
    > = std::collections::HashMap::new();
    for aut in &ir.automata {
        let mut per_state: std::collections::HashMap<
            String,
            std::collections::BTreeMap<String, String>,
        > = std::collections::HashMap::new();
        for s in &aut.states {
            if let Some(v) = &s.valuations {
                per_state.insert(s.name.clone(), v.clone());
            }
        }
        if !per_state.is_empty() {
            state_valuations.insert(aut.name.clone(), per_state);
        }
    }

    Ok(AdapterOutput {
        sidecars: Vec::new(),
        ctxdsl: emit_result.ctxdsl,
        warnings,
        source_info: SourceInfo {
            format: SourceFormat::Btor2,
            title: None,
            signal_count,
            state_count,
            property_count,
        },
        state_valuations,
        transition_observations: Default::default(),
        partition_summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterOptions;

    /// Measure-first probe (register-width wall, 2026-07-26). Decomposes a property CONE's
    /// state registers into the classes a *sound* width-reducer can address, sizing the ceiling
    /// of F2 (counter) + data-independence BEFORE either is built:
    /// - `counter(F2)`   — `detect_down_counter`: value-range re-encodable to ⌈log₂⌉ bits.
    /// - `enum(F1)`      — `detect_enum_states`: already re-encoded by #381.
    /// - `frozen(F4)`    — next = self / constant: constant-foldable.
    /// - `data-movement` — value only moved/copied (concat/slice/mux-branch/eq-vs-var), never
    ///   branched on: a Wolper data-independence candidate (shrinkable to 1–2 bits).
    /// - `data-dependent`— value reaches arithmetic / ordering / eq-vs-const / reduction / a
    ///   mux-select: genuinely value-dependent → not soundly width-reducible (research / ⊥).
    ///
    /// `#[ignore]` (a measurement harness, not an assertion). Reads `MUNUNU_PROBE_BTOR2` (a
    /// lifted design.btor) and optional `MUNUNU_PROBE_ATOMS` (comma-separated seed signal names
    /// for the cone; empty ⇒ whole design). The data-movement/-dependent split approximates
    /// Wolper data-independence and is **hardened** three ways so it does not over-count
    /// movement: (1) *property-aware* — a register whose value reaches a seed signal is
    /// data-dependent (the property compares it against a constant); (2) *sequential* — the walk
    /// follows `next` edges so a value moved through pipeline/shift stages before interpretation
    /// is traced; (3) *table-aware* — a value used as a `Read` index (S-box / lookup) is
    /// interpreted. Measured (2026-07-26): i2c 86% / gost 100% / present 100% data-dependent —
    /// the residual register wall for value-comparing recoverability is datapath-dependence,
    /// not width the F1/F2/F4 re-encoders can reclaim.
    #[test]
    #[ignore]
    fn probe_cone_register_width_decomposition() {
        use crate::adapter::btor2::ast::{Line, Nid, Node, Op};
        use crate::adapter::btor2::dep_graph::cone_leaf_nids;
        use std::collections::{BTreeMap, HashMap, HashSet};

        let Ok(path) = std::env::var("MUNUNU_PROBE_BTOR2") else {
            eprintln!(
                "SKIPPED: this is an opt-in measurement probe, not an assertion — set \
                 MUNUNU_PROBE_BTOR2=<design.btor2> to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read btor2");
        let file = parser::parse(&content).expect("parse btor2");
        let atoms: Vec<String> = std::env::var("MUNUNU_PROBE_ATOMS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // state register widths
        let mut state_width: HashMap<Nid, u32> = HashMap::new();
        for l in &file.lines {
            if let Node::State { sort, .. } = &l.node {
                state_width.insert(l.nid, parser::bv_width(&file, *sort).unwrap_or(0));
            }
        }
        let enum_nids: HashSet<Nid> = detect_enum_states(&file).iter().map(|m| m.nid).collect();
        let counter_nids: HashSet<Nid> = detect_down_counter(&file).iter().map(|m| m.nid).collect();

        // cone state registers (whole design if no atoms given)
        let cone_states: Vec<Nid> = if atoms.is_empty() {
            state_width.keys().copied().collect()
        } else {
            let leaves = cone_leaf_nids(&file, &atoms);
            state_width
                .keys()
                .copied()
                .filter(|n| leaves.contains(n))
                .collect()
        };

        // next-value operand per state (frozen check)
        let mut next_of: HashMap<Nid, Nid> = HashMap::new();
        for l in &file.lines {
            if let Node::Next { state, value, .. } = &l.node {
                next_of.insert(*state, value.nid().abs());
            }
        }
        fn is_const_node(file: &Btor2File, nid: Nid) -> bool {
            matches!(file.lookup(nid).map(|l| &l.node), Some(Node::Const { .. }))
        }
        let frozen = |s: Nid| match next_of.get(&s) {
            None => true,
            Some(&nv) => nv == s || is_const_node(&file, nv),
        };

        // uses map: nid -> [(user_op, arg_index, user_nid)]
        let mut uses: HashMap<Nid, Vec<(Op, usize, Nid)>> = HashMap::new();
        for l in &file.lines {
            if let Node::Op { op, args, .. } = &l.node {
                for (i, a) in args.iter().enumerate() {
                    uses.entry(a.nid().abs()).or_default().push((*op, i, l.nid));
                }
            }
        }
        // SEQUENTIAL edges: a `next` node's value-root feeds its state register, so a value that
        // MOVES through a register (shift / pipeline stage) before being interpreted is traced.
        // Without this the forward walk stops at register boundaries and under-flags dependence.
        let mut next_drives: HashMap<Nid, Vec<Nid>> = HashMap::new();
        for l in &file.lines {
            if let Node::Next { state, value, .. } = &l.node {
                next_drives
                    .entry(value.nid().abs())
                    .or_default()
                    .push(*state);
            }
        }
        // PROPERTY-AWARE sink: the recoverability property compares each seed signal against a
        // constant (`target == K`), so ANY register whose value reaches a seed signal is
        // value-relevant to that comparison — data-dependent w.r.t. the property regardless of
        // how the design routes it. Resolve each atom name to its signal node.
        let mut seed_nids: HashSet<Nid> = HashSet::new();
        for l in &file.lines {
            match &l.node {
                Node::Op {
                    symbol: Some(s), ..
                }
                | Node::State {
                    symbol: Some(s), ..
                }
                | Node::Input {
                    symbol: Some(s), ..
                } if atoms.iter().any(|a| a == s) => {
                    seed_nids.insert(l.nid);
                }
                Node::Output {
                    signal,
                    symbol: Some(s),
                } if atoms.iter().any(|a| a == s) => {
                    seed_nids.insert(signal.nid().abs());
                }
                _ => {}
            }
        }
        eprintln!(
            "  (property-aware seed nodes resolved: {})",
            seed_nids.len()
        );
        fn other_operand_is_const(file: &Btor2File, user_nid: Nid, this: Nid) -> bool {
            if let Some(Line {
                node: Node::Op { args, .. },
                ..
            }) = file.lookup(user_nid)
            {
                args.iter()
                    .any(|a| a.nid().abs() != this && is_const_node(file, a.nid().abs()))
            } else {
                false
            }
        }
        // Wolper data-independence approximation: does the register's value (propagated through
        // value-movement ops) reach a use that distinguishes specific values?
        fn is_data_dependent(
            file: &Btor2File,
            uses: &HashMap<Nid, Vec<(Op, usize, Nid)>>,
            next_drives: &HashMap<Nid, Vec<Nid>>,
            seed_nids: &HashSet<Nid>,
            start: Nid,
        ) -> bool {
            let arith = |o: Op| {
                matches!(
                    o,
                    Op::Add
                        | Op::Sub
                        | Op::Mul
                        | Op::Neg
                        | Op::Inc
                        | Op::Dec
                        | Op::Sdiv
                        | Op::Udiv
                        | Op::Smod
                        | Op::Srem
                        | Op::Urem
                        | Op::Sll
                        | Op::Srl
                        | Op::Sra
                        | Op::Rol
                        | Op::Ror
                )
            };
            let ordering = |o: Op| {
                matches!(
                    o,
                    Op::Sgt
                        | Op::Ugt
                        | Op::Sgte
                        | Op::Ugte
                        | Op::Slt
                        | Op::Ult
                        | Op::Slte
                        | Op::Ulte
                )
            };
            let reduction = |o: Op| matches!(o, Op::Redand | Op::Redor | Op::Redxor);
            let mut seen = HashSet::new();
            let mut stack = vec![start];
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                if seed_nids.contains(&n) {
                    return true; // value reaches the property-compared signal (`target == K`)
                }
                // Sequential movement: this node roots some register's next-state → follow into
                // that register so the value's downstream interpretation is seen.
                if let Some(driven) = next_drives.get(&n) {
                    stack.extend(driven.iter().copied());
                }
                let Some(users) = uses.get(&n) else { continue };
                for &(op, idx, un) in users {
                    if arith(op) || ordering(op) || reduction(op) {
                        return true; // value's magnitude/bits are computed on
                    }
                    if matches!(op, Op::Eq | Op::Neq) && other_operand_is_const(file, un, n) {
                        return true; // compared against a specific constant
                    }
                    if op == Op::Ite && idx == 0 {
                        return true; // register drives a control select
                    }
                    if op == Op::Read && idx == 1 {
                        return true; // data indexes a table (S-box / lookup) — interpreted
                    }
                    stack.push(un); // movement: concat/slice/uext/sext/bitwise/ite-branch/eq-vs-var
                }
            }
            false
        }

        let mut bits: BTreeMap<&str, u32> = BTreeMap::new();
        let mut cnt: BTreeMap<&str, u32> = BTreeMap::new();
        let mut hard: Vec<u32> = Vec::new();
        for &s in &cone_states {
            let w = *state_width.get(&s).unwrap_or(&0);
            let class = if counter_nids.contains(&s) {
                "counter(F2)"
            } else if enum_nids.contains(&s) {
                "enum(F1)"
            } else if frozen(s) {
                "frozen(F4)"
            } else if is_data_dependent(&file, &uses, &next_drives, &seed_nids, s) {
                hard.push(w);
                "data-dependent(research/⊥)"
            } else {
                "data-movement(indep.)"
            };
            *bits.entry(class).or_default() += w;
            *cnt.entry(class).or_default() += 1;
        }
        let tot: u32 = bits.values().sum();
        eprintln!(
            "=== cone register width decomposition: {} regs, {} bits (cap 64) ===",
            cone_states.len(),
            tot
        );
        for (c, b) in &bits {
            eprintln!(
                "  {:28} {:3} regs {:5} bits ({}%)",
                c,
                cnt[c],
                b,
                100 * b / tot.max(1)
            );
        }
        hard.sort_unstable_by(|a, b| b.cmp(a));
        eprintln!(
            "  widest data-dependent reg widths: {:?}",
            &hard[..hard.len().min(10)]
        );
        // F2 REDUCTION POTENTIAL: a counter re-encode only saves bits when the reachable range
        // [0, init] is ≪ 2^width (an over-wide counter register). Report per in-cone counter.
        eprintln!("  --- F2 counter reduction potential (in-cone) ---");
        let mut f2_total = 0u32;
        for m in detect_down_counter(&file) {
            let in_cone = if atoms.is_empty() {
                true
            } else {
                let leaves = cone_leaf_nids(&file, &atoms);
                leaves.contains(&m.nid)
            };
            if !in_cone {
                continue;
            }
            let init =
                init_value_of(&file, m.nid).and_then(|op| resolve_btor2_constant(&file, op.nid()));
            let re_bits = match init {
                Some(v) if v >= 1 => 64 - v.leading_zeros(), // ⌈log₂(v+1)⌉ for a 0..v range
                Some(_) => 1,
                None => m.width, // unknown init ⇒ assume no saving
            };
            let saving = m.width.saturating_sub(re_bits);
            f2_total += saving;
            eprintln!(
                "    counter nid={} width={} init={:?} → reencoded {} bits, saving {}",
                m.nid, m.width, init, re_bits, saving
            );
        }
        eprintln!("  F2 total bit-saving on this cone: {f2_total}");
    }

    /// MK.0 feasibility probe for mixed-precision KMTS (`.claude/plans/mixed-precision-kmts.md`).
    /// The go/no-go before building the pipeline: partition the cone's state registers into
    /// CONTROL (`C`, kept concrete) vs DATAPATH (`D`, predicate-abstracted), and bound the
    /// reachable concrete-control state count `|Reach_C|`. If `|Reach_C|` is enumerable the
    /// approach applies; if it explodes on a control-web design (i2c) the control cannot stay
    /// concrete and mixed-precision degrades to the plain cube.
    ///
    /// A register is DATAPATH iff its value (through value-movement ops + register stages)
    /// reaches an ARITHMETIC op or a READ index — its magnitude/bits are computed on, so it
    /// cannot be enumerated. CONTROL uses — driving an `ite` SELECT (`args[0]`), `eq`/ordering
    /// COMPARISONS, bit REDUCTIONS — do NOT make a register datapath; those are exactly the
    /// decision-tree edges the concrete control LTS must keep exact. Small-value-set registers
    /// (`detect_enum_states`, `detect_down_counter`) and frozen/config registers are always
    /// control with a KNOWN reachable count (the "tight" bound); a narrow non-enum control
    /// register contributes a CRUDE `2^width` upper bound (its reachable subset is unknown to a
    /// syntactic probe — that gap is what MK.1's actual reachability closes).
    ///
    /// `MUNUNU_PROBE_BTOR2=<file> MUNUNU_PROBE_ATOMS=<sig[,sig...]>`; no atoms ⇒ whole design.
    #[test]
    #[ignore = "MK.0 probe — set MUNUNU_PROBE_BTOR2 + MUNUNU_PROBE_ATOMS"]
    fn probe_mixed_precision_reach_c_partition() {
        use crate::adapter::btor2::ast::{Nid, Node, Op};
        use crate::adapter::btor2::dep_graph::cone_leaf_nids;
        use std::collections::{HashMap, HashSet};

        let Ok(path) = std::env::var("MUNUNU_PROBE_BTOR2") else {
            eprintln!(
                "SKIPPED: this is an opt-in measurement probe, not an assertion — set \
                 MUNUNU_PROBE_BTOR2=<design.btor2> to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read btor2");
        let file = parser::parse(&content).expect("parse btor2");
        let atoms: Vec<String> = std::env::var("MUNUNU_PROBE_ATOMS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // state widths + the shipped small-value-set detectors
        let mut state_width: HashMap<Nid, u32> = HashMap::new();
        for l in &file.lines {
            if let Node::State { sort, .. } = &l.node {
                state_width.insert(l.nid, parser::bv_width(&file, *sort).unwrap_or(0));
            }
        }
        let enum_values: HashMap<Nid, usize> = detect_enum_states(&file)
            .iter()
            .map(|m| (m.nid, m.values.len()))
            .collect();
        let counter_nids: HashSet<Nid> = detect_down_counter(&file).iter().map(|m| m.nid).collect();

        let cone_states: Vec<Nid> = if atoms.is_empty() {
            state_width.keys().copied().collect()
        } else {
            let leaves = cone_leaf_nids(&file, &atoms);
            state_width
                .keys()
                .copied()
                .filter(|n| leaves.contains(n))
                .collect()
        };

        // frozen check + forward-use map + sequential (next) movement
        let mut next_of: HashMap<Nid, Nid> = HashMap::new();
        let mut uses: HashMap<Nid, Vec<(Op, usize, Nid)>> = HashMap::new();
        let mut next_drives: HashMap<Nid, Vec<Nid>> = HashMap::new();
        for l in &file.lines {
            match &l.node {
                Node::Next { state, value, .. } => {
                    next_of.insert(*state, value.nid().abs());
                    next_drives
                        .entry(value.nid().abs())
                        .or_default()
                        .push(*state);
                }
                Node::Op { op, args, .. } => {
                    for (i, a) in args.iter().enumerate() {
                        uses.entry(a.nid().abs()).or_default().push((*op, i, l.nid));
                    }
                }
                _ => {}
            }
        }
        let is_const =
            |nid: Nid| matches!(file.lookup(nid).map(|l| &l.node), Some(Node::Const { .. }));
        let frozen = |s: Nid| match next_of.get(&s) {
            None => true,
            Some(&nv) => nv == s || is_const(nv),
        };

        // datapath discriminator: value reaches an arith op or a Read index
        fn feeds_datapath(
            uses: &HashMap<Nid, Vec<(Op, usize, Nid)>>,
            next_drives: &HashMap<Nid, Vec<Nid>>,
            start: Nid,
        ) -> bool {
            let arith = |o: Op| {
                matches!(
                    o,
                    Op::Add
                        | Op::Sub
                        | Op::Mul
                        | Op::Neg
                        | Op::Inc
                        | Op::Dec
                        | Op::Sdiv
                        | Op::Udiv
                        | Op::Smod
                        | Op::Srem
                        | Op::Urem
                        | Op::Sll
                        | Op::Srl
                        | Op::Sra
                        | Op::Rol
                        | Op::Ror
                )
            };
            let control = |o: Op| {
                matches!(
                    o,
                    Op::Eq
                        | Op::Neq
                        | Op::Sgt
                        | Op::Ugt
                        | Op::Sgte
                        | Op::Ugte
                        | Op::Slt
                        | Op::Ult
                        | Op::Slte
                        | Op::Ulte
                        | Op::Redand
                        | Op::Redor
                        | Op::Redxor
                )
            };
            let mut seen = HashSet::new();
            let mut stack = vec![start];
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                if let Some(driven) = next_drives.get(&n) {
                    stack.extend(driven.iter().copied());
                }
                let Some(users) = uses.get(&n) else { continue };
                for &(op, idx, un) in users {
                    if arith(op) {
                        return true; // magnitude/bits computed on ⇒ datapath
                    }
                    if op == Op::Read && idx == 1 {
                        return true; // indexes a table ⇒ datapath
                    }
                    if control(op) || (op == Op::Ite && idx == 0) {
                        continue; // control comparison / select — NOT datapath, stop here
                    }
                    stack.push(un); // value-movement: concat/slice/uext/sext/ite-branch/bitwise
                }
            }
            false
        }

        const WIDE: u32 = 16; // a non-enum non-counter register wider than this is treated as data
        let mut c_regs: Vec<(Nid, u32, f64, &str)> = Vec::new();
        let mut d_regs: Vec<(Nid, u32, &str)> = Vec::new();
        let mut log2_tight = 0.0f64; // enum/counter/frozen — reachable count KNOWN
        let mut log2_flags = 0.0f64; // 1-bit control (FSM/flags) — genuinely control, correlated
        let mut log2_multibit = 0.0f64; // multi-bit non-enum control — the RISK (shift/byte/data?)
        let mut coupled = 0usize; // D registers that ALSO drive a control decision (div r_bit)
        let mut counters: Vec<(Nid, u32, u64)> = Vec::new(); // nid, width, reachable range
        let mut multibit: Vec<(Nid, u32)> = Vec::new(); // the risk regs MK.1 must measure

        for &s in &cone_states {
            let w = *state_width.get(&s).unwrap_or(&0);
            if let Some(&nv) = enum_values.get(&s) {
                let l = (nv.max(1) as f64).log2();
                log2_tight += l;
                c_regs.push((s, w, l, "enum"));
            } else if counter_nids.contains(&s) {
                let init =
                    init_value_of(&file, s).and_then(|op| resolve_btor2_constant(&file, op.nid()));
                let range = init
                    .map(|v| v.saturating_add(1))
                    .unwrap_or(1u64 << w.min(63));
                let l = (range.max(1) as f64).log2();
                log2_tight += l;
                counters.push((s, w, range));
                c_regs.push((s, w, l, "counter"));
            } else if frozen(s) {
                c_regs.push((s, w, 0.0, "frozen/config(1 val)"));
            } else if feeds_datapath(&uses, &next_drives, s) {
                let drives_ctrl = uses.get(&s).is_some_and(|us| {
                    us.iter().any(|&(op, idx, _)| {
                        (op == Op::Ite && idx == 0) || matches!(op, Op::Eq | Op::Neq)
                    })
                });
                if drives_ctrl {
                    coupled += 1;
                }
                d_regs.push((s, w, "datapath(arith/index)"));
            } else if w > WIDE {
                d_regs.push((s, w, "wide-nonenum(data)"));
            } else if w == 1 {
                log2_flags += 1.0;
                c_regs.push((s, w, 1.0, "flag(1-bit)"));
            } else {
                log2_multibit += w as f64;
                multibit.push((s, w));
                c_regs.push((s, w, w as f64, "multibit-control(2^w)"));
            }
        }

        let log2_crude = log2_flags + log2_multibit;
        let bits_c = log2_tight + log2_crude;
        let bits_d: u32 = d_regs.iter().map(|(_, w, _)| *w).sum();
        eprintln!(
            "=== MK.0 mixed-precision partition + |Reach_C| bound: {} cone regs, atoms={:?} ===",
            cone_states.len(),
            atoms
        );
        let mut by_tag: std::collections::BTreeMap<&str, (usize, u32)> =
            std::collections::BTreeMap::new();
        for (_, w, _, t) in &c_regs {
            let e = by_tag.entry(t).or_default();
            e.0 += 1;
            e.1 += w;
        }
        eprintln!("  CONTROL (C): {} regs (kept concrete)", c_regs.len());
        for (t, (n, b)) in &by_tag {
            eprintln!("    {:24} {:3} regs {:4} bits", t, n, b);
        }
        eprintln!(
            "  DATAPATH (D): {} regs, {} bits (predicate-abstracted)",
            d_regs.len(),
            bits_d
        );
        if !counters.is_empty() {
            eprintln!("  counters (concrete-enumeration cost = the reachable RANGE, not 1 bit):");
            for (nid, w, range) in &counters {
                let note = if *range > (1 << 12) {
                    " ⚠ WIDE — needs counter/ranking-abstraction, not concrete enumeration"
                } else {
                    ""
                };
                eprintln!("    nid={nid} width={w} range={range}{note}");
            }
        }
        eprintln!(
            "  |Reach_C| ≤ 2^{:.1}  =  tight(enum/counter) 2^{:.1} · flags(1-bit) 2^{:.1} · multibit-control 2^{:.1}",
            bits_c, log2_tight, log2_flags, log2_multibit
        );
        if bits_c <= 40.0 {
            eprintln!("     crude bound ≈ {} states", bits_c.exp2() as u128);
        }
        eprintln!(
            "  mixed C-D coupling (D regs that also drive control, e.g. div r_bit): {coupled}"
        );
        // Concrete enumeration cost is dominated by the counter RANGES + the multibit-control regs;
        // 1-bit flags are FSM-correlated (reachable ≪ 2^flags). A WIDE counter (range ≫ 2^12) means
        // the control itself needs counter/ranking-abstraction — the flat 2-way split degrades.
        let wide_counter = counters.iter().any(|(_, _, r)| *r > (1 << 12));
        let hard_core = log2_tight + log2_multibit; // enumeration cost ignoring flag correlation
        let verdict = if bits_c <= 20.0 {
            "GREEN — |Reach_C| enumerable even at the crude bound ⇒ mixed-precision APPLIES cleanly"
        } else if wide_counter {
            "AMBER — a WIDE counter sits in the control cone: fully-concrete control is NOT small; \
             mixed-precision needs the control ITSELF layered (concrete FSM + ranking-abstracted \
             counter + predicate data), a 3-way split beyond the design's flat concrete-control/\
             predicate-data. The FSM/flag skeleton may still be small — MK.1 must measure it after \
             lifting the counters out."
        } else if hard_core <= 20.0 {
            "INCONCLUSIVE(cheap) — enumeration cost (counters + multibit-control) is small; the \
             large bound is only the 1-bit-flag 2^w over-estimate (FSM-correlated, reachable ≪ 2^n) \
             ⇒ plausibly tractable, MK.1 reachability confirms"
        } else {
            "RED — multibit-control state alone is large ⇒ concrete control does NOT stay small; \
             those registers must move to D (predicated) or the split degrades to the cube"
        };
        eprintln!("  VERDICT: {verdict}");
        if !multibit.is_empty() {
            eprintln!(
                "  multibit-control regs (the RISK — MK.1 must measure their reachable subset or move to D):"
            );
            for (nid, w) in &multibit {
                eprintln!("    nid={nid} width={w}");
            }
        }
    }

    /// MK.0 cross-check — run the #385 recoverability-⊥ diagnosis (+ the scalable verdict) on a
    /// lifted design, to decide whether a register-dominated ⊥ is (a) config/sequence/counter
    /// dependent (mixed-precision's concrete-FSM does NOT help) or (b) "coarse-but-closable"
    /// abstraction, i.e. FSM-predicate-coarseness (the one case a concrete control skeleton fixes).
    /// `MUNUNU_PROBE_BTOR2=<file> MUNUNU_PROBE_GOOD="REG == VALUE"`.
    #[test]
    #[ignore = "MK.0 cross-check — set MUNUNU_PROBE_BTOR2 + MUNUNU_PROBE_GOOD"]
    fn probe_recoverability_bot_diagnosis() {
        let Ok(path) = std::env::var("MUNUNU_PROBE_BTOR2") else {
            eprintln!(
                "SKIPPED: this is an opt-in measurement probe, not an assertion — set \
                 MUNUNU_PROBE_BTOR2=<design.btor2> to run it."
            );
            return;
        };
        let good = std::env::var("MUNUNU_PROBE_GOOD").expect("set MUNUNU_PROBE_GOOD");
        let content = std::fs::read_to_string(&path).expect("read btor2");
        let verdict =
            crate::adapter::recoverability::verify_recoverability_scalable(&content, &good, &[]);
        let diag = crate::adapter::recoverability::diagnose_recoverability_bot(&content, &good);
        eprintln!("=== MK.0 cross-check: recoverability ⊥ diagnosis for `AG EF ({good})` ===");
        eprintln!("  scalable verdict: {verdict:?}");
        eprintln!(
            "  uncertified gating counters: {:?}",
            diag.uncertified_counters
        );
        eprintln!("  wide data/config influences: {:?}", diag.wide_influences);
        eprintln!(
            "  is_empty (coarse-but-closable ⇒ possible FSM-coarseness): {}",
            diag.is_empty()
        );
        eprintln!("  HINT: {}", diag.hint());
        // The shipped diagnosis is SYMBOL-gated (skips unnamed regs), so on yosys lifts it misses
        // internal counters. Re-scan the cone by NID to reveal what the symbol gate dropped.
        if let Ok(file) = parser::parse(&content) {
            use crate::adapter::btor2::ast::{Nid, Node};
            use crate::adapter::btor2::dep_graph::cone_leaf_nids;
            let atom = good.split("==").next().unwrap_or(&good).trim().to_string();
            let leaves = cone_leaf_nids(&file, std::slice::from_ref(&atom));
            let named: std::collections::HashMap<Nid, String> = file
                .lines
                .iter()
                .filter_map(|l| match &l.node {
                    Node::State {
                        symbol: Some(s), ..
                    }
                    | Node::Op {
                        symbol: Some(s), ..
                    } => Some((l.nid, s.clone())),
                    _ => None,
                })
                .collect();
            eprintln!("  --- cone down-counters BY NID (bypassing the diagnosis symbol gate) ---");
            for c in detect_down_counter(&file) {
                if leaves.contains(&c.nid) {
                    let nm = named.get(&c.nid).map(|s| s.as_str()).unwrap_or("<UNNAMED>");
                    eprintln!(
                        "    counter nid={} width={} threshold={} name={} — the diagnosis {} it",
                        c.nid,
                        c.width,
                        c.threshold,
                        nm,
                        if named.contains_key(&c.nid) {
                            "sees"
                        } else {
                            "SKIPS (unnamed)"
                        }
                    );
                }
            }
        }
    }

    /// MEASUREMENT — control-automaton state/transition scaling (synthetic, explicit BFS; no
    /// invented numbers). A synchronous controller's Kripke automaton has one STATE per register
    /// valuation, so worst-case |S| = 2^W for W total register bits (the per-state VALUATION width);
    /// TRANSITIONS are δ(s, input)→s' — worst-case |δ| = |S|·2^I explicit edges. The automaton is
    /// never explicitly built: verification runs symbolic BDD fixpoints over the transition DAG.
    /// This probe MEASURES the *reachable* |S| and |δ| of the canonical protocol-controller control
    /// shape — a K-bit prescale counter driving an N-state FSM — to show how each register/counter
    /// parameter drives the size (a COUNTER dimension is tight: 2^K reachable; an FSM dimension is
    /// its state count N, independent of its bit-encoding width).
    #[test]
    #[ignore = "measurement — synthetic control-automaton scaling (explicit BFS)"]
    fn measure_control_automaton_scaling() {
        use std::collections::{HashSet, VecDeque};

        // (1) An ISOLATED K-bit reloading down-counter (free-running): cnt' = (cnt==0)?MAX:cnt-1.
        // MEASURE its reachable-state count by BFS — the empirical basis for "a counter reaches all
        // 2^K values" (so its contribution to |S| is TIGHT: measured == worst-case).
        fn counter_reach(k: u32) -> u64 {
            let max = (1u64 << k) - 1;
            let mut seen = HashSet::new();
            let mut q = VecDeque::new();
            seen.insert(max);
            q.push_back(max);
            while let Some(c) = q.pop_front() {
                let nc = if c == 0 { max } else { c - 1 };
                if seen.insert(nc) {
                    q.push_back(nc);
                }
            }
            seen.len() as u64
        }
        eprintln!("=== (1) isolated K-bit reloading down-counter — MEASURED reachable states ===");
        eprintln!(
            "  {:>3} {:>14} {:>16}",
            "K", "2^K (worst)", "measured |reach|"
        );
        for k in [4u32, 8, 12, 16, 20] {
            let m = counter_reach(k);
            eprintln!("  {:>3} {:>14} {:>16}", k, 1u64 << k, m);
            assert_eq!(
                m,
                1u64 << k,
                "a reloading K-bit counter reaches EXACTLY its 2^K values"
            );
        }

        // (2) The canonical control shape: a K-bit prescaler driving an N-state (one-hot) FSM. The
        // counter decrements each enabled cycle; on expiry it reloads and the FSM advances; an
        // `enable` input (nondeterministic) gates the decrement (a stall self-loop when low). State =
        // (fsm ∈ 0..N, cnt ∈ 0..2^K). MEASURE reachable |S| + transition |δ| by BFS.
        fn prescaler_fsm(n: u32, k: u32) -> (u64, u64) {
            let max = (1u32 << k) - 1;
            let init = (0u32, max);
            let mut seen = HashSet::new();
            let mut edges = 0u64;
            let mut q = VecDeque::new();
            seen.insert(init);
            q.push_back(init);
            while let Some((fsm, cnt)) = q.pop_front() {
                for enable in [false, true] {
                    let next = if !enable {
                        (fsm, cnt) // stall (enable low)
                    } else if cnt == 0 {
                        ((fsm + 1) % n, max) // expiry: advance FSM, reload
                    } else {
                        (fsm, cnt - 1) // decrement
                    };
                    edges += 1;
                    if seen.insert(next) {
                        q.push_back(next);
                    }
                }
            }
            (seen.len() as u64, edges)
        }
        eprintln!(
            "=== (2) prescaler(K) + one-hot FSM(N) — MEASURED reachable |S|, transitions |δ| ==="
        );
        eprintln!(
            "  one-hot valuation width W = N+K bits; binary-encoded W = ⌈log2 N⌉+K. worst |S| = 2^W."
        );
        eprintln!(
            "  {:>3} {:>3} {:>7} {:>7} {:>16} {:>14} {:>14} {:>10}",
            "N",
            "K",
            "W_1hot",
            "W_bin",
            "worst 2^W_1hot",
            "measured |S|",
            "measured |δ|",
            "|S|/2^W1h"
        );
        for &(n, k) in &[
            (4u32, 4u32),
            (4, 8),
            (4, 12),
            (4, 16),
            (8, 12),
            (16, 12),
            (4, 18),
        ] {
            let (s, e) = prescaler_fsm(n, k);
            let w_1hot = n + k;
            let w_bin = (32 - (n - 1).leading_zeros()) + k;
            let worst = if w_1hot < 63 {
                1u128 << w_1hot
            } else {
                u128::MAX
            };
            eprintln!(
                "  {:>3} {:>3} {:>7} {:>7} {:>16} {:>14} {:>14} {:>10.2e}",
                n,
                k,
                w_1hot,
                w_bin,
                worst,
                s,
                e,
                s as f64 / worst as f64
            );
            // The MEASURED law: |S| = N · 2^K — the counter dimension is tight (2^K), the FSM
            // dimension is its state count N (independent of the N-bit one-hot encoding width).
            assert_eq!(
                s,
                n as u64 * (1u64 << k),
                "measured reachable |S| = N · 2^K (counter tight, FSM = its N states)"
            );
        }
        eprintln!(
            "  ⇒ MEASURED law: |S| = N·2^K. The K-bit counter multiplies |S| by 2^K (irreducible — it \
             reaches all values); the FSM multiplies by its state count N (≪ 2^N when one-hot). So a \
             WIDE counter, not the FSM, is what makes |S| explode: K=16 ⇒ ×65 536; K=26 ⇒ ×67 108 864."
        );
    }

    /// MEASUREMENT — the STRUCTURAL automaton size of a real lifted design (exact, reproducible from
    /// the btor2; no simulation). Reports the valuation width W (= total state-register bits), the
    /// worst-case |S| = 2^W, the transition-DAG size (btor2 nodes = the *symbolic* transition
    /// relation, which is what is actually built — the explicit automaton never is), the free-input
    /// width I (worst-case explicit fan-out 2^I per state), and — the key number — each in-cone
    /// down-counter's width K and range R. A counter's R reachable values are TIGHT (measured, part
    /// 1 of `measure_control_automaton_scaling`), so `Π R_counter` is a MEASURED lower bound on the
    /// reachable |S|. `MUNUNU_PROBE_BTOR2=<file>` (whole design).
    #[test]
    #[ignore = "measurement — real-design structural automaton size"]
    fn measure_structural_automaton_size() {
        use crate::adapter::btor2::ast::{Node, Op};
        let Ok(path) = std::env::var("MUNUNU_PROBE_BTOR2") else {
            eprintln!(
                "SKIPPED: this is an opt-in measurement probe, not an assertion — set \
                 MUNUNU_PROBE_BTOR2=<design.btor2> to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read btor2");
        let file = parser::parse(&content).expect("parse btor2");

        let mut reg_bits = 0u64;
        let mut regs = 0u64;
        let mut input_bits = 0u64;
        let mut nodes = 0u64;
        for l in &file.lines {
            match &l.node {
                Node::State { sort, .. } => {
                    regs += 1;
                    reg_bits += parser::bv_width(&file, *sort).unwrap_or(0) as u64;
                }
                Node::Input { sort, .. } => {
                    input_bits += parser::bv_width(&file, *sort).unwrap_or(0) as u64;
                }
                Node::Op { .. } => nodes += 1,
                _ => {}
            }
        }
        // measured lower bounds on reachable |S| from the down-counters. A single counter reaching R
        // distinct values forces |S| ≥ R (SOLID — no independence assumption). The product of ranges
        // is a STRONGER lower bound but only valid if the counters are jointly independent, so we
        // report both, clearly separated. Degenerate range-1 counters (init resolved to 0) contribute
        // nothing and are flagged.
        let counters: Vec<(u32, u64)> = detect_down_counter(&file)
            .iter()
            .map(|c| {
                let range = init_value_of(&file, c.nid)
                    .and_then(|op| resolve_btor2_constant(&file, op.nid()))
                    .map(|v| v.saturating_add(1))
                    .unwrap_or(1u64 << c.width.min(63));
                (c.width, range)
            })
            .collect();
        let max_range = counters.iter().map(|(_, r)| *r).max().unwrap_or(1);
        let log2_prod: f64 = counters
            .iter()
            .map(|(_, r)| (*r as f64).max(1.0).log2())
            .sum();
        let _ = Op::Add; // (Op imported for symmetry with the other probes)
        eprintln!(
            "=== STRUCTURAL automaton size: {} ===",
            path.rsplit('/').next().unwrap_or(&path)
        );
        eprintln!("  state registers          : {regs}");
        eprintln!("  valuation width  W        : {reg_bits} bits  (per-state label)");
        eprintln!(
            "  worst-case |S| = 2^W      : 2^{reg_bits}{}",
            if reg_bits <= 40 {
                format!(" = {}", 1u128 << reg_bits.min(40))
            } else {
                String::new()
            }
        );
        eprintln!(
            "  free-input width I        : {input_bits} bits  (worst-case fan-out 2^I per state)"
        );
        eprintln!(
            "  transition-DAG nodes      : {nodes}  (the SYMBOLIC δ actually built; explicit automaton never is)"
        );
        eprintln!("  in-design down-counters   : {}", counters.len());
        for (w, r) in &counters {
            let note = if *r <= 1 {
                "  (degenerate: init=0 ⇒ contributes ×1)"
            } else {
                ""
            };
            eprintln!("     width {w:>2}  range {r}  (reaches all {r} values ⇒ ×{r} to |S|){note}");
        }
        if max_range <= 1 {
            eprintln!(
                "  no non-degenerate down-counter ⇒ no counter floor; reachable |S| ≤ 2^W (exact-\
                 feasible iff W is small — e.g. a crypto round-FSM)."
            );
        } else {
            eprintln!(
                "  SOLID measured LB   |S| ≥ max counter range = {max_range}  (= 2^{:.0}) — one counter alone",
                (max_range as f64).log2()
            );
            eprintln!(
                "  independence-assumed LB (Π ranges)          = 2^{:.1}  (≈ {}) — if the counters are jointly free",
                log2_prod,
                if log2_prod <= 63.0 {
                    format!("{}", log2_prod.exp2() as u128)
                } else {
                    ">2^63".to_string()
                }
            );
            eprintln!(
                "  ⇒ reachable |S| ∈ [ max counter range (solid measured LB) , 2^W (worst-case UB) ]. \
                 The wide counter sets a floor the FSM cannot reduce — why exact enumeration is infeasible."
            );
        }
    }

    #[test]
    fn detect_enum_states_finds_fsm_rejects_counter() {
        use crate::adapter::btor2::parser::parse;
        // A 4-bit one-hot FSM (init 0001, next = go ? 0010 : hold) + a 4-bit counter
        // (next = cnt + 1). Detection must find the FSM (a constant-mux value path) and
        // REJECT the counter (an `add` on the value path is not a fixed constant set).
        let btor = r#"
1 sort bitvec 1
2 sort bitvec 4
3 state 2 fsm
4 const 2 0001
5 const 2 0010
6 init 2 3 4
7 input 1 go
8 ite 2 7 5 3
9 next 2 3 8
10 state 2 cnt
11 one 2
12 add 2 10 11
13 init 2 10 4
14 next 2 10 12
"#;
        let file = parse(btor).expect("parses");
        let oh = detect_enum_states(&file);
        assert_eq!(
            oh.len(),
            1,
            "only the FSM has a fixed constant set, not the counter: {oh:?}"
        );
        assert_eq!(oh[0].nid, 3);
        assert_eq!(oh[0].width, 4);
        assert_eq!(oh[0].values, vec![1, 2], "the reachable values 0001, 0010");
    }

    /// mununu#602 — the differential switch as an OPTION, not an env var: tests run in parallel
    /// threads and a process-global `MUNUNU_COI` would leak into every other test's cone.
    const SIGNAL_LEVEL: crate::adapter::btor2::symbolic_bitblast::ExactSymbolicOptions =
        crate::adapter::btor2::symbolic_bitblast::ExactSymbolicOptions {
            antecedent_shadow_enabled: true,
            signal_level_coi: true,
        };

    /// mununu#602 — `narrow_leaves` keeps a partially-read register's NID and symbol on a
    /// narrow leaf, reconstructs the full width with zeros, re-targets `next` / `init` to the
    /// kept bits, and leaves whole-kept / un-kept leaves alone. The verdict on a slice property
    /// is identical with and without the rewrite (the signal-level cone is the oracle).
    #[test]
    fn x602_narrow_leaves_rewrites_a_partial_register_and_preserves_the_verdict() {
        use crate::adapter::btor2::dep_graph::{bit_cone, bit_cone_bits};
        use crate::adapter::btor2::symbolic_bitblast::{
            ExactVerdict, exact_symbolic_verdict, exact_symbolic_verdict_with_options,
        };
        // A 16-bit counter whose upper byte is read: `hi = cnt[15:8]`; `cnt` counts by 256 from
        // 0, so `hi` runs 0,1,2,…; the lower byte never leaves 0 and is out of the cone — but
        // the carry chain into bit 8 makes bits [7:0] of the ADDEND matter: `cnt + 256` reads
        // bits ≤ i for bit i, so the cone keeps cnt[15:0] through `add`. Use a shift-free
        // design instead: two registers, one read through a slice.
        let btor = "1 sort bitvec 16\n2 sort bitvec 8\n3 sort bitvec 1\n\
                    4 input 1 din\n5 state 1 words\n6 zero 1\n7 init 1 5 6\n8 next 1 5 4\n\
                    9 state 1 words_sync\n10 init 1 9 6\n11 next 1 9 5\n\
                    12 slice 2 9 15 8 hi\n13 constd 2 7\n14 ult 3 12 13\n15 uext 3 14 0 hi_small\n";
        let file = parser::parse(btor).unwrap();
        let cone = bit_cone(&file, &["hi".to_string()]).unwrap();
        assert_eq!(bit_cone_bits(&cone), 24, "din/words/words_sync × [15:8]");
        let (narrowed, keep) = narrow_leaves(&file, &cone);
        assert_eq!(keep, [4, 5, 9].into_iter().collect());
        // The leaves are 8 bits wide now, under their own NIDs and names.
        let width_of = |f: &Btor2File, nid: Nid| -> u32 {
            match &f.lookup(nid).unwrap().node {
                Node::State { sort, .. } | Node::Input { sort, .. } => {
                    parser::bv_width(f, *sort).unwrap()
                }
                other => panic!("{other:?}"),
            }
        };
        for nid in [4, 5, 9] {
            assert_eq!(width_of(&narrowed, nid), 8, "nid {nid}");
        }
        assert_eq!(
            parser::collect_symbols(&narrowed)
                .get(&9)
                .map(String::as_str),
            Some("words_sync")
        );
        // The slice now reads a reconstruction (a concat of the narrow leaf and zeros), not
        // the leaf itself; the reconstruction is 16 bits wide.
        let slice = narrowed.lookup(12).unwrap();
        let Node::Op { args, .. } = &slice.node else {
            panic!()
        };
        assert_ne!(args[0].nid(), 9);
        let Node::Op {
            op: Op::Concat,
            sort,
            ..
        } = &narrowed.lookup(args[0].nid()).unwrap().node
        else {
            panic!("the reader was redirected to the reconstruction")
        };
        assert_eq!(parser::bv_width(&narrowed, *sort), Some(16));
        // The narrowed file is a valid BTOR2 the exact engine decides, and the verdict matches
        // the un-narrowed (signal-level) run — on a property over the slice.
        let f = crate::mu_calculus::parser::parse("nu X. ((hi_small != 0) && [] X)").unwrap();
        let bit = exact_symbolic_verdict(btor, &f).expect("bit-level decides");
        let signal = exact_symbolic_verdict_with_options(btor, &f, &SIGNAL_LEVEL);
        assert_eq!(bit, signal.expect("signal-level decides"));
        assert_eq!(
            bit,
            ExactVerdict::Violated,
            "`din` is free, so `hi` reaches 7 and `hi_small` drops to 0"
        );
        // And a HOLDS: `hi_small == 1` at the (zero) initial state, reachable.
        let f = crate::mu_calculus::parser::parse("mu X. ((hi_small == 1) || <> X)").unwrap();
        assert_eq!(
            exact_symbolic_verdict(btor, &f).unwrap(),
            ExactVerdict::Holds
        );
    }

    /// mununu#602 — the consumer's shape: a 23-word (736-bit) packed vector crossed twice, a
    /// 32-bit field read through a constant part-select. At signal granularity the cone is
    /// 3 × 736 = 2,208 bits and the exact engine abstains on the bit cap; at bit granularity
    /// it is 96 bits and decides. The wall-class representative for the issue.
    #[test]
    fn x602_a_field_of_a_crossed_vector_decides_at_bit_granularity() {
        use crate::adapter::btor2::symbolic_bitblast::{
            ExactVerdict, exact_symbolic_verdict, exact_symbolic_verdict_with_options,
        };
        let btor = "1 sort bitvec 736\n2 input 1 rd_words\n3 state 1 words\n4 next 1 3 2\n\
                    5 state 1 words_sync\n6 next 1 5 3\n7 sort bitvec 32\n8 sort bitvec 1\n\
                    9 slice 7 5 159 128 word4\n10 slice 8 9 0 0 flag\n11 uext 8 10 0 flag_o\n";
        // `AG(flag_o == 0)` is violated (din is free), `EF(flag_o == 1)` holds: both decided.
        let f = crate::mu_calculus::parser::parse("nu X. ((flag_o == 0) && [] X)").unwrap();
        assert_eq!(
            exact_symbolic_verdict(btor, &f).expect("96-bit cone fits"),
            ExactVerdict::Violated
        );
        let signal = exact_symbolic_verdict_with_options(btor, &f, &SIGNAL_LEVEL);
        let err = signal.expect_err("2,208 register+input bits abstain on the cap");
        assert!(err.contains("BIT CAP"), "{err}");
    }

    #[test]
    fn enum_reencode_is_verdict_preserving_differential() {
        use crate::adapter::btor2::parser::parse;
        use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
        // A 4-bit one-hot FSM (idle 0000 → A 0001 → B 0010) with a combinational output
        // `done = (fsm == B)` and `busy = |fsm` (redor). The property targets the OUTPUT
        // (preserved through the read-rewrite), not the register's raw value.
        let btor = r#"
1 sort bitvec 1
2 sort bitvec 4
3 state 2 fsm
4 const 2 0001
5 const 2 0010
6 const 2 0000
7 init 2 3 6
8 input 1 go
9 ite 2 8 5 4
10 next 2 3 9
11 eq 1 3 5
12 output 11 done
13 redor 1 3
14 output 13 busy
"#;
        let orig = parse(btor).expect("parses");
        let metas = detect_enum_states(&orig);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].values, vec![0, 1, 2], "idle + A + B");
        let reenc = enum_reencode(&orig, &metas[0]).expect("re-encodable");
        // fsm is now 2-bit (3 states) vs 4-bit — the compression.
        let w = reenc.lines.iter().find_map(|l| match &l.node {
            Node::State { sort, .. } if l.nid == 3 => parser::bv_width(&reenc, *sort),
            _ => None,
        });
        assert_eq!(w, Some(2), "fsm re-encoded 4 bits → 2 (ceil log2 3)");
        let reenc_str = crate::adapter::btor2::emit::emit_btor2(&reenc);
        // DIFFERENTIAL ORACLE: identical verdicts on original vs re-encoded, over outputs.
        for prop in [
            "mu X.((done == 1) || <> X)", // EF done  (B reachable via go)
            "mu X.((busy == 1) || <> X)", // EF busy  (fsm leaves idle)
            "nu Y.((done == 0) && [] Y)", // AG !done (VIOLATED — go reaches B)
        ] {
            let f = crate::mu_calculus::parser::parse(prop).expect("formula");
            let vo = exact_symbolic_verdict(btor, &f).expect("orig verdict");
            let vr = exact_symbolic_verdict(&reenc_str, &f).expect("reenc verdict");
            assert_eq!(
                vo, vr,
                "re-encode changed the verdict for `{prop}`: {vo:?} vs {vr:?}"
            );
        }
    }

    #[test]
    fn enum_reencode_handles_non_onehot_binary_fsm_differential() {
        use crate::adapter::btor2::parser::parse;
        use crate::adapter::btor2::symbolic_bitblast::exact_symbolic_verdict;
        // A 4-bit BINARY-encoded FSM: idle 0000 → (go ? 0101 : 0011). The written values
        // 0011 (=3) and 0101 (=5) have popcount 2 — NOT one-hot, so the old one-hot-only
        // detector rejected this register; the general enum detector accepts it (a fixed
        // constant set {0,3,5}) and re-encodes 4 bits → 2. Output `at5 = (st == 5)`,
        // `active = |st` (redor). Gap A's core: a non-one-hot value set compresses soundly.
        let btor = r#"
1 sort bitvec 1
2 sort bitvec 4
3 state 2 st
4 const 2 0011
5 const 2 0101
6 const 2 0000
7 init 2 3 6
8 input 1 go
9 ite 2 8 5 4
10 next 2 3 9
11 eq 1 3 5
12 output 11 at5
13 redor 1 3
14 output 13 active
"#;
        let orig = parse(btor).expect("parses");
        let metas = detect_enum_states(&orig);
        assert_eq!(
            metas.len(),
            1,
            "the binary FSM is a fixed constant set: {metas:?}"
        );
        assert_eq!(
            metas[0].values,
            vec![0, 3, 5],
            "reachable NON-one-hot values 0000, 0011, 0101"
        );
        let reenc = enum_reencode(&orig, &metas[0]).expect("re-encodable");
        let w = reenc.lines.iter().find_map(|l| match &l.node {
            Node::State { sort, .. } if l.nid == 3 => parser::bv_width(&reenc, *sort),
            _ => None,
        });
        assert_eq!(w, Some(2), "st re-encoded 4 bits → 2 (ceil log2 3)");
        let reenc_str = crate::adapter::btor2::emit::emit_btor2(&reenc);
        // DIFFERENTIAL ORACLE: identical verdicts on original (4-bit) vs re-encoded (2-bit),
        // read through the rewritten `eq(st,5)→eq(st',2)` and `redor(st)→neq(st',0)`.
        for prop in [
            "mu X.((at5 == 1) || <> X)",    // EF at5    (5 reachable via go)
            "mu X.((active == 1) || <> X)", // EF active (st leaves idle)
            "nu Y.((at5 == 0) && [] Y)",    // AG !at5   (VIOLATED — go reaches 5)
        ] {
            let f = crate::mu_calculus::parser::parse(prop).expect("formula");
            let vo = exact_symbolic_verdict(btor, &f).expect("orig verdict");
            let vr = exact_symbolic_verdict(&reenc_str, &f).expect("reenc verdict");
            assert_eq!(
                vo, vr,
                "enum re-encode changed the verdict for `{prop}`: {vo:?} vs {vr:?}"
            );
        }
    }

    /// b1 — a 3-bit down-counter `cnt` (reload 4 at expiry, decrement otherwise)
    /// gating a 1-bit `phase` toggle. Detection must find the COUNTER (a `sub`
    /// stride on its value path + only `eq(cnt, 0)` external uses) and REJECT
    /// `phase` (a `not` on its value path — not an arithmetic counter).
    const GATED_PHASE_COUNTER: &str = r#"
1 sort bitvec 3
2 sort bitvec 1
3 state 1 cnt
4 state 2 phase
5 const 1 100
6 const 1 001
7 zero 1
8 zero 2
9 eq 2 3 7
10 sub 1 3 6
11 ite 1 9 5 10
12 next 1 3 11
13 init 1 3 5
14 not 2 4
15 ite 2 9 14 4
16 next 2 4 15
17 init 2 4 8
"#;

    #[test]
    fn detect_down_counter_finds_gated_counter_rejects_toggle() {
        use crate::adapter::btor2::parser::parse;
        let file = parse(GATED_PHASE_COUNTER).expect("parses");
        let cs = detect_down_counter(&file);
        assert_eq!(cs.len(), 1, "only cnt is a counter, not phase: {cs:?}");
        assert_eq!(cs[0].nid, 3);
        assert_eq!(cs[0].width, 3);
        assert_eq!(cs[0].threshold, 0, "compared only against cnt == 0");
    }

    /// b1 SOUNDNESS DIFFERENTIAL — the may-abstraction must **simulate** the
    /// concrete model (over-approximation): for every concrete step
    /// `c → c'`, there is a choice of the fresh may-input under which the
    /// abstract model steps `α(c) → α(c')`, where `α` maps `cnt ↦ (cnt == T)`
    /// and leaves other registers untouched. Simulation ⇒ every universal
    /// (ν / AG-safety) property that holds on the abstract model holds on the
    /// concrete one — the direction b1 claims. A rewrite bug (wrong reload
    /// constant, dropped edge, inverted threshold) breaks the simulation here.
    #[test]
    fn counter_may_abstract_simulates_concrete_sound_overapprox() {
        use crate::adapter::btor2::parser::parse;
        use std::collections::HashMap;
        let concrete = parse(GATED_PHASE_COUNTER).expect("parses");
        let meta = &detect_down_counter(&concrete)[0];
        let abstract_file = counter_may_abstract(&concrete, meta).expect("abstractable");

        // cnt collapsed 3 → 1 bit; phase unchanged.
        let cnt_w = abstract_file.lines.iter().find_map(|l| match &l.node {
            Node::State { sort, .. } if l.nid == 3 => parser::bv_width(&abstract_file, *sort),
            _ => None,
        });
        assert_eq!(cnt_w, Some(1), "cnt re-encoded to the {{==0}} bit");

        // Re-parse the serialized abstract model + collect the may-input symbol(s).
        let abstract_src = crate::adapter::btor2::emit::emit_btor2(&abstract_file);
        let abstract_re = parse(&abstract_src).expect("abstract re-parses");
        let may_syms: Vec<String> = abstract_re
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Input {
                    symbol: Some(s), ..
                } if s.starts_with("__cnt") => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(!may_syms.is_empty(), "the stride became a may-input");

        let alpha = |cnt: u128| -> u128 { if cnt == meta.threshold as u128 { 1 } else { 0 } };
        // Exhaustive: every concrete (cnt, phase). The counter is autonomous
        // (no primary inputs), so the abstract model's only inputs are may-inputs.
        for cnt in 0..(1u128 << meta.width) {
            for phase in 0..2u128 {
                let cregs = HashMap::from([("cnt".to_string(), cnt), ("phase".to_string(), phase)]);
                let cnext =
                    simulate_one_step(&concrete, &cregs, &HashMap::new()).expect("concrete step");
                let want_cnt = alpha(cnext["cnt"]);
                let want_phase = cnext["phase"];

                // ∃ may-input choice reproducing α(c') in the abstract model.
                let mut matched = false;
                for may in 0..(1u128 << may_syms.len()) {
                    let aregs = HashMap::from([
                        ("cnt".to_string(), alpha(cnt)),
                        ("phase".to_string(), phase),
                    ]);
                    let ainps: HashMap<String, u128> = may_syms
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (s.clone(), (may >> i) & 1))
                        .collect();
                    let anext =
                        simulate_one_step(&abstract_re, &aregs, &ainps).expect("abstract step");
                    if anext["cnt"] == want_cnt && anext["phase"] == want_phase {
                        matched = true;
                        break;
                    }
                }
                assert!(
                    matched,
                    "no abstract step simulates concrete cnt={cnt} phase={phase} \
                     → (cnt'={}, phase'={want_phase}); abstraction is NOT a sound \
                     over-approximation",
                    cnext["cnt"]
                );
            }
        }
    }

    /// b1 GENERALIZED — the DOMINANT real-RTL counter idiom: an INPUT reload value
    /// (`rld`, a prescale register/input) and a `redor(cnt)` (`~|cnt`) expiry gate.
    /// Detection must find it (the const-reload/`eq`-gate form is essentially
    /// synthetic-only), and the may-abstraction must still SIMULATE the concrete model.
    #[test]
    fn counter_may_abstract_generalized_input_reload_redor_gate() {
        use crate::adapter::btor2::parser::parse;
        use std::collections::HashMap;
        // cnt: `~|cnt ? reload(rld) : cnt-1` (redor gate + INPUT reload); phase toggles
        // at expiry (cnt==0). rld is a 3-bit input; the redor node (9) gates both.
        let src = r#"
1 sort bitvec 3
2 sort bitvec 1
3 state 1 cnt
4 state 2 phase
5 input 1 rld
6 const 1 001
7 const 1 100
8 zero 2
9 redor 2 3
10 sub 1 3 6
11 ite 1 9 10 5
12 next 1 3 11
13 init 1 3 7
14 not 2 9
15 not 2 4
16 ite 2 14 15 4
17 next 2 4 16
18 init 2 4 8
"#;
        let concrete = parse(src).expect("parses");
        let cs = detect_down_counter(&concrete);
        assert_eq!(
            cs.len(),
            1,
            "input-reload + redor-gated counter must be detected: {cs:?}"
        );
        assert_eq!(cs[0].nid, 3);
        assert_eq!(cs[0].threshold, 0, "redor(cnt) fixes the threshold at 0");
        let ab = counter_may_abstract(&concrete, &cs[0]).expect("abstractable");
        let cnt_w = ab.lines.iter().find_map(|l| match &l.node {
            Node::State { sort, .. } if l.nid == 3 => parser::bv_width(&ab, *sort),
            _ => None,
        });
        assert_eq!(cnt_w, Some(1));
        let ab_src = crate::adapter::btor2::emit::emit_btor2(&ab);
        let ab_re = parse(&ab_src).expect("abstract re-parses");
        let may: Vec<String> = ab_re
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Input {
                    symbol: Some(s), ..
                } if s.starts_with("__cnt") => Some(s.clone()),
                _ => None,
            })
            .collect();
        let alpha = |c: u128| if c == 0 { 1 } else { 0 };
        // Exhaustive over concrete (cnt, phase, rld input); ∃ may-input reproduces α(c').
        for cnt in 0..8u128 {
            for phase in 0..2u128 {
                for rld in 0..8u128 {
                    let cregs =
                        HashMap::from([("cnt".to_string(), cnt), ("phase".to_string(), phase)]);
                    let cinps = HashMap::from([("rld".to_string(), rld)]);
                    let cnext =
                        simulate_one_step(&concrete, &cregs, &cinps).expect("concrete step");
                    let (want_cnt, want_phase) = (alpha(cnext["cnt"]), cnext["phase"]);
                    let mut matched = false;
                    for m in 0..(1u128 << may.len()) {
                        let aregs = HashMap::from([
                            ("cnt".to_string(), alpha(cnt)),
                            ("phase".to_string(), phase),
                        ]);
                        let mut ainps: HashMap<String, u128> = may
                            .iter()
                            .enumerate()
                            .map(|(i, s)| (s.clone(), (m >> i) & 1))
                            .collect();
                        ainps.insert("rld".to_string(), rld); // kept but dead in the abstract
                        let anext =
                            simulate_one_step(&ab_re, &aregs, &ainps).expect("abstract step");
                        if anext["cnt"] == want_cnt && anext["phase"] == want_phase {
                            matched = true;
                            break;
                        }
                    }
                    assert!(
                        matched,
                        "no abstract step simulates cnt={cnt} phase={phase} rld={rld}"
                    );
                }
            }
        }
    }

    /// b1 RESET-MUX SEE-THROUGH — the yosys async-reset lowering, where the counter's
    /// value reaches its stride AND its expiry gate through a mux `ite(arst, cnt, 0)`
    /// (i2c's clock divider, nid 143: `sub(ite(arst,143,0),1)` + `redor(ite(arst,143,0))`).
    /// Detection must see the counter through the alias, and the may-abstraction must
    /// still SIMULATE the concrete model (α maps `cnt ↦ cnt==0`).
    #[test]
    fn counter_may_abstract_reset_mux_alias_detects_and_simulates() {
        use crate::adapter::btor2::parser::parse;
        use std::collections::HashMap;
        // mux = arst ? cnt : 0 (the reset-mux alias, node 11); the decrement (14) and the
        // expiry gate (12 = redor(mux)) both read the ALIAS, not raw cnt. next(cnt) =
        // arst ? (expired ? rld : mux-1) : 0. phase toggles at expiry.
        let src = r#"
1 sort bitvec 3
2 sort bitvec 1
3 state 1 cnt
4 state 2 phase
5 input 2 arst
6 input 1 rld
7 const 1 001
8 zero 1
9 zero 2
10 const 1 100
11 ite 1 5 3 8
12 redor 2 11
13 not 2 12
14 sub 1 11 7
15 ite 1 13 6 14
16 ite 1 5 15 8
17 next 1 3 16
18 init 1 3 10
19 not 2 4
20 ite 2 13 19 4
21 next 2 4 20
22 init 2 4 9
"#;
        let concrete = parse(src).expect("parses");
        let cs = detect_down_counter(&concrete);
        assert_eq!(
            cs.len(),
            1,
            "the counter must be detected THROUGH the reset-mux alias: {cs:?}"
        );
        assert_eq!(cs[0].nid, 3);
        assert_eq!(cs[0].threshold, 0, "redor(mux) fixes threshold 0");
        let ab = counter_may_abstract(&concrete, &cs[0]).expect("abstractable");
        let cnt_w = ab.lines.iter().find_map(|l| match &l.node {
            Node::State { sort, .. } if l.nid == 3 => parser::bv_width(&ab, *sort),
            _ => None,
        });
        assert_eq!(cnt_w, Some(1));
        let ab_re = parse(&crate::adapter::btor2::emit::emit_btor2(&ab)).expect("re-parses");
        let may: Vec<String> = ab_re
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Input {
                    symbol: Some(s), ..
                } if s.starts_with("__cnt") => Some(s.clone()),
                _ => None,
            })
            .collect();
        let alpha = |c: u128| if c == 0 { 1 } else { 0 };
        // Exhaustive over concrete (cnt, phase, arst, rld); ∃ may-input reproduces α(c').
        for cnt in 0..8u128 {
            for phase in 0..2u128 {
                for arst in 0..2u128 {
                    for rld in 0..8u128 {
                        let cregs =
                            HashMap::from([("cnt".to_string(), cnt), ("phase".to_string(), phase)]);
                        let cinps =
                            HashMap::from([("arst".to_string(), arst), ("rld".to_string(), rld)]);
                        let cnext =
                            simulate_one_step(&concrete, &cregs, &cinps).expect("concrete step");
                        let (want_cnt, want_phase) = (alpha(cnext["cnt"]), cnext["phase"]);
                        let mut matched = false;
                        for m in 0..(1u128 << may.len()) {
                            let aregs = HashMap::from([
                                ("cnt".to_string(), alpha(cnt)),
                                ("phase".to_string(), phase),
                            ]);
                            let mut ainps: HashMap<String, u128> = may
                                .iter()
                                .enumerate()
                                .map(|(i, s)| (s.clone(), (m >> i) & 1))
                                .collect();
                            ainps.insert("arst".to_string(), arst);
                            ainps.insert("rld".to_string(), rld);
                            let anext =
                                simulate_one_step(&ab_re, &aregs, &ainps).expect("abstract step");
                            if anext["cnt"] == want_cnt && anext["phase"] == want_phase {
                                matched = true;
                                break;
                            }
                        }
                        assert!(
                            matched,
                            "no abstract step simulates cnt={cnt} phase={phase} arst={arst} rld={rld}"
                        );
                    }
                }
            }
        }
    }

    // AR-S2 — the `shl_sat128`/`shr_sat128` saturating point-patches (and their
    // regression test) are retired: `Bv` shifts are exact at any width, so a
    // wide concat/shift (e.g. keymgr_ctrl's 256-bit key state) no longer
    // truncates. Shift correctness is covered by the `bv` module's differential
    // tests (20k iterations/op vs u128) + the wide-roundtrip test.

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 Option-4 step 1a — BvTermBackend equivalence.
    //
    // Proves `walk_design::<ConcreteBackend>` produces the SAME
    // per-NID env as the bespoke `evaluate_pure_with_uf_rep` loop,
    // bit-for-bit, over a spread of operators. This is the
    // load-bearing gate for the step-1b cutover: it certifies the
    // unified seam is a faithful re-expression of the concrete
    // evaluator before the production path switches to it.
    // ─────────────────────────────────────────────────────────────

    /// Seed an env by binding every input + state NID to a
    /// deterministic per-NID value (so both evaluators start from an
    /// identical state). Mirrors what `make_*_env` does in
    /// production, but with synthetic values.
    fn seed_env(file: &Btor2File) -> Env {
        let mut env = Env::default();
        for line in &file.lines {
            let (nid, sort) = match &line.node {
                Node::Input { sort, .. } | Node::State { sort, .. } => (line.nid, *sort),
                _ => continue,
            };
            let width = parser::bv_width(file, sort).unwrap_or(1);
            // A deterministic non-trivial value per NID: low bits of
            // the NID, masked to width.
            let raw = (line.nid as u128).wrapping_mul(0x9E37) ^ 0x5A5A;
            env.values.insert(nid, Bv::from_u128_width(raw, width));
        }
        env
    }

    /// Run BOTH evaluators on the same seeded env + assert the
    /// resulting `Nid → Bv` maps are identical.
    fn assert_backend_matches_bespoke(src: &str, honor_init: bool) {
        use crate::adapter::btor2::term_backend::walk_design;
        let file = super::parser::parse(src).expect("parse fixture");

        // Bespoke path.
        let mut bespoke_env = seed_env(&file);
        evaluate_pure_with_uf_rep(
            &file,
            &mut bespoke_env,
            honor_init,
            None,
            UfRepresentative::Zero,
        )
        .expect("bespoke eval");

        // Unified-seam path.
        let mut backend =
            ConcreteBackend::new(seed_env(&file), honor_init, None, UfRepresentative::Zero);
        walk_design(&file, &mut backend).expect("walk_design eval");
        let backend_env = backend.into_env();

        assert_eq!(
            bespoke_env.values, backend_env.values,
            "walk_design::<ConcreteBackend> must match evaluate_pure_with_uf_rep bit-for-bit"
        );
    }

    #[test]
    fn option4_backend_matches_bespoke_arith_and_logic() {
        // A spread of operators: const, and/or/xor, add/sub/mul,
        // eq/ult/slt, ite, concat, slice, uext/sext.
        let src = r#"
1 sort bitvec 8
2 sort bitvec 1
3 sort bitvec 16
4 input 1 a
5 input 1 b
6 constd 1 100
7 add 1 4 5
8 sub 1 4 5
9 mul 1 7 8
10 and 1 4 5
11 or 1 4 5
12 xor 1 10 11
13 eq 2 4 5
14 ult 2 4 5
15 slt 2 4 5
16 ite 1 13 4 5
17 concat 3 4 5
18 slice 1 17 7 0
19 uext 3 4 8
20 sext 3 4 8
"#;
        assert_backend_matches_bespoke(src, false);
    }

    #[test]
    fn option4_backend_matches_bespoke_shifts_and_reductions() {
        let src = r#"
1 sort bitvec 8
2 sort bitvec 1
3 input 1 x
4 input 1 y
5 sll 1 3 4
6 srl 1 3 4
7 sra 1 3 4
8 rol 1 3 4
9 ror 1 3 4
10 redand 2 3
11 redor 2 3
12 redxor 2 3
13 not 1 3
14 neg 1 3
15 inc 1 3
16 dec 1 3
"#;
        assert_backend_matches_bespoke(src, false);
    }

    #[test]
    fn option4_backend_matches_bespoke_with_init() {
        // honor_init = true exercises the Node::Init copy path in
        // both evaluators.
        let src = r#"
1 sort bitvec 4
2 constd 1 7
3 state 1 q
4 init 1 3 2
5 add 1 3 2
"#;
        assert_backend_matches_bespoke(src, true);
    }

    #[test]
    fn toggle_latch_safety_unrealizable_when_bad_reachable() {
        // 1-bit latch q: q' = !q; init q=0; bad when q=1.
        // Bad is reachable, so safety property must be present.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 not 1 3
6 next 1 3 5
7 bad 3
"#;
        let opts = AdapterOptions::default();
        let out = translate(src, &opts).expect("translate");
        assert_eq!(out.source_info.state_count, 2);
        assert_eq!(out.source_info.property_count, 1);
        assert!(out.ctxdsl.contains("safety_bad_0"));
    }

    #[test]
    fn rejects_unsupported_op() {
        let src = r#"
1 sort bitvec 4
2 input 1 a
3 input 1 b
4 udiv 1 2 3
"#;
        let opts = AdapterOptions::default();
        let err = translate(src, &opts).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
    }

    #[test]
    fn rejects_state_space_overflow() {
        // `MAX_STATE_BITS + 1` 1-bit states → 2^(N+1) > MAX_STATE_BITS.
        let mut src = "1 sort bitvec 1\n".to_string();
        let overflow_count = MAX_STATE_BITS as usize + 1;
        for i in 0..overflow_count {
            src.push_str(&format!("{} state 1 s{}\n", i + 2, i));
        }
        let err = translate(&src, &AdapterOptions::default()).unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::StateSpaceOverflow);
    }

    /// R.4.6 — the "joint busts cap, clusters fit" capability at the
    /// bit-blast layer. Two independent 11-bit registers (22 state bits
    /// jointly, > MAX_STATE_BITS = 20). `a_hot = (reg_a == 1)` so reg_a
    /// is in the cone and reg_b is not. Without a restriction the design
    /// busts the cap; restricting to `a_hot`'s cone drops reg_b's 11 bits
    /// and the cluster (11 bits) fits + translates.
    #[test]
    fn cone_restriction_unlocks_cap_busting_design_per_cluster() {
        let src = r#"
1 sort bitvec 1
2 sort bitvec 11
3 zero 2
4 state 2 reg_a
5 init 2 4 3
6 state 2 reg_b
7 init 2 6 3
8 one 2
9 eq 1 4 8
10 output 9 a_hot
"#;
        // 1) Joint — both 11-bit registers → 22 state bits → cap-bust.
        let joint = translate(src, &AdapterOptions::default());
        assert!(
            joint.is_err(),
            "joint design (22 state bits) must bust the cap"
        );
        assert_eq!(
            joint.unwrap_err().kind,
            AdapterErrorKind::StateSpaceOverflow
        );

        // 2) Per-cluster — restrict to a_hot's cone {reg_a}; reg_b's
        // 11 bits are cut, leaving 11 ≤ MAX_STATE_BITS → translates.
        let opts = AdapterOptions {
            cone_restrict_atoms: Some(vec!["a_hot".to_string()]),
            ..Default::default()
        };
        let out = translate(src, &opts)
            .expect("cone-restricted cluster (11 state bits) must fit the cap");
        assert!(
            out.ctxdsl.contains("automaton"),
            "restricted cluster must produce an automaton; ctxdsl:\n{}",
            out.ctxdsl
        );
    }

    /// R.4.6 — a restriction whose cone covers *every* state cell is a
    /// no-op: nothing is dropped, so a design that already busts the cap
    /// still busts it (the restriction cannot manufacture headroom it has
    /// no out-of-cone cells to reclaim).
    #[test]
    fn cone_restriction_covering_all_cells_does_not_unlock_cap() {
        // Two registers, but the atom's cone reaches BOTH (out = reg_a ==
        // reg_b), so neither is droppable.
        let src = r#"
1 sort bitvec 1
2 sort bitvec 11
3 zero 2
4 state 2 reg_a
5 init 2 4 3
6 state 2 reg_b
7 init 2 6 3
8 eq 1 4 6
9 output 8 both_hot
"#;
        let opts = AdapterOptions {
            cone_restrict_atoms: Some(vec!["both_hot".to_string()]),
            ..Default::default()
        };
        let err = translate(src, &opts).expect_err("cone covers both regs → still 22 bits");
        assert_eq!(err.kind, AdapterErrorKind::StateSpaceOverflow);
    }

    #[test]
    fn effective_bits_is_ceil_log2() {
        assert_eq!(effective_bits(1), 0);
        assert_eq!(effective_bits(2), 1);
        assert_eq!(effective_bits(3), 2);
        assert_eq!(effective_bits(4), 2);
        assert_eq!(effective_bits(5), 3);
        assert_eq!(effective_bits(2048), 11);
    }

    /// R46-6 / GAP-2 — a wide state cell the sidecar param-concretizes
    /// counts `ceil(log2(value-set))` effective bits, not its raw width,
    /// so a design that busts the cap on raw width fits once concretized.
    #[test]
    fn effective_cap_counts_concretized_wide_cell_by_value_set() {
        // 24-bit state cell `cnt` — over the cap on raw width.
        let src = r#"
1 sort bitvec 1
2 sort bitvec 24
3 zero 2
4 state 2 cnt
5 init 2 4 3
6 ones 2
7 eq 1 4 6
8 output 7 cnt_max
"#;
        let file = parser::parse(src).expect("parse");

        // Without a sidecar: 24 raw state bits → StateSpaceOverflow.
        let bare = translate(src, &AdapterOptions::default());
        assert_eq!(
            bare.unwrap_err().kind,
            AdapterErrorKind::StateSpaceOverflow,
            "24-bit cell must bust the cap on raw width"
        );

        // With BoundedCounter bound=3 → {0,1,2,3} = 4 values → 2 bits.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [ { "name": "cnt", "abstraction": "bounded_counter", "bound": 3 } ],
        })
        .to_string();
        let states: Vec<&Line> = file.states().collect();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let eff = sidecar_effective_state_bits(&file, &states, &opts).expect("eff bits");
        assert_eq!(
            eff, 2,
            "24-bit cell concretized to 4 values must count 2 effective bits"
        );
        let out = translate(src, &opts).expect("concretized design must fit the cap");
        assert!(out.ctxdsl.contains("automaton"));
    }

    /// R46-2 — the per-cluster fallback. Two independent 11-bit registers
    /// (22 state bits jointly → over the cap), with one property over each
    /// (disjoint cones). With the manifest's per-property seeds threaded,
    /// `to_ir` partitions into two clusters, bit-blasts each restricted to
    /// its own 11-bit cone, and returns two automata plus a routing map.
    const TWO_DISJOINT_CONES_BTOR2: &str = r#"
1 sort bitvec 1
2 sort bitvec 11
3 zero 2
4 state 2 reg_a
5 init 2 4 3
6 state 2 reg_b
7 init 2 6 3
8 one 2
9 eq 1 4 8
10 output 9 a_hot
11 eq 1 6 8
12 output 11 b_hot
"#;

    #[test]
    fn per_cluster_fallback_splits_cap_busting_design() {
        let file = parser::parse(TWO_DISJOINT_CONES_BTOR2).expect("parse");
        let opts = AdapterOptions {
            property_seeds: vec![
                ("pa".to_string(), vec!["a_hot".to_string()]),
                ("pb".to_string(), vec!["b_hot".to_string()]),
            ],
            ..Default::default()
        };
        let (ir, _warnings, summary) =
            to_ir(&file, &opts).expect("per-cluster fallback must rescue the cap-busting design");

        // Two clusters → two automata, each restricted to its 11-bit cone.
        assert_eq!(ir.automata.len(), 2, "expected one automaton per cluster");
        let names: std::collections::HashSet<&str> =
            ir.automata.iter().map(|a| a.name.as_str()).collect();
        assert!(
            names.contains("Circuit__cl0") && names.contains("Circuit__cl1"),
            "automata must be named per cluster; got {names:?}"
        );

        // Routing map sends each property to a distinct cluster automaton.
        let routing = summary
            .expect("per-cluster summary")
            .cluster_routing
            .expect("cluster_routing must be Some on the per-cluster path");
        assert_eq!(routing.len(), 2);
        assert!(routing.contains_key("pa") && routing.contains_key("pb"));
        assert_ne!(
            routing["pa"], routing["pb"],
            "disjoint-cone properties must route to different automata"
        );
        assert!(names.contains(routing["pa"].as_str()));
    }

    /// R46-2 — `cone_slice` keeps the backward cone (what feeds the
    /// atom's state) AND the forward naming/observation aliases computed
    /// from it (the `uext … cnt_x` lines Yosys uses to name registers),
    /// while dropping the other cone entirely. The forward-alias keep is
    /// load-bearing: without it the sliced state cell stays anonymous and
    /// emits no predicate (the bug this test guards against).
    #[test]
    fn cone_slice_keeps_forward_naming_aliases_and_drops_other_cone() {
        // Mirrors the Yosys output shape: anonymous state cells named via
        // `uext` alias lines; one comparison-output per cone.
        let src = r#"
1 sort bitvec 1
2 sort bitvec 4
3 zero 2
4 state 2
5 init 2 4 3
6 state 2
7 init 2 6 3
8 ones 2
9 eq 1 4 8
10 output 9 a_max_o
11 uext 2 4 0 cnt_a
12 eq 1 6 8
13 output 12 b_max_o
14 uext 2 6 0 cnt_b
"#;
        let file = parser::parse(src).expect("parse");
        let sliced = cone_slice(&file, &["a_max_o".to_string()]);

        let symbols: std::collections::HashSet<&str> = sliced
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Op {
                    symbol: Some(s), ..
                }
                | Node::Output {
                    symbol: Some(s), ..
                } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            symbols.contains("a_max_o"),
            "cluster-A cone must keep its output a_max_o; got {symbols:?}"
        );
        assert!(
            symbols.contains("cnt_a"),
            "cluster-A cone must keep the forward `uext cnt_a` naming alias \
             (else the state cell is anonymous and emits no predicate); got {symbols:?}"
        );
        assert!(
            !symbols.contains("b_max_o") && !symbols.contains("cnt_b"),
            "cluster-B cone must be dropped entirely; got {symbols:?}"
        );
        // Exactly one state cell survives (cnt_a's register, nid 4).
        assert_eq!(
            sliced.states().count(),
            1,
            "the out-of-cone state cell must be removed from the slice"
        );
    }

    /// R46-6a (constraint/fairness pullback, soundness fix) — a
    /// `constraint` coupling an in-cone register to an out-of-cone register
    /// must pull the out-of-cone register AND the constraint into the slice.
    ///
    /// Before the fix, `cone_slice` seeded only from property atoms and
    /// closed only over `next`/`init` data-flow, then kept a `constraint`
    /// line only if *all* its operands were already in-cone — so a
    /// constraint mentioning an out-of-cone signal was silently dropped.
    /// The joint bit-blaster enforces every constraint via
    /// `constraints_hold` (see `constraint_filters_transitions`), so the
    /// slice became strictly more permissive: an over-approximation that
    /// can report a spurious counterexample, not the documented exact /
    /// bisimilar reduction.
    #[test]
    fn cone_slice_pulls_in_constraint_coupled_register() {
        // `reg_a` is the property atom; `reg_b` is reached by no `next`
        // that `reg_a` depends on, so it is out-of-cone by data-flow alone.
        // The `constraint (reg_a == reg_b)` couples them — `reg_b` restricts
        // `reg_a`'s reachable values, so it is in the true cone of influence.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 reg_a
4 init 1 3 2
5 input 1 tgl
6 next 1 3 5
7 state 1 reg_b
8 init 1 7 2
9 next 1 7 2
10 eq 1 3 7
11 constraint 10
12 bad 3
"#;
        let file = parser::parse(src).expect("parse");
        let sliced = cone_slice(&file, &["reg_a".to_string()]);

        let state_syms: std::collections::HashSet<&str> = sliced
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::State {
                    symbol: Some(s), ..
                } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            state_syms.contains("reg_a"),
            "in-cone register `reg_a` must be retained; got {state_syms:?}"
        );
        assert!(
            state_syms.contains("reg_b"),
            "constraint-coupled out-of-cone register `reg_b` must be retained \
             (else the assumption `reg_a == reg_b` is silently dropped and the \
             slice over-approximates); got {state_syms:?}"
        );
        assert!(
            sliced
                .lines
                .iter()
                .any(|l| matches!(l.node, Node::Constraint { .. })),
            "the coupling `constraint` line must survive the slice"
        );
    }

    /// R46-6a — the same pullback for `fair` (fairness) lines: a fairness
    /// obligation referencing an in-cone signal shapes which infinite paths
    /// count, so the signals it references are in the cone and must be
    /// retained. Dropping fairness is unsound for liveness verdicts (the
    /// abstraction can manufacture or destroy spurious progress).
    #[test]
    fn cone_slice_pulls_in_fairness_coupled_register() {
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 reg_a
4 init 1 3 2
5 input 1 tgl
6 next 1 3 5
7 state 1 reg_b
8 init 1 7 2
9 next 1 7 2
10 and 1 3 7
11 fair 10
12 bad 3
"#;
        let file = parser::parse(src).expect("parse");
        let sliced = cone_slice(&file, &["reg_a".to_string()]);
        let has_reg_b = sliced
            .lines
            .iter()
            .any(|l| matches!(&l.node, Node::State { symbol: Some(s), .. } if s == "reg_b"));
        assert!(
            has_reg_b,
            "fairness-coupled register `reg_b` must be retained in the slice"
        );
        assert!(
            sliced
                .lines
                .iter()
                .any(|l| matches!(l.node, Node::Fair { .. })),
            "the coupling `fair` line must survive the slice"
        );
    }

    /// R46-6a — a constraint whose signals are ALL out-of-cone (it
    /// constrains a different cluster's registers) does NOT pull anything
    /// in: it cannot restrict any in-cone signal, so dropping it stays
    /// sound and the clustering reduction is preserved.
    #[test]
    fn cone_slice_drops_constraint_disjoint_from_cone() {
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 reg_a
4 init 1 3 2
5 input 1 tgl
6 next 1 3 5
7 state 1 reg_b
8 init 1 7 2
9 next 1 7 2
10 state 1 reg_c
11 init 1 10 2
12 next 1 10 2
13 eq 1 7 10
14 constraint 13
15 bad 3
"#;
        let file = parser::parse(src).expect("parse");
        let sliced = cone_slice(&file, &["reg_a".to_string()]);
        let state_syms: std::collections::HashSet<&str> = sliced
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::State {
                    symbol: Some(s), ..
                } => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            state_syms.contains("reg_a"),
            "in-cone register retained; got {state_syms:?}"
        );
        assert!(
            !state_syms.contains("reg_b") && !state_syms.contains("reg_c"),
            "a constraint over only out-of-cone registers must NOT pull them \
             into the slice (it cannot restrict the cone); got {state_syms:?}"
        );
        assert!(
            !sliced
                .lines
                .iter()
                .any(|l| matches!(l.node, Node::Constraint { .. })),
            "the cone-disjoint constraint must be dropped"
        );
    }

    #[test]
    fn per_cluster_fallback_absent_without_property_seeds() {
        // Same cap-busting design, no per-property seeds → no partition
        // information → the joint cap error stands (legacy behaviour).
        let err = translate(TWO_DISJOINT_CONES_BTOR2, &AdapterOptions::default())
            .expect_err("no seeds → joint cap error");
        assert_eq!(err.kind, AdapterErrorKind::StateSpaceOverflow);
    }

    #[test]
    fn per_cluster_fallback_declines_single_cluster() {
        // Both properties read reg_a (cones identical → one cluster).
        // A single cluster spans the joint cone, so per-cluster cannot
        // beat the joint design and the cap error stands.
        let file = parser::parse(TWO_DISJOINT_CONES_BTOR2).expect("parse");
        let opts = AdapterOptions {
            property_seeds: vec![
                ("p0".to_string(), vec!["a_hot".to_string()]),
                ("p1".to_string(), vec!["a_hot".to_string()]),
            ],
            ..Default::default()
        };
        let err =
            to_ir(&file, &opts).expect_err("single cluster → no per-cluster benefit → cap error");
        assert_eq!(err.kind, AdapterErrorKind::StateSpaceOverflow);
    }

    #[test]
    fn empty_design_produces_one_state() {
        let src = "1 sort bitvec 1\n";
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert_eq!(out.source_info.state_count, 1);
    }

    #[test]
    fn input_pruning_via_sidecar_unlocks_designs_past_raw_input_cap() {
        // Twelve 1-bit inputs → raw input width 12, exceeds
        // MAX_INPUT_BITS = 10. Without sidecar pruning, translation
        // must reject. With a sidecar declaring three of the inputs
        // as `ignored`, the effective input space is 2^9 = 512, well
        // under the cap, and translation succeeds.
        let mut src = "1 sort bitvec 1\n".to_string();
        src.push_str("2 zero 1\n");
        src.push_str("3 state 1 q\n");
        src.push_str("4 init 1 3 2\n");
        for i in 0..12 {
            src.push_str(&format!("{} input 1 in{}\n", i + 5, i));
        }

        // 1) Without sidecar — should reject on the input-bit cap.
        let bare = translate(&src, &AdapterOptions::default());
        assert!(bare.is_err(), "expected raw-input-cap rejection");
        let err = bare.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::StateSpaceOverflow);

        // 2) With a sidecar pruning three inputs to `ignored`, the
        // effective input space drops below the cap and translation
        // succeeds.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [],
            "inputs": [
                { "name": "in0", "abstraction": "ignored" },
                { "name": "in1", "abstraction": "ignored" },
                { "name": "in2", "abstraction": "ignored" },
            ],
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let out = translate(&src, &opts).expect("sidecar-pruned design should translate");
        assert!(out.ctxdsl.contains("automaton"));
    }

    #[test]
    fn input_pruning_ignored_inputs_collapse_to_one_value() {
        // One state, two inputs. Without sidecar: 4 input combos
        // → 2 transitions per input combo across 2 states. With
        // sidecar declaring one input as `ignored`: 2 input combos
        // total — the enumeration count is observably smaller.
        let mut src = "1 sort bitvec 1\n".to_string();
        src.push_str("2 zero 1\n");
        src.push_str("3 state 1 q\n");
        src.push_str("4 init 1 3 2\n");
        src.push_str("5 input 1 keep\n");
        src.push_str("6 input 1 drop\n");
        src.push_str("7 next 1 3 5\n"); // q' = keep (drop is unused)

        // Sidecar pins `drop` to 0.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [],
            "inputs": [
                { "name": "drop", "abstraction": "ignored" },
            ],
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let out = translate(&src, &opts).expect("translate");
        // Labels should mention `keep_0` and `keep_1` but never
        // `drop_1` (drop is pinned to 0).
        let ctxdsl = &out.ctxdsl;
        assert!(
            ctxdsl.contains("keep_0") || ctxdsl.contains("keep_1"),
            "expected keep-labeled transitions; ctxdsl was:\n{ctxdsl}"
        );
        assert!(
            !ctxdsl.contains("drop_1"),
            "drop should be pinned to 0; ctxdsl was:\n{ctxdsl}"
        );
    }

    #[test]
    fn c1_3_unmatched_sidecar_signal_name_warns() {
        // C1.3 — a sidecar `signals[]` entry whose name matches no
        // state cell is a silent no-op today. The warning catches the
        // mistyped-dotted-name case (the common error when editing a
        // `mununu sv discover` skeleton).
        let src = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n";
        let file = parser::parse(src).expect("parse");
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [
                { "name": "nonexistent_reg", "abstraction": "ignored" },
            ],
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let (_ir, warnings, _summary) = to_ir(&file, &opts).expect("translate");
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("nonexistent_reg")
                    && w.message.contains("matched no state cell")),
            "expected an unmatched-sidecar-signal warning; got {warnings:?}"
        );
    }

    #[test]
    fn c1_3_matched_sidecar_signal_name_does_not_warn() {
        // The mirror: a name that DOES resolve to a state cell must not
        // trip the unmatched-name warning.
        let src = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n";
        let file = parser::parse(src).expect("parse");
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [
                { "name": "q", "abstraction": "boolean" },
            ],
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let (_ir, warnings, _summary) = to_ir(&file, &opts).expect("translate");
        assert!(
            !warnings
                .iter()
                .any(|w| w.message.contains("matched no state cell")),
            "matched signal must not warn; got {warnings:?}"
        );
    }

    #[test]
    fn c1_3_unmatched_sidecar_signal_via_drives_warns() {
        // When `drives` is the resolution target, the warning names both
        // the sidecar entry and the unresolved `drives` value.
        let src = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n";
        let file = parser::parse(src).expect("parse");
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "demo",
            "signals": [
                { "name": "alias", "drives": "ghost", "abstraction": "ignored" },
            ],
        })
        .to_string();
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar),
            ..Default::default()
        };
        let (_ir, warnings, _summary) = to_ir(&file, &opts).expect("translate");
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("alias") && w.message.contains("drives = \"ghost\"")),
            "expected drives-note in warning; got {warnings:?}"
        );
    }

    #[test]
    fn partition_summary_populated_on_adapter_output() {
        // Phase A.3 step 3.6 — BTOR2 adapter populates
        // `AdapterOutput.partition_summary` with the right counts and
        // bit-width totals.
        //
        // Fixture: one named state `s_keep` reachable from a `bad` line
        // (Kept), one named state `s_drop` not reachable (Dropped),
        // and one named input `trigger` that drives `s_keep` (Kept by
        // transitive walk). state_bits_before = 1+1 = 2, state_bits_after
        // = 1 (after dropping s_drop's 1-bit width).
        let src = r#"
1 sort bitvec 1
2 zero 1
3 input 1 trigger
4 state 1 s_keep
5 init 1 4 2
6 next 1 4 3
7 state 1 s_drop
8 init 1 7 2
9 next 1 7 2
10 bad 4
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        let summary = out
            .partition_summary
            .expect("partition_summary must be populated");
        assert!(
            summary.dropped_coi >= 1,
            "expected ≥ 1 dropped signal (s_drop); got {summary:?}"
        );
        assert!(
            summary.kept >= 2,
            "expected ≥ 2 kept signals; got {summary:?}"
        );
        assert_eq!(summary.datapath_uf, 0, "datapath UF disabled in A.3");
        // Width tracking should be present for BTOR2.
        let before = summary
            .state_bits_before
            .expect("BTOR2 always tracks widths");
        let after = summary
            .state_bits_after
            .expect("BTOR2 always tracks widths");
        assert!(
            after <= before,
            "state_bits_after ({after}) must not exceed state_bits_before ({before})"
        );
        assert!(
            after < before,
            "expected reduction from dropping s_drop; got before={before} after={after}"
        );
    }

    #[test]
    fn cluster_coi_report_surfaced_when_property_seeds_present() {
        // R4W-2 (R.4 clustered-COI wiring) — when the caller threads
        // per-property COI seeds via `AdapterOptions::property_seeds`,
        // the bit-blaster computes a joint-vs-clustered cone comparison
        // over its dep graph and surfaces it on
        // `PartitionSummary::cluster_coi`.
        //
        // Fixture: two independent 2-state chains. Chain A is
        // `sa -> sa2 -> 0`; chain B is `sb -> sb2 -> 0`. The two cones
        // are disjoint, so a per-property cluster keeps only 2 signals
        // while the naive joint COI keeps all 4 — the M.3 reduction.
        let src = r#"
1 sort bitvec 1
2 zero 1
4 state 1 sa
5 state 1 sa2
6 init 1 4 2
7 init 1 5 2
8 next 1 4 5
9 next 1 5 2
10 state 1 sb
11 state 1 sb2
12 init 1 10 2
13 init 1 11 2
14 next 1 10 11
15 next 1 11 2
16 bad 4
"#;
        let options = AdapterOptions {
            property_seeds: vec![
                ("propA".to_string(), vec!["sa".to_string()]),
                ("propB".to_string(), vec!["sb".to_string()]),
            ],
            ..AdapterOptions::default()
        };
        let out = translate(src, &options).expect("translate");
        let summary = out
            .partition_summary
            .expect("partition_summary must be populated");
        let report = summary
            .cluster_coi
            .expect("cluster_coi must be Some when property_seeds is non-empty");

        // Joint COI over {sa, sb} reaches {sa, sa2, sb, sb2}.
        assert_eq!(
            report.joint_cone_size, 4,
            "joint cone = {{sa, sa2, sb, sb2}}; got {report:?}"
        );
        // Disjoint cones at the 0.5 floor → two singleton clusters.
        assert_eq!(
            report.clusters.len(),
            2,
            "disjoint cones must yield 2 clusters; got {report:?}"
        );
        // Each cluster's cone is exactly 2 signals.
        assert_eq!(
            report.max_cluster_cone_size, 2,
            "each per-cluster cone = 2 signals; got {report:?}"
        );
        // The load-bearing M.3 claim: clustering shrinks the binding cone.
        assert!(
            report.max_cluster_cone_size < report.joint_cone_size,
            "clustering must reduce the max cone vs the joint cone; got {report:?}"
        );
    }

    #[test]
    fn cluster_coi_absent_without_property_seeds() {
        // R4W-2 — the legacy intrinsic-seed-only path leaves
        // `cluster_coi` as `None` (no behaviour change).
        let src = r#"
1 sort bitvec 1
2 zero 1
4 state 1 sa
6 init 1 4 2
8 next 1 4 2
16 bad 4
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        let summary = out
            .partition_summary
            .expect("partition_summary must be populated");
        assert!(
            summary.cluster_coi.is_none(),
            "cluster_coi must be None when property_seeds is empty; got {summary:?}"
        );
    }

    #[test]
    fn cluster_similarity_floor_is_honored_through_adapter_options() {
        // R4W-3 — the bit-blaster honours
        // `AdapterOptions::cluster_similarity_floor`, not a hardcoded
        // 0.5. Fixture: two chains that SHARE one signal, so their cones
        // partially overlap (Jaccard = |{shared}| / |{sa,sa2,sb,sb2,shared}|
        // = 1/5 = 0.2):
        //   chain A: sa' = and(sa2, shared)  → cone {sa, sa2, shared}
        //   chain B: sb' = and(sb2, shared)  → cone {sb, sb2, shared}
        // A floor of 0.0 (≤ 0.2) merges them into one cluster; a floor of
        // 0.9 (> 0.2) keeps them apart. The cluster count flipping with
        // the floor proves the override threads end-to-end.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 shared
4 init 1 3 2
5 next 1 3 2
6 state 1 sa
7 state 1 sa2
8 init 1 6 2
9 init 1 7 2
10 and 1 7 3
11 next 1 6 10
12 next 1 7 2
13 state 1 sb
14 state 1 sb2
15 init 1 13 2
16 init 1 14 2
17 and 1 14 3
18 next 1 13 17
19 next 1 14 2
20 bad 6
"#;
        let seeds = vec![
            ("propA".to_string(), vec!["sa".to_string()]),
            ("propB".to_string(), vec!["sb".to_string()]),
        ];

        // Loose floor (0.0): the 0.2-overlapping cones merge → 1 cluster
        // whose cone is the full joint cone (5 signals).
        let loose = translate(
            src,
            &AdapterOptions {
                property_seeds: seeds.clone(),
                cluster_similarity_floor: Some(0.0),
                ..AdapterOptions::default()
            },
        )
        .expect("translate")
        .partition_summary
        .and_then(|s| s.cluster_coi)
        .expect("cluster_coi");
        assert_eq!(loose.clusters.len(), 1, "floor 0.0 must merge; {loose:?}");
        assert_eq!(
            loose.max_cluster_cone_size, loose.joint_cone_size,
            "merged cluster cone == joint cone; {loose:?}"
        );

        // Tight floor (0.9): 0.2 < 0.9 → the cones stay in 2 clusters,
        // each smaller than the joint cone.
        let tight = translate(
            src,
            &AdapterOptions {
                property_seeds: seeds,
                cluster_similarity_floor: Some(0.9),
                ..AdapterOptions::default()
            },
        )
        .expect("translate")
        .partition_summary
        .and_then(|s| s.cluster_coi)
        .expect("cluster_coi");
        assert_eq!(tight.clusters.len(), 2, "floor 0.9 must split; {tight:?}");
        assert!(
            tight.max_cluster_cone_size < tight.joint_cone_size,
            "split clusters reduce the binding cone; {tight:?}"
        );
    }

    #[test]
    fn constraint_filters_transitions() {
        // input drives state; constraint = input must be 0; bad line
        // pinned to state `q` so auto-COI (Phase A.3) keeps `q` in the
        // partition. Without that pin, COI would correctly drop `q`
        // because no property atom referenced it — collapsing the
        // test's state space below the count required to assert that
        // *constraint* filtering takes effect.
        //
        // Without constraint: 2 states × 2 inputs = 4 transitions.
        // With constraint:    2 states × 1 input  = 2 transitions.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 input 1 a
6 next 1 3 5
7 not 1 5
8 constraint 7
9 bad 3
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert_eq!(out.source_info.state_count, 2);
        // Transitions encoded in CTXDSL — we just check the run completed.
        assert!(out.ctxdsl.contains("automaton"));
    }

    // ---- Path 3 / Option A (§Phase 8 §8.2 residual) — anyconst-init enumeration ----

    #[test]
    fn path3_baseline_single_init_when_all_cells_have_init_lines() {
        // 1-bit state q with explicit init 0; q' = q (latch). Single
        // init state — Path 3 fallback path returns the deterministic
        // index. Validates the legacy behaviour stays untouched when
        // there are no anyconst cells.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        // Two abstract states (q=0, q=1); only q=0 is initial.
        assert_eq!(out.source_info.state_count, 2);
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(initial_count, 1, "exactly one initial-state declaration");
    }

    #[test]
    fn path3_uninit_cell_without_anyconst_emits_soundness_warning() {
        // 2-bit state q with NO init line and NO sidecar anyconst
        // declaration — the cell defaults to zero. Sound only under
        // `setundef -zero`; unsound under any nondeterministic policy.
        // The translator must emit a soundness warning naming the
        // uncovered cell.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        let soundness_warning = out
            .warnings
            .iter()
            .find(|w| w.message.contains("default to zero"))
            .expect("expected SOUNDNESS warning for uninit cell");
        assert!(
            soundness_warning.message.contains("q"),
            "warning should name the uncovered cell: {}",
            soundness_warning.message
        );
        assert!(
            soundness_warning.message.contains("init_policy: anyconst"),
            "warning should point at the remediation"
        );
    }

    #[test]
    fn path3_uninit_cell_with_anyconst_emits_no_soundness_warning() {
        // Same fixture as above, but `q` is declared anyconst in the
        // sidecar — the soundness concern is resolved by Path 3
        // enumeration, no warning emitted.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "q", "abstraction": "bit_blast", "init_policy": "anyconst"}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert!(
            !out.warnings
                .iter()
                .any(|w| w.message.contains("default to zero")),
            "no soundness warning expected when cell is covered by anyconst sidecar"
        );
    }

    #[test]
    fn path3_init_line_present_emits_no_soundness_warning() {
        // Baseline: cell has an explicit Init line — no warning needed
        // because the init value is determined, not defaulted.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert!(
            !out.warnings
                .iter()
                .any(|w| w.message.contains("default to zero")),
            "no soundness warning when cell has explicit init"
        );
    }

    // ---- R-S2a (§Phase 9 §9.1) — BTOR2 init-line seeding ----

    #[test]
    fn r_s2a_seeds_init_value_when_sidecar_enum_lacks_it() {
        // 2-bit state q with init=3. Sidecar declares q as enum
        // with only variant {Q_0: 0}. R-S2a should auto-add
        // {q_3: 3} so the formula can reference q_3.
        let src = r#"
1 sort bitvec 2
2 constd 1 3
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "q",
                    "abstraction": "enum",
                    "variants": ["Q_0"],
                    "value_map": [{"name": "Q_0", "value": 0}]
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // After R-S2a seeding, the abstraction set has {0, 3} → 2 states.
        assert_eq!(out.source_info.state_count, 2);
    }

    #[test]
    fn mig2_escape_routes_to_oob_sink_not_dropped() {
        // MIG-2 — a 2-bit counter `cnt` that increments, but the
        // sidecar declares it `enum {0, 1}`. From cnt=1 the next value
        // is 2, which escapes the declared value set. Previously this
        // transition was DROPPED (under-approximation); now it routes
        // to the `__mununu_oob__` sink (sound over-approximation).
        let src = r#"
1 sort bitvec 1
2 sort bitvec 2
3 constd 2 0
4 state 2 cnt
5 init 2 4 3
6 one 2
7 add 2 4 6
8 next 2 4 7
9 eq 1 4 3
10 bad 9
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "cnt",
                    "abstraction": "enum",
                    "variants": ["C_0", "C_1"],
                    "value_map": [
                        {"name": "C_0", "value": 0},
                        {"name": "C_1", "value": 1}
                    ]
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // The escaping transition (cnt=1 → cnt=2) routes to the OOB
        // sink: the sink state + its `__mununu_oob__` marker valuation
        // appear in the emitted CTXDSL, and a transition targets it.
        assert!(
            out.ctxdsl.contains(OOB_SINK_KEY),
            "OOB sink must appear in the CTXDSL when an abstraction escape occurs:\n{}",
            out.ctxdsl
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.message.contains("routed to the OOB sink")),
            "an OOB-routing warning must be emitted"
        );
    }

    #[test]
    fn r_s2a_no_op_when_init_value_already_in_value_map() {
        // Same fixture as above but value_map already includes 3.
        // R-S2a should skip (additive, dedupes).
        let src = r#"
1 sort bitvec 2
2 constd 1 3
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "q",
                    "abstraction": "enum",
                    "variants": ["Q_0", "Q_3"],
                    "value_map": [
                        {"name": "Q_0", "value": 0},
                        {"name": "Q_3", "value": 3}
                    ]
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // value_map has 2 entries (0, 3) → 2 states; R-S2a no-op.
        assert_eq!(out.source_info.state_count, 2);
    }

    #[test]
    fn r_s2a_skips_signals_without_sidecar_entry() {
        // q has init=1 but no sidecar entry. R-S2a should skip
        // (cells without sidecar use full bit-blast — init already
        // in the admissible set).
        let src = r#"
1 sort bitvec 2
2 one 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        // No sidecar → full bit-blast over 2 bits = 4 states.
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert_eq!(out.source_info.state_count, 4);
    }

    #[test]
    fn r_s2a_skips_non_enum_abstractions() {
        // q has init=1 and sidecar bounded_counter abstraction.
        // R-S2a only widens EnumValues; bounded_counter handles its
        // value set via the (lo, hi) bound and doesn't take per-value
        // variant names.
        let src = r#"
1 sort bitvec 4
2 one 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "q", "abstraction": "bounded_counter", "bound": 2}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // bounded_counter [0, 2] = 3 states; R-S2a no-op.
        assert_eq!(out.source_info.state_count, 3);
    }

    // ---- R-Y3 (§Phase 8) — BTOR2 init smart defaults for unsidecared cells ----

    // ---- §Phase 10 §10.2 stage 1 — BTOR2 $mem lifter extension ----

    /// BTOR2 fixture with one array state cell (5-bit address × 8-bit data).
    /// Has Write + Next + Read ops on the array; mirrors what Yosys
    /// emits for a small `logic [7:0] m [0:31]` SV declaration.
    const PHASE10_FIXTURE_WITH_MEMORY: &str = r#"
1 sort bitvec 1
2 sort bitvec 5
3 sort bitvec 8
4 sort array 2 3
5 state 4 rf_reg
6 input 1 we
7 input 2 addr
8 input 3 wdata
9 ite 4 6 5 5
10 next 4 5 9
11 read 3 5 7
12 zero 3
13 eq 1 11 12
14 bad 13
"#;

    #[test]
    fn phase10_stage1_no_sidecar_emits_actionable_template_error() {
        let err = translate(PHASE10_FIXTURE_WITH_MEMORY, &AdapterOptions::default())
            .expect_err("should error on undeclared memory");
        assert!(
            err.message.contains("§Phase 10"),
            "error should cite §Phase 10: {}",
            err.message
        );
        assert!(
            err.message.contains("rf_reg"),
            "error should name the undeclared memory cell: {}",
            err.message
        );
        assert!(
            err.message.contains("\"address_width\":5"),
            "error template should include the BTOR2-derived address_width: {}",
            err.message
        );
        assert!(
            err.message.contains("\"data_width\":8"),
            "error template should include the BTOR2-derived data_width: {}",
            err.message
        );
        assert!(
            err.message.contains("\"abstraction\":\"havoc\""),
            "error template should suggest havoc as the stage-1 default: {}",
            err.message
        );
    }

    #[test]
    fn phase10_stage1_non_havoc_abstraction_passes_to_op_check() {
        // With a consistent UF-mode sidecar declaration, stage 1
        // validation passes, but the lift errors at the Read/Write
        // op check (stage 3 territory — UF is not yet shipped).
        // The error message includes the §Phase 10 hint.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "uf"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let err = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect_err("stage 1 still errors at Read/Write op check under non-havoc abstraction");
        assert!(
            err.message.contains("§Phase 10"),
            "op-check error should include the §Phase 10 hint: {}",
            err.message
        );
        assert!(
            err.message.contains("stage 3") || err.message.contains("stage 4"),
            "op-check hint should point at stages 3/4: {}",
            err.message
        );
    }

    // ---- §Phase 10 §10.2 stage 3.a — UF-mode recognition layer ----

    #[test]
    fn phase10_stage3a_uf_declaration_yields_uf_specific_hint() {
        // When the sidecar declares `abstraction: uf` on a memory
        // that's referenced by an Op::Read in the BTOR2, the
        // op-check error message must include the stage-3.a hint
        // naming the memory + pointing at stages 3.b/3.c. This is
        // strictly more specific than the generic stage 1
        // "stage 3/4 not yet shipped" message.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "uf"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let err = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect_err("stage 1 still errors at Read/Write op check under uf abstraction");
        assert!(
            err.message.contains("stage 3.a"),
            "stage 3.a hint should appear when UF was declared: {}",
            err.message
        );
        assert!(
            err.message.contains("rf_reg"),
            "stage 3.a hint should name the UF-declared memory: {}",
            err.message
        );
        assert!(
            err.message.contains("stage 3.b") || err.message.contains("stage 3.c"),
            "stage 3.a hint should point at stages 3.b/3.c: {}",
            err.message
        );
    }

    #[test]
    fn phase10_stage3a_havoc_declaration_does_not_get_uf_hint() {
        // Confirm the stage 3.a UF-specific hint does NOT fire on
        // havoc-declared memories. The havoc path lifts end-to-end
        // (see phase10_stage1b_havoc_rewrites_and_lifts_end_to_end)
        // so the op-check error doesn't fire at all here — but we
        // double-check by asserting that translate succeeds.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        // Translate succeeds end-to-end on the havoc path → no
        // error to inspect for stage-3.a hint leakage. The mere
        // success here proves the stage-3.a recognition doesn't
        // interfere with the havoc lift.
        let _out = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect("havoc path lifts end-to-end, regardless of stage 3.a recognition");
    }

    // ---- §Phase 10 §10.2 stage 1b — havoc-mode BTOR2 rewriting ----

    #[test]
    fn phase10_stage1b_havoc_rewrites_and_lifts_end_to_end() {
        // With `abstraction: havoc` the rewriter drops the memory
        // state cell, drops Init/Next/Write lines targeting it, and
        // converts Read into a fresh nondet input. The bit-blaster
        // then succeeds end-to-end (no Read/Write ops survive).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect("havoc rewrite should make the lift succeed");
        // The memory cell vanishes from the state space; no scalar
        // state cells survive the rewrite either, so state_count = 1
        // (the BTOR2 has a single state cell, the rf_reg array, which
        // gets dropped).
        assert_eq!(
            out.source_info.state_count, 1,
            "rewritten BTOR2 has no surviving state cells → trivial 1-state model"
        );
        // The havoc warning must fire and name the abstracted memory.
        let havoc_warn = out
            .warnings
            .iter()
            .find(|w| w.message.contains("§Phase 10 §10.2 stage 1b"))
            .expect("havoc warning should fire");
        assert!(
            havoc_warn.message.contains("rf_reg"),
            "havoc warning should name the abstracted memory: {}",
            havoc_warn.message
        );
        assert!(
            havoc_warn.message.contains("SOUND for safety"),
            "havoc warning should state soundness posture: {}",
            havoc_warn.message
        );
    }

    #[test]
    fn phase10_stage1b_havoc_input_added_at_read_nid() {
        // The havoc rewrite replaces the Read at NID 11 with an
        // Input at the same NID, carrying a `__havoc_read_11` symbol
        // for traceability. We probe by checking the surviving
        // input count grows by 1 (we, addr, wdata, plus __havoc_read_11).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 5,
                    "data_width": 8,
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts).expect("havoc lift should succeed");
        // The synthetic input should appear in the generated CTXDSL
        // text as a label name (the rewrite gave it a symbol prefixed
        // with `__havoc_read_`).
        assert!(
            out.ctxdsl.contains("__havoc_read_"),
            "rewritten CTXDSL should mention the synthetic havoc-read input; got:\n{}",
            out.ctxdsl
        );
    }

    #[test]
    fn phase10_stage1b_havoc_does_not_fire_without_sidecar_entry() {
        // A sidecar with no `memories[]` entry causes stage 1a to
        // emit the actionable error — stage 1b never runs (no
        // declared abstraction).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": []
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let err = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect_err("undeclared memory should still error at stage 1a");
        assert!(
            err.message.contains("§Phase 10"),
            "stage 1a actionable error should fire: {}",
            err.message
        );
    }

    #[test]
    fn phase10_stage1b_no_op_when_no_memories() {
        // Fixture without any memory cells — neither stage 1a nor
        // 1b run; no havoc warning fires. Legacy behaviour preserved.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert!(
            out.warnings.iter().all(|w| !w.message.contains("stage 1b")),
            "no stage 1b warning should fire on memory-free fixture"
        );
    }

    #[test]
    fn phase10_stage1_sidecar_mismatched_dimensions_errors() {
        // Sidecar declares wrong dimensions; validation should flag
        // the mismatch and emit a corrective template.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "memories": [
                {
                    "name": "rf_reg",
                    "address_width": 6,  // wrong (BTOR2 says 5)
                    "data_width": 16,    // wrong (BTOR2 says 8)
                    "abstraction": "havoc"
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let err = translate(PHASE10_FIXTURE_WITH_MEMORY, &opts)
            .expect_err("should error on dimension mismatch");
        assert!(
            err.message.contains("mismatch"),
            "error should flag mismatch: {}",
            err.message
        );
        assert!(
            err.message
                .contains("BTOR2 has address_width=5, data_width=8"),
            "error should show the BTOR2-detected dimensions: {}",
            err.message
        );
        assert!(
            err.message
                .contains("sidecar declared address_width=6, data_width=16"),
            "error should show the sidecar-declared dimensions: {}",
            err.message
        );
    }

    #[test]
    fn phase10_stage1_no_memory_cells_no_op() {
        // Fixture without any array state cells: validation runs but
        // emits no error; existing behaviour preserved.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert!(
            out.warnings
                .iter()
                .all(|w| !w.message.contains("§Phase 10"))
        );
    }

    // ---- R-Y6 (§Phase 8) — reset-sequence-aware init ----

    #[test]
    fn r_y6_no_sidecar_no_op() {
        // Counter that increments every cycle; init=0. Without R-Y6
        // sequence, init state is 0.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 state 1 q
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 input 1 rst_n
9 ones 1
10 eq 1 3 9
11 bad 10
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        // Single init state (q=0), exactly 1 "initial" marker.
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(initial_count, 1);
    }

    #[test]
    fn r_y6_with_sidecar_advances_init_state_by_k_cycles() {
        // Counter q increments every cycle UNLESS rst_n=0 (active-low
        // reset) which holds q at 0. R-Y6 with hold_cycles=2,
        // asserted_value=0 → q held at 0 for 2 cycles, then init is
        // computed from the post-reset state (still 0; counter never
        // increments while rst_n=0).
        let src = r#"
1 sort bitvec 2
2 zero 1
3 state 1 q
4 init 1 3 2
5 one 1
6 add 1 3 5
7 input 1 rst_n
8 ite 1 7 6 2
9 next 1 3 8
10 ones 1
11 eq 1 3 10
12 bad 11
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "reset_sequence": {
                "reset_input": "rst_n",
                "asserted_value": 0,
                "hold_cycles": 2
            }
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // Single init state — q still 0 after 2 reset cycles.
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(initial_count, 1);
    }

    #[test]
    fn r_y6_runs_design_logic_for_k_cycles_without_reset() {
        // Counter q increments every cycle unconditionally (no reset
        // logic in the next-state function). R-Y6 still pins the
        // declared "reset" input but the design ignores it. After
        // hold_cycles=3 cycles, q should be at value 3.
        let src = r#"
1 sort bitvec 3
2 zero 1
3 state 1 q
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 input 1 rst_n
9 ones 1
10 eq 1 3 9
11 bad 10
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "reset_sequence": {
                "reset_input": "rst_n",
                "asserted_value": 0,
                "hold_cycles": 3
            }
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // q advances from 0 → 1 → 2 → 3 over the 3 reset cycles.
        // The bad signal `eq 1 3 9` (=q==7) is not satisfied at q=3,
        // so init state q=3 satisfies. The CTXDSL has a `q = 3`
        // valuation block on the initial state.
        assert!(
            out.ctxdsl.contains("q = 3"),
            "expected init valuation q = 3 after 3-cycle reset hold; got:\n{}",
            out.ctxdsl
        );
    }

    #[test]
    fn r_y6_unknown_reset_input_is_silent_no_op() {
        // Sidecar names a reset signal that doesn't exist in the
        // BTOR2. R-Y6 silently skips (additive); init state computed
        // normally.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 state 1 q
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 ones 1
9 eq 1 3 8
10 bad 9
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "reset_sequence": {
                "reset_input": "nonexistent_reset",
                "asserted_value": 0,
                "hold_cycles": 3
            }
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // Without applicable reset signal, init state is cycle 0 (q=0).
        assert!(out.ctxdsl.contains("q = 0"));
    }

    // ---- F1 (S-track KMTS-fidelity) — async-reset modeling ----

    #[test]
    fn f1_looks_like_reset_heuristic() {
        assert_eq!(looks_like_reset("rst"), Some(true));
        assert_eq!(looks_like_reset("reset"), Some(true));
        assert_eq!(looks_like_reset("arst"), Some(true));
        assert_eq!(looks_like_reset("i_rst"), Some(true)); // i_ affix stripped
        assert_eq!(looks_like_reset("rst_n"), Some(false)); // active-low
        assert_eq!(looks_like_reset("resetn"), Some(false));
        assert_eq!(looks_like_reset("arst_n"), Some(false));
        assert_eq!(looks_like_reset("rst_ni"), Some(false));
        assert_eq!(looks_like_reset("clk"), None);
        assert_eq!(looks_like_reset("data"), None);
        assert_eq!(looks_like_reset("address"), None); // contains no reset word
    }

    #[test]
    fn f1_auto_reset_inits_at_reset_value_and_excludes_rst_label() {
        // Async-reset shape: state `fsm` has NO `init` line; its next is
        // `ite(rst, 1, 2)` — asserting rst forces fsm to the reset value
        // 1, otherwise it goes to 2. The name `rst` is auto-detected
        // (active-high), so F1 should (a) init at fsm=1 (reset value),
        // NOT fsm=0 (uninitialised power-on), and (b) pin rst inactive →
        // no `rst_*` label in the alphabet.
        let src = r#"
1 sort bitvec 1
2 sort bitvec 2
3 state 2 fsm
4 constd 2 1
5 constd 2 2
6 input 1 rst
7 ite 2 6 4 5
8 next 2 3 7
9 ones 2
10 eq 1 3 9
11 bad 10
"#;
        let opts = AdapterOptions::default();
        let out = translate(src, &opts).expect("translate");
        // F1.1 — initial state is the reset value (fsm=1 → state name s1),
        // not the uninitialised default (fsm=0 → s0).
        assert!(
            out.ctxdsl.contains("s1 initial"),
            "F1.1: expected initial state = reset value fsm=1 (s1); got:\n{}",
            out.ctxdsl
        );
        assert!(
            !out.ctxdsl.contains("s0 initial"),
            "F1.1: initial state must not be the uninitialised default fsm=0 (s0); got:\n{}",
            out.ctxdsl
        );
        // F1.2 — rst is pinned inactive and excluded from the label
        // alphabet (no `rst_0`/`rst_1` labels), matching native.
        assert!(
            !out.ctxdsl.contains("rst_0") && !out.ctxdsl.contains("rst_1"),
            "F1.2: reset input must be excluded from the transition labels; got:\n{}",
            out.ctxdsl
        );
    }

    #[test]
    fn f1_no_reset_leaves_inputs_unpinned() {
        // Negative control: an input named `enable` (not a reset) must
        // still be a free label — F1 must not over-pin non-reset inputs.
        let src = r#"
1 sort bitvec 1
2 sort bitvec 2
3 state 2 fsm
4 constd 2 1
5 constd 2 2
6 input 1 enable
7 ite 2 6 4 5
8 next 2 3 7
9 ones 2
10 eq 1 3 9
11 bad 10
"#;
        let opts = AdapterOptions::default();
        let out = translate(src, &opts).expect("translate");
        assert!(
            out.ctxdsl.contains("enable_0") || out.ctxdsl.contains("enable_1"),
            "non-reset input `enable` must remain a free label; got:\n{}",
            out.ctxdsl
        );
    }

    // ---- F2 (S-track KMTS-fidelity) — combinational-output support ----

    #[test]
    fn f2_property_combinational_candidate_names_extracts_signal() {
        // A `<sig>_T_state_VARIANT` compound predicate names the boolean
        // signal `<sig>`; the extractor returns the prefix before `_T_`.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "properties": [
                {"id": "no_overlap",
                 "formula": "nu X. (!overlap_T_state_IDLE && !overlap_T_state_AES_ACCESS && [] X)"},
                {"id": "no_bypass",
                 "formula": "nu X. (!bypass_T_state_GRANTED && [] X)"}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let cands = property_combinational_candidate_names(&opts);
        assert!(
            cands.contains("overlap"),
            "expected `overlap`; got {cands:?}"
        );
        assert!(cands.contains("bypass"), "expected `bypass`; got {cands:?}");
        // `state` is the SECOND signal in the compound; it is NOT a `_T_`
        // prefix, so it is not extracted here (and is a register anyway,
        // filtered by combinational_signal_nids).
        assert!(
            !cands.contains("state"),
            "should not extract `state`; got {cands:?}"
        );
    }

    #[test]
    fn state_splitting_resolves_joint_mutex_unit() {
        // Two named combinational signals that are MUTUALLY EXCLUSIVE per
        // input: sa = a && !b, sb = !a && b. A joint mutex `!(sa_T && sb_T)`
        // therefore HOLDS. Per-signal ∃-priority would wrongly flag it
        // (sa can be T for a=1,b=0; sb for a=0,b=1) → false; full
        // state-splitting over the JOINT (sa, sb) assignment resolves it
        // correctly → true. Pure BTOR2 path (no yosys).
        let src = r#"
1 sort bitvec 1
2 input 1 a
3 input 1 b
4 not 1 3
5 and 1 2 4 sa
6 not 1 2
7 and 1 6 3 sb
8 state 1 state
9 zero 1
10 init 1 8 9
11 next 1 8 8
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "state", "abstraction": "enum", "variants": ["S0", "S1"]},
                {"name": "sa", "abstraction": "boolean", "combinational": true},
                {"name": "sb", "abstraction": "boolean", "combinational": true}
            ],
            "properties": [
                {"id": "no_both",
                 "formula": "nu X. (!(sa_T_state_S0 && sb_T_state_S0) && !(sa_T_state_S1 && sb_T_state_S1) && [] X)"}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        let doc = crate::context_dsl::parse(&out.ctxdsl).expect("parse");
        let realized = crate::context_dsl::realize_context(&doc, &[]).expect("realize");
        let over = realized.context.clts_names().first().unwrap().clone();
        let clts = realized.context.clts(&over).unwrap();
        let env = realized.environment_for(&over);
        let rf = realized.formulas.get("no_both").expect("no_both formula");
        let result = realized
            .context
            .evaluate_mu(&over, &rf.formula, &env, None)
            .expect("eval");
        let inits: Vec<_> = clts.initial_states().iter().copied().collect();
        assert!(!inits.is_empty(), "expected initial states");
        let satisfied = inits
            .iter()
            .all(|s| result.get(s.index()).map(|b| *b).unwrap_or(false));
        assert!(
            satisfied,
            "joint mutex must HOLD via state-splitting (sa, sb mutually \
             exclusive); ∃-priority would give false.\nctxdsl:\n{}",
            out.ctxdsl
        );
    }

    // ---- R-Y4 (§Phase 8) — bounded-havoc init value sets ----

    #[test]
    fn r_y4_bounded_init_restricts_path3_enumeration() {
        // 2-bit state q with anyconst init policy + bounded_init: [0, 2].
        // Path 3 should enumerate exactly 2 init states (q=0, q=2)
        // instead of all 4 admissible values.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "q",
                    "abstraction": "bit_blast",
                    "init_policy": "anyconst",
                    "bounded_init": [0, 2]
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // 4 abstract states; 2 are initial (the bounded set).
        assert_eq!(out.source_info.state_count, 4);
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 2,
            "bounded_init restricts enumeration to {{0, 2}} = 2 init states"
        );
    }

    #[test]
    fn r_y4_empty_bounded_init_falls_back_to_full_anyconst() {
        // bounded_init: [] should be treated as "no bounding" — Path 3
        // enumerates the full admissible set per the anyconst policy.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "q",
                    "abstraction": "bit_blast",
                    "init_policy": "anyconst",
                    "bounded_init": []
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 4,
            "empty bounded_init → full anyconst enumeration"
        );
    }

    #[test]
    fn r_y4_no_op_when_init_policy_not_anyconst() {
        // bounded_init declared but init_policy not anyconst — Path 3
        // does not fire (no nondet_init_nids); bounded_init is
        // collected but ignored. Legacy single-init runs.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 init 1 4 2
6 next 1 4 4
7 eq 1 4 3
8 bad 7
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {
                    "name": "q",
                    "abstraction": "bit_blast",
                    "bounded_init": [1, 2]
                }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 1,
            "no anyconst → no Path 3 enumeration → legacy single-init"
        );
    }

    #[test]
    fn r_y3_default_off_preserves_full_bit_blast() {
        // 2-bit state q with init=1; NO sidecar entry. Default
        // R-Y3 off → full bit-blast = 4 states.
        let src = r#"
1 sort bitvec 2
2 one 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert_eq!(out.source_info.state_count, 4);
    }

    #[test]
    fn r_y3_enabled_pins_unsidecared_cell_to_init_value() {
        // Same fixture; R-Y3 ON → q pinned to init=1, single state.
        let src = r#"
1 sort bitvec 2
2 one 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let opts = AdapterOptions {
            smart_init_defaults: true,
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // q pinned to init=1 → 1 state.
        assert_eq!(out.source_info.state_count, 1);
        // Warning fires naming the affected cell.
        let warning = out
            .warnings
            .iter()
            .find(|w| w.message.contains("R-Y3"))
            .expect("R-Y3 warning expected");
        assert!(
            warning.message.contains("q"),
            "warning should name the pinned cell: {}",
            warning.message
        );
        assert!(
            warning.message.contains("unsound for safety"),
            "warning should surface the soundness tradeoff"
        );
    }

    #[test]
    fn r_y3_skips_sidecared_cells() {
        // q has init=1 AND sidecar entry. R-Y3 should NOT override
        // the sidecar declaration (only fills in for cells WITHOUT one).
        let src = r#"
1 sort bitvec 4
2 one 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
6 eq 1 3 2
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "q", "abstraction": "bounded_counter", "bound": 2}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            smart_init_defaults: true,
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // Sidecar wins: bounded_counter [0, 2] = 3 states; R-Y3 no-op.
        assert_eq!(out.source_info.state_count, 3);
    }

    #[test]
    fn r_y3_skips_cells_without_init_line() {
        // q has NO init line AND no sidecar. R-Y3 skips (nothing to
        // pin to); falls back to full bit-blast.
        let src = r#"
1 sort bitvec 2
2 state 1 q
3 next 1 2 2
4 zero 1
5 eq 1 2 4
6 bad 5
"#;
        let opts = AdapterOptions {
            smart_init_defaults: true,
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // Full bit-blast (no init to pin to) = 4 states.
        assert_eq!(out.source_info.state_count, 4);
    }

    #[test]
    fn path3_no_anyconst_sidecar_means_legacy_single_init_even_for_uninit_cells() {
        // 2-bit state q with NO init line, NO sidecar. The `bad` line
        // pins q into the property cone so auto-COI doesn't drop it.
        // Path 3 is surgical — without sidecar anyconst, q defaults
        // to zero (legacy single-init).
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let out = translate(src, &AdapterOptions::default()).expect("translate");
        assert_eq!(out.source_info.state_count, 4);
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 1,
            "no sidecar anyconst declaration ⇒ legacy single-init"
        );
    }

    #[test]
    fn path3_anyconst_sidecar_enumerates_all_admissible_init_values() {
        // 2-bit state q with NO init line + bad q==3 (so partition
        // keeps q). Sidecar declares anyconst on q → all 4 admissible
        // values become initial states.
        let src = r#"
1 sort bitvec 2
2 zero 1
3 ones 1
4 state 1 q
5 next 1 4 4
6 eq 1 4 3
7 bad 6
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "q", "abstraction": "bit_blast", "init_policy": "anyconst"}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert_eq!(out.source_info.state_count, 4);
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 4,
            "sidecar-declared anyconst ⇒ every admissible init value is initial"
        );
    }

    #[test]
    fn path3_mixed_some_deterministic_some_anyconst_init() {
        // Two state cells: q1 (init 0), q2 (no init). Sidecar declares
        // anyconst on q2 only. The `bad` line keeps both in the cone.
        // Expected initial states: {(q1=0, q2=0), (q1=0, q2=1)} = 2.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 ones 1
4 state 1 q1
5 state 1 q2
6 init 1 4 2
7 next 1 4 4
8 next 1 5 5
9 eq 1 4 3
10 bad 9
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "signals": [
                {"name": "q1", "abstraction": "bit_blast"},
                {"name": "q2", "abstraction": "bit_blast", "init_policy": "anyconst"}
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert_eq!(out.source_info.state_count, 4);
        let initial_count = out.ctxdsl.lines().filter(|l| l.contains("initial")).count();
        assert_eq!(
            initial_count, 2,
            "cartesian product collapses on the pinned cell"
        );
    }

    // ---- R.5b lifter integration MVP — UF wrapping tests ----

    /// BTOR2 fixture for R.5b: 2-bit register `cnt` that gets
    /// `mul_inst` (a wide-arithmetic Op aliased via Yosys's `uext`
    /// pattern) as its next-value. With `uf_wrap = ["mul_inst"]`,
    /// the wrapped Op's result is forced to 0 → cnt's next-value
    /// is always 0 → cnt stays at its init value.
    ///
    /// All sort refs use NID 1 (the only `sort bitvec 2` line);
    /// `const 1 11` denotes "constant of sort 1 with binary value 11"
    /// = 3 in decimal.
    const R5B_UF_FIXTURE: &str = r#"
1 sort bitvec 2
2 zero 1
3 const 1 11
4 state 1 cnt
5 init 1 4 2
6 add 1 4 3
7 uext 1 6 0 mul_inst
8 next 1 4 7
"#;

    #[test]
    fn r5b_collect_uf_wrapped_nids_empty_without_sidecar() {
        let file = crate::adapter::btor2::parser::parse(R5B_UF_FIXTURE).expect("parse");
        let opts = AdapterOptions::default();
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            nids.is_empty(),
            "without a sidecar, no UF wrapping should fire"
        );
    }

    #[test]
    fn r5b_collect_uf_wrapped_nids_finds_named_op() {
        // Sidecar uf_wrap = ["mul_inst"] — the helper must find the
        // Op NID 7 (the uext alias carrying that symbol).
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["mul_inst"]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let file = crate::adapter::btor2::parser::parse(R5B_UF_FIXTURE).expect("parse");
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert_eq!(nids.len(), 1, "must find exactly one wrapped Op");
        assert!(nids.contains(&7), "must contain the mul_inst Op NID (7)");
    }

    #[test]
    fn r5b_uf_unwrap_overrides_uf_wrap() {
        // Both wrap + unwrap declared on the same symbol → unwrap wins.
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_wrap": ["mul_inst"],
            "uf_unwrap": ["mul_inst"]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let file = crate::adapter::btor2::parser::parse(R5B_UF_FIXTURE).expect("parse");
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            nids.is_empty(),
            "uf_unwrap must override uf_wrap when both list the same name"
        );
    }

    // ---- R.5b default UF policy tests ----

    /// BTOR2 fixture with one `Op::Mul` (NID 5) of width 2 — should
    /// be UF-wrapped under the default policy regardless of sidecar
    /// declarations.
    const R5B_DEFAULT_MUL_FIXTURE: &str = r#"
1 sort bitvec 2
2 const 1 10
3 const 1 01
4 state 1 acc
5 mul 1 4 2
6 next 1 4 5
"#;

    /// BTOR2 fixture with one `Op::Add` of width 64 — should be
    /// UF-wrapped under the default policy (width > 32).
    const R5B_DEFAULT_WIDE_ADD_FIXTURE: &str = r#"
1 sort bitvec 64
2 const 1 0
3 const 1 1
4 state 1 wide_cnt
5 add 1 4 3
6 next 1 4 5
"#;

    /// BTOR2 fixture with one `Op::Add` of width 8 — should NOT be
    /// UF-wrapped (width ≤ 32 = below the default-policy threshold).
    const R5B_NARROW_ADD_FIXTURE: &str = r#"
1 sort bitvec 8
2 const 1 0
3 const 1 1
4 state 1 narrow_cnt
5 add 1 4 3
6 next 1 4 5
"#;

    #[test]
    fn r5b_default_policy_wraps_op_mul() {
        // Default policy wraps every Op::Mul regardless of sidecar.
        let file = crate::adapter::btor2::parser::parse(R5B_DEFAULT_MUL_FIXTURE).expect("parse");
        let opts = AdapterOptions::default();
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            nids.contains(&5),
            "R.5b default policy must wrap Op::Mul (NID 5); got {nids:?}"
        );
    }

    #[test]
    fn r5b_default_policy_wraps_wide_add_above_threshold() {
        // 64-bit add > 32-bit threshold → wrapped under default.
        let file =
            crate::adapter::btor2::parser::parse(R5B_DEFAULT_WIDE_ADD_FIXTURE).expect("parse");
        let opts = AdapterOptions::default();
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            nids.contains(&5),
            "R.5b default policy must wrap 64-bit Op::Add (NID 5); got {nids:?}"
        );
    }

    #[test]
    fn r5b_default_policy_does_not_wrap_narrow_add() {
        // 8-bit add ≤ 32-bit threshold → NOT wrapped under default.
        let file = crate::adapter::btor2::parser::parse(R5B_NARROW_ADD_FIXTURE).expect("parse");
        let opts = AdapterOptions::default();
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            nids.is_empty(),
            "R.5b default policy must NOT wrap 8-bit Op::Add (below threshold); got {nids:?}"
        );
    }

    #[test]
    fn r5b_uf_unwrap_overrides_default_policy_on_mul() {
        // Op::Mul carrying a symbol the sidecar lists in uf_unwrap →
        // default-policy wrap is suppressed.
        let mul_with_symbol = r#"
1 sort bitvec 4
2 const 1 0010
3 state 1 acc
4 uext 1 3 0 raw_acc
5 mul 1 3 2
6 uext 1 5 0 small_mul
7 next 1 3 5
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "uf_unwrap": ["small_mul"]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let file = crate::adapter::btor2::parser::parse(mul_with_symbol).expect("parse");
        // The uext at NID 6 carries the symbol "small_mul"; it is
        // not the Mul itself (NID 5). uf_unwrap matches by symbol on
        // the carrying Op node. To force the Mul itself off the
        // default policy, the user should give the symbol to the
        // Mul's NID directly OR ensure no symbol-bearing alias maps
        // to it. The MVP semantics: uf_unwrap suppresses the
        // symbol-carrying Op (NID 6); NID 5 (the Mul) still gets
        // default-wrapped because its alias is what carries the
        // user-visible name. Test asserts that NID 6 (the uext alias)
        // is NOT in the wrap set (uf_unwrap honored).
        let nids = collect_uf_wrapped_nids(&file, &opts);
        assert!(
            !nids.contains(&6),
            "uf_unwrap must suppress the symbol-carrying Op (NID 6 / `small_mul`); got {nids:?}"
        );
    }

    // P1 #4 Phase 1 (Q2 = B) — observability-rich step primitive.
    // q' = en; g = q & en (combinational, symbol `g`); `en` is
    // constrained to be high.
    const STEP_OBSERVE_FIXTURE: &str =
        "1 sort bitvec 1\n2 state 1 q\n3 input 1 en\n4 and 1 2 3 g\n5 next 1 2 3\n6 constraint 3\n";

    #[test]
    fn p1_4_simulate_one_step_observe_reports_observed_and_admissible() {
        let file = crate::adapter::btor2::parser::parse(STEP_OBSERVE_FIXTURE).expect("parse");
        let st = |q: u128| std::collections::HashMap::from([("q".to_string(), q)]);
        let inp = |en: u128| std::collections::HashMap::from([("en".to_string(), en)]);

        // en=1: g = q&en = 1, next q = en = 1, constraint (en) holds.
        let out =
            simulate_one_step_observe(&file, &st(1), &inp(1), &["g".to_string()]).expect("step");
        assert_eq!(out.next_state.get("q"), Some(&1), "q' = en = 1");
        assert_eq!(out.observed.get("g"), Some(&1), "g = q & en = 1");
        assert!(out.admissible, "constraint `en` holds when en=1");

        // en=0: g = 1&0 = 0, next q = 0, constraint (en) VIOLATED.
        let out0 =
            simulate_one_step_observe(&file, &st(1), &inp(0), &["g".to_string()]).expect("step");
        assert_eq!(out0.next_state.get("q"), Some(&0), "q' = en = 0");
        assert_eq!(out0.observed.get("g"), Some(&0), "g = q & en = 0");
        assert!(!out0.admissible, "constraint `en` violated when en=0");

        // next_state matches the narrow simulate_one_step (no drift).
        let narrow = simulate_one_step(&file, &st(1), &inp(1)).expect("narrow step");
        assert_eq!(
            narrow, out.next_state,
            "observe variant must match simulate_one_step's next-state"
        );

        // An unresolvable observe name is simply omitted.
        let out_missing =
            simulate_one_step_observe(&file, &st(1), &inp(1), &["no_such_signal".to_string()])
                .expect("step");
        assert!(
            !out_missing.observed.contains_key("no_such_signal"),
            "unresolvable observe names are omitted"
        );
    }

    #[test]
    fn p1_4_phase2a_observe_follows_output_port_signal() {
        // P1 #4 Phase 2a — an OUTPUT-port name resolves to the value of its
        // referenced signal (matching `output_port_nids`), not the output
        // node itself. `myport` is an output of the combinational `q & en`
        // (nid 4), which carries no symbol of its own.
        let src = "1 sort bitvec 1\n2 state 1 q\n3 input 1 en\n4 and 1 2 3\n5 output 4 myport\n6 next 1 2 3\n";
        let file = crate::adapter::btor2::parser::parse(src).expect("parse");
        let st = |q: u128| std::collections::HashMap::from([("q".to_string(), q)]);
        let inp = |en: u128| std::collections::HashMap::from([("en".to_string(), en)]);

        let out = simulate_one_step_observe(&file, &st(1), &inp(1), &["myport".to_string()])
            .expect("step");
        assert_eq!(
            out.observed.get("myport"),
            Some(&1),
            "output port follows its signal `q & en` = 1"
        );
        let out0 = simulate_one_step_observe(&file, &st(1), &inp(0), &["myport".to_string()])
            .expect("step");
        assert_eq!(
            out0.observed.get("myport"),
            Some(&0),
            "output port follows its signal `q & en` = 0"
        );
    }

    #[test]
    fn r5b_simulate_one_step_with_uf_substitutes_zero_for_wrapped_op() {
        // Without UF wrap: cnt = 0 → simulate_one_step → next cnt = add(0, 3) = 3.
        // With UF wrap on mul_inst (the uext alias of add): next cnt = 0.
        let file = crate::adapter::btor2::parser::parse(R5B_UF_FIXTURE).expect("parse");
        let mut regs = std::collections::HashMap::new();
        regs.insert("cnt".to_string(), 0u128);
        let inputs = std::collections::HashMap::new();

        // No UF wrap: next cnt = 3.
        let no_uf = simulate_one_step(&file, &regs, &inputs).expect("step ok");
        assert_eq!(
            no_uf.get("cnt").copied(),
            Some(3),
            "without UF wrap, cnt should advance to add(0, 3) = 3"
        );

        // UF wrap on NID 7 (mul_inst): next cnt = 0 (the wrapped Op returns 0).
        let mut wrapped = std::collections::HashSet::new();
        wrapped.insert(7i64);
        let with_uf = simulate_one_step_with_uf(&file, &regs, &inputs, &wrapped).expect("step ok");
        assert_eq!(
            with_uf.get("cnt").copied(),
            Some(0),
            "with UF wrap on mul_inst, cnt should stay at 0 (UF substitutes zero)"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.4 (§Phase 9 §9.1) — reset-simulation seeding helper
    // ─────────────────────────────────────────────────────────────

    use crate::adapter::domain::{AbstractValue, AbstractionType, FieldDomain};
    use crate::adapter::verilator::RegisterValuation;

    fn enum_field(name: &str, variants: Vec<&str>) -> FieldDomain {
        FieldDomain {
            name: name.to_string(),
            abstraction: AbstractionType::EnumValues,
            bound: None,
            lower_bound: None,
            variants: Some(variants.into_iter().map(|s| s.to_string()).collect()),
            initial: AbstractValue::Counter(0),
        }
    }

    fn counter_field(name: &str, bound: i64) -> FieldDomain {
        FieldDomain {
            name: name.to_string(),
            abstraction: AbstractionType::BoundedCounter,
            bound: Some(bound),
            lower_bound: Some(0),
            variants: None,
            initial: AbstractValue::Counter(0),
        }
    }

    #[test]
    fn r_s2b_4_seeds_observed_value_when_sidecar_enum_lacks_it() {
        // signal `q` declared as enum with variants {Q_0: 0}.
        // Verilator reset simulation observed q=3.
        // R-S2b.4 must append (q_3, 3) to the value_map.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        let valuations = vec![RegisterValuation {
            name: "q".to_string(),
            value: 3,
        }];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (fd, value_map) = cell_domains.get(&42).expect("cell still present");
        assert_eq!(
            value_map,
            &vec![("Q_0".to_string(), 0), ("q_3".to_string(), 3)]
        );
        assert_eq!(
            fd.variants.as_deref(),
            Some(vec!["Q_0".to_string(), "q_3".to_string()].as_slice())
        );
    }

    #[test]
    fn r_s2b_4_no_op_when_observed_value_already_in_value_map() {
        // value_map already covers q=3; R-S2b.4 must skip
        // (additive, dedupes).
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (
                enum_field("q", vec!["Q_0", "Q_3"]),
                vec![("Q_0".to_string(), 0), ("Q_3".to_string(), 3)],
            ),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        let valuations = vec![RegisterValuation {
            name: "q".to_string(),
            value: 3,
        }];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        assert_eq!(
            value_map.len(),
            2,
            "no duplicate added; value_map={value_map:?}"
        );
    }

    #[test]
    fn r_s2b_4_skips_signals_without_matching_state_cell() {
        // Valuation mentions `unknown_reg`, which has no sidecar
        // entry / no symbol mapping. R-S2b.4 must silently skip
        // (the typical cause is a Yosys rename or a typo; R-S2b.6
        // will surface a CLI warning).
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        let valuations = vec![RegisterValuation {
            name: "unknown_reg".to_string(),
            value: 5,
        }];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        assert_eq!(
            value_map,
            &vec![("Q_0".to_string(), 0)],
            "unmatched valuation must be silently skipped"
        );
    }

    #[test]
    fn r_s2b_4_skips_non_enum_abstractions() {
        // signal `counter` declared as BoundedCounter. R-S2b.4
        // only widens EnumValues (mirrors R-S2a) — counters
        // already use a different value-set construction.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(7, (counter_field("counter", 15), Vec::new()));
        let nid_widths: [(Nid, u32); 1] = [(7, 4)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(7i64, "counter".to_string());
        let valuations = vec![RegisterValuation {
            name: "counter".to_string(),
            value: 3,
        }];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (fd, value_map) = cell_domains.get(&7).expect("cell still present");
        assert!(
            matches!(fd.abstraction, AbstractionType::BoundedCounter),
            "abstraction unchanged"
        );
        assert!(
            value_map.is_empty(),
            "BoundedCounter cells must not receive R-S2b.4 widening; got {value_map:?}"
        );
    }

    #[test]
    fn r_s2b_4_masks_value_to_cell_width() {
        // 4-bit signal observed as 0xFF (8 bits) — R-S2b.4 must
        // mask to the cell's width (4 bits), giving 0xF = 15.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            11,
            (enum_field("x", vec!["X_0"]), vec![("X_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(11, 4)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(11i64, "x".to_string());
        let valuations = vec![RegisterValuation {
            name: "x".to_string(),
            value: 0xFF,
        }];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (_, value_map) = cell_domains.get(&11).expect("cell still present");
        // Expect the masked value (15) to have been appended, not 255.
        assert!(value_map.iter().any(|(_, v)| *v == 15));
        assert!(value_map.iter().all(|(_, v)| *v != 0xFF));
    }

    #[test]
    fn r_s2b_4_handles_multiple_valuations_independently() {
        // Two signals each get one new value. Both should land in
        // their respective cells without interfering.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            100,
            (enum_field("a", vec!["A_0"]), vec![("A_0".to_string(), 0)]),
        );
        cell_domains.insert(
            200,
            (enum_field("b", vec!["B_0"]), vec![("B_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 2] = [(100, 8), (200, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(100i64, "a".to_string());
        symbols.insert(200i64, "b".to_string());
        let valuations = vec![
            RegisterValuation {
                name: "a".to_string(),
                value: 1,
            },
            RegisterValuation {
                name: "b".to_string(),
                value: 7,
            },
        ];

        apply_reset_simulation_seeding(&valuations, &nid_widths, &symbols, &mut cell_domains);

        let (_, value_map_a) = cell_domains.get(&100).expect("cell a");
        let (_, value_map_b) = cell_domains.get(&200).expect("cell b");
        assert!(value_map_a.iter().any(|(_, v)| *v == 1));
        assert!(value_map_b.iter().any(|(_, v)| *v == 7));
        assert!(
            !value_map_a.iter().any(|(_, v)| *v == 7),
            "valuation for `b` must not leak into cell `a`"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R-S6.5 tests — VCD trace-mining seeding helper
    // ─────────────────────────────────────────────────────────────

    use crate::adapter::vcd::VcdValueStats;

    fn stats(
        id: &str,
        heavy: Vec<(u64, usize)>,
        min: Option<u64>,
        max: Option<u64>,
    ) -> VcdValueStats {
        VcdValueStats {
            id: id.to_string(),
            heavy_hitters: heavy,
            min,
            max,
            indeterminate_count: 0,
        }
    }

    #[test]
    fn r_s6_5_seeds_top_n_heavy_hitters() {
        // Cell `q` has 3 distinct values in the trace (top 3 only;
        // R-S6.5 budget cap = 2). The 3rd value must be dropped.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        // Heavy hitters: value 1 (count 10), value 2 (count 5),
        // value 3 (count 2). Budget = 2 → only values 1 and 2
        // should be seeded.
        let signal_stats = vec![stats("q", vec![(1, 10), (2, 5), (3, 2)], Some(1), Some(3))];

        apply_vcd_seeding(
            &signal_stats,
            /* max_heavy_hitters_per_signal */ 2,
            /* seed_boundary_values */ false,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        // Pre-existing 0 + heavy-hitters 1, 2. No 3 (budget cap).
        let values: Vec<i64> = value_map.iter().map(|(_, v)| *v).collect();
        assert!(values.contains(&0));
        assert!(values.contains(&1));
        assert!(values.contains(&2));
        assert!(
            !values.contains(&3),
            "value 3 must not be seeded; got {value_map:?}"
        );
    }

    #[test]
    fn r_s6_5_seeds_boundary_values_when_enabled() {
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        // No heavy-hitters under the budget cap (cap = 0); rely on
        // boundary values to seed.
        let signal_stats = vec![stats("q", vec![(1, 1), (5, 1), (9, 1)], Some(1), Some(9))];

        apply_vcd_seeding(
            &signal_stats,
            /* max_heavy_hitters_per_signal */ 0,
            /* seed_boundary_values */ true,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        // 0 was pre-existing; 1 (min) and 9 (max) seeded.
        let values: Vec<i64> = value_map.iter().map(|(_, v)| *v).collect();
        assert!(values.contains(&0));
        assert!(values.contains(&1), "min must be seeded; got {value_map:?}");
        assert!(values.contains(&9), "max must be seeded; got {value_map:?}");
    }

    #[test]
    fn r_s6_5_skips_boundary_when_disabled() {
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        // Heavy-hitters list empty + boundary values 1 and 9
        // present in the stats. With cap=0 + boundary=false, no
        // seeding should occur.
        let signal_stats = vec![stats("q", vec![], Some(1), Some(9))];

        apply_vcd_seeding(
            &signal_stats,
            0,
            false,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        assert_eq!(value_map.len(), 1, "no values seeded; got {value_map:?}");
    }

    #[test]
    fn r_s6_5_skips_non_enum_abstractions() {
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(7, (counter_field("counter", 15), Vec::new()));
        let nid_widths: [(Nid, u32); 1] = [(7, 4)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(7i64, "counter".to_string());
        let signal_stats = vec![stats("counter", vec![(1, 10)], Some(1), Some(1))];

        apply_vcd_seeding(
            &signal_stats,
            10,
            true,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (fd, value_map) = cell_domains.get(&7).expect("cell still present");
        assert!(matches!(fd.abstraction, AbstractionType::BoundedCounter));
        assert!(
            value_map.is_empty(),
            "BoundedCounter cells must not receive R-S6.5 widening; got {value_map:?}"
        );
    }

    #[test]
    fn r_s6_5_skips_signals_without_matching_cell() {
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (enum_field("q", vec!["Q_0"]), vec![("Q_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        // Stats name = "unknown_reg" → no matching cell.
        let signal_stats = vec![stats("unknown_reg", vec![(5, 10)], Some(5), Some(5))];

        apply_vcd_seeding(
            &signal_stats,
            10,
            true,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        assert_eq!(value_map.len(), 1, "no values seeded; got {value_map:?}");
    }

    #[test]
    fn r_s6_5_dedupes_against_existing_value_map() {
        // Value 3 already in the value_map; the trace's heavy-
        // hitter top-N also includes 3 → must not duplicate.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            42,
            (
                enum_field("q", vec!["Q_0", "Q_3"]),
                vec![("Q_0".to_string(), 0), ("Q_3".to_string(), 3)],
            ),
        );
        let nid_widths: [(Nid, u32); 1] = [(42, 8)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(42i64, "q".to_string());
        let signal_stats = vec![stats("q", vec![(3, 10), (7, 3)], Some(3), Some(7))];

        apply_vcd_seeding(
            &signal_stats,
            10,
            true,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&42).expect("cell still present");
        // Expect: 0 (pre-existing) + 3 (pre-existing, deduped) + 7 (new).
        // No duplicate 3.
        let count_3 = value_map.iter().filter(|(_, v)| *v == 3).count();
        assert_eq!(count_3, 1, "value 3 must not duplicate; got {value_map:?}");
        let values: Vec<i64> = value_map.iter().map(|(_, v)| *v).collect();
        assert!(values.contains(&7));
    }

    #[test]
    fn r_s6_5_masks_values_to_cell_width() {
        // 4-bit cell with a trace value 0xFF — must mask to 0xF.
        let mut cell_domains: CellDomainMap = std::collections::HashMap::new();
        cell_domains.insert(
            11,
            (enum_field("x", vec!["X_0"]), vec![("X_0".to_string(), 0)]),
        );
        let nid_widths: [(Nid, u32); 1] = [(11, 4)];
        let mut symbols = std::collections::HashMap::new();
        symbols.insert(11i64, "x".to_string());
        let signal_stats = vec![stats("x", vec![(0xFF, 5)], Some(0xFF), Some(0xFF))];

        apply_vcd_seeding(
            &signal_stats,
            10,
            true,
            &nid_widths,
            &symbols,
            &mut cell_domains,
        );

        let (_, value_map) = cell_domains.get(&11).expect("cell still present");
        assert!(value_map.iter().any(|(_, v)| *v == 15));
        assert!(value_map.iter().all(|(_, v)| *v != 0xFF));
    }

    // ─────────────────────────────────────────────────────────────
    // R-S2b.6 tests — translate() integration + graceful fallback
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r_s2b_6_translate_no_op_when_sidecar_omits_simulate_reset() {
        // No simulate_reset block → R-S2b.6 must skip cleanly
        // without spawning Verilator and without emitting any
        // R-S2b.6 warning.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test"
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            sv_source_path: Some(std::path::PathBuf::from("/nonexistent.sv")),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert!(
            !out.warnings.iter().any(|w| w.message.contains("R-S2b.6")),
            "no R-S2b.6 warning when sidecar omits simulate_reset; got: {:?}",
            out.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn r_s2b_6_translate_no_op_when_sv_source_path_missing() {
        // simulate_reset present but sv_source_path=None → must
        // skip silently (no SV to feed Verilator).
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "simulate_reset": {
                "clock_signal": "clk",
                "reset_signal": "rst",
                "reset_asserted": 1,
                "hold_cycles": 1,
                "observe_registers": ["q"]
            }
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            // sv_source_path intentionally omitted.
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert!(
            !out.warnings.iter().any(|w| w.message.contains("R-S2b.6")),
            "no R-S2b.6 warning when sv_source_path is absent; got: {:?}",
            out.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn r_s2b_6_translate_emits_warning_when_verilator_absent() {
        // simulate_reset + sv_source_path both set, but Verilator
        // is forced absent via env var. Translate must succeed AND
        // emit the R-S2b.6 "Verilator not discoverable" warning
        // (graceful fallback).
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "simulate_reset": {
                "clock_signal": "clk",
                "reset_signal": "rst",
                "reset_asserted": 1,
                "hold_cycles": 1,
                "observe_registers": ["q"]
            }
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            sv_source_path: Some(std::path::PathBuf::from("/nonexistent.sv")),
            ..Default::default()
        };

        // SAFETY: required for env-var manipulation in tests; the
        // value is restored before the test returns.
        let original = std::env::var("MUNUNU_VERILATOR_PATH").ok();
        unsafe {
            std::env::set_var(
                "MUNUNU_VERILATOR_PATH",
                "/nonexistent/path/to/verilator/binary",
            );
        }
        let out = translate(src, &opts).expect("translate");
        unsafe {
            match original {
                Some(v) => std::env::set_var("MUNUNU_VERILATOR_PATH", v),
                None => std::env::remove_var("MUNUNU_VERILATOR_PATH"),
            }
        }

        let r_s2b_6_warning = out
            .warnings
            .iter()
            .find(|w| w.message.contains("R-S2b.6"))
            .expect("R-S2b.6 must emit a warning when Verilator is absent");
        assert!(
            r_s2b_6_warning.message.contains("not discoverable")
                || r_s2b_6_warning.message.contains("Verilator"),
            "warning must mention Verilator absence; got: {}",
            r_s2b_6_warning.message
        );
    }

    #[test]
    fn r_s2b_6_translate_no_op_when_sidecar_malformed() {
        // sidecar_json is non-empty but doesn't parse as
        // SvAnnotation. Translate must continue (the unrelated
        // BTOR2 lifter doesn't depend on the sidecar for the
        // minimal fixture below) and not emit a panic / R-S2b.6
        // warning.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let opts = AdapterOptions {
            sidecar_json: Some("{ not valid json at all".to_string()),
            sv_source_path: Some(std::path::PathBuf::from("/nonexistent.sv")),
            ..Default::default()
        };
        // Translate may fail for unrelated reasons (the malformed
        // sidecar might trip some other parser), but R-S2b.6's
        // labeled-block must break early, not propagate the JSON
        // error. Either way: no R-S2b.6 warning, no panic.
        let result = translate(src, &opts);
        if let Ok(out) = result {
            assert!(
                !out.warnings.iter().any(|w| w.message.contains("R-S2b.6")),
                "no R-S2b.6 warning on malformed sidecar"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────
    // R-S6.6 tests — translate() integration + graceful fallback
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r_s6_6_translate_no_op_when_sidecar_omits_vcd_traces() {
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test"
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        assert!(
            !out.warnings.iter().any(|w| w.message.contains("R-S6.6")),
            "no R-S6.6 warning when sidecar omits vcd_traces; got: {:?}",
            out.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn r_s6_6_translate_emits_warning_for_missing_trace_file() {
        // Sidecar declares a vcd_traces entry but the path doesn't
        // exist; R-S6.6 must emit a warning + continue cleanly.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "vcd_traces": [
                { "path": "/nonexistent/trace.vcd" }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        let warn = out
            .warnings
            .iter()
            .find(|w| w.message.contains("R-S6.6"))
            .expect("R-S6.6 must emit a warning when trace file is missing");
        assert!(
            warn.message.contains("failed to read") || warn.message.contains("nonexistent"),
            "warning must mention the read failure; got: {}",
            warn.message
        );
    }

    #[test]
    fn r_s6_6_translate_emits_warning_for_relative_path_without_sidecar_path() {
        // Relative path + sidecar_path=None → R-S6.6 must emit a
        // warning (no resolution context) + continue.
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "vcd_traces": [
                { "path": "relative/trace.vcd" }
            ]
        });
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar.to_string()),
            sidecar_path: None,
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        let warn = out
            .warnings
            .iter()
            .find(|w| w.message.contains("R-S6.6"))
            .expect("R-S6.6 must emit a warning when relative path lacks resolution context");
        assert!(
            warn.message.contains("relative") || warn.message.contains("sidecar_path"),
            "warning must mention the resolution-context issue; got: {}",
            warn.message
        );
    }

    #[test]
    fn r_s6_6_translate_consumes_real_trace_and_emits_summary_warning() {
        // Happy path: write a small VCD to a tempdir, point a
        // sidecar at it, and assert that R-S6.6 emits the summary
        // warning naming the trace. The trace covers signal `q`
        // (which matches the BTOR2 state cell's symbol) with
        // values 0 and 1.
        let tmp = std::env::temp_dir().join(format!("mununu-r-s6-6-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir tempdir");
        let trace_path = tmp.join("q.vcd");
        std::fs::write(
            &trace_path,
            "$timescale 1ns $end\n\
             $scope module top $end\n\
             $var wire 1 ! q $end\n\
             $upscope $end\n\
             $enddefinitions $end\n\
             #0\n\
             0 !\n\
             #1\n\
             1 !\n",
        )
        .expect("write vcd");
        let sidecar_path = tmp.join("test.mununu.json");
        let src = r#"
1 sort bitvec 1
2 zero 1
3 state 1 q
4 init 1 3 2
5 next 1 3 3
"#;
        let sidecar_json = serde_json::json!({
            "$schema": "mununu_sv_annotation_v1",
            "module": "test",
            "vcd_traces": [
                { "path": "q.vcd" }
            ]
        });
        std::fs::write(&sidecar_path, sidecar_json.to_string()).expect("write sidecar");
        let opts = AdapterOptions {
            sidecar_json: Some(sidecar_json.to_string()),
            sidecar_path: Some(sidecar_path.clone()),
            ..Default::default()
        };
        let out = translate(src, &opts).expect("translate");
        // Cleanup before assertions so failures don't leak the dir.
        let _ = std::fs::remove_dir_all(&tmp);
        let warn = out
            .warnings
            .iter()
            .find(|w| w.message.contains("R-S6.6"))
            .expect("R-S6.6 must emit a summary warning for a successfully mined trace");
        assert!(
            warn.message.contains("mined"),
            "warning must mention the mining summary; got: {}",
            warn.message
        );
    }
}
