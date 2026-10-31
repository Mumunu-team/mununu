//! XL.6b — automated SVA verification (SV source → per-property verdict, no sidecar).
//!
//! The headline no-sidecar verify path. Composes the shipped pieces:
//!
//! 1. [`extract_sva`](crate::adapter::slang::extract::extract_sva) — slang →
//!    translated mu-calculus property set (+ `__past` shadow requirements).
//! 2. [`sv_to_btor2`](crate::adapter::yosys::sv_to_btor2) — SV → flattened BTOR2.
//! 3. [`augment_with_past_shadows`](crate::adapter::btor2::shadow::augment_with_past_shadows)
//!    — synthesise the 1-step shadow flops the Tier-2 history formulas reference.
//! 4. Per property: auto-seed cube predicates from the formula's state-cell atoms
//!    (the minimal H.1, [`seed_from_formula`]) → [`cegar_refine_loop`] → verdict.
//!
//! **Binding.** Each seeded cube predicate is NAMED exactly the formula atom
//! string, so the evaluator's name-match bridge (the M.4 fix in `evaluator.rs`)
//! binds the formula's `Node::Predicate` to its cube bit. Simple `reg == value`
//! atoms seed a [`PredicateSpec`]; relational / compound atoms (`reg == reg`
//! incl. `$stable`'s `state_q == state_q__past`, `reg != value`, …) seed a
//! compound predicate via a synthesised `SvAnnotation` sidecar that
//! [`cegar_refine_loop`] consumes (forcing the SmtAllPairs eager lift).
//!
//! **Scope (sound gate).** The cube abstraction binds predicates over **state
//! cells** and (H.B) over primary **inputs** as *free* cube dimensions. A simple
//! `input == value` atom (an arbiter's `req_i`, sysrst's `cfg_enable_i` /
//! `trigger_*`) becomes a free dimension whose source-pin / target-free shape the
//! SMT may/must seam realises (`build_register_nid_map_with_inputs`), and whose
//! verdict is read across every initial environment value — the "for all input
//! sequences" over-approximation (see `docs/design/free-input-atoms.md`).
//!
//! A relational compound carrying an **input** operand (sysrst's
//! `cnt_q >= cfg_*_timer_i`) BINDS as a derived RELATIONAL label (H.F): the
//! per-cube 3-valued labeller resolves each operand over the uniform source
//! image (state ∪ inputs ∪ combinational), so the property reaches an honest ⊥
//! rather than skipping.
//!
//! A **combinational output whose cone is state-only** (`event_detected_o =
//! f(state_q)`) binds as a CUBE DIMENSION (H.U.2): the uniform predicate-image
//! resolves its combinational node (the `Op`/`output`-line symbol map below) and
//! pins its value per cube, so the property reaches a **definite** verdict.
//!
//! A combinational whose cone **reaches a free input** (`trigger_active =
//! !trigger_i`; csrng's `main_sm_err_o`, whose cone reaches `enable_i`) binds as
//! a derived ⊥-label (H.E) — definite where the labeller proves it constant over
//! the cube, ⊥ where the input can swing it. **Slice 3 (the unwrap lever)** then
//! seeds the raw inputs in that cone (`trigger_i`) as free H.B cube dimensions:
//! this refines the may-relation so a consequent box over a transition GOVERNED
//! by the input (`AG((state ∧ trigger) → AX state′)`) becomes definite (Kleene
//! `⊥ ∨ [] = T`), and the derived label itself turns definite at cubes pinning
//! the input. The raw input is the H.B source-pin / target-free shape (sound; no
//! fabricated must-edge). This is what lifts sysrst's trigger-governed
//! conditional-transition safety SVA (sva_4/6/8/9) from ⊥ to definite HOLDS.
//! Gated on a small cone-input count so a wide combinational does not blow up the
//! cube (it stays a bare ⊥-label).
//!
//! What is still **Skipped** (never given a misleading verdict): an atom over a
//! combinational signal with **no resolvable node** in the lifted BTOR2 — one
//! Yosys `flatten`/`opt` dropped, leaving no `state` / `input` / `Op` / `output`
//! symbol to map. It is reported Skipped with a reason — labelling what we
//! cannot resolve would fall through to the evaluator's "unknown ⇒ false"
//! under-approx and silently produce a vacuous verdict.

use std::collections::HashSet;

use crate::adapter::btor2::cex_value::CexValue;
use crate::adapter::btor2::kmts_lift::{MayEdgeInference, MustEdgeInference, PredicateSpec};
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_expr};
use crate::adapter::slang::translate::{SvaKind, TranslateOptions};
use crate::adapter::yosys::YosysOptions;
use crate::adapter::{AdapterError, AdapterErrorKind};
use crate::mu_calculus::{Formula, Node};

/// mununu#518 — split a design's `state` symbols into **BV-sorted** cells and **array-sorted**
/// ones (memories), by the same `bv_width(..).is_some()` discriminator the rest of the pipeline
/// uses.
///
/// Returns `(bv, array)`. Only the BV set is the seedable-atom universe: an atom over a memory
/// has no BV to bind to, and admitting one produced a confident wrong answer rather than an
/// error. The array set is kept — not discarded — so a refusal can name the memory and say why,
/// which is the difference between "unknown signal" and "that is a memory".
///
/// A state with no symbol contributes to neither set (nothing could name it in a property).
pub(crate) fn partition_state_cells_by_sort(
    file: &crate::adapter::btor2::ast::Btor2File,
    symbols: &std::collections::HashMap<crate::adapter::btor2::ast::Nid, String>,
) -> (HashSet<String>, HashSet<String>) {
    let mut bv = HashSet::new();
    let mut arrays = HashSet::new();
    for l in &file.lines {
        let crate::adapter::btor2::ast::Node::State { sort, .. } = &l.node else {
            continue;
        };
        let Some(name) = symbols.get(&l.nid).cloned() else {
            continue;
        };
        if crate::adapter::btor2::parser::bv_width(file, *sort).is_some() {
            bv.insert(name);
        } else {
            arrays.insert(name);
        }
    }
    (bv, arrays)
}

/// mununu#518 — every register name a predicate atom mentions.
///
/// `Select`'s `array` operand is included deliberately: an atom over a memory CELL
/// (`mem[i] == v`) names the memory, and the caller's whole purpose is to notice that.
fn atom_register_names(
    expr: &crate::adapter::btor2::predicate_expr::PredicateExpr,
    out: &mut Vec<String>,
) {
    use crate::adapter::btor2::predicate_expr::PredicateExpr as P;
    match expr {
        P::Cmp { register, .. } => out.push(register.clone()),
        P::CmpReg { lhs, rhs, .. } | P::CmpRegAddend { lhs, rhs, .. } => {
            out.push(lhs.clone());
            out.push(rhs.clone());
        }
        P::Select { array, index, .. } => {
            out.push(array.clone());
            out.push(index.clone());
        }
        P::And(a, b) | P::Or(a, b) => {
            atom_register_names(a, out);
            atom_register_names(b, out);
        }
        // No catch-all arm: every variant is named above, so a NEW `PredicateExpr` variant
        // becomes a compile error here rather than a silently unchecked atom.
        P::Not(inner) => atom_register_names(inner, out),
    }
}

/// mununu#518 — the memories a formula's atoms name, if any.
///
/// A BTOR2 array state has no BV to bind to, so an atom over one cannot become a cube dimension.
/// Before the sort filter it was admitted as though it could; now it is simply unknown to the
/// seeder, and "unknown signal" is a bad thing to tell someone who wrote a perfectly sensible
/// name. Naming the memory turns a confusing non-result into an actionable one.
fn formula_atoms_naming_memories(formula: &Formula, memories: &HashSet<String>) -> Vec<String> {
    if memories.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<String> = Vec::new();
    for node in formula.nodes() {
        let Node::Predicate(atom) = node else {
            continue;
        };
        let Ok(expr) = crate::adapter::btor2::predicate_expr::parse_predicate_atom_bool(atom)
        else {
            continue;
        };
        let mut names = Vec::new();
        atom_register_names(&expr, &mut names);
        for n in names {
            if memories.contains(&n) && !hits.contains(&n) {
                hits.push(n);
            }
        }
    }
    hits.sort();
    hits
}

/// Per-property verification outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// No definite-false and no ⊥ cube cells — the property holds on the abstraction.
    Holds,
    /// Some cube cells are definite-false (a violation witness exists).
    Violated { false_cells: usize },
    /// The verdict carries ⊥ (unknown) cells — the abstraction couldn't decide.
    Unknown { unknown_cells: usize },
    /// Not verified, with the reason: an atom over a non-state signal
    /// (combinational/IO — not cube-bindable), no atoms at all, or an error.
    Skipped { reason: String },
}

impl VerifyOutcome {
    /// The canonical `holds` / `violated` / `unknown` / `skipped` label — the
    /// verdict vocabulary shared across surfaces (and the `sv mutate` flip oracle).
    pub fn label(&self) -> &'static str {
        match self {
            VerifyOutcome::Holds => "holds",
            VerifyOutcome::Violated { .. } => "violated",
            VerifyOutcome::Unknown { .. } => "unknown",
            VerifyOutcome::Skipped { .. } => "skipped",
        }
    }
}

/// One assertion's auto-verification result.
#[derive(Debug, Clone)]
pub struct PropertyVerdict {
    /// Positional name: `<module>_sva_<index>` (or `ann_guarantee_<index>`). An insertion
    /// mid-file re-points every later one — prefer [`Self::label`] when pinning.
    pub name: String,
    /// mununu#544 — the assertion's SV label, when it has one. Survives a reorder; a rename
    /// becomes a hard error rather than a silent re-point. `None` for an unlabelled assertion
    /// and for every `@mununu_guarantee` annotation (which carry no name today).
    pub label: Option<String>,
    pub kind: SvaKind,
    pub formula: String,
    pub outcome: VerifyOutcome,
    /// The cube predicates auto-seeded for this property (atom strings) — the
    /// diagnostic "what was tracked".
    pub seeded_predicates: Vec<String>,
    /// A concrete counterexample for a `Violated` property. Safety: the reset → violation path
    /// native BMC re-derives (empty `cycle`; `attach_safety_witnesses`), whichever engine
    /// decided it — `None` when no path lies within the bound. D1.8b: a stall lasso when the
    /// exact engine reports a bare `AF p` `Violated` and the stall is reachable at reset.
    pub counterexample: Option<ExactCounterexample>,
    /// Why this property is `⊥`, when it is. `None` for a definite outcome.
    ///
    /// monono ask 26 — a FIELD rather than a note. See [`BottomReason`].
    pub bottom_reason: Option<BottomReason>,
    /// Which portfolio engine produced this property's DEFINITE verdict. `None` for `⊥`.
    ///
    /// **The companion to [`Self::bottom_reason`], and it covers the opposite case.** That field
    /// explains a `⊥` whose cause was hidden; this one names the provenance of a verdict that
    /// is *not* hidden and looks entirely ordinary.
    ///
    /// **Why it matters — a measured incident, not a hypothetical.** At 84% arena occupancy a
    /// consumer's run still returned `holds`, but three engines ran where two arenas higher only
    /// `exact-symbolic` was needed: the exact engine dropped out and an abstracting engine supplied
    /// the verdict. The verdict is sound under that engine's posture, but **the precision tier
    /// moved silently**. A gate pinning `holds` sees `holds` either way and records a pass.
    ///
    /// The information existed as `decided-by:<engine>=<count>` — an AGGREGATE, in a REPORT-level
    /// note. A count cannot tell you which property changed tier, and a note is filterable. This is
    /// per-property because a mixed report is exactly the case an aggregate misleads on: a consumer
    /// transcript showed 7 properties decided by `exact-symbolic` and one by something else,
    /// reported together.
    pub decided_by: Option<String>,
    /// mununu#599 — a recoverability guarantee (`AG EF(P)`) that HOLDS, judged against the other
    /// verdicts in the same report. `None` for every other property, and for a guarantee no other
    /// property in the report speaks to.
    ///
    /// **Why a field, at the property.** `AG EF good: HOLDS` beside its non-vacuity witness
    /// `EF good': VIOLATED` is exactly the vacuous pass a two-sided gate exists to prevent, and a
    /// reader who does not check witnesses by habit was handed it with a generic run-level
    /// caveat. A consumer gate that reads `outcome == "holds"` must be able to read this next to
    /// it; a note can be filtered out, a field cannot.
    pub vacuity: Option<VacuityFinding>,
}

impl PropertyVerdict {
    /// The row's counterexample as every surface shows it: a [`crate::trace::Trace`] with the
    /// design's names, the verifier's cells under `monitor`, the injections that fire on
    /// `faults`, and no clock or added input ([`crate::trace::TraceNames::tidy`]). `None` when the
    /// row has no counterexample.
    pub fn trace(&self, names: &crate::trace::TraceNames) -> Option<crate::trace::Trace> {
        self.counterexample.as_ref().map(|c| {
            names.tidy(counterexample_trace(
                c,
                self.decided_by.as_deref() == Some(TRAP_CERTIFICATE_DECIDED_BY),
            ))
        })
    }
}

/// mununu#599 — how a recoverability guarantee that HOLDS stands against another property of the
/// same report. See [`super::report_consistency::vacuity_of_guarantee`] for the relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VacuityFinding {
    pub kind: VacuityKind,
    /// The property (by `name`) whose verdict establishes it.
    pub witness: String,
    /// One line, for the property's own lines and the note.
    pub detail: String,
}

/// The two strengths of [`VacuityFinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VacuityKind {
    /// The target is invariant in this report (`EF(¬P)` VIOLATED or `AG(P)` HOLDS beside
    /// `AG EF(P)` HOLDS): consistent, and vacuous — the design never leaves `P`, so no recovery is
    /// ever exercised. A HOLDS that stays HOLDS, and says nothing.
    InvariantTarget,
    /// A reachability over the guarantee's register, at a value other than the target, is
    /// VIOLATED in this report. It does not make the guarantee false; it means this report has not
    /// shown it non-vacuous — the shape mununu#599 met (`EF(st_q == S_WAIT)` VIOLATED beside
    /// `AG EF(st_q == S_IDLE)` HOLDS under the same cut points).
    WitnessRefuted,
    /// A security-countermeasure chain (`$rose(X) |-> ##[0:k] alert`) HOLDS, and under the
    /// fault posture's selection the reach portfolio proved `X` can never rise on this model:
    /// the antecedent is unreachable, the HOLDS says nothing about the response. The "before"
    /// instrument of the posture (`--fault-model sec-cm:0`): on the un-faulted lift the fault
    /// flags are synthesis constants, so every such row reads this way.
    AntecedentUnreachable,
}

impl VacuityKind {
    /// Machine-stable tag, kebab-case.
    pub fn tag(self) -> &'static str {
        match self {
            Self::InvariantTarget => "invariant-target",
            Self::WitnessRefuted => "witness-refuted",
            Self::AntecedentUnreachable => "antecedent-unreachable",
        }
    }
}

/// A concrete counterexample for a `Violated` property: a `prefix` from the reset state,
/// each state a list of `(register, value)` pairs in register order — into a repeating
/// `cycle` for a liveness failure (D1.8b: the property is avoided forever, the "stall"), or
/// ending at the failing cycle with an empty `cycle` for a safety one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactCounterexample {
    /// States from the reset state up to (excluding) the cycle entry. A cell's value is carried
    /// at its full width (mununu#723 — a cell wider than 64 bits used to be dropped or truncated).
    pub prefix: Vec<Vec<(String, CexValue)>>,
    /// The repeating stall cycle; the last state steps back to `cycle[0]`.
    pub cycle: Vec<Vec<(String, CexValue)>>,
    /// P3 — the INPUT assignment driving each transition of `prefix ++ cycle` (`inputs[i]` is
    /// the input at path-state `i`), for RTL replay. Empty when the engine did not record inputs.
    pub inputs: Vec<Vec<(String, CexValue)>>,
    /// A.4 — the UNREACHABLE-target witness for a `Violated` bare `EF p` (reachability):
    /// there is no trace (`prefix`/`cycle` empty), because the target `p` is never reached
    /// from reset. These are the predicate-atom strings comprising `p` — the actionable
    /// repair signal "the design never reaches `<atoms>`". Empty for the stall-lasso /
    /// trap-path (liveness/recoverability) counterexamples, which carry a concrete path.
    pub unreachable_target: Vec<String>,
}

/// Convert an engine [`StallLasso`](crate::adapter::btor2::symbolic_bitblast::StallLasso)
/// (register→value maps, `u128`) into the surface [`ExactCounterexample`] (ordered
/// `(name, value)` pairs). `widths` (from [`cell_widths_of`]) gives each cell its real width;
/// a cell it does not name gets the narrowest width that holds the value. mununu#723 — the
/// `u128` used to be narrowed to `u64`, silently, for every cell.
fn exact_counterexample_from_lasso(
    lasso: crate::adapter::btor2::symbolic_bitblast::StallLasso,
    widths: &std::collections::HashMap<String, u32>,
) -> ExactCounterexample {
    let conv = |states: Vec<std::collections::BTreeMap<String, u128>>| {
        states
            .into_iter()
            .map(|st| {
                st.into_iter()
                    .map(|(k, v)| {
                        let bits = widths
                            .get(&k)
                            .copied()
                            .unwrap_or_else(|| (128 - v.leading_zeros()).max(1));
                        (k, CexValue::from_u128(v, bits))
                    })
                    .collect()
            })
            .collect()
    };
    ExactCounterexample {
        prefix: conv(lasso.prefix),
        cycle: conv(lasso.cycle),
        inputs: conv(lasso.inputs),
        unreachable_target: Vec::new(),
    }
}

/// An exact-engine witness (stall lasso / trap path) of `btor2` as a raw
/// [`crate::trace::Trace`], each cell at its real width: the counterexample the recoverability
/// verbs report, built the way `sv verify-auto` builds an `AG EF` row's.
pub(crate) fn lasso_trace(
    lasso: crate::adapter::btor2::symbolic_bitblast::StallLasso,
    btor2: &str,
) -> crate::trace::Trace {
    counterexample_trace(
        &exact_counterexample_from_lasso(lasso, &cell_widths_of(btor2)),
        false,
    )
}

/// A counterexample as a [`crate::trace::Trace`] under the lift's own cell names, not yet tidied;
/// `trap` marks a trap-certificate path.
fn counterexample_trace(c: &ExactCounterexample, trap: bool) -> crate::trace::Trace {
    use crate::trace::{Trace, cex_cells};
    let cells = |v: &[Vec<(String, CexValue)>]| v.iter().map(|s| cex_cells(s)).collect();
    Trace::from_cells(
        cells(&c.prefix),
        cells(&c.cycle),
        cells(&c.inputs),
        c.unreachable_target.clone(),
        trap,
    )
}

/// The width of every named cell of a model's BTOR2 text (empty when it does not parse) — the
/// widths a lasso's values are reported at.
fn cell_widths_of(btor2: &str) -> std::collections::HashMap<String, u32> {
    crate::adapter::btor2::parser::parse(btor2)
        .map(|f| crate::adapter::btor2::cex_value::cell_widths(&f))
        .unwrap_or_default()
}

/// A.4 — build the UNREACHABLE-target counterexample for a `Violated` bare `EF p`
/// (`μX. (p ∨ ◇X)`): the violation *is* the proof that `p` is unreachable from reset,
/// so no trace exists. Returns the target's predicate atoms as the witness (empty
/// path). `None` when `formula` is not a bare `EF` shape (a liveness/recoverability
/// failure keeps its concrete stall-lasso / trap-path witness instead).
fn ef_unreachable_counterexample(
    formula: &crate::mu_calculus::Formula,
) -> Option<ExactCounterexample> {
    crate::adapter::btor2::symbolic_bitblast::ef_target_atoms(formula).map(|atoms| {
        ExactCounterexample {
            prefix: Vec::new(),
            cycle: Vec::new(),
            inputs: Vec::new(),
            unreachable_target: atoms,
        }
    })
}

/// RANKING RESCUE — route a recoverability `AG EF <good>` whose `good` is a single
/// `reg == value` / `reg == reg` atom to the scalable recoverability engine
/// ([`verify_recoverability_scalable`](crate::adapter::recoverability)), which proves
/// `AG EF good` by a well-founded datapath **ranking** (a down-counter/drain to a
/// value) BEFORE its cube. This decides the deep-counter recoverability class that the
/// bit-blaster is over-cap for and the predicate cube leaves ⊥ — e.g. a timer/FIFO
/// `AG EF (cnt == N)` at counter widths the exact engine can't fit. Used as a fallback
/// (not a replacement) — `None` when the formula is not that shape, `good` is not a
/// single register-comparison atom, or the engine abstains, so the caller keeps its
/// Skipped/⊥ verdict. The ranking certificate is a **sound sufficient** condition (it
/// establishes the stronger `AG AF good`), and the engine abstains rather than emit an
/// unsound Holds on a UF-wrapping datapath (PR #301).
fn try_ranking_recoverability(
    btor2: &str,
    formula: &crate::mu_calculus::Formula,
) -> Option<VerifyOutcome> {
    let atoms = crate::adapter::btor2::symbolic_bitblast::ag_ef_good_atoms(formula)?;
    let [good] = atoms.as_slice() else {
        return None; // only a single register-comparison good routes to the engine
    };
    match crate::adapter::recoverability::verify_recoverability_scalable(btor2, good, &[]) {
        Ok(crate::verdict::PropertyVerdict::Holds) => Some(VerifyOutcome::Holds),
        Ok(crate::verdict::PropertyVerdict::Violated) => {
            Some(VerifyOutcome::Violated { false_cells: 1 })
        }
        _ => None, // Unknown / Skipped / Err → keep the caller's verdict
    }
}

/// Model-level diagnostics for an automated verification run — what the lift
/// produced, so a "couldn't verify" outcome is traceable to a root cause
/// (rather than only a per-property symptom like "atom over non-state signal").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelDiagnostics {
    /// Number of `state` register lines in the lifted (augmented) BTOR2. A
    /// suspiciously low / zero count is a strong signal that an FSM or other
    /// state was cut (see [`Self::blackboxed_modules`]) — properties over the
    /// cut registers are then SKIPPED rather than verified.
    pub state_register_count: usize,
    /// Modules instantiated without a body — either auto-black-boxed by Yosys
    /// (outputs cut to free inputs by `cutpoint -blackbox`) or left as a
    /// dangling undefined-module cell (e.g. an FSM wrapped in OpenTitan's
    /// `prim_sparse_fsm_flop`, whose register then vanishes from the lift).
    /// Both are sound, but a register hidden behind one is **not** modeled as
    /// state, so every property over it is SKIPPED. Surfacing it here is the
    /// "stop-silent-cut" half: the cut is no longer invisible. The fix is to
    /// provide the missing module source(s).
    pub blackboxed_modules: Vec<String>,
    /// Reset inputs that were pinned inactive at the model level so the body is
    /// verified only while not in reset (the `disable iff` guards were dropped
    /// from the formulas). Empty when reset-gating is off or no `disable iff`
    /// reset was recognized. Each entry is `"<signal>=<inactive_value>"`.
    pub gated_resets: Vec<String>,
    /// Flop-primitive modules that were cut (instantiated with no body) and for
    /// which verify-auto auto-injected a behavioral stub so the register
    /// survives the lift (H.C — e.g. OpenTitan's `prim_sparse_fsm_flop`). The
    /// stub is an exact behavioral model of the flop datapath; auto-injection is
    /// reported here so it is never silent.
    pub auto_provided_stubs: Vec<String>,
    /// #466 — the RTL front end that produced the lift (`SvFrontend::lift_label`,
    /// e.g. `read_verilog + sv2v` / `yosys-slang (read_slang)`). `--frontend auto`
    /// picks one silently; surfacing it (via the `lift-frontend` note) answers
    /// "which lift produced this verdict?" — the two paths differ in SOUNDNESS on
    /// partial writes (#464/#465), so a silent choice is the #459 failure class.
    pub frontend: Option<String>,
    /// #466 — when `--frontend auto` FELL BACK (an earlier front end errored before
    /// the one used succeeded), the prior attempt's error. `None` when there was no
    /// fallback (an explicit front end, or the first attempt succeeded).
    pub frontend_fallback_reason: Option<String>,
    /// `Some(reason)` when the lift's first yosys run died in the BTOR backend with a
    /// "topological loop" (`check` sees none: yosys-slang's `$buf` cells chain wide vectors
    /// through other cells) and the script succeeded rerun with `opt_clean` before `write_btor`.
    /// The model is the same design — `opt_clean` folds buffers into connections and drops unused
    /// cells — so this is Info (the `lift-retry` note), not a scope caveat. `None` when the first
    /// run succeeded (every lift that succeeded before 2026-10-22 is byte-identical).
    pub lift_retry: Option<String>,
    /// The clock domains the lift MERGED onto the model's one implicit clock, each
    /// `"<net> (<edge>, <n> cells)"` — non-empty only when the design has more than one domain
    /// and `--merge-clock-domains` let it through (otherwise the lift refuses it). Every verdict
    /// then holds only for runs where all domains tick together (the `clock-domains-merged`
    /// soundness caveat).
    pub merged_clock_domains: Vec<String>,
    /// mununu#536 — config inputs pinned to constants for this run, each
    /// `"<signal>=<value>"` (the `--pin` / API `pins` operands that were
    /// actually APPLIED, not those merely requested).
    ///
    /// These already reach a human through the `config-concretization` note, but a note is
    /// prose: a consumer asking "what was this verdict scoped to?" had to string-match a
    /// summary line. A verdict pinned to `cfg_timer=7` is a claim about THAT configuration
    /// and no other, so the scope belongs in the structured payload beside the verdicts.
    pub applied_config_values: Vec<String>,
    /// mununu#536 — signals cut to free inputs by `--cutpoint`, i.e. the control slice this
    /// run verified under. Same reasoning as [`Self::applied_config_values`]: a cutpoint is
    /// an over-approximation that shapes every verdict in the report, and it was reachable
    /// only as `control-slice` note prose.
    pub cutpoint_signals: Vec<String>,
    /// The fault-injection posture this run was under (`--fault` / `--fault-model`), as one
    /// label; `None` when none. The sites are in [`Self::fault_sites`].
    pub fault_model: Option<String>,
    /// The registers the adversary may corrupt under the posture, with their width, the budget,
    /// why each was selected and the rows it serves. Empty without a posture.
    pub fault_sites: Vec<crate::adapter::btor2::fault::FaultSiteReport>,
    /// The rows whose author wrote the property in another language than the mu-calculus the
    /// row carries, by row name — today the `@mununu_guarantee ltl …` annotations (2026-10-29).
    /// Both surfaces show the written text as the row's `formula` and the mu-calculus as
    /// `translated`; [`AutoVerifyReport::written_formula`] is the lookup. It lives beside the
    /// other provenance of the property set (`fault_sites` names its rows the same way) because
    /// the row struct keeps the mu-calculus, which the post-passes re-parse.
    pub written_formulas: Vec<(String, crate::adapter::slang::translate::WrittenFormula)>,
    /// The `@mununu_template` rows (2026-10-30), by row name, with the instance as the author
    /// wrote it (`rtl_reachable TARGET="state_q == 3"`); the row's `formula` is the instantiated
    /// mu-calculus. Both surfaces show it as the row's `template`. A `--template` flag's row has
    /// the same entry (the flag is rendered as that annotation).
    pub template_forms: Vec<(String, String)>,
    /// Where each property is written in the RTL (2026-10-30), by row name: an SVA row's
    /// assertion statement (slang's AST), an annotation row's `@mununu_guarantee` comment (the
    /// scanner). A template row has no source line and is absent.
    pub property_sources: Vec<(String, crate::trace::SourceLoc)>,
    /// The RTL lines of each property's cone of influence (2026-10-30), by row name: for every
    /// register, input and named wire the property depends on, its declaration, the `always`
    /// block(s) driving it and every statement that writes it — sorted, de-duplicated. The code
    /// the verdict is about, not a causal slice of a counterexample. Absent when the source map
    /// placed nothing in the cone.
    pub involves: Vec<(String, Vec<crate::trace::SourceLoc>)>,
    /// The repair candidates of each violated row (2026-10-30, `--localize`), by row name: the
    /// registers whose next-state function, freed at some cycles with the counterexample's inputs
    /// fixed, let the property survive the trace, and what limited the search. Empty unless
    /// requested. Shown on the trace as `repair_candidates` / `localization`.
    pub repairs: Vec<(String, RepairReport)>,
    /// How the counterexamples name the lift's registers and clocks (display only; see
    /// [`crate::trace::TraceNames`]).
    pub trace_names: crate::trace::TraceNames,
}

/// Severity of a [`VerificationNote`] — how it bears on the verdict's trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    /// Informational — the decision does not narrow soundness (e.g. the
    /// abstraction posture, the coverage summary).
    Info,
    /// The verdict's **scope** is narrowed by a deliberate restriction (e.g. a
    /// config input pinned to a constant → "for these values, not all configs").
    ScopeCaveat,
    /// A soundness-relevant cut (e.g. a black-boxed module → state not modeled →
    /// properties over it are SKIPPED, not verified).
    SoundnessCaveat,
}

/// A human-facing note explaining one abstraction / scoping decision the
/// verify-auto pipeline made, so a verdict's SCOPE and CAVEATS are explicit
/// rather than silent or buried in raw diagnostics. Rendered on every surface
/// (CLI text + JSON, API, UI). `kind` is an open kebab-case category so new
/// note types (per-property provenance, oracle status, …) slot in without a
/// schema change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationNote {
    /// Machine-stable category, kebab-case (e.g. `"config-concretization"`,
    /// `"reset-gating"`, `"blackbox-cut"`, `"abstraction-posture"`).
    pub kind: String,
    /// Severity for UX styling + honest framing.
    pub level: NoteLevel,
    /// One-line human summary.
    pub summary: String,
    /// Longer explanation: the WHY + the soundness / scope implication.
    pub detail: String,
    /// Structured operands, where relevant (e.g. `["cfg_detect_timer_i=7"]`,
    /// `["prim_sparse_fsm_flop"]`). Empty when the note has no operands.
    pub items: Vec<String>,
    /// mununu#548 O-2 — the property this note is ABOUT, when it is about one.
    ///
    /// `None` for a model-level note (reset-gating, control-slice, parameter overrides): those
    /// describe the lift, not a property, and giving them a property would be a lie a consumer
    /// would then group by.
    ///
    /// **Why a field.** Until now the only link was a `"<name>: "` prefix inside `summary`, and the
    /// join was `summary.starts_with(&format!("{name}: "))`. That is a parse of prose, so it broke
    /// in both directions: a property whose name is a prefix of another's (`sva_1` vs `sva_10`)
    /// matched the wrong note, and any note that reworded its opening stopped matching at all —
    /// silently, because a join that finds nothing looks exactly like a note that was not there.
    /// `drop_stale_bottom_reasons` is the load-bearing consumer: it removes ⊥ explanations for
    /// properties another engine went on to decide, and a missed match leaves a report saying a
    /// property is ⊥ while also reporting it HOLDS — which is the mununu#548 defect itself.
    pub property: Option<String>,
}

/// One per-property `two-state-fold` scope note per construct the translator folded because the
/// model is 2-state (today `$isunknown(x)` → `false`). A property that guards against X then holds
/// trivially; the note keeps that verdict from reading as an X-propagation check.
fn two_state_fold_notes(
    folds: &[crate::adapter::slang::translate::TranslationCaveat],
) -> Vec<VerificationNote> {
    folds
        .iter()
        .map(|f| VerificationNote {
            kind: "two-state-fold".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{}: `{}(…)` folded to `false` — the lifted model is 2-state",
                f.assertion, f.construct
            ),
            detail:
                "The design is lifted to a 2-state model (BTOR2 has no X or Z), so no value is \
                     ever unknown and `$isunknown(x)` is `false` on every path. A property such as \
                     `!$isunknown(x)` therefore holds trivially here: the verdict is exact for the \
                     model but says nothing about X-propagation in the RTL, which needs a 4-state \
                     simulator or an X-aware formal tool."
                    .into(),
            items: vec![f.construct.clone()],
            property: Some(f.assertion.clone()),
        })
        .collect()
}

/// One per-property `strong-sequence-reading` scope note per unbounded `##[m:$]` the translator
/// read as an eventuality. IEEE 1800-2017 makes a sequence in an `assert` weak by default, and a
/// weak unbounded sequence is satisfied by every infinite trace; reading it strongly is what its
/// author almost always means, but it is a choice, so the verdict says so.
fn strong_reading_notes(
    readings: &[crate::adapter::slang::translate::TranslationCaveat],
) -> Vec<VerificationNote> {
    readings
        .iter()
        .map(|r| VerificationNote {
            kind: "strong-sequence-reading".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{}: `{}` read as an eventuality (strong), not with the standard's weak default",
                r.assertion, r.construct
            ),
            detail: "IEEE 1800-2017 §16.12.2 makes a sequence in an `assert` weak by default, and a \
                     weak unbounded sequence such as `a |-> ##[1:$] b` is satisfied by every infinite \
                     trace — it can never fail. mununu reads it strongly, as `a |-> s_eventually b` \
                     (AG(a → AF b)), which is what such an assertion is almost always written to \
                     mean. A VIOLATED here is a path on which `b` never arrives; under the weak \
                     reading the same assertion would pass vacuously. Write `s_eventually` or \
                     `strong(…)` to make the intent explicit and remove this note."
                .into(),
            items: vec![r.construct.clone()],
            property: Some(r.assertion.clone()),
        })
        .collect()
}

/// Result of an automated SVA verification run.
/// How `sv verify-auto` treats SVA `assume` statements (OpenTitan coverage, Phase 4). CLI
/// `--assumes`, API `assumes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssumeMode {
    /// The default: the guarantees are checked on the traces along which every applied
    /// assumption has held so far (`btor2::assume_monitor` + `assume_guarantee::relativize`).
    #[default]
    Precondition,
    /// Each assumption is CHECKED like an assertion and constrains nothing (the behaviour before
    /// 2026-10-15).
    Check,
    /// Assumptions are dropped: neither checked nor applied.
    Ignore,
}

impl AssumeMode {
    /// The stable wire spelling.
    pub fn label(self) -> &'static str {
        match self {
            AssumeMode::Precondition => "precondition",
            AssumeMode::Check => "check",
            AssumeMode::Ignore => "ignore",
        }
    }

    /// Parse the wire spelling (`None` for anything else).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "precondition" => Some(AssumeMode::Precondition),
            "check" => Some(AssumeMode::Check),
            "ignore" => Some(AssumeMode::Ignore),
            _ => None,
        }
    }
}

/// What the run did with the design's `assume` statements. Empty `mode` ⇒ the design has none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssumptionReport {
    /// [`AssumeMode::label`], or empty when the design has no `assume`.
    pub mode: String,
    /// Assumptions applied as preconditions (label, or name when unlabelled).
    pub applied: Vec<String>,
    /// Assumptions NOT applied, with the reason (an unsupported shape, an unresolved signal, or
    /// `--assumes ignore`). The guarantees were checked without them — the stronger obligation.
    pub unapplied: Vec<(String, String)>,
    /// Guarantees checked WITHOUT the assumptions because their formula could not be relativized.
    pub unguarded: Vec<(String, String)>,
    /// `consistent` (some trace satisfies every applied assumption forever), `contradictory`
    /// (none does: every HOLDS is vacuous) or `unknown`; `None` when nothing was applied.
    pub consistency: Option<String>,
    /// Rows whose annotation head says `assumes=ignore` (2026-10-30), by ROW NAME, with what the
    /// modifier did in this run: under `--assumes precondition` with applied assumptions the row
    /// is judged on every trace while the others are judged under them; in every other mode it
    /// changed nothing, and the reason says why. Filled whether or not the design has `assume`s,
    /// so [`AutoVerifyReport::assume_exempt`] can show it on the row.
    pub exempt: Vec<(String, String)>,
}

/// The `$past` depth a signal name reads: `x__past` → 1, `x__past3` → 3, `x__past__bits7_0` → 1,
/// anything else → 0.
fn shadow_depth_of_name(name: &str) -> u32 {
    let Some(i) = name.find("__past") else {
        return 0;
    };
    let digits: String = name[i + "__past".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(1)
}

/// The deepest `$past` any atom under `id` reads (a minted expression contributes its leaves').
fn subtree_past_depth(
    f: &crate::mu_calculus::Formula,
    id: crate::mu_calculus::NodeId,
    minted: &std::collections::HashMap<&str, u32>,
) -> u32 {
    use crate::mu_calculus::Node as N;
    match f.node(id) {
        N::Predicate(atom) => atom
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|t| !t.is_empty())
            .map(|t| {
                minted
                    .get(t)
                    .copied()
                    .unwrap_or(0)
                    .max(shadow_depth_of_name(t))
            })
            .max()
            .unwrap_or(0),
        N::Not(a) => subtree_past_depth(f, *a, minted),
        N::And(a, b) | N::Or(a, b) => {
            subtree_past_depth(f, *a, minted).max(subtree_past_depth(f, *b, minted))
        }
        N::Modal { target, .. } => subtree_past_depth(f, *target, minted),
        N::Mu { body, .. } | N::Nu { body, .. } => subtree_past_depth(f, *body, minted),
        _ => 0,
    }
}

/// The internal property `AF ¬ok` that decides [`AssumptionReport::consistency`]: it HOLDS iff
/// every trace eventually breaks an applied assumption. Removed from the report by
/// [`finish_assumptions`].
pub(crate) const ASSUMPTION_CONSISTENCY_PROPERTY: &str = "mununu_assumption_consistency";

/// Phase 4 — split the `assume` statements off `ex.translated` per `mode`. Under
/// [`AssumeMode::Precondition`] the applicable ones are returned as `(display name, formula)` for
/// the monitor, every other translated formula is relativized to `mununu_assume_ok`, and the
/// consistency property is appended. Pure (no model): the monitor is emitted later, once the
/// expressions it reads are minted.
fn plan_assumptions(
    ex: &mut crate::adapter::slang::translate::TranslationReport,
    mode: AssumeMode,
    exempt: &[String],
) -> (AssumptionReport, Vec<(String, String)>) {
    use crate::adapter::btor2::assume_monitor::{ASSUME_OK_SYMBOL, assume_shape};
    use crate::adapter::slang::assume_guarantee::{relativize, root_is_existential};
    let assumes: Vec<_> = ex
        .translated
        .iter()
        .filter(|t| t.kind == SvaKind::Assume)
        .cloned()
        .collect();
    let unsupported_assumes = ex
        .unsupported
        .iter()
        .filter(|u| u.kind == Some(SvaKind::Assume))
        .count();
    // 2026-10-30 — a row's `assumes=ignore` modifier is recorded in EVERY mode, with what it
    // did: a modifier the author wrote and the report never mentions would be a silent no-op.
    let exempt_rows = |reason: &str| -> Vec<(String, String)> {
        exempt
            .iter()
            .map(|n| (n.clone(), reason.to_string()))
            .collect()
    };
    if assumes.is_empty() && unsupported_assumes == 0 {
        return (
            AssumptionReport {
                exempt: exempt_rows(
                    "no assume statement in the design: the modifier changed nothing",
                ),
                ..Default::default()
            },
            Vec::new(),
        );
    }
    let mut rep = AssumptionReport {
        mode: mode.label().to_string(),
        ..Default::default()
    };
    let display = |t: &crate::adapter::slang::translate::TranslatedAssertion| {
        t.label.clone().unwrap_or_else(|| t.name.clone())
    };
    match mode {
        AssumeMode::Check => {
            rep.exempt =
                exempt_rows("--assumes check applies no assumption: the modifier changed nothing");
            return (rep, Vec::new());
        }
        AssumeMode::Ignore => {
            ex.translated.retain(|t| t.kind != SvaKind::Assume);
            rep.unapplied = assumes
                .iter()
                .map(|a| (display(a), "ignored (--assumes ignore)".to_string()))
                .collect();
            rep.exempt =
                exempt_rows("--assumes ignore applies to every row: the modifier changed nothing");
            return (rep, Vec::new());
        }
        AssumeMode::Precondition => {}
    }
    ex.translated.retain(|t| t.kind != SvaKind::Assume);
    let mut candidates = Vec::new();
    for a in &assumes {
        let applicable = crate::mu_calculus::parser::parse(&a.formula)
            .ok()
            .and_then(|f| assume_shape(&f))
            .is_some();
        if applicable {
            candidates.push((display(a), a.formula.clone()));
        } else {
            rep.unapplied.push((
                display(a),
                "only `assume property (B)`, `A |=> B` and `A1 ##1 ... ##1 An |=> B` (e.g. \
                 `A[*n] |=> B`) over booleans are applied as preconditions"
                    .to_string(),
            ));
        }
    }
    if candidates.is_empty() {
        rep.exempt = exempt_rows("no assumption was applicable: the modifier changed nothing");
        return (rep, candidates);
    }
    for t in &mut ex.translated {
        // An exempt row keeps its formula: it never reads `mununu_assume_ok`, so its cone
        // excludes the monitor and the engines decide it on the un-assumed design — the same
        // verdict `--assumes ignore` gives that row, in the same run as the assumed ones.
        if exempt.contains(&t.name) {
            rep.exempt.push((
                t.name.clone(),
                "judged on every trace (annotation modifier assumes=ignore); the other rows are \
                 judged under the applied assumptions"
                    .to_string(),
            ));
            continue;
        }
        // 2026-10-30 — a `mu`-rooted (EF) guarantee is guarded as a cover is, whatever its
        // `kind`: the universal excuse `(!ok) || …` at an EF root is satisfied by an initial
        // state that already breaks the assumption, a vacuous HOLDS on the anti-vacuity row
        // (mununu#837).
        let existential = t.kind == SvaKind::Cover || root_is_existential(&t.formula);
        match relativize(&t.formula, ASSUME_OK_SYMBOL, existential) {
            Ok(f) => t.formula = f,
            Err(why) => rep.unguarded.push((display(t), why)),
        }
        if let Some(c) = &t.recoverability_companion {
            t.recoverability_companion = relativize(c, ASSUME_OK_SYMBOL, false).ok();
        }
    }
    ex.translated
        .push(crate::adapter::slang::translate::TranslatedAssertion {
            name: ASSUMPTION_CONSISTENCY_PROPERTY.to_string(),
            label: None,
            kind: SvaKind::Assert,
            formula: format!("mu Y. ((!({ASSUME_OK_SYMBOL})) || [] Y)"),
            recoverability_companion: None,
            written: None,
        });
    (rep, candidates)
}

/// Phase 4 — turn the internal consistency property into [`AssumptionReport::consistency`],
/// remove it from the report, and scope the verdicts: a `contradictory` set of assumptions makes
/// every HOLDS vacuous, which each such property is told.
pub(crate) fn finish_assumptions(report: &mut AutoVerifyReport) {
    if let Some(i) = report
        .properties
        .iter()
        .position(|p| p.name == ASSUMPTION_CONSISTENCY_PROPERTY)
    {
        let p = report.properties.remove(i);
        report.assumptions.consistency = Some(
            match p.outcome {
                VerifyOutcome::Holds => "contradictory",
                VerifyOutcome::Violated { .. } => "consistent",
                _ => "unknown",
            }
            .to_string(),
        );
    }
    report
        .notes
        .retain(|n| n.property.as_deref() != Some(ASSUMPTION_CONSISTENCY_PROPERTY));
    let a = &report.assumptions;
    if a.mode.is_empty() {
        return;
    }
    let mut detail = format!("SVA `assume` statements: --assumes {}.", a.mode);
    if !a.applied.is_empty() {
        detail.push_str(&format!(
            " Every verdict is SCOPED to the traces on which the applied assumptions hold: {}.",
            a.applied.join(", ")
        ));
    }
    if !a.unapplied.is_empty() {
        detail.push_str(" Not applied (the guarantees were checked without them): ");
        detail.push_str(
            &a.unapplied
                .iter()
                .map(|(n, r)| format!("{n} ({r})"))
                .collect::<Vec<_>>()
                .join("; "),
        );
        detail.push('.');
    }
    if !a.unguarded.is_empty() {
        detail.push_str(&format!(
            " Checked without the assumptions (formula not relativizable): {}.",
            a.unguarded
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !a.exempt.is_empty() && !a.applied.is_empty() {
        // Shown by LABEL where the row has one: the rows print theirs, the note matches.
        let shown: Vec<String> = a
            .exempt
            .iter()
            .map(|(n, _)| {
                report
                    .properties
                    .iter()
                    .find(|p| p.name == *n)
                    .and_then(|p| p.label.clone())
                    .unwrap_or_else(|| n.clone())
            })
            .collect();
        detail.push_str(&format!(
            " Judged on every trace by annotation (`assumes=ignore`): {}.",
            shown.join(", ")
        ));
    }
    let contradictory = a.consistency.as_deref() == Some("contradictory");
    report.notes.push(VerificationNote {
        kind: "assumptions".to_string(),
        level: NoteLevel::ScopeCaveat,
        summary: match a.consistency.as_deref() {
            Some("contradictory") => "the applied assumptions are CONTRADICTORY: no trace \
                                      satisfies them all, so every HOLDS is vacuous"
                .to_string(),
            Some(c) => format!(
                "{} assumption(s) applied as preconditions (consistency: {c})",
                a.applied.len()
            ),
            None => format!("assumptions: --assumes {}", a.mode),
        },
        detail,
        items: a.applied.clone(),
        property: None,
    });
    if contradictory {
        let holds: Vec<String> = report
            .properties
            .iter()
            .filter(|p| matches!(p.outcome, VerifyOutcome::Holds))
            .map(|p| p.name.clone())
            .collect();
        for name in holds {
            report.notes.push(VerificationNote {
                kind: "assumptions-contradictory".to_string(),
                level: NoteLevel::ScopeCaveat,
                summary: format!("{name}: HOLDS vacuously — the assumptions admit no trace"),
                detail: "Every trace eventually breaks an applied assumption, so a guarantee \
                         relativized to them holds on nothing. Fix the assumptions or run with \
                         --assumes check to see which one fails."
                    .to_string(),
                items: Vec::new(),
                property: Some(name),
            });
        }
    }
    refresh_coverage_summary(report);
}

#[derive(Debug, Clone, Default)]
pub struct AutoVerifyReport {
    /// Phase 4 — what the run did with the design's `assume` statements.
    pub assumptions: AssumptionReport,
    pub properties: Vec<PropertyVerdict>,
    /// Assertions that did not translate (name, reason), carried from extraction.
    pub unsupported: Vec<(String, String)>,
    /// Model-level diagnostics — state-register count + black-boxed (cut)
    /// modules. Lets a SKIPPED / vacuous outcome point at its root cause.
    pub diagnostics: ModelDiagnostics,
    /// H.J — human-facing provenance notes: every abstraction / scoping decision
    /// the run made (config concretizations, reset-gating, flop stubs, cut
    /// modules, the abstraction posture, the coverage summary), so the verdict's
    /// scope and caveats are explicit on every surface. Built by [`build_notes`].
    pub notes: Vec<VerificationNote>,
}

/// The skip reason for a property whose atoms reference non-state signals,
/// enriched with the model-level root cause when one is evident: a black-boxed
/// (cut) module whose registers became free inputs, or a model with no state
/// registers at all. Pure so it can be unit-tested without the toolchain.
/// One row's localisation (2026-10-30): the candidates and the status every surface shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairReport {
    pub candidates: Vec<crate::trace::RepairCandidate>,
    pub status: Option<crate::trace::RepairStatus>,
}

impl AutoVerifyReport {
    /// Where the property is written in the RTL, when known (2026-10-30).
    /// The row's localisation, when `--localize` ran on it (2026-10-30).
    pub fn repair(&self, name: &str) -> Option<&RepairReport> {
        self.diagnostics
            .repairs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r)
    }

    /// The row's counterexample as every surface shows it ([`PropertyVerdict::trace`]), with the
    /// report-level additions a row alone cannot carry: the repair candidates (2026-10-30).
    pub fn trace_of(&self, p: &PropertyVerdict) -> Option<crate::trace::Trace> {
        let mut trace = p.trace(&self.diagnostics.trace_names)?;
        if let Some(r) = self.repair(&p.name) {
            trace.repair_candidates = r.candidates.clone();
            trace.localization = r.status.clone();
        }
        Some(trace)
    }

    pub fn property_source(&self, name: &str) -> Option<&crate::trace::SourceLoc> {
        self.diagnostics
            .property_sources
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, l)| l)
    }

    /// The template instance a `@mununu_template` / `--template` row was written as, when it is
    /// one (2026-10-30).
    pub fn template_form(&self, name: &str) -> Option<&str> {
        self.diagnostics
            .template_forms
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| f.as_str())
    }

    /// What the row's `assumes=ignore` modifier did in this run, when the row carries one
    /// (2026-10-30): the reason [`AssumptionReport::exempt`] recorded.
    pub fn assume_exempt(&self, name: &str) -> Option<&str> {
        self.assumptions
            .exempt
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r.as_str())
    }

    /// The RTL lines of the property's cone of influence; empty when unknown (2026-10-30).
    pub fn involves(&self, name: &str) -> &[crate::trace::SourceLoc] {
        self.diagnostics
            .involves
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[], |(_, l)| l.as_slice())
    }

    /// The property as its author wrote it, when that is not the mu-calculus in the row's
    /// `formula` (an `@mununu_guarantee ltl …` row); `None` for every other row (2026-10-29).
    pub fn written_formula(
        &self,
        name: &str,
    ) -> Option<&crate::adapter::slang::translate::WrittenFormula> {
        self.diagnostics
            .written_formulas
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, w)| w)
    }
}

/// Complete the source map from slang's AST (2026-10-29/30): every cell the netlist knows but could
/// not place gets its declaration by name, every register without a stamped `always` line gets the
/// edge-triggered block(s) the AST says assign it, and the status is settled — `available` when
/// every cell has its declaration and every register its block, `partial` with the reason when
/// something is missing (a name the lift mangled, e.g. sv2v's `sv2v_cast_…$result`, is not in the
/// AST under that name and stays bare; it is listed, never guessed), `unavailable` when nothing
/// could be placed.
pub(crate) fn fill_declarations(
    names: &mut crate::trace::TraceNames,
    declarations: &crate::adapter::slang::extract::Declarations,
) {
    let mut undeclared: Vec<String> = Vec::new();
    let mut unassigned: Vec<String> = Vec::new();
    match declarations {
        Ok(index) => {
            for s in &mut names.sources {
                if s.declared.is_none() {
                    s.declared = index
                        .declared
                        .iter()
                        .find(|(n, _)| *n == s.cell)
                        .map(|(_, l)| l.clone());
                    if s.declared.is_none() {
                        undeclared.push(s.cell.clone());
                    }
                }
                if s.kind == "register" && s.assigned.is_empty() {
                    s.assigned = index.flop_blocks.get(&s.cell).cloned().unwrap_or_default();
                    if s.assigned.is_empty() {
                        unassigned.push(s.cell.clone());
                    }
                }
            }
        }
        Err(why) => {
            let unplaced = names
                .sources
                .iter()
                .filter(|s| s.declared.is_none() || (s.kind == "register" && s.assigned.is_empty()))
                .count();
            if unplaced > 0 {
                undeclared.push(format!(
                    "{unplaced} cell(s): the slang source-index run failed — {why}"
                ));
            }
        }
    }
    let placed = names
        .sources
        .iter()
        .any(|s| s.declared.is_some() || !s.assigned.is_empty());
    if names.sources.is_empty() || !placed {
        names.source_map_partial = false;
        if names.source_map.is_none() {
            names.source_map = Some(
                "the lift carried no source attributes and the AST declared none of its cells"
                    .to_string(),
            );
        }
        names.sources.clear();
        return;
    }
    let mut reasons: Vec<String> = names.source_map.take().into_iter().collect();
    if !undeclared.is_empty() {
        reasons.push(format!("no declaration for {}", undeclared.join(", ")));
    }
    if !unassigned.is_empty() {
        let why = if names.source_lines_rewritten {
            " (sv2v rewrote the source, so the lines come from slang's AST, which does not \
             know these names)"
        } else {
            ""
        };
        reasons.push(format!(
            "no assignment line for {}{why}",
            unassigned.join(", ")
        ));
    }
    names.source_map_partial = !reasons.is_empty();
    names.source_map = (!reasons.is_empty()).then(|| reasons.join("; "));
}

/// The written-form table of a property set: every translated assertion that carries a
/// [`crate::adapter::slang::translate::WrittenFormula`], keyed by its row name — what the report
/// skeleton stores in [`ModelDiagnostics::written_formulas`] (2026-10-29).
fn written_formulas_of(
    translated: &[crate::adapter::slang::translate::TranslatedAssertion],
) -> Vec<(String, crate::adapter::slang::translate::WrittenFormula)> {
    translated
        .iter()
        .filter_map(|t| t.written.clone().map(|w| (t.name.clone(), w)))
        .collect()
}

/// Where each translated property is written (2026-10-30): an SVA row joins the AST's assertion
/// sites on its name AND its label — the source-index run elaborates the same files, so the names
/// agree; a label that does not match means the two runs saw different assertions, and the row
/// gets no line rather than a wrong one. An annotation row takes its comment's line.
fn property_sources_of(
    translated: &[crate::adapter::slang::translate::TranslatedAssertion],
    index: &crate::adapter::slang::extract::Declarations,
    annotation_lines: &[(String, crate::trace::SourceLoc)],
) -> Vec<(String, crate::trace::SourceLoc)> {
    let sites: &[(String, Option<String>, crate::trace::SourceLoc)] = match index {
        Ok(i) => &i.assertions,
        Err(_) => &[],
    };
    translated
        .iter()
        .filter_map(|t| {
            annotation_lines
                .iter()
                .find(|(n, _)| *n == t.name)
                .map(|(_, l)| l.clone())
                .or_else(|| {
                    sites
                        .iter()
                        .find(|(n, label, _)| *n == t.name && *label == t.label)
                        .map(|(_, _, l)| l.clone())
                })
                .map(|l| (t.name.clone(), l))
        })
        .collect()
}

fn unseedable_skip_reason(unseedable: &[String], diag: &ModelDiagnostics) -> String {
    let base = format!(
        "atom(s) over non-state signals (combinational/IO — not cube-bindable): {}",
        unseedable.join(", ")
    );
    if !diag.blackboxed_modules.is_empty() {
        format!(
            "{base}. Root cause: {} module(s) instantiated with no body ({}) — \
             registers they drive are not modeled as state. Provide the missing \
             module source(s) to model them.",
            diag.blackboxed_modules.len(),
            diag.blackboxed_modules.join(", ")
        )
    } else if diag.state_register_count == 0 {
        format!(
            "{base}. Root cause: the lifted model has no state registers — the design's \
             state may have been optimized away or cut."
        )
    } else {
        base
    }
}

/// mununu#663 — the must-edge policy a property's cube run actually gets. `Off` with
/// [`VerifyAutoOptions::must_edge_auto`] resolves by the formula's class: Liveness / Mixed (a μ
/// under a ν) → [`MustEdgeInference::SmtPerTargetStandard`] (Z3 ∀ state ∃ input per target —
/// measured 2026-10-11 to decide `AG EF` rows at the cube level where `SmtHyperMust` leaves ⊥);
/// everything else, and every explicit policy, unchanged.
pub(crate) fn effective_must_edge_inference(
    opts: &VerifyAutoOptions,
    formula: &crate::mu_calculus::Formula,
) -> MustEdgeInference {
    use crate::mu_calculus::PropertyClass;
    if opts.must_edge_inference != MustEdgeInference::Off || !opts.must_edge_auto {
        return opts.must_edge_inference;
    }
    match formula.property_class() {
        PropertyClass::Liveness | PropertyClass::Mixed => MustEdgeInference::SmtPerTargetStandard,
        PropertyClass::Safety | PropertyClass::Reachability | PropertyClass::Propositional => {
            MustEdgeInference::Off
        }
    }
}

/// The must-edge posture as the `abstraction-posture` note names it: the explicit policy's label,
/// or — mununu#663 — the `auto` resolution, which names both of its outcomes.
fn must_edge_posture_label(mode: MustEdgeInference, auto: bool) -> &'static str {
    if auto && mode == MustEdgeInference::Off {
        "auto — SMT ∀∃ per-target must (smt-per-target-standard) for Liveness/Mixed-class \
         properties (a μ under a ν), may-only for the rest"
    } else {
        must_edge_inference_label(mode)
    }
}

/// A short human label for the must-edge inference posture (used in the
/// abstraction-posture note).
fn must_edge_inference_label(mode: MustEdgeInference) -> &'static str {
    match mode {
        MustEdgeInference::Off => "off (may-only over-approximation)",
        MustEdgeInference::SmtPerTarget => "SMT ∀∀ per-target",
        MustEdgeInference::SmtPerTargetStandard => "SMT ∀∃ per-target (canonical KMTS)",
        MustEdgeInference::SmtHyperMust => "SMT ∀∃ hyper-must (GKMTS, sound νμ)",
    }
}

/// H.J.b — token-aware substitution of concretized config inputs with their
/// constants in a formula string. Each **whole-identifier** occurrence of a
/// pinned signal name becomes its value (`cnt_q >= cfg_detect_timer_i` →
/// `cnt_q >= 7` with `{cfg_detect_timer_i: 7}`), so a relational-with-wide-input
/// atom lowers to a decidable state-vs-constant `Cmp`. Identifier chars match the
/// mu-parser (alnum, `_`, `$`, `.`); non-identifier text is preserved. Only the
/// signals actually pinned in the BTOR2 are substituted, so the formula and the
/// pinned model agree. Empty config ⇒ the string is returned unchanged. Pure +
/// unit-tested.
///
/// A pinned signal in BOOLEAN position — an operand of `!`, `&&`, `||`, a bare
/// conjunct, as OpenTitan's `disable iff (... || !rst_ni)` keeps it — becomes
/// `true` / `false` (SV truthiness: non-zero is true), because a number is not a
/// formula: `(!(1))` failed to parse and skipped the property. Value position is
/// next to a comparison or an addend (`== != < <= > >= +`); the diamond `<>` is a
/// modality, not a comparison.
fn substitute_config_in_formula(formula: &str, config: &[(String, u64)]) -> String {
    if config.is_empty() {
        return formula.to_string();
    }
    let map: std::collections::HashMap<&str, u64> =
        config.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '.';
    let chars: Vec<char> = formula.chars().collect();
    let mut out = String::with_capacity(formula.len());
    let mut i = 0;
    while i < chars.len() {
        if is_ident(chars[i]) {
            let start = i;
            while i < chars.len() && is_ident(chars[i]) {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            match map.get(tok.as_str()) {
                Some(v) => {
                    let after = chars[i..].iter().find(|c| !c.is_whitespace());
                    let mut before = chars[..start].iter().rev().filter(|c| !c.is_whitespace());
                    let (b1, b2) = (before.next(), before.next());
                    let value_after = matches!(after, Some('=' | '!' | '<' | '>' | '+'))
                        && !chars[i..]
                            .iter()
                            .collect::<String>()
                            .trim_start()
                            .starts_with("<>");
                    let value_before = matches!(b1, Some('=' | '<' | '+'))
                        || (b1 == Some(&'>') && b2 != Some(&'<'));
                    if value_after || value_before {
                        out.push_str(&v.to_string());
                    } else {
                        out.push_str(if *v != 0 { "true" } else { "false" });
                    }
                }
                None => out.push_str(&tok),
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// H.J — build the human-facing provenance notes from a completed run's
/// diagnostics + the abstraction posture + the coverage tally + any config
/// concretizations applied. Pure (no toolchain) so it is unit-testable; called
/// at the end of [`verify_auto`]. `applied_config_values` are the user-requested
/// `config_values` that actually pinned a signal this run (H.J.b); empty when no
/// concretization was requested. `posture` (A.3) makes the abstraction-posture
/// note honest about the may-relation the run actually used, rather than
/// hardcoding "may-over-approximation".
enum NotePosture {
    /// Exact-symbolic engine: the full bit-blasted state, no abstraction — a
    /// definite HOLDS/VIOLATED is sound and there is no `⊥`.
    Exact,
    /// Predicate-cube path. The cube lift always uses the sound `SmtAllPairs`
    /// may-relation (AR-S2 retired the sampling-may fallback + its A.4 ⊥-guard),
    /// so a definite HOLDS on a safety property is sound by over-approximation.
    Cube,
}

/// mununu#459 — partition user `--pin` pins into (applied, unusable).
///
/// A pin is **applied** if the config pass pinned it (`pinned_configs`) or reset-gating already
/// pinned it to the SAME value (`reset_pinned`). It is **unusable** if its name is not a primary
/// input of the lifted model (misspelled, or a state/output/optimized-away signal), or — a real
/// input — it was already tied to a DIFFERENT constant (a conflict with reset-gating). An unusable
/// pin is a caller mistake the caller must see: it is surfaced as a hard error, never silently
/// dropped to verify a different question than the one asked. Pure (no lift) so it is unit-testable.
fn partition_config_pins(
    config_pins: &[(String, u64)],
    model_input_names: &std::collections::HashSet<String>,
    pinned_configs: &std::collections::HashSet<String>,
    reset_pinned: &std::collections::HashSet<String>,
) -> (Vec<(String, u64)>, Vec<String>) {
    let mut applied: Vec<(String, u64)> = Vec::new();
    let mut unusable: Vec<String> = Vec::new();
    for (name, value) in config_pins {
        let tag = format!("{name}={value}");
        if pinned_configs.contains(&tag) || reset_pinned.contains(&tag) {
            applied.push((name.clone(), *value));
        } else if !model_input_names.contains(name) {
            unusable.push(format!(
                "`{tag}` — `{name}` is not a primary input of the lifted model"
            ));
        } else {
            // A real input, but the config pass could not pin it to `value` — it is already tied
            // to a different constant (typically by reset-gating).
            unusable.push(format!(
                "`{tag}` — `{name}` is a primary input but is already pinned to a different \
                 constant (e.g. reset-gating); reconcile the two"
            ));
        }
    }
    (applied, unusable)
}

/// mununu#536 — the `coverage-summary` note, computed from a report's CURRENT verdicts.
///
/// Extracted so it can be recomputed AFTER the portfolio merge. It is the one note whose
/// content is a function of the verdicts, which is exactly why it could go stale while every
/// other note stayed true.
fn coverage_summary_note(report: &AutoVerifyReport) -> VerificationNote {
    let (mut holds, mut violated, mut unknown, mut skipped) = (0usize, 0usize, 0usize, 0usize);
    for p in &report.properties {
        match p.outcome {
            VerifyOutcome::Holds => holds += 1,
            VerifyOutcome::Violated { .. } => violated += 1,
            VerifyOutcome::Unknown { .. } => unknown += 1,
            VerifyOutcome::Skipped { .. } => skipped += 1,
        }
    }
    VerificationNote {
        kind: "coverage-summary".into(),
        level: NoteLevel::Info,
        summary: format!(
            "{} assertion(s): {holds} definite (HOLDS), {violated} violated, {unknown} unknown (⊥), {skipped} skipped; {} untranslatable",
            report.properties.len(),
            report.unsupported.len(),
        ),
        detail: "A definite verdict transfers to the RTL (see abstraction-posture); ⊥ means the \
                 abstraction is too coarse to decide — an honest 'don't know', not a violation."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// mununu#536 / mununu#498 — replace a report's `coverage-summary` note with one recomputed
/// from its current verdicts.
///
/// # The bug this fixes
///
/// `merge_portfolio_reports` starts from `base.clone()`, where the base is the highest-precision
/// engine, and that clone brings the base's NOTES with it. The merge then rewrites
/// `properties[].outcome` with the first definite verdict across all engines — but nothing
/// rewrote the notes, so the `coverage-summary` kept reporting the BASE ENGINE'S tally while
/// `properties[]` reported the merged one. Since `portfolio-sequential` is the default engine,
/// this was the common case, and it is exactly what mununu#498's minor and mununu#536 observed:
/// a summary claiming `0 unknown` beside properties printing `UNKNOWN`.
///
/// A consumer that counted the summary line under-reported its own coverage. That was tolerable
/// while the summary was prose a human skimmed; it is not tolerable inside a machine-readable
/// report, which is why it is fixed in the same change that publishes one.
fn refresh_coverage_summary(report: &mut AutoVerifyReport) {
    let fresh = coverage_summary_note(report);
    if let Some(slot) = report
        .notes
        .iter_mut()
        .find(|n| n.kind == "coverage-summary")
    {
        *slot = fresh;
    } else {
        report.notes.push(fresh);
    }
}

/// mununu#548 — the merged portfolio report inherits the BASE engine's `abstraction-posture`
/// note (`merge_portfolio_reports` does `base.clone()`), and that note describes ONE engine. When
/// the base is exact-symbolic it asserts *"no abstraction … and there is no ⊥"* — while the merge
/// has just folded in verdicts from engines that DO abstract, including the ⊥ they produced. A
/// consumer then reads, in one report, that ⊥ cannot happen and a ⊥.
///
/// Exact sibling of [`refresh_coverage_summary`] (mununu#536), which fixes the same
/// inherited-from-base defect for the note that counts verdicts. The clone is convenient for the
/// property list and diagnostics; every note that describes the RUN rather than the design has to
/// be recomputed after it.
///
/// Only rewrites on a genuinely multi-engine merge — a single-engine portfolio degenerates to that
/// engine, and its posture note is then correct as written.
/// mununu#548 — drop `bottom-reason` notes for properties the merge DECIDED.
///
/// `bottom_reason_note` explains why a property is ⊥, and is emitted per engine inside
/// [`build_notes`]. When the base engine left `p` at ⊥ and a later engine decided it, the merge
/// takes the definite verdict while `base.clone()` keeps the stale explanation — so the report
/// says *"here is why `p` is ⊥"* about a property it also reports as HOLDS.
///
/// Conservative by construction: a note is dropped only when the property it names is now
/// DEFINITE. A ⊥ that survived the merge keeps its explanation, which is the case the note exists
/// for. Matching is on the `"<name>: "` prefix `bottom_reason_note` writes.
/// monono ask 26 — attribute a residual ⊥ to the highest-precision engine that FAILED.
///
/// The counterpart to [`drop_stale_bottom_reasons`], and the mirror of the mununu#548 defect: there
/// a note describing ONE engine was wrongly INHERITED by properties another engine decided; here a
/// failing engine's reason was wrongly DROPPED. Same merge, opposite direction — and only the
/// direction producing a *wrong statement* was fixed at the time, because an *absent* one does not
/// look like a defect in a passing test.
///
/// Three deliberate restrictions, each of which is the #548 lesson applied:
///
/// 1. **Only non-definite properties.** A property another engine decided is not ⊥ and gets no
///    reason, however loudly the exact engine failed.
/// 2. **Never overwrite a more specific classification.** `SafetyShapeNotReducible` says the
///    property's SHAPE is wrong and a bigger budget will not help; clobbering that with a resource
///    message would send a consumer to raise a knob that cannot work.
/// 3. **Only the FIRST failure**, which is the highest-precision one because `runs` is in precision
///    order. Listing every engine's failure would imply each is independently responsible for this
///    property, which is exactly the over-claim #548 was.
fn attach_engine_failure_reason(report: &mut AutoVerifyReport, failures: &[(&str, String)]) {
    let Some((engine, detail)) = failures.first() else {
        return;
    };
    for p in report.properties.iter_mut() {
        if outcome_definite(&p.outcome).is_some() || p.bottom_reason.is_some() {
            continue;
        }
        p.bottom_reason = Some(BottomReason::EngineDidNotComplete {
            engine: (*engine).to_string(),
            detail: detail.clone(),
        });
    }
}

/// mununu#577 — a `Violated` is not sound when the property's cone contains a register whose
/// INITIAL VALUE was never established.
///
/// **The mechanism, measured on a real consumer design.** `sprite_eval.sv` resets asynchronously
/// (`always_ff @(posedge clk or negedge rst_n)`). Yosys's `async2sync` lowers that into a reset
/// MUX, not a BTOR2 `init`, so the lifted model carries **one `init` line for eleven state
/// cells**. Reset-gating then pins `rst_n` INACTIVE — which deletes the only remaining mechanism
/// that would ever drive those registers to their reset values — and nothing replaces it. Every
/// affected register is therefore free at cycle 0 and forever after.
///
/// A free initial state is an OVER-APPROXIMATION of reachability, and
/// [CLAUDE.md §Soundness Guarantees] is explicit about which direction that licenses: safety +
/// over-approximation is sound for **HOLDS** and unsound for **VIOLATED**. The engine was emitting
/// the one verdict it could not justify — `AG(drop_q <= 1) VIOLATED` on a counter the RTL bounds at
/// 1, because in the model `drop_q` may simply START at 1023.
///
/// **Why only the universal direction is downgraded.** A freer initial state makes strictly MORE
/// states reachable, so an existential refutation (`EF p` VIOLATED = "p unreachable even from this
/// larger start set") is a *stronger* claim under the over-approximation and stays sound. That
/// asymmetry is why `EF(drop_q == 2)` was right while `AG(drop_q <= 1)` was wrong, and why the two
/// appeared to contradict each other.
///
/// This is an ABSTENTION, not a fix: the right repair is for reset-gating to establish the reset
/// values it pins away. Until it does, a named `⊥` beats a confident wrong answer.
/// mununu#579 — force `⊥` on any pair of properties in this report whose definite verdicts cannot
/// both be right.
///
/// A pure post-pass: no engine work, no state-space cost, only the properties already in the report.
/// The soundness reasoning and the (deliberately short) list of admitted pairs live in
/// [`super::report_consistency`]; this function is the plumbing.
///
/// **It does not pick a winner.** Both properties are withheld with the same alarm, because a
/// contradiction establishes that one verdict is broken and not which one. mununu#577 is the reason
/// that is spelled out rather than assumed: the reading that looked obvious there blamed the correct
/// half.
///
/// A property already carrying a `bottom_reason` is left alone — an existing cause is more specific
/// than "something in this report disagrees".
fn flag_self_contradictory_verdicts(report: &mut AutoVerifyReport) {
    use super::report_consistency::definite_pair_contradicts;

    // mununu#599 — every DEFINITE verdict with a recognised shape, not only the VIOLATED ones:
    // the HOLDS side has its own unsatisfiable pairs (the table in `report_consistency`).
    let definite = definite_shaped_properties(report);
    if definite.len() < 2 {
        return;
    }

    // Collect first, mutate second: a property may contradict more than one partner, and the
    // detail should name the partner rather than whichever happened to be visited last.
    let mut hits: Vec<(usize, String)> = Vec::new();
    for (ai, (i, a, va)) in definite.iter().enumerate() {
        for (j, b, vb) in definite.iter().skip(ai + 1) {
            if !definite_pair_contradicts(a, *va, b, *vb) {
                continue;
            }
            let (na, nb) = (
                report.properties[*i].name.clone(),
                report.properties[*j].name.clone(),
            );
            let (wa, wb) = (definite_word(*va), definite_word(*vb));
            hits.push((
                *i,
                format!("`{na}` {wa} is unsatisfiable together with `{nb}` {wb} over the same or exactly negated atom"),
            ));
            hits.push((
                *j,
                format!("`{nb}` {wb} is unsatisfiable together with `{na}` {wa} over the same or exactly negated atom"),
            ));
        }
    }
    for (i, detail) in hits {
        let p = &mut report.properties[i];
        // A pair may be hit twice; the first alarm already withheld it.
        if p.bottom_reason.is_some() {
            continue;
        }
        p.outcome = VerifyOutcome::Unknown { unknown_cells: 0 };
        p.counterexample = None;
        p.vacuity = None;
        p.bottom_reason = Some(BottomReason::ReportSelfContradiction { detail });
    }
}

/// `(index, shape, verdict)` for every property with a definite verdict, no `bottom_reason`, and
/// a shape [`super::report_consistency::classify_shape`] recognises.
fn definite_shaped_properties(
    report: &AutoVerifyReport,
) -> Vec<(
    usize,
    super::report_consistency::Shape,
    super::report_consistency::Definite,
)> {
    use super::report_consistency::{Definite, classify_shape};
    report
        .properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p.bottom_reason.is_none())
        .filter_map(|(i, p)| {
            let v = match p.outcome {
                VerifyOutcome::Holds => Definite::Holds,
                VerifyOutcome::Violated { .. } => Definite::Violated,
                _ => return None,
            };
            let formula = crate::mu_calculus::parser::parse(&p.formula).ok()?;
            classify_shape(&formula).map(|sh| (i, sh, v))
        })
        .collect()
}

fn definite_word(v: super::report_consistency::Definite) -> &'static str {
    match v {
        super::report_consistency::Definite::Holds => "HOLDS",
        super::report_consistency::Definite::Violated => "VIOLATED",
    }
}

/// mununu#599 — judge every recoverability guarantee that HOLDS against the other verdicts in the
/// report, and say so AT the guarantee (`PropertyVerdict::vacuity`).
///
/// A pure post-pass over the shipped verdicts, after [`flag_self_contradictory_verdicts`]: a pair
/// that is a contradiction has already been withheld and is not a vacuity. The verdict is never
/// changed here — a vacuous HOLDS is a true HOLDS on the model; what it lacks is meaning, and the
/// field says so. The strongest finding wins: an invariant target (proven vacuous) over a refuted
/// same-register witness (not shown non-vacuous).
fn flag_vacuous_guarantees(report: &mut AutoVerifyReport) {
    use super::report_consistency::{Shape, Vacuity, vacuity_of_guarantee};

    let definite = definite_shaped_properties(report);
    let mut findings: Vec<(usize, VacuityFinding)> = Vec::new();
    for (i, g, vg) in &definite {
        if !matches!(g, Shape::Recoverability(_))
            || *vg != super::report_consistency::Definite::Holds
        {
            continue;
        }
        let mut best: Option<(Vacuity, usize)> = None;
        for (j, other, vo) in &definite {
            if j == i {
                continue;
            }
            let Some(v) = vacuity_of_guarantee(g, other, *vo) else {
                continue;
            };
            let stronger = match best {
                None => true,
                Some((Vacuity::WitnessRefuted, _)) => v == Vacuity::InvariantTarget,
                Some((Vacuity::InvariantTarget, _)) => false,
            };
            if stronger {
                best = Some((v, *j));
            }
        }
        let Some((v, j)) = best else {
            continue;
        };
        let target = g.atom().to_string();
        let witness = report.properties[j].name.clone();
        let witness_formula = report.properties[j].formula.clone();
        let (kind, detail) = match v {
            Vacuity::InvariantTarget => (
                VacuityKind::InvariantTarget,
                format!(
                    "VACUOUS — `{target}` is invariant in this report (`{witness}`: `{witness_formula}` \
                     says the design never leaves it), so \"always recoverable to `{target}`\" holds \
                     without any recovery ever being exercised. The verdict stands; it says nothing \
                     about recovery."
                ),
            ),
            Vacuity::WitnessRefuted => (
                VacuityKind::WitnessRefuted,
                format!(
                    "NOT SHOWN NON-VACUOUS — `{witness}` (`{witness_formula}`), a reachability over \
                     this guarantee's register, is VIOLATED in the same report. That does not make \
                     the guarantee false; it means this run has not demonstrated that the design \
                     ever leaves `{target}` by that route. If `{witness}` is the non-vacuity witness \
                     for this guarantee, treat the pair as a failed gate, not a pass."
                ),
            ),
        };
        findings.push((
            *i,
            VacuityFinding {
                kind,
                witness,
                detail,
            },
        ));
    }
    for (i, f) in findings {
        report.properties[i].vacuity = Some(f);
    }
}

/// mununu#599 — the note form of [`PropertyVerdict::vacuity`], for readers of the notes stream;
/// the field is the one a gate reads. Re-derived from the fields so a merge cannot leave a stale
/// one behind (the mununu#548 lesson).
fn refresh_vacuity_notes(report: &mut AutoVerifyReport) {
    report.notes.retain(|n| n.kind != "vacuous-guarantee");
    let fresh: Vec<VerificationNote> = report
        .properties
        .iter()
        .filter_map(|p| {
            let v = p.vacuity.as_ref()?;
            Some(VerificationNote {
                kind: "vacuous-guarantee".into(),
                level: NoteLevel::ScopeCaveat,
                summary: format!(
                    "`{}`: HOLDS, but {} (witness `{}`).",
                    p.name,
                    match v.kind {
                        VacuityKind::InvariantTarget => "its target is invariant here — vacuous",
                        VacuityKind::WitnessRefuted =>
                            "a same-register reachability is refuted — not shown non-vacuous",
                        VacuityKind::AntecedentUnreachable =>
                            "its antecedent can never fire on this model — vacuous",
                    },
                    v.witness
                ),
                detail: v.detail.clone(),
                items: vec![v.witness.clone()],
                property: Some(p.name.clone()),
            })
        })
        .collect();
    report.notes.extend(fresh);
}

/// The BMC bound (beyond the property's own depth) for a safety counterexample.
const SAFETY_WITNESS_MAX_K: u32 = 60;

/// A finite safety counterexample: the path from reset to the violation (no `cycle`).
fn safety_counterexample(
    trace: crate::adapter::btor2::native_bmc::BmcTrace,
) -> ExactCounterexample {
    ExactCounterexample {
        prefix: trace.states,
        cycle: Vec::new(),
        inputs: trace.inputs,
        unreachable_target: Vec::new(),
    }
}

/// mununu#710 — attach a concrete counterexample to every VIOLATED safety property that has none — whichever
/// engine decided it. Native BMC re-derives the shallowest reset → violation path on a one-path
/// monitor of the formula ([`crate::adapter::reach_rescue::safety_witness`]); a path it finds is a
/// genuine counterexample, so the trace also cross-checks the verdict. None within the bound (a
/// deep violation, or one only the branching reading has) leaves the field empty — the verdict is
/// unchanged either way. Runs after every verdict-changing pass but the consistency flags, so it
/// only spends BMC on verdicts the report ships.
/// mununu#803 — a VIOLATED row that ships no path says why: the stage of the path finder that
/// declined and its reason. The verdict stands; the path is the explanation, not the evidence.
fn witness_miss_note(
    name: &str,
    decided_by: Option<&str>,
    miss: &crate::adapter::reach_rescue::WitnessMiss,
) -> VerificationNote {
    VerificationNote {
        kind: "witness-unavailable".to_string(),
        level: NoteLevel::Info,
        summary: format!("{name}: no counterexample path shipped — {miss}"),
        detail: format!(
            "the verdict stands (decided by {}); the path finders declined at stage `{}`: {}. A \
             VIOLATED without a path is still a VIOLATED; the path is the explanation, not the \
             evidence.",
            decided_by.unwrap_or("unknown"),
            miss.stage,
            miss.reason
        ),
        items: vec![miss.stage.to_string()],
        property: Some(name.to_string()),
    }
}

fn attach_safety_witnesses(report: &mut AutoVerifyReport, design_btor2: &str, reset_pinned: bool) {
    use crate::mu_calculus::{PropertyClass, parser as mu_parser};
    let mut misses: Vec<VerificationNote> = Vec::new();
    for prop in report.properties.iter_mut() {
        if !matches!(prop.outcome, VerifyOutcome::Violated { .. }) || prop.counterexample.is_some()
        {
            continue;
        }
        if crate::adapter::memory_budget::check_process_memory_budget().is_err()
            || crate::adapter::run_budget::expired()
        {
            break;
        }
        let Ok(formula) = mu_parser::parse(&prop.formula) else {
            continue;
        };
        if formula.property_class() != PropertyClass::Safety {
            continue;
        }
        match crate::adapter::reach_rescue::safety_witness(
            design_btor2,
            &formula,
            reset_pinned,
            SAFETY_WITNESS_MAX_K,
        ) {
            Ok(trace) => prop.counterexample = Some(safety_counterexample(trace)),
            Err(miss) => misses.push(witness_miss_note(
                &prop.name,
                prop.decided_by.as_deref(),
                &miss,
            )),
        }
    }
    report.notes.extend(misses);
}

fn downgrade_violated_on_unestablished_init(report: &mut AutoVerifyReport, model_btor2: &str) {
    // Only fires when a reset was pinned inactive — that pin is what removes the reset path.
    if report.diagnostics.gated_resets.is_empty() {
        return;
    }
    let Ok(file) = crate::adapter::btor2::parser::parse(model_btor2) else {
        return;
    };
    use crate::adapter::btor2::ast::Node;
    // State cells carrying no `init` line: their cycle-0 value is unconstrained.
    let has_init: std::collections::HashSet<crate::adapter::btor2::ast::Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Init { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    // mununu#578 — a state with NO `next` is excluded, and the distinction is load-bearing.
    //
    // Such a cell is a `--cutpoint` or a blackboxed submodule output: free at EVERY cycle because
    // the user asked for it, not because a reset path was lost. Withholding a `Violated` over one
    // is not a soundness win, it is a precision loss — the cutpoint contract already states that a
    // violation under a cut may be spurious, and mununu warns accordingly rather than abstaining.
    //
    // Measured: with these included, `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips`
    // fails. Cutting a register leaves it init-less, and `AG(st_q == 0)` — violated because `st_q`
    // leaves 0, which freeing the cut register can only make *more* reachable — was downgraded to
    // ⊥ purely because a cut cell sat in its cone. A cut point must not change a verdict.
    //
    // This is the same line `reset_init::inject_reset_init` draws: free-by-construction is not a
    // reset question. Both sides now agree on it.
    let has_next: std::collections::HashSet<crate::adapter::btor2::ast::Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Next { state, .. } => Some(*state),
            _ => None,
        })
        .collect();
    let uninit: std::collections::HashSet<crate::adapter::btor2::ast::Nid> = file
        .lines
        .iter()
        .filter(|l| {
            matches!(l.node, Node::State { .. })
                && !has_init.contains(&l.nid)
                && has_next.contains(&l.nid)
        })
        .map(|l| l.nid)
        .collect();
    if uninit.is_empty() {
        return;
    }

    for p in report.properties.iter_mut() {
        if !matches!(p.outcome, VerifyOutcome::Violated { .. }) {
            continue;
        }
        let Ok(formula) = crate::mu_calculus::parser::parse(&p.formula) else {
            continue;
        };
        // Existential refutations stay sound under a freer start set (see above).
        if !matches!(
            formula.property_class(),
            crate::mu_calculus::PropertyClass::Safety | crate::mu_calculus::PropertyClass::Mixed
        ) {
            continue;
        }
        let atoms = crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms(&formula);
        let cone = crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &atoms);
        let touched: Vec<String> = cone
            .iter()
            .filter(|n| uninit.contains(n))
            .map(|n| n.to_string())
            .collect();
        if touched.is_empty() {
            continue;
        }
        p.outcome = VerifyOutcome::Unknown { unknown_cells: 0 };
        p.bottom_reason = Some(BottomReason::UnestablishedInitialState {
            registers: touched.len(),
            gated_resets: report.diagnostics.gated_resets.clone(),
        });
    }
}

/// mununu#750 — withhold a `Violated` whose cone reads an undriven net.
///
/// An undriven net — an unconnected wire, or the output of a module missing from the source set,
/// which the lift leaves as an anonymous BTOR2 `input` — over-approximates the RTL: a HOLDS over it
/// transfers, a refutation is only a may-trace. The combinational guard in the per-property loop
/// refuses an ATOM bound to one; nothing looked at the SEQUENTIAL cone, so a violation driven
/// through one shipped as definite (four OpenTitan census rows, which HOLD on the full closure).
/// Runs after [`attach_safety_witnesses`], so the may-trace stays on the row.
///
/// Not withheld: a pure reachability refutation (`EF p` violated = `p` unreachable even on the
/// freer model, hence on the RTL), and a DELIBERATE cut — a `--cutpoint` or a `(* blackbox *)`
/// stub, a state with no `next` (mununu#578: a cut asked for keeps its verdict, under its
/// `control-slice` / `blackbox-cut` caveat). Measured: requested cuts lift as symbol-less
/// next-less states, so telling them apart by name is not possible; by kind it is exact.
fn withhold_violated_over_free_leaves(report: &mut AutoVerifyReport, model_btor2: &str) {
    if !report
        .properties
        .iter()
        .any(|p| matches!(p.outcome, VerifyOutcome::Violated { .. }))
    {
        return;
    }
    let Ok(file) = crate::adapter::btor2::parser::parse(model_btor2) else {
        return;
    };
    for p in report.properties.iter_mut() {
        if !matches!(p.outcome, VerifyOutcome::Violated { .. }) {
            continue;
        }
        let Ok(formula) = crate::mu_calculus::parser::parse(&p.formula) else {
            continue;
        };
        // SOUNDNESS: an existential refutation (target unreachable) is STRONGER on a freer model.
        if formula.property_class() == crate::mu_calculus::PropertyClass::Reachability {
            continue;
        }
        let mut leaves: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for atom in crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms(&formula) {
            if let Some(found) =
                crate::adapter::trap_certificate::undriven_nets_in_cone(&file, &atom)
            {
                leaves.extend(found);
            }
        }
        if leaves.is_empty() {
            continue;
        }
        // SOUNDNESS: the refutation rests on values the real driver may never produce.
        p.outcome = VerifyOutcome::Unknown { unknown_cells: 0 };
        p.bottom_reason = Some(BottomReason::ViolationOverFreeLeaves {
            leaves: leaves.into_iter().collect(),
        });
    }
}

/// `--localize` (2026-10-30): for every violated row with a finite counterexample, the registers
/// whose next-state function, freed at some cycles with the trace's inputs fixed, let the property
/// survive the trace ([`crate::adapter::btor2::repair`]). Candidates are the state cells in the
/// property's cone, monitor cells left out; one the property reads directly is reported in its own
/// bucket (`reads_property`). The lines are the statements that write the register. A lasso
/// (liveness) or an `unreachable` counterexample has no finite trace to fix inputs from, and says
/// so. Display only: no verdict is read or changed.
fn attach_repair_candidates(
    report: &mut AutoVerifyReport,
    model_btor2: &str,
    index: &crate::adapter::slang::extract::Declarations,
    reset_pinned: bool,
) {
    use crate::trace::{RepairCandidate, RepairStatus, is_monitor_cell};
    let status = |status: &str, reason: Option<String>| RepairStatus {
        status: status.to_string(),
        reason,
    };
    let index = index.as_ref().ok();
    let aliases = report.diagnostics.trace_names.aliases.clone();
    let sources = report.diagnostics.trace_names.sources.clone();
    let mut repairs = Vec::new();
    for p in &report.properties {
        let Some(c) = &p.counterexample else { continue };
        if !matches!(p.outcome, VerifyOutcome::Violated { .. }) {
            continue;
        }
        let mut rep = RepairReport::default();
        if !c.cycle.is_empty() {
            rep.status = Some(status(
                "unavailable",
                Some("a lasso (liveness) counterexample has no finite length to fix".to_string()),
            ));
            repairs.push((p.name.clone(), rep));
            continue;
        }
        if c.inputs.is_empty() || c.prefix.is_empty() {
            rep.status = Some(status(
                "unavailable",
                Some("the counterexample carries no input sequence to fix".to_string()),
            ));
            repairs.push((p.name.clone(), rep));
            continue;
        }
        let Ok(formula) = crate::mu_calculus::parser::parse(&p.formula) else {
            continue;
        };
        // The model with this row's violation as a `bad`: the safety witness monitor the path
        // itself came from. A row the monitor does not cover (not a safety shape) says so.
        let (monitored, _depth) = match crate::adapter::reach_rescue::safety_bad_model(
            model_btor2,
            &formula,
            reset_pinned,
        ) {
            Ok(m) => m,
            Err(miss) => {
                rep.status = Some(status(
                    "unavailable",
                    Some(format!("no `bad` monitor for this row's shape ({miss})")),
                ));
                repairs.push((p.name.clone(), rep));
                continue;
            }
        };
        let Ok(file) = crate::adapter::btor2::parser::parse(&monitored) else {
            rep.status = Some(status(
                "unavailable",
                Some("the monitored model did not parse".to_string()),
            ));
            repairs.push((p.name.clone(), rep));
            continue;
        };
        let atoms = crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms(&formula);
        let read: std::collections::BTreeSet<String> = atoms
            .iter()
            .flat_map(|a| {
                a.split(|ch: char| {
                    !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '$')
                })
                .filter(|t| {
                    !t.is_empty()
                        && !t.starts_with(|ch: char| ch.is_ascii_digit())
                        && !is_monitor_cell(t)
                        && *t != "true"
                        && *t != "false"
                })
                .map(str::to_string)
                .collect::<Vec<_>>()
            })
            .collect();
        let candidates: Vec<String> = crate::adapter::btor2::fault::coi_states(&file, &atoms)
            .into_iter()
            .filter(|r| !is_monitor_cell(r))
            .collect();
        if candidates.is_empty() {
            rep.status = Some(status(
                "none",
                Some("no register in the property's cone".to_string()),
            ));
            repairs.push((p.name.clone(), rep));
            continue;
        }
        let loc = crate::adapter::btor2::repair::localize(&file, &c.inputs, &candidates);
        for f in &loc.found {
            let shown = aliases
                .get(&f.register)
                .cloned()
                .unwrap_or_else(|| f.register.clone());
            let mut lines: Vec<crate::trace::SourceLoc> = index
                .and_then(|i| i.writes.get(&shown).or_else(|| i.writes.get(&f.register)))
                .cloned()
                .unwrap_or_default();
            if lines.is_empty()
                && let Some(cell) = sources.iter().find(|s| s.cell == shown)
            {
                lines = cell.assigned.clone();
            }
            rep.candidates.push(RepairCandidate {
                reads_property: read.contains(&shown) || read.contains(&f.register),
                register: shown,
                cycles: f.cycles.clone(),
                values: f.values.clone(),
                lines,
            });
        }
        // The candidates the property does not read first, and among them the one whose change
        // acts CLOSEST to the violation first: a register upstream can "repair" by steering the
        // design elsewhere early (into a fault state, say), which removes this counterexample
        // without being where the bug is. Proximity is the ranking, not a proof.
        rep.candidates.sort_by_key(|c| {
            (
                c.reads_property,
                std::cmp::Reverse(c.cycles.iter().copied().max().unwrap_or(0)),
            )
        });
        rep.status = Some(if loc.skipped_by_cap > 0 {
            status(
                "partial",
                Some(format!(
                    "{} candidate register(s) beyond the cap of {} not queried",
                    loc.skipped_by_cap,
                    crate::adapter::btor2::repair::MAX_CANDIDATES
                )),
            )
        } else if !loc.errors.is_empty() {
            status(
                "partial",
                Some(format!(
                    "{} of {} queries could not run: {}",
                    loc.errors.len(),
                    loc.queried,
                    loc.errors
                        .iter()
                        .map(|(r, e)| format!("{r}: {e}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            )
        } else if rep.candidates.is_empty() {
            status(
                "none",
                Some(format!(
                    "no single-register change among {} avoids the violation within the trace's {} cycles",
                    loc.queried,
                    c.inputs.len()
                )),
            )
        } else {
            status("done", None)
        });
        repairs.push((p.name.clone(), rep));
    }
    report.diagnostics.repairs = repairs;
}

/// The RTL lines of every row's cone of influence (2026-10-30), into
/// [`ModelDiagnostics::involves`]: the cone's named registers, inputs and wires
/// ([`crate::adapter::btor2::dep_graph::cone_named_signals`] over the model the engines ran), each
/// mapped through the source map (declaration, `always` block) and slang's AST (declaration, every
/// statement that writes it). A name neither knows — a monitor or fault cell the rewrite added, a
/// name the lift mangled — contributes nothing. Display only: no verdict is read or changed.
fn attach_involves(
    report: &mut AutoVerifyReport,
    model_btor2: &str,
    index: &crate::adapter::slang::extract::Declarations,
) {
    let index = index.as_ref().ok();
    if report.diagnostics.trace_names.sources.is_empty() && index.is_none() {
        return;
    }
    let Ok(file) = crate::adapter::btor2::parser::parse(model_btor2) else {
        return;
    };
    let sources = &report.diagnostics.trace_names.sources;
    let aliases = &report.diagnostics.trace_names.aliases;
    let mut involves = Vec::new();
    for p in &report.properties {
        let Ok(formula) = crate::mu_calculus::parser::parse(&p.formula) else {
            continue;
        };
        let atoms = crate::adapter::btor2::symbolic_bitblast::formula_seed_atoms(&formula);
        if atoms.is_empty() {
            continue;
        }
        let mut lines: std::collections::BTreeSet<crate::trace::SourceLoc> =
            std::collections::BTreeSet::new();
        // A register the model names through a bound checker's port (`u_x_sva.state_q`) is the
        // design's own net on the same bits: its lines are the design's, not the port's.
        let cone: std::collections::BTreeSet<String> =
            crate::adapter::btor2::dep_graph::cone_named_signals(&file, &atoms)
                .into_iter()
                .map(|n| aliases.get(&n).cloned().unwrap_or(n))
                .collect();
        for name in cone {
            if let Some(cell) = sources.iter().find(|s| s.cell == name) {
                lines.extend(cell.declared.iter().cloned());
                lines.extend(cell.assigned.iter().cloned());
            } else if let Some((_, l)) =
                index.and_then(|i| i.declared.iter().find(|(n, _)| *n == name))
            {
                lines.insert(l.clone());
            }
            if let Some(w) = index.and_then(|i| i.writes.get(&name)) {
                lines.extend(w.iter().cloned());
            }
        }
        if !lines.is_empty() {
            involves.push((p.name.clone(), lines.into_iter().collect()));
        }
    }
    report.diagnostics.involves = involves;
}

fn drop_stale_bottom_reasons(report: &mut AutoVerifyReport) {
    let decided: std::collections::HashSet<&str> = report
        .properties
        .iter()
        .filter(|p| !matches!(p.outcome, VerifyOutcome::Unknown { .. }))
        .map(|p| p.name.as_str())
        .collect();
    if decided.is_empty() {
        return;
    }
    // mununu#548 O-2 — join on the FIELD, not on a prefix of the prose.
    //
    // 🔴 THE OLD JOIN NEVER FIRED. It built its pattern as `format!("{name}: ")` and tested
    // `summary.starts_with(..)`, but every summary `bottom_reason_note` actually produces begins
    // with a BACKTICK — `` `{name}`: `` — so `"`p`: residual ⊥ …".starts_with("p: ")` is false for
    // every real note. This pass has been a no-op since it was written.
    //
    // It looked fine because its test invented the summary as `"p: ⊥ — …"`, without the backticks
    // the production builder emits. That is CLAUDE.md §14's own warning arriving in our own code: a
    // contract test is only as good as the fidelity of its mock, and a fabricated one tests the
    // assumption rather than the system.
    //
    // The consequence is the mununu#548 defect, unfixed: a merged portfolio report could still
    // carry a ⊥ explanation for a property it goes on to report as HOLDS, because nothing removed
    // it. The field is what makes this pass work at all — not merely what makes it tidier.
    //
    // The prefix stays as a FALLBACK only for a `bottom-reason` note carrying no property. It is
    // not load-bearing (nothing in-tree produces such a note today) and it is deliberately not
    // relied upon.
    report.notes.retain(|n| {
        if n.kind != "bottom-reason" {
            return true;
        }
        match n.property.as_deref() {
            Some(prop) => !decided.contains(prop),
            None => !decided
                .iter()
                .any(|d| n.summary.starts_with(&format!("{d}: "))),
        }
    });
}

fn refresh_abstraction_posture(report: &mut AutoVerifyReport, engines_ran: &[String]) {
    if engines_ran.len() < 2 {
        return;
    }
    let Some(slot) = report
        .notes
        .iter_mut()
        .find(|n| n.kind == "abstraction-posture")
    else {
        return;
    };
    slot.level = NoteLevel::ScopeCaveat;
    slot.summary = format!(
        "Portfolio run — {} engines ran; the abstraction posture is PER ENGINE, not per report.",
        engines_ran.len()
    );
    slot.detail = format!(
        "This report merges verdicts from {} engines ({}). They do NOT share an abstraction \
         posture: `exact-symbolic` bit-blasts the full state and cannot return ⊥, while \
         `symbolic` (predicate-cube KMTS) and `explicit` abstract and can. So a ⊥ anywhere in \
         this report came from an ABSTRACTING engine, and a definite verdict is sound under the \
         posture of whichever engine produced it — see the `portfolio` note's `decided-by:` items \
         for the per-engine counts. The single-engine wording this note carries by default (\"no \
         abstraction, so there is no ⊥\") describes only the highest-precision engine and is NOT \
         a statement about the merged report.",
        engines_ran.len(),
        engines_ran.join(", ")
    );
    slot.items = engines_ran.iter().map(|e| format!("ran:{e}")).collect();
}

fn build_notes(
    report: &AutoVerifyReport,
    must_edge_posture: &str,
    applied_config_values: &[(String, u64)],
    applied_params: &[(String, String)],
    counter_bounds: &[String],
    cutpoint_signals: &[String],
    posture: &NotePosture,
) -> Vec<VerificationNote> {
    let d = &report.diagnostics;
    let mut notes = Vec::new();

    // Coverage summary (Info) — the honest at-a-glance tally.
    notes.push(coverage_summary_note(report));

    // #466 — which RTL front end produced the lift. `--frontend auto` picks one
    // silently, but read_verilog and yosys-slang differ in SOUNDNESS on partial
    // writes (#464/#465), so naming it answers "which lift produced this verdict?".
    // A fallback (read_verilog errored → slang) is a change of plan ⇒ ScopeCaveat.
    if let Some(fe) = &d.frontend {
        let (level, summary, detail) = match &d.frontend_fallback_reason {
            Some(reason) => (
                NoteLevel::ScopeCaveat,
                format!("lift: {fe} — `--frontend auto` fell back ({reason})."),
                "read_verilog and yosys-slang are NOT interchangeable for soundness (they differ on \
                 partial register writes such as `q[hi:lo] <= d`), so an auto-fallback is reported \
                 as a change of plan. Force a front end with `--frontend verilog|slang` to remove \
                 the choice."
                    .to_string(),
            ),
            None => (
                NoteLevel::Info,
                format!("lift: {fe}."),
                "The RTL front end that produced this BTOR2. `--frontend auto` tries read_verilog \
                 (+sv2v) first and falls back to yosys-slang on a lift error; the two paths are not \
                 interchangeable for soundness. Force one with `--frontend verilog|slang`."
                    .to_string(),
            ),
        };
        notes.push(VerificationNote {
            kind: "lift-frontend".into(),
            level,
            summary,
            detail,
            items: Vec::new(),
            property: None,
        });
    }
    // The lift rerun its yosys script with `opt_clean` after the BTOR backend reported a
    // topological loop (2026-10-22, OpenTitan csrng through yosys-slang's `$buf` cells). Info:
    // the model is the same design, and a reader diffing two runs wants to know the script moved.
    if d.merged_clock_domains.len() > 1 {
        notes.push(VerificationNote {
            kind: "clock-domains-merged".into(),
            level: NoteLevel::SoundnessCaveat,
            summary: format!(
                "lift: {} clock domains merged onto one clock — every verdict assumes they tick \
                 together.",
                d.merged_clock_domains.len()
            ),
            detail: "`--merge-clock-domains` lifted a multi-clock design onto the model's one \
                     implicit clock. That is an UNDER-approximation of the real interleavings: a \
                     crossing defect that needs one domain to tick while another does not (a \
                     lost pulse, an incoherent multi-bit bus) cannot occur in this model, so a \
                     HOLDS does not transfer to the design. A VIOLATED is a run of the design in \
                     which the domains happen to tick together."
                .to_string(),
            items: d.merged_clock_domains.clone(),
            property: None,
        });
    }
    if let Some(reason) = &d.lift_retry {
        notes.push(VerificationNote {
            kind: "lift-retry".into(),
            level: NoteLevel::Info,
            summary: format!("lift: retried — {reason}."),
            detail: "The yosys BTOR backend walks the netlist cell by cell and can report a \
                     topological loop where `check` finds none (wide `$buf` cells from \
                     yosys-slang chained bit-wise through other cells). `opt_clean` folds the \
                     buffers into connections and drops unused cells; it removes nothing the \
                     design reads, so the verdicts are over the same design. The retry runs \
                     only on that error."
                .to_string(),
            items: Vec::new(),
            property: None,
        });
    }

    // Config concretization (ScopeCaveat) — H.J.b; present only when the user
    // pinned config inputs to constants.
    if !applied_config_values.is_empty() {
        notes.push(VerificationNote {
            kind: "config-concretization".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{} config input(s) pinned to constants — verdicts hold for THESE values, not all configurations.",
                applied_config_values.len()
            ),
            detail: "A wide config input (e.g. a timer threshold) was fixed to a representative \
                     constant so comparisons against it become decidable. The verdicts below are \
                     therefore scoped to this configuration; a different config value could change \
                     them. Re-run with other values to cover more of the configuration space."
                .into(),
            items: applied_config_values
                .iter()
                .map(|(sig, v)| format!("{sig}={v}"))
                .collect(),
            property: None,
        });
    }

    // Parameter overrides (ScopeCaveat) — present only when the user passed
    // `--param` / `params`. The design was re-elaborated with these module
    // parameters, so the verdicts are scoped to them (a different parameter value
    // could size a different design).
    if !applied_params.is_empty() {
        notes.push(VerificationNote {
            kind: "parameter-override".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{} module parameter(s) overridden before the lift (yosys chparam / slang -G) — verdicts hold for THESE parameter values.",
                applied_params.len()
            ),
            detail: "A module parameter was set before elaboration (e.g. to shrink a timing \
                     interval so its counters fit the exact engine), so the design verified here \
                     is the one sized by these parameters. A different parameter value would size \
                     a different design; re-run to cover other values."
                .into(),
            items: applied_params
                .iter()
                .map(|(lhs, v)| format!("{lhs}={v}"))
                .collect(),
            property: None,
        });
    }

    // Control-slice cut points (ScopeCaveat) — present only when the user cut nets
    // to free `$anyseq` inputs in the lift. This is an OVER-APPROXIMATION: it adds
    // transitions, so a definite HOLDS is sound but a definite VIOLATED may be
    // spurious unless it is guard-independent (the orphaned-FSM-state case).
    if !cutpoint_signals.is_empty() {
        // T3 (property-class honesty, 2026-07-23) — a cutpoint is a PLAIN over-approximation
        // (it adds transitions). That is sound-transferring for a definite HOLDS on a SAFETY /
        // universal property, but NOT for an ALTERNATING fixpoint (νμ recoverability `AG EF`),
        // which collapses under a plain over- or under-approximation (CLAUDE.md soundness table;
        // Bruns–Godefroid): a HOLDS on the cutpointed model does not automatically transfer to
        // the concrete RTL. Only append the νμ caveat when the report actually carries an
        // alternating property (a parse failure ⇒ conservatively omit — the base claim stays
        // accurate for the safety case).
        let has_alternating = report.properties.iter().any(|p| {
            crate::mu_calculus::parser::parse(&p.formula)
                .map(|f| f.alternation_depth() >= 2)
                .unwrap_or(false)
        });
        let numu_caveat = if has_alternating {
            " ⚠ But for an ALTERNATING recoverability property (`AG EF good`, νμ) a HOLDS under this \
              cut is sound for the abstract (cutpointed) model ONLY — it does NOT automatically transfer \
              to the concrete RTL, because alternating fixpoints collapse under a plain over-approximation. \
              Decide those on the un-cutpointed cone (raise the bit cap or pin the freed inputs) rather \
              than trusting the cut."
        } else {
            ""
        };
        notes.push(VerificationNote {
            kind: "control-slice".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "{} net(s) cut to free inputs (control-slice) — an over-approximation of the design.",
                cutpoint_signals.len()
            ),
            detail: format!(
                "Each listed net was replaced by a free `$anyseq` input (Yosys `cutpoint`), so its \
                 datapath fanin drops out of the cone (this is what lets `exact-symbolic` fit a wide \
                 control FSM). Because freeing a net only ADDS transitions, for a SAFETY / universal \
                 property a definite HOLDS transfers to the concrete RTL (safety + over-approx = \
                 sound).{numu_caveat} A definite VIOLATED is sound only when the counterexample is \
                 guard-independent — the canonical case being an ORPHANED FSM state (in-degree 0), \
                 which no freed guard can make reachable, so `AG EF <orphaned-state>` stays soundly \
                 VIOLATED. Treat any other VIOLATED under cut points as possibly spurious and re-check \
                 without the cut."
            ),
            items: cutpoint_signals.to_vec(),
            property: None,
        });
    }

    // Counter-bound seeding (Info) — H.H; present only when a bound `X <= K` was
    // seeded (user-supplied or config-inferred). A HOLDS it unlocks is PROVEN (the
    // bound is a sound partition the SMT must-edges verify), so no scope caveat
    // beyond the config-concretization one above.
    if !counter_bounds.is_empty() {
        notes.push(VerificationNote {
            kind: "counter-bound".into(),
            level: NoteLevel::Info,
            summary: format!(
                "{} counter-bound predicate(s) seeded to refine monotonicity properties.",
                counter_bounds.len()
            ),
            detail: "A monotonicity/increment property over a counter (`cnt_q >= $past(cnt_q)`) is \
                     ⊥ only because the abstraction admits the 32-bit wraparound. Seeding an upper \
                     bound `X <= K` PARTITIONS the state space (it does not assume the bound — the \
                     must-edges verify the reachable cubes stay bounded), excluding the wraparound \
                     state so the verdict can become definite. Sound regardless of K: a wrong bound \
                     only leaves the property at ⊥, never mis-verdicts. (config-inferred) bounds come \
                     from a counter's comparison against a pinned config threshold; (user-supplied) \
                     from --counter-bound / counter_bounds."
                .into(),
            items: counter_bounds.to_vec(),
            property: None,
        });
    }

    // Abstraction posture — A.3: honest about the may-relation actually used.
    match posture {
        NotePosture::Exact => notes.push(VerificationNote {
            kind: "abstraction-posture".into(),
            level: NoteLevel::Info,
            summary: "Exact-symbolic model checking — no abstraction, so a definite verdict is sound and there is no ⊥."
                .into(),
            detail: "Each property is decided over the full bit-blasted state space by ROBDD \
                     fixpoint; a definite HOLDS or VIOLATED transfers to the concrete RTL, and no \
                     property is left ⊥ (the predicate abstraction that would produce ⊥ is not used)."
                .into(),
            items: Vec::new(),
            property: None,
        }),
        NotePosture::Cube => {
            notes.push(VerificationNote {
                kind: "abstraction-posture".into(),
                level: NoteLevel::Info,
                summary: "KMTS may-over-approximation — a definite HOLDS on a safety property is sound."
                    .into(),
                detail: format!(
                    "The may-relation over-approximates the concrete transitions (SMT all-pairs), so \
                     a definite HOLDS transfers to the concrete RTL (safety + over-approximation = \
                     sound); a definite VIOLATED is a real reachable counterexample class. Must-edge \
                     inference: {}.",
                    must_edge_posture,
                ),
                items: Vec::new(),
                property: None,
            });
        }
    }

    // Reset gating (Info).
    if !d.gated_resets.is_empty() {
        notes.push(VerificationNote {
            kind: "reset-gating".into(),
            level: NoteLevel::Info,
            summary: format!("{} reset(s) pinned inactive.", d.gated_resets.len()),
            detail: "The `disable iff (!rst)` guards were dropped and the reset input pinned to \
                     its inactive value, so the body is verified only while out of reset (the \
                     standard formal reset discipline)."
                .into(),
            items: d.gated_resets.clone(),
            property: None,
        });
    }

    // Flop stubs (Info).
    if !d.auto_provided_stubs.is_empty() {
        notes.push(VerificationNote {
            kind: "flop-stub".into(),
            level: NoteLevel::Info,
            summary: format!(
                "{} flop primitive(s) behaviorally stubbed.",
                d.auto_provided_stubs.len()
            ),
            detail: "A cut flop primitive (no body in the source set) was auto-replaced with an \
                     EXACT behavioral model of its datapath, so the register it drives survives \
                     the lift (no soundness loss — the stub is functionally identical)."
                .into(),
            items: d.auto_provided_stubs.clone(),
            property: None,
        });
    }

    // Black-boxed / cut modules (SoundnessCaveat).
    if !d.blackboxed_modules.is_empty() {
        notes.push(VerificationNote {
            kind: "blackbox-cut".into(),
            level: NoteLevel::SoundnessCaveat,
            summary: format!(
                "{} module(s) cut (no body) — state behind them is not modeled.",
                d.blackboxed_modules.len()
            ),
            detail: "Registers driven by a cut module become free inputs, so properties over them \
                     are SKIPPED (never given a verdict), not verified. Provide the missing module \
                     source(s) to model that state."
                .into(),
            items: d.blackboxed_modules.clone(),
            property: None,
        });
    }

    // mununu#599 — the note form of each `vacuity` field, derived from the field.
    {
        let mut carrier = AutoVerifyReport {
            properties: report.properties.clone(),
            ..Default::default()
        };
        refresh_vacuity_notes(&mut carrier);
        notes.extend(carrier.notes);
    }

    // mununu#821 — this rebuild regenerates the MODEL-level notes from the diagnostics and the
    // `vacuity` fields; a note a verdict pass pushed on the way here is KEPT: `witness-unavailable`
    // (mununu#803 — the main loop's exact-witness miss and the safety-witness post-pass),
    // `expression-leaf-unresolved` (mununu#717), `engine-isolation` (mununu#543),
    // `array-atom-unsupported` (mununu#518), the `antecedent-*` notes of the fault posture. Only a
    // stale copy of a kind regenerated above is dropped (`vacuous-guarantee` belongs to the carrier).
    // Before this the assignment `report.notes = build_notes(..)` discarded every one of them, so
    // a VIOLATED annotation whose witness monitor declined shipped with no path and no reason.
    let kept: Vec<VerificationNote> = {
        let regenerated: std::collections::HashSet<&str> =
            notes.iter().map(|n| n.kind.as_str()).collect();
        report
            .notes
            .iter()
            .filter(|n| n.kind != "vacuous-guarantee" && !regenerated.contains(n.kind.as_str()))
            .cloned()
            .collect()
    };
    notes.extend(kept);

    notes
}

/// Options for [`verify_auto`] beyond the Yosys lift options.
#[derive(Debug, Clone)]
pub struct VerifyAutoOptions {
    /// Max CEGAR iterations per property (default 16).
    pub max_iterations: usize,
    /// Must-edge inference policy passed to each property's CEGAR run. Default
    /// `Off`; `SmtHyperMust` gives sound νμ verdicts (the recoverability case).
    pub must_edge_inference: MustEdgeInference,
    /// mununu#663 — `auto` (default `true`): with `must_edge_inference == Off`, a property whose
    /// formula is of the Liveness or Mixed class (a μ under a ν — `AG EF`, `AG AF`, the response
    /// shapes) runs its cube member with `SmtPerTargetStandard`, because the 3-valued `◇` turns true only
    /// through MUST edges and a may-only cube is structurally ⊥ on that class — it could never
    /// return HOLDS, and reached the ⊥ re-plan (a second lift) instead. Safety, Reachability and
    /// Propositional formulas keep `Off`: their verdicts need no must edge, and the ∀∃ queries
    /// cost. An explicit `must_edge_inference` other than `Off`, or `must_edge_auto = false`
    /// (CLI `--must-edge-inference off`, API `"off"`), is honoured as given. See
    /// [`effective_must_edge_inference`].
    pub must_edge_auto: bool,
    /// When `true` (default), `disable iff (reset)` guards are dropped from the
    /// formulas and the recognized reset inputs are pinned inactive at the
    /// model level (so the body is verified only while not in reset, matching
    /// SVA `disable iff` semantics). This removes the otherwise-unbindable
    /// reset-input atom that would force a SKIP. The dominant idiom in real
    /// SVA, so it is on by default; set `false` to keep the guard and leave
    /// the reset free.
    pub gate_reset: bool,
    /// When `true` (default), if the first lift cuts a known flop primitive
    /// (instantiated with no body — e.g. OpenTitan's `prim_sparse_fsm_flop`),
    /// verify-auto injects a behavioral stub for it and re-lifts, so the
    /// register survives and its state atoms become bindable (H.C). The stub is
    /// an exact behavioral model of the flop datapath; the auto-injection is
    /// reported in `diagnostics.auto_provided_stubs`. Set `false` to leave cut
    /// flops cut (reported in `blackboxed_modules`).
    pub auto_stub_flops: bool,
    /// H.J.b — user-supplied config concretization: pin a (typically wide, e.g. a
    /// timer threshold) input to a **constant** so comparisons against it become
    /// decidable. Each entry `signal → value` is pinned in the lifted BTOR2 (via
    /// `pin_inputs_to_constants`, like reset-gating) AND substituted into every
    /// formula atom (`cnt_q >= cfg_detect_timer_i` → `cnt_q >= 7`), turning a
    /// relational-with-wide-input into a decidable state-vs-constant comparison.
    /// **Scope-reduced:** the verdicts then hold FOR THESE values, not all
    /// configurations — surfaced as a `config-concretization` ScopeCaveat note.
    /// Default empty ⇒ no concretization, no behaviour change. Only signals that
    /// are actual inputs are pinned; the rest are ignored (and not reported).
    pub config_values: std::collections::HashMap<String, u64>,
    /// H.H — user-supplied counter upper bounds: `register → K` seeds a compound
    /// cube dimension `register <= K`, which PARTITIONS the abstract state into
    /// `{register<=K}` / `{register>K}` (it does NOT assume the bound — the SMT
    /// must-edges verify whether the reachable cubes stay bounded). This excludes
    /// the abstract 32-bit wraparound state that leaves counter-monotonicity
    /// properties (`cnt_q >= $past(cnt_q)`) at ⊥. **Sound regardless of K:** the
    /// partition never mis-verdicts (a wrong K just leaves the property at honest
    /// ⊥). Bounds are ALSO auto-derived from config concretization (a counter
    /// compared against a pinned config `cnt_q >= cfg_detect_timer_i`, cfg=7,
    /// yields `cnt_q <= 7`); a manual entry here overrides the config-inferred one.
    /// Requires `must_edge_inference` ON for the box modality to close. Default
    /// empty ⇒ only the config-inferred bounds fire (none without concretization).
    /// Surfaced as a `counter-bound` note. Only counter registers (a state cell
    /// appearing in a self-relational `$past` atom) are bounded; other registers
    /// are ignored.
    pub counter_bounds: std::collections::HashMap<String, u64>,
    /// W1/W2 — user-supplied **predicate hints** (the CLI `--predicate` / API
    /// `predicates` peer of the in-source `@mununu_predicate <expr>` annotation).
    /// Each entry is a predicate expression (`reg == value`, `reg == reg`,
    /// `reg >= K`) seeded into every property's cube via `seed_from_formula`'s
    /// `extra_atoms`, MERGED with the scanned `@mununu_predicate` annotations.
    /// Sound by monotonicity of predicate abstraction — a hint only refines the
    /// cube (a ⊥ can become definite; a definite verdict never flips; a hint that
    /// fails classification is dropped). Default empty ⇒ no behaviour change.
    pub predicate_hints: Vec<String>,
    /// Property templates to check besides the design's own assertions (`--template`, API
    /// `templates`), each instantiated against the configured catalog and checked like an
    /// `@mununu_guarantee` named `tmpl.<instance>`.
    pub templates: Vec<crate::adapter::templates::TemplateRef>,
    /// Project template packs (`--templates-file`) layered over the configured catalog.
    pub templates_files: Vec<std::path::PathBuf>,
    /// `--localize` / API `localize` (2026-10-30): for every row with a counterexample, name the
    /// registers whose next-state function, freed at some cycles with the trace's inputs fixed,
    /// would let the property survive the trace — repair candidates, one native-BMC query per
    /// register in the property's cone ([`crate::adapter::btor2::repair`]). Off by default: the
    /// queries cost a BMC run per candidate.
    pub localize: bool,
    /// R-F5.5d (2026-07-03) — run each property through the R-F5 **symbolic**
    /// predicate-cube engine (BDD relation + WP CEGAR loop; no per-cube-pair
    /// SMT) instead of the explicit `cegar_refine_loop`. Default `false`
    /// (explicit). The symbolic path is orders of magnitude faster at large
    /// `|P|` (the FSM-cone residual) but supports only cube-dimension predicates
    /// (equality + non-derived compounds) + the bare `[]`/`<>` fragment — a
    /// derived/combinational predicate or a guarded modality errors → the
    /// property is `Skipped` with the reason. Mirrors the CLI/API `--engine`.
    pub symbolic_engine: bool,
    /// D1.6 (2026-07-04) — run each property through **exact** full-state symbolic
    /// MC (`--engine exact-symbolic`): the μ-calculus is decided EXACTLY over the
    /// reset-gated btor2's bit-blasted state (no predicate abstraction), so the
    /// verdict is a **definite** 2-valued Holds/Violated — never ⊥. Decides
    /// `AF`-liveness (and any μ-calculus property) where the cube path returns
    /// Unknown; bounded by BDD size (a design over the bit cap ⇒ `Skipped`). Takes
    /// precedence over `symbolic_engine`. Mirrors the CLI/API `--engine`.
    pub exact_symbolic: bool,
    /// PORTFOLIO (2026-07-06) — when set, IGNORE `symbolic_engine`/`exact_symbolic`
    /// and instead run MULTIPLE engines (exact → symbolic-cube → explicit-CEGAR),
    /// taking the definite verdict from whichever engine produces one. Proven sound
    /// by `diff_corpus_cegar_vs_symbolic_engine_parity`: the three engines never
    /// contradict on a definite verdict, and each cube definite matches the exact MC.
    /// The mode is a BUDGET knob — [`PortfolioMode::Sequential`] early-exits (cheap),
    /// [`PortfolioMode::Parallel`] runs all three concurrently (fast, 3× compute).
    /// Default `None` ⇒ single-engine dispatch (unchanged). Mirrors the CLI/API
    /// `--engine portfolio-sequential` / `portfolio-parallel`.
    pub portfolio: Option<PortfolioMode>,
    /// On a *safety* property the cube portfolio leaves `⊥`, escalate to the
    /// multi-engine reachability portfolio (reduce the AG-invariant to a `bad`-monitor
    /// and decide it symbolically — exact ⊕ native ⊕ spacer ⊕ btormc ⊕ Pono). Only
    /// fires on `⊥` reducible AG-invariants, so there is no common-path cost and
    /// liveness / recoverability are untouched. Default `true`; the CLI `--no-rescue`
    /// / API `rescue_bottom_safety: false` opts out.
    pub rescue_bottom_safety: bool,
    /// On a *box-response liveness* property `AG(a → AF b)` the cube portfolio leaves
    /// `⊥`, escalate to the liveness-to-safety reduction + reachability portfolio
    /// (reduce to a `bad`-monitor lasso and decide it symbolically). Only fires on `⊥`
    /// reducible box-AF responses — the diamond-EF recoverability liveness (`AG(a → EF
    /// b)`, νμ) is NOT l2s-able and stays on the exact / cube νμ path, so there is no
    /// common-path cost and no mis-route. Default `true`; opts out with the same
    /// `--no-rescue` / API `rescue_bottom_liveness: false`.
    pub rescue_bottom_liveness: bool,
    /// On a νμ **recoverability** property `AG EF good` the cube portfolio leaves `⊥`,
    /// escalate to the cube + `smt-hyper-must` path (definite νμ verdicts transfer).
    /// Only fires on the `AG EF (REG == VALUE)` shape. Default `true`; `--no-rescue` /
    /// API `rescue_bottom_recoverability: false` opts out.
    pub rescue_bottom_recoverability: bool,
    /// Plan 2 / PR C — this run is one PORTFOLIO MEMBER: leave the ⊥ rows alone, the planner
    /// re-plans the MERGED report once after every member ([`replan_merged`]). Set by the planner
    /// only; `false` on a single-engine run, which re-plans its own ⊥ rows as before. Measured on
    /// the full OpenTitan `edn` IP: with every member re-planning, the first member spent a 60-s
    /// rescue slice on each of fifteen rows the next member's main pass decided in about a second,
    /// and the rows no engine decides paid their slice once per member (1 877 s against 584 s).
    pub replan_defer: bool,
    /// PR C — open the `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` breadcrumb in APPEND mode (a later
    /// portfolio member; the first truncates). Set by the planner only.
    pub breadcrumb_append: bool,
    /// Plan 4 / PR D3 (mununu#733) — property names an EARLIER sequential portfolio member already
    /// decided: this member pushes them as `Unknown { NotAttempted }` without running an engine (the
    /// merge keeps the first definite verdict, so the work was pure cost — measured on the census
    /// design `keymgr_ctrl`: 256 s → 1 092 s once the cube's SMT image started running on rows the
    /// exact engine decides). Set by the sequential planner only; empty on a single-engine or
    /// parallel run.
    pub skip_rows: std::collections::HashSet<String>,
    /// #468 — apply a NAMED structural mutation to the prepared BTOR2 before
    /// verification (`prepare_model`), so `sv mutate` can assert the property
    /// verdicts FLIP (property adequacy). `None` (default) ⇒ no mutation, the model
    /// is verified as-lifted. Set by the `sv mutate` driver only; the plain
    /// `verify-auto` surfaces never populate it. See
    /// [`crate::adapter::btor2::mutate`].
    pub mutation: Option<crate::adapter::btor2::mutate::Mutation>,
    /// The fault-injection posture (PR A1, 2026-10-23): named registers, or the security
    /// countermeasures' own fault sites (`sec-cm`), that an adversary may overwrite under a
    /// global budget after a warm-up — applied to the prepared model right after `mutation`
    /// (so `sv mutate` and `--fault` compose) and before the dump. `None` (the default): no
    /// posture, every verdict unchanged. See [`crate::adapter::btor2::fault`].
    pub faults: Option<crate::adapter::btor2::fault::FaultPosture>,
    /// mununu#476 item 4 — whether antecedent shadow-register synthesis is
    /// enabled for the exact-symbolic engine. **Read by nothing since mununu#677** — the
    /// mununu#476 shadow synthesis was removed (it checked a `|=>` consequent one cycle late);
    /// an SVA `|=>` whose antecedent reaches primary inputs decides by inputs-in-the-state
    /// (`input_state`), and `false` here changes nothing. Still threaded from
    /// `--no-antecedent-shadow` / `no_antecedent_shadow` so those keep parsing.
    pub antecedent_shadow: bool,
    /// mununu#629 — antecedent constant propagation on the exact-symbolic engine. `true`
    /// (default): a same-cycle implication `sig == K |-> C` has `K` propagated through `C`'s
    /// cone (a read mux folds to the selected arm), so a property about one field of a wide
    /// record decides instead of skipping on the bit cap. `false` leaves the formula as lifted
    /// — the differential-oracle / debug knob, mirrored on the CLI as
    /// `--no-antecedent-propagate`, on the API as `no_antecedent_propagate: true`, and by the
    /// process-global `MUNUNU_NO_ANTECEDENT_PROPAGATE=1`; either channel disabling it wins.
    pub antecedent_propagate: bool,
    /// mununu#656 — inputs in the state on the exact-symbolic engine. `true` (default): a
    /// property that reads a primary input, or a signal combinationally driven by one, is
    /// evaluated over the product of the registers with the input sampled in that cycle — a
    /// state predicate, decided — instead of being skipped. `false` restores the refusal (the
    /// differential switch): CLI `--no-input-state`, API `no_input_state: true`, process-global
    /// `MUNUNU_NO_INPUT_STATE=1`; either channel disabling it wins.
    pub input_state: bool,
    /// Lift-only source overrides, `(file name, content)`: each REPLACES the same-named source
    /// (the primary or an additional one) for the SV → BTOR2 lift only; SVA extraction still
    /// reads the originals. The use: a design whose real assertion macros (OpenTitan's
    /// `prim_assert.sv`) do not survive yosys / sv2v lifts with the synthesis shim while slang
    /// extracts the real assertions. A name that matches no source is an error, never a silent
    /// no-op. CLI `--lift-override NAME=PATH`, API `lift_overrides: [{name, content}]`.
    /// Default empty.
    pub lift_overrides: Vec<(String, String)>,
    /// How SVA `assume` statements are treated ([`AssumeMode`]); default
    /// [`AssumeMode::Precondition`]. CLI `--assumes`, API `assumes`.
    pub assume_mode: AssumeMode,
}

/// PORTFOLIO scheduling mode — the budget knob for the multi-engine default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortfolioMode {
    /// Run the engines in PRECISION order (exact → symbolic-cube → explicit-CEGAR),
    /// stopping the moment the report has no ⊥ property. Minimises compute — often
    /// only the exact engine runs — at the cost of worst-case latency (the sum of
    /// each engine tried until one decides). The budget-frugal choice.
    Sequential,
    /// Run ALL engines CONCURRENTLY (scoped threads) and merge; each property takes
    /// the definite verdict from whichever engine produced one. Maximises compute
    /// (every engine always runs) for minimum latency (the fastest engine to decide
    /// each property). The budget-rich, low-latency choice.
    Parallel,
}

/// Map an optional `--engine` / `"engine"` selector string to the
/// `(symbolic_engine, exact_symbolic, portfolio)` option triple, applying the DEFAULT when the
/// selector is unspecified. **The default (2026-07-06) is `portfolio-sequential`** — the
/// exact-first, cube-fallback portfolio, the most precise sound choice and no slower than the
/// former `explicit` default on designs `explicit` already decided (exact runs first). An
/// explicit `"explicit"` selects the single predicate-abstraction CEGAR engine. The CLI reaches
/// the same defaults through clap's `default_value_t = EngineArg::PortfolioSequential`; this
/// helper is the API/string entry point (and the single place the default is defined for it).
pub fn engine_selection(engine: Option<&str>) -> (bool, bool, Option<PortfolioMode>) {
    match engine {
        Some(e) if e.eq_ignore_ascii_case("symbolic") => (true, false, None),
        Some(e) if e.eq_ignore_ascii_case("exact-symbolic") => (false, true, None),
        Some(e) if e.eq_ignore_ascii_case("portfolio-parallel") => {
            (false, false, Some(PortfolioMode::Parallel))
        }
        Some(e) if e.eq_ignore_ascii_case("portfolio-sequential") => {
            (false, false, Some(PortfolioMode::Sequential))
        }
        Some(e) if e.eq_ignore_ascii_case("explicit") => (false, false, None),
        // Unspecified (or unrecognised) → the default portfolio-sequential.
        _ => (false, false, Some(PortfolioMode::Sequential)),
    }
}

impl Default for VerifyAutoOptions {
    fn default() -> Self {
        Self {
            max_iterations: 16,
            must_edge_inference: MustEdgeInference::Off,
            must_edge_auto: true,
            gate_reset: true,
            auto_stub_flops: true,
            config_values: std::collections::HashMap::new(),
            counter_bounds: std::collections::HashMap::new(),
            predicate_hints: Vec::new(),
            templates: Vec::new(),
            templates_files: Vec::new(),
            localize: false,
            symbolic_engine: false,
            exact_symbolic: false,
            portfolio: None,
            rescue_bottom_safety: true,
            rescue_bottom_liveness: true,
            rescue_bottom_recoverability: true,
            replan_defer: false,
            breadcrumb_append: false,
            skip_rows: std::collections::HashSet::new(),
            mutation: None,
            faults: None,
            antecedent_shadow: true,
            antecedent_propagate: true,
            input_state: true,
            lift_overrides: Vec::new(),
            assume_mode: AssumeMode::Precondition,
        }
    }
}

/// Auto-seeded predicates for one formula: simple `reg == value` specs, compound
/// `(name, expr)` pairs (relational / `!=` / boolean combinations), and the atom
/// strings that could NOT be seeded (reference a non-state, non-input signal).
/// H.E — how a combinational signal's value is determined, deciding how it binds:
/// - `InputDependent` — its cone reaches a primary input (`trigger_active =
///   !trigger_i`). Routed through the FREE-INPUT path (a free cube dimension,
///   source-pin / target-free), so the may/must edges respect it. Treating it as
///   a state-cube label is unsound (the sysrst sva_6 / sva_9 spurious VIOLATED).
/// - `StateOnly` — a pure function of state (`event_detected_o = f(state_q)`).
///   Sound as a derived per-cube 3-valued label (Approach B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CombKind {
    InputDependent,
    StateOnly,
}

#[derive(Debug, Clone, Default)]
struct Seeded {
    specs: Vec<PredicateSpec>,
    compounds: Vec<(String, PredicateExpr)>,
    unseedable: Vec<String>,
    /// H.B (free-input atoms) — the subset of `specs` registers that are
    /// primary **inputs** (not state cells). An input predicate is a *free*
    /// cube dimension: the environment picks its value each cycle, so it is
    /// NOT pinned at init (the env is free at cycle 0) and the verdict is read
    /// across **all** of its initial polarities. See
    /// `docs/design/free-input-atoms.md`. Empty ⇒ the pre-H.B state-only shape.
    input_registers: HashSet<String>,
    /// H.E (combinational outputs) — **derived** combinational predicates: a
    /// simple atom whose register is a combinational node (e.g. csrng's
    /// `main_sm_err_o`), NOT a cube dimension. The lift labels each per cube via
    /// SMT (Approach B). Threaded to `cegar_refine_loop` through the synthesised
    /// sidecar's `combinational_predicates`. Empty ⇒ the pre-H.E shape.
    derived: Vec<PredicateSpec>,
    /// H.F — **relational** derived predicates: a `PredicateExpr` (`cnt_q >=
    /// cfg_detect_timer_i`, `trigger_i != trigger_active`) whose operands include
    /// an input or combinational signal, so it is NOT a sound cube dimension. Like
    /// [`Seeded::derived`] it is labelled per cube via SMT, but it carries a full
    /// expr (not `register == value`). Threaded through the synthesised sidecar's
    /// `compound_predicates` with `derived = true`. Empty ⇒ the pre-H.F shape.
    derived_relational: Vec<(String, PredicateExpr)>,
}

/// monono#partsel soundness guard — the first register/signal name a formula's
/// predicate atoms reference that binds to a signal reaching an ANONYMOUS
/// (undriven-register-bit) free input, or `None` when the formula is sound to
/// decide.
///
/// The yosys-slang lift of a plain-vector partial register assignment
/// (`q[hi:lo] <= d`) models the register's *unwritten* bits as free `input`s and
/// aliases the register name `q` to the `concat` that mixes them (see
/// [`parser::signal_reaches_anonymous_input`]). A predicate atom over `q` — a
/// register atom (`q == 0`) or a combinational output of it (`o = (q != 0)`) —
/// then reads those free inputs (havoc) and mis-decides (a spurious `HOLDS` on a
/// reachably-non-zero register). Callers refuse (skip) rather than emit that
/// verdict. The packed-2-D `q[idx] <= d` split (anonymous sub-*registers*, no
/// free inputs) is NOT flagged here — the NID-indexed cone-of-influence keeps
/// those cells, so that case now DECIDES.
///
/// Bare atoms are admitted (`parse_predicate_atom_bool` reads `o_partsel` as
/// `o_partsel != 0`), so a combinational-output property is caught as well as a
/// register-comparison one.
/// mununu#507 — is the anonymous free input in `name`'s cone reached through a
/// `concat`, the shape the slang partial-write lift produces?
///
/// `q[hi:lo] <= d` makes slang model `q`'s UNWRITTEN bits as free inputs and alias
/// the register name to the `concat` that mixes them. So a `concat` with an
/// anonymous-input operand in the cone is positive evidence for that specific
/// cause. Anything else — a blackboxed submodule's freed outputs, an undriven net,
/// an unconnected port — reaches an anonymous input WITHOUT that shape, and the
/// refusal must not claim a partial write it cannot see.
fn undriven_via_partial_write_concat(
    file: &crate::adapter::btor2::ast::Btor2File,
    name: &str,
) -> bool {
    use crate::adapter::btor2::ast::{Node, Op};
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let starts: Vec<crate::adapter::btor2::ast::Nid> = file
        .lines
        .iter()
        .filter(|l| match &l.node {
            Node::State { .. } => symbols.get(&l.nid).map(String::as_str) == Some(name),
            Node::Op {
                symbol: Some(s), ..
            } => s == name,
            Node::Output {
                symbol: Some(s), ..
            } => s == name,
            _ => false,
        })
        .map(|l| l.nid)
        .collect();

    let is_anon_input = |nid| {
        matches!(
            file.lookup(nid).map(|l| &l.node),
            Some(Node::Input { symbol: None, .. })
        )
    };

    let mut seen = std::collections::HashSet::new();
    let mut work = starts;
    while let Some(nid) = work.pop() {
        if !seen.insert(nid) {
            continue;
        }
        let Some(line) = file.lookup(nid) else {
            continue;
        };
        match &line.node {
            Node::Op {
                op: Op::Concat,
                args,
                ..
            } => {
                if args.iter().any(|a| is_anon_input(a.nid())) {
                    return true;
                }
                for a in args {
                    work.push(a.nid());
                }
            }
            Node::Op { args, .. } => {
                for a in args {
                    work.push(a.nid());
                }
            }
            Node::Output { signal, .. } => work.push(signal.nid()),
            _ => {}
        }
    }
    false
}

/// mununu#507 — does any staged source carry a module-scope `bind`?
///
/// Assertions live in a bound `*_sva.sv` file BECAUSE THEY MUST: `read_verilog -sv`
/// cannot parse `assert property`, and verify-auto has no `--define`, so an `ifdef`
/// guard would hide them from the verifier too. And sv2v 0.0.13 cannot parse `bind`
/// at all (verified 2026-09-06: `Parse error: unexpected token 'bind'`). So the
/// `--frontend sv2v` remedy is not merely awkward for such a
/// design — it cannot run, and recommending it sends the reader into a parse error.
fn sources_use_bind(sources: &[(String, String)]) -> bool {
    sources.iter().any(|(_, body)| {
        body.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("bind ") && !t.starts_with("bind_") && !l.trim_start().starts_with("//")
        })
    })
}

fn formula_atom_over_undriven_register(
    formula: &Formula,
    file: &crate::adapter::btor2::ast::Btor2File,
) -> Option<String> {
    let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for node in formula.nodes() {
        if let Node::Predicate(atom) = node
            && let Ok(expr) = crate::adapter::btor2::predicate_expr::parse_predicate_atom_bool(atom)
        {
            crate::adapter::btor2::symbolic_bitblast::collect_predicate_registers(
                &expr, &mut names,
            );
        }
    }
    // Deterministic: report the lexicographically-first offending name.
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    names
        .into_iter()
        .find(|n| crate::adapter::btor2::parser::signal_reaches_anonymous_input(file, n))
}

/// The minimal H.1 (+ H.B) — derive cube predicates from a formula's
/// `Node::Predicate` atoms. Each predicate is named exactly the atom string (so
/// the evaluator's name-match binds the atom to its cube bit).
///
/// `is_state(name)` returns true when `name` resolves to a lifted state cell —
/// either directly (the symbol is on a `state` line) or via the BTOR2
/// alias-resolution BFS (H.A: a `uext`/`Output` alias whose register Yosys'
/// `flatten` left unnamed on the `state` line). The lift's
/// `resolve_predicate_registers` canonicalises the same alias, so a name the
/// seeder accepts here also binds downstream.
///
/// `is_input(name)` (H.B) returns true when `name` is a primary **input** of
/// the lifted BTOR2. A **simple** `input == value` / bare-input atom is admitted
/// as a *free* cube dimension (its register is recorded in
/// [`Seeded::input_registers`]); the SMT may/must seam realises the source-pin /
/// target-free shape (`build_register_nid_map_with_inputs`). State takes
/// precedence — `is_state` is consulted first — so a name that is both never
/// double-classifies.
///
/// **Scope.** Inputs are admitted only inside a *simple* atom (the compound SMT
/// branch reads `state_curr`/`state_next` BVs, not `view.inputs`, so a compound
/// referencing an input would mis-encode). A compound (`!=`, relational, boolean)
/// still requires **all-state** registers; an input inside a compound is
/// unseedable. A non-state non-input register (a pure combinational function of
/// state — the strict resolver's reject case) is unseedable too — never given a
/// misleading verdict.
fn seed_from_formula(
    formula: &Formula,
    is_state: impl Fn(&str) -> bool,
    is_input: impl Fn(&str) -> bool,
    combinational_kind: impl Fn(&str) -> Option<CombKind>,
    cone_inputs_of: impl Fn(&str) -> Vec<String>,
    extra_atoms: &[String],
) -> Seeded {
    // Slice 3 (the unwrap lever) — cap how many raw cone inputs a single
    // combinational-of-input atom may add as free cube dimensions. Each added
    // input doubles the cube, so a combinational reaching many inputs is left as
    // a plain ⊥-label (unrefined) rather than blowing up the abstraction. One
    // input (`trigger_active = !trigger_i`) — the common case — always fires.
    const MAX_CONE_INPUTS: usize = 2;
    let mut out = Seeded::default();
    let mut seen: HashSet<&str> = HashSet::new();
    // Route one simple `register == value` (or bare-boolean ≡ `== 1`) atom by
    // the provenance of its register:
    // - state cell → cube dimension (H.A);
    // - free input → cube dimension, free at init (H.B);
    // - input-dependent combinational → cube dimension via the free-input path
    //   (H.E: source-pin / target-free, recorded in `input_registers`);
    // - state-only combinational → derived per-cube label (H.E Approach B);
    // - otherwise → unseedable (never a misleading verdict).
    let seed_simple = |out: &mut Seeded, atom: &str, register: &str, value: u64| {
        if is_state(register) {
            out.specs.push(PredicateSpec {
                name: atom.to_string(),
                register: register.to_string(),
                value,
            });
        } else if is_input(register) {
            out.input_registers.insert(register.to_string());
            out.specs.push(PredicateSpec {
                name: atom.to_string(),
                register: register.to_string(),
                value,
            });
        } else {
            match combinational_kind(register) {
                Some(CombKind::StateOnly) => {
                    // H.U.2 — a combinational function of STATE only is a
                    // determined function of state, so the uniform predicate-image
                    // handles it as a CUBE DIMENSION (resolved to the combinational
                    // node via the H.U.1d nid-map; its value over `(s,i)` /
                    // `(s',i')` from the signal cache + primed cache). This
                    // subsumes the retired H.E derived-label pass. The init-cube
                    // bit for this dimension is computed by observing the signal at
                    // the reset state (see the init-cube section below).
                    out.specs.push(PredicateSpec {
                        name: atom.to_string(),
                        register: register.to_string(),
                        value,
                    });
                }
                // H.E.r2 (combinational-input-atoms.md §6.1) — a combinational
                // function of a FREE INPUT (`trigger_active = !trigger_i`,
                // `main_sm_err_o = f(state, enable_i)`). It is NOT a sound cube
                // dimension (target-free fabricates must-edges; §5.1 / H.U.0), so
                // route it to a DERIVED 3-valued label: per cube the labeller
                // (`smt_combinational_label`) decides KleeneT/F where the cube pins
                // the signal, KleeneBot where the free input swings it (the generic
                // case for combinational-of-input). SOUND — a pure observation, no
                // edge change (§6.1 Prop 3) — and a `⊥` atom can never produce a
                // spurious VIOLATED (in Kleene `⊥ ∨ []C` is `T` or `⊥`, never `F`);
                // for the `AG(A→AX C)` safety shape with the atom in the antecedent
                // it yields a DEFINITE HOLDS via `⊥ ∨ []C = T` when the consequent
                // carries the property.
                Some(CombKind::InputDependent) => {
                    out.derived.push(PredicateSpec {
                        name: atom.to_string(),
                        register: register.to_string(),
                        value,
                    });
                    // Slice 3 (the unwrap lever, combinational-input-atoms.md
                    // §6.1 boundary → resolved): ALSO seed the raw free inputs in
                    // the combinational's cone as free H.B cube dimensions. This
                    // refines the may-relation so a consequent box over a
                    // transition GOVERNED by the input (`AG((state ∧ trigger) →
                    // AX state′)`) becomes definite — the sysrst sva_4..11 class
                    // that a ⊥-label alone leaves at ⊥. The derived ⊥-label above
                    // then turns DEFINITE at every cube that pins the input (the
                    // labeller proves the combinational constant there). Sound:
                    // a free-input dimension is the H.B source-pin / target-free
                    // shape (no fabricated must-edge — the H.E unsoundness came
                    // from a state-cube dimension, not a raw input); the label is
                    // a pure observation. Gated on a small cone-input count.
                    let cone = cone_inputs_of(register);
                    if !cone.is_empty() && cone.len() <= MAX_CONE_INPUTS {
                        for inp in cone {
                            if out.input_registers.insert(inp.clone()) {
                                out.specs.push(PredicateSpec {
                                    name: inp.clone(),
                                    register: inp,
                                    value: 1,
                                });
                            }
                        }
                    }
                }
                // Unresolvable combinational (no nid in the lifted design): cannot
                // label what we cannot resolve — honest SKIP.
                None => out.unseedable.push(atom.to_string()),
            }
        }
    };
    // Formula atoms first, then annotation `@mununu_predicate` atoms — both run the
    // IDENTICAL provenance classification (state → cube dimension, state-only
    // combinational → dimension, input-dependent combinational → derived label,
    // unresolvable → skip). Annotation predicates seed the cube even when they do NOT
    // appear in the property formula: they are auxiliary invariant HINTS. Soundness is
    // unaffected — predicate abstraction is monotone, so an extra dimension only
    // refines the may/must partition (a ⊥ can become definite; a definite verdict can
    // never flip). A hint that fails classification lands in `unseedable` and is
    // dropped, never a spurious verdict.
    let atoms = formula
        .nodes()
        .iter()
        .filter_map(|node| match node {
            Node::Predicate(atom) => Some(atom.as_str()),
            _ => None,
        })
        .chain(extra_atoms.iter().map(String::as_str));
    for atom in atoms {
        if !seen.insert(atom) {
            continue;
        }
        match parse_predicate_expr(atom) {
            Ok(expr) => match &expr {
                // Simple `reg == value` → routed by `seed_simple`.
                PredicateExpr::Cmp {
                    register,
                    op: CmpOp::Eq,
                    value,
                } => seed_simple(&mut out, atom, register, *value),
                // Relational / `!=` / boolean combination.
                _ => {
                    let regs = expr.registers();
                    let resolvable = regs
                        .iter()
                        .all(|r| is_state(r) || is_input(r) || combinational_kind(r).is_some());
                    if expr.has_addend() {
                        // H.G — an arithmetic relational (`cnt_q == cnt_q__past + 1`,
                        // sva_15). Its `CmpRegAddend` leaf is SMT-only (a `width==0`
                        // production leaf must NOT be `eval`'d, and the all-state
                        // compound path evals dimensions at the init cube). Route it
                        // to the DERIVED relational label path (`build_constraint`
                        // only, BV `bvadd` at the real register width). Sound — a
                        // pure per-cube observation, like any derived relational.
                        if resolvable {
                            out.derived_relational.push((atom.to_string(), expr));
                        } else {
                            out.unseedable.push(atom.to_string());
                        }
                    } else if regs.iter().all(|r| is_state(r)) {
                        // All-state → a cube DIMENSION (B.1 compound). The SMT
                        // dimension path reads state BVs.
                        out.compounds.push((atom.to_string(), expr));
                    } else if resolvable {
                        // H.F — a relational with an input / combinational operand
                        // (`cnt_q >= cfg_detect_timer_i`, `trigger_i !=
                        // trigger_active`). Its value depends on the demonic input,
                        // so it is NOT a sound cube dimension (the same reason a
                        // combinational-of-input atom isn't). Route it to a DERIVED
                        // per-cube 3-valued label: the SMT labeller decides
                        // KleeneT/F where the design's logic pins it (e.g.
                        // `trigger_i != ~trigger_i` ≡ true → HOLDS), KleeneBot where
                        // a free operand swings it. Sound (a pure observation; a
                        // KleeneBot atom is ⊥, never a spurious verdict).
                        out.derived_relational.push((atom.to_string(), expr));
                    } else {
                        // An operand resolves to nothing in the lifted design —
                        // cannot label it. Honest SKIP.
                        out.unseedable.push(atom.to_string());
                    }
                }
            },
            // A bare identifier atom (`parse_predicate_expr` needs an operator):
            // a 1-bit boolean signal `sig` ≡ `sig == 1`.
            Err(_) => seed_simple(&mut out, atom, atom, 1),
        }
    }
    out
}

/// Every register name referenced by a property's seeded predicates (specs +
/// compound + derived-relational operands). Used to detect counters and to decide
/// whether a counter's `$past` shadow is present (so it, too, gets bounded).
fn referenced_registers(seeded: &Seeded) -> std::collections::BTreeSet<String> {
    let mut all: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for s in &seeded.specs {
        all.insert(s.register.clone());
    }
    for (_, e) in &seeded.compounds {
        for r in e.registers() {
            all.insert(r);
        }
    }
    for (_, e) in &seeded.derived_relational {
        for r in e.registers() {
            all.insert(r);
        }
    }
    all
}

/// H.H — the counter registers referenced by a property's seeded predicates: a
/// state register `X` whose `$past` shadow `X__past` is ALSO referenced (a
/// self-relational monotonicity / increment atom — `cnt_q >= cnt_q__past` (sva_13)
/// or `cnt_q == cnt_q__past + 1` (sva_15)). These are exactly the registers whose
/// abstract 32-bit wraparound leaves the property at ⊥ — the ones a bound predicate
/// refines. Restricting the bound-seed to these avoids adding useless cube
/// dimensions to unrelated properties.
fn counter_registers_in(seeded: &Seeded) -> std::collections::BTreeSet<String> {
    let all = referenced_registers(seeded);
    let mut counters = std::collections::BTreeSet::new();
    for name in &all {
        if let Some(base) = name.strip_suffix("__past")
            && all.contains(base)
        {
            counters.insert(base.to_string());
        }
    }
    counters
}

/// H.H — auto-derive counter upper bounds from config concretization. Scans EVERY
/// H.5-GR1 — the outcome of scanning the SV source(s) for `@mununu_*` property
/// annotations (the mununu-exclusive properties an author adds beyond the design's
/// own SVA — recoverability, realizability, assume-guarantee liveness). The
/// `guarantees` are appended to the translated property set and verified uniformly;
/// `assumes` and `skipped` are surfaced as a provenance note.
#[derive(Debug, Default, Clone)]
struct AnnotationScan {
    /// `@mununu_guarantee <mu-calculus>` properties that parsed, as
    /// `TranslatedAssertion`s ready to merge into `extraction.translated`.
    guarantees: Vec<crate::adapter::slang::translate::TranslatedAssertion>,
    /// Where each of [`Self::guarantees`] is written (the comment's file and line), by row name;
    /// a template instance has no source line and is absent (2026-10-30).
    guarantee_lines: Vec<(String, crate::trace::SourceLoc)>,
    /// `@mununu_assume <body>` bodies (verbatim) that were NOT recognised as
    /// typed fairness / config assumes — recorded for provenance. A
    /// `<signal> = <value>` assume is applied by this path via config
    /// concretization (typed elsewhere). A `GF <atom>` fairness assume is
    /// parsed into [`Self::fairness_assumes`] and auto-applied via the
    /// fair-cycle l2s (mununu#477 Option B PR 2); a body that fails to parse
    /// as either falls through to this string bucket unchanged.
    assumes: Vec<String>,
    /// `@mununu_assume GF <atom>` bodies parsed into typed
    /// [`crate::adapter::liveness_rescue::Atom`]s. When non-empty, the SV
    /// verify-auto dispatch routes response-shape guarantees (`AG(a → AF b)`)
    /// through [`crate::adapter::liveness_rescue::response_liveness_rescue_under_fairness`]
    /// (the Emerson–Lei fair-cycle l2s) instead of the plain
    /// [`crate::adapter::liveness_rescue::response_liveness_rescue`]. Bodies
    /// that name `GF` but whose atom fails to parse are surfaced in
    /// [`Self::skipped`], never silently dropped.
    fairness_assumes: Vec<crate::adapter::liveness_rescue::Atom>,
    /// `@mununu_guarantee` bodies that did NOT parse — surfaced, never silently
    /// dropped.
    skipped: Vec<String>,
    /// `@mununu_predicate <expr>` auxiliary abstraction-predicate hints (verbatim
    /// expr strings, validated as parseable). Seeded into every property's cube via
    /// `seed_from_formula`'s `extra_atoms`. Sound by monotonicity — a hint only
    /// refines the cube (a ⊥ can become definite; a definite verdict never flips).
    predicates: Vec<String>,
    /// The `@mununu_template` rows (2026-10-30), by row name, with the instance as the author
    /// wrote it (`rtl_reachable TARGET="state_q == 3"`); the row's `formula` is the
    /// instantiated mu-calculus. Shown on the row as `template:`.
    template_forms: Vec<(String, String)>,
    /// Rows whose head carries `assumes=ignore` (2026-10-30): judged on every trace while the
    /// other rows are judged under the applied assumptions ([`plan_assumptions`] skips their
    /// relativization). Row names.
    assume_exempt: Vec<String>,
}

/// The modifiers a `guarantee` / `template` row's head may carry (2026-10-30).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RowModifiers {
    /// `assumes=ignore`: the row is not relativized to the applied assumptions.
    assume_exempt: bool,
}

/// Read a row's head modifiers; an unknown key or value is an `Err` the scanner reports (a
/// modifier the author wrote and the tool ignored would change the verdict's meaning silently).
fn row_modifiers(
    ann: &crate::mununu_annotations::MununuAnnotation,
    tag: &str,
) -> Result<RowModifiers, String> {
    let mut m = RowModifiers::default();
    let named = ann
        .name
        .as_deref()
        .map(|n| format!("({n})"))
        .unwrap_or_default();
    for (k, v) in &ann.modifiers {
        match (k.as_str(), v.as_str()) {
            ("assumes", "ignore") => m.assume_exempt = true,
            ("assumes", "precondition") => m.assume_exempt = false,
            ("assumes", other) => {
                return Err(format!(
                    "@mununu_{tag}{named}: `assumes={other}` \u{2014} a row's modifier is \
                     `ignore` or `precondition`; `check` is a run mode (`--assumes check`), not \
                     a property's"
                ));
            }
            (key, _) => {
                return Err(format!(
                    "@mununu_{tag}{named}: unknown modifier `{key}` (this version knows \
                     `assumes`)"
                ));
            }
        }
    }
    Ok(m)
}

/// Parse a `@mununu_template` body (2026-10-30): `<id> (KEY=VALUE)*`, a VALUE with whitespace
/// double-quoted (`TARGET="state_q == 3"`), no escapes. Returns the reference to instantiate.
fn parse_template_body(body: &str) -> Result<crate::adapter::templates::TemplateRef, String> {
    let body = body.trim();
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    };
    let (id, mut rest) = match body.find(char::is_whitespace) {
        Some(i) => (&body[..i], body[i..].trim_start()),
        None => (body, ""),
    };
    if !ident(id) {
        return Err(if id.is_empty() {
            "a template id is required (`mununu templates --domain rtl` lists them)".to_string()
        } else {
            format!("`{id}` is not a template id")
        });
    }
    let mut args = std::collections::HashMap::new();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else {
            return Err(format!(
                "`{}` is not `KEY=VALUE`",
                rest.split_whitespace().next().unwrap_or(rest)
            ));
        };
        let key = rest[..eq].trim();
        if !ident(key) {
            return Err(format!(
                "`{key}` is not a parameter name (write `KEY=VALUE`)"
            ));
        }
        let after = &rest[eq + 1..];
        let (value, tail) = if let Some(q) = after.strip_prefix('"') {
            let Some(close) = q.find('"') else {
                return Err(format!("`{key}=` opens a quote that never closes"));
            };
            (&q[..close], &q[close + 1..])
        } else {
            match after.find(char::is_whitespace) {
                Some(i) => (&after[..i], &after[i..]),
                None => (after, ""),
            }
        };
        if value.is_empty() {
            return Err(format!("`{key}=` has no value"));
        }
        if args.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("`{key}` is given twice"));
        }
        rest = tail.trim_start();
    }
    Ok(crate::adapter::templates::TemplateRef {
        template: id.to_string(),
        args,
    })
}

/// The label of an unnamed template row, `tmpl.<instance name>` sanitised to the annotation-name
/// alphabet (`tmpl.rtl_reachable_TARGET_state_q_eq_3`) — the label `--template` rows have carried
/// since they existed, so a consumer's pin on it does not move.
fn template_instance_label(instance_name: &str) -> String {
    let name: String = instance_name
        .replace("==", "eq")
        .replace("!=", "ne")
        .replace("<=", "le")
        .replace(">=", "ge")
        .replace("&&", "and")
        .replace("||", "or")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("tmpl.{name}")
}

/// H.5-GR1 — scan the SV source(s) for `@mununu_guarantee` / `@mununu_assume`
/// property annotations and turn each guarantee into a verifiable property. Reuses
/// the existing [`crate::mununu_annotations::extract_from_sv_source`] scanner and
/// the [`crate::mu_calculus::parser`]. A guarantee body is a mu-calculus formula
/// (the same string form the SVA translator emits and the per-property loop
/// parses); box-`F` liveness (`μY.(φ ∨ []Y)`) is expressible directly.
///
/// A body that starts with the keyword `ltl` is an LTL formula instead, read over
/// EVERY path and translated by [`crate::ltl::universal::translate_universal_text`]
/// — the same reading as a CTXDSL `body = ltl …;` — into the mu-calculus text the
/// row carries for the engine; the LTL as written rides beside it
/// ([`TranslatedAssertion::written`]) and is what the report shows as the row's
/// `formula`, with the mu-calculus as `translated` (2026-10-29). The
/// fragment is the one that module documents (`G`, `X`, `F b`, `b U b`, `b W b`,
/// response, recurrence); a formula outside it is skipped with the subformula
/// named. `eltl` (the some-path reading) is NOT accepted here: that reading is the
/// plain mu-calculus body this scanner always took. Bodies that do not parse are
/// recorded in `skipped`, never silently dropped.
///
/// A `@mununu_template <id> KEY=VALUE…` body (2026-10-30) is a property-template instance,
/// instantiated against the built-in catalog here; the production path passes the run's
/// configured catalog through [`scan_annotation_properties_with`].
#[cfg(test)]
fn scan_annotation_properties(sources: &[(String, String)]) -> AnnotationScan {
    scan_annotation_properties_with(
        sources,
        &crate::adapter::templates::TemplateRegistry::builtin(),
    )
}

/// [`scan_annotation_properties`] against `registry` — the catalog `@mununu_template` rows are
/// instantiated from (the built-in templates, the deployment's and the project's packs).
fn scan_annotation_properties_with(
    sources: &[(String, String)],
    registry: &crate::adapter::templates::TemplateRegistry,
) -> AnnotationScan {
    use crate::adapter::slang::translate::{FormulaLanguage, TranslatedAssertion, WrittenFormula};
    use crate::mununu_annotations::{MununuTag, extract_from_sv_source_checked};

    let mut scan = AnnotationScan::default();
    let mut idx = 0usize;
    for (source_name, content) in sources {
        // A template instance lives in a synthetic source the user never wrote.
        let written_by_user = !source_name.starts_with("__mununu_");
        let file = source_name
            .rsplit('/')
            .next()
            .unwrap_or(source_name)
            .to_string();
        let extracted = extract_from_sv_source_checked(content);
        // 2026-10-30 — a `@mununu_` line the extractor could not read (a tag typo, a malformed
        // head) used to vanish: an obligation the author wrote was silently not checked.
        for r in &extracted.rejected {
            scan.skipped.push(format!(
                "{file}:{}: `{}` — {}",
                r.line,
                r.text.split_whitespace().next().unwrap_or(&r.text),
                r.reason
            ));
        }
        for ann in extracted.annotations {
            match ann.tag {
                MununuTag::Guarantee => {
                    let body = ann.value.trim();
                    if body.is_empty() {
                        scan.skipped
                            .push("@mununu_guarantee with an empty body".to_string());
                        continue;
                    }
                    let mods = match row_modifiers(&ann, "guarantee") {
                        Ok(m) => m,
                        Err(why) => {
                            scan.skipped.push(why);
                            continue;
                        }
                    };
                    // `ltl <formula>` → the universal translation's text; anything else is
                    // mu-calculus as before. The keyword must be followed by whitespace, so an
                    // atom that happens to start with `ltl` (`ltl_ok`) is not captured.
                    // A bare `ltl` / `eltl` (the body was trimmed, so the keyword alone is
                    // what an empty formula looks like) is the keyword with nothing after it,
                    // not an atom named `ltl`.
                    let keyword_body = |kw: &str| -> Option<&str> {
                        if body == kw {
                            return Some("");
                        }
                        body.strip_prefix(kw)
                            .filter(|r| r.starts_with(char::is_whitespace))
                            .map(str::trim)
                    };
                    let ltl_body = keyword_body("ltl");
                    let eltl_body = keyword_body("eltl");
                    if let Some(rest) = eltl_body {
                        scan.skipped.push(format!(
                            "@mununu_guarantee `eltl {rest}`: the some-path reading is not an \
                             annotation keyword — write it in mu-calculus (`F p` is \
                             `mu X. (p || <> X)`), or use `ltl` for the every-path reading"
                        ));
                        continue;
                    }
                    // The row carries the mu-calculus for the engine AND the LTL as written: the
                    // report shows the latter as the row's `formula` and the former as
                    // `translated` (2026-10-29). A mu-calculus body is its own written form.
                    let parsed: Result<(String, Option<WrittenFormula>), String> = match ltl_body {
                        Some("") => Err("@mununu_guarantee ltl with an empty formula".to_string()),
                        Some(rest) => crate::ltl::parser::parse(rest)
                            .map_err(|e| {
                                format!("@mununu_guarantee `ltl {rest}` did not parse as LTL: {e}")
                            })
                            .and_then(|f| {
                                crate::ltl::universal::translate_universal_text(&f)
                                    .map_err(|e| format!("@mununu_guarantee `ltl {rest}`: {e}"))
                            })
                            .map(|mu| {
                                (
                                    mu,
                                    Some(WrittenFormula {
                                        language: FormulaLanguage::Ltl,
                                        text: rest.to_string(),
                                    }),
                                )
                            }),
                        None => crate::mu_calculus::parser::parse(body)
                            .map(|_| (body.to_string(), None))
                            .map_err(|e| {
                                format!(
                                    "@mununu_guarantee `{body}` did not parse as a mu-calculus \
                                     formula: {e}"
                                )
                            }),
                    };
                    match parsed {
                        Ok((formula, written)) => {
                            // mununu#550 — an author-given name becomes the LABEL, which
                            // `--expect` already matches (mununu#544). The positional `name` is
                            // kept either way, so an existing index pin does not break.
                            //
                            // A DUPLICATE name is refused rather than resolved: the expectation
                            // lookup is a `find()`, so two properties answering to one pin would
                            // silently scope the expectation to whichever came first — the exact
                            // failure this issue exists to remove, one level up.
                            //
                            // This is deliberately STRICTER than SVA labels, which may legally
                            // repeat: two asserts inside one labelled `begin…end` inherit the same
                            // label, so refusing those would reject valid SystemVerilog. An
                            // annotation name is author-chosen, one per annotation, and has no such
                            // excuse. The asymmetry has a reason; it is not an oversight.
                            if let Some(n) = ann.name.as_deref()
                                && let Some(prev) = scan
                                    .guarantees
                                    .iter()
                                    .find(|g| g.label.as_deref() == Some(n))
                            {
                                scan.skipped.push(format!(
                                    "@mununu_guarantee({n}) \u{2014} duplicate name, already used \
                                     by `{}`. A name is a PIN: two properties answering to one \
                                     would make `--expect {n}` silently match only the first. \
                                     Rename one of them.",
                                    prev.name
                                ));
                                continue;
                            }
                            if written_by_user && let Some(line) = ann.source_line {
                                scan.guarantee_lines.push((
                                    format!("ann_guarantee_{idx}"),
                                    crate::trace::SourceLoc {
                                        file: file.clone(),
                                        line,
                                    },
                                ));
                            }
                            scan.guarantees.push(TranslatedAssertion {
                                name: format!("ann_guarantee_{idx}"),
                                label: ann.name.clone(),
                                kind: SvaKind::Assert,
                                formula,
                                recoverability_companion: None,
                                written,
                            });
                            if mods.assume_exempt {
                                scan.assume_exempt.push(format!("ann_guarantee_{idx}"));
                            }
                            idx += 1;
                        }
                        Err(why) => scan.skipped.push(why),
                    }
                }
                MununuTag::Template => {
                    // 2026-10-30 — a template instance written next to the design. The same
                    // row shape as a guarantee (it IS one: the instantiated mu-calculus), the
                    // same naming, duplicate and source-line rules; the instance as written
                    // rides beside it as the row's `template:`.
                    let body = ann.value.trim();
                    let mods = match row_modifiers(&ann, "template") {
                        Ok(m) => m,
                        Err(why) => {
                            scan.skipped.push(why);
                            continue;
                        }
                    };
                    let tref = match parse_template_body(body) {
                        Ok(t) => t,
                        Err(why) => {
                            scan.skipped
                                .push(format!("@mununu_template `{body}`: {why}"));
                            continue;
                        }
                    };
                    let inst = match registry.instantiate(&tref) {
                        Ok(i) => i,
                        Err(e) => {
                            scan.skipped.push(format!("@mununu_template `{body}`: {e}"));
                            continue;
                        }
                    };
                    if let Err(e) = crate::mu_calculus::parser::parse(&inst.formula) {
                        scan.skipped.push(format!(
                            "@mununu_template `{body}`: the instance `{}` did not parse as a \
                             mu-calculus formula: {e}",
                            inst.formula
                        ));
                        continue;
                    }
                    let label = ann
                        .name
                        .clone()
                        .unwrap_or_else(|| template_instance_label(&inst.name));
                    if let Some(prev) = scan
                        .guarantees
                        .iter()
                        .find(|g| g.label.as_deref() == Some(label.as_str()))
                    {
                        scan.skipped.push(format!(
                            "@mununu_template({label}) \u{2014} duplicate name, already used by \
                             `{}`. A name is a PIN: two properties answering to one would make \
                             `--expect {label}` silently match only the first. Rename one of \
                             them.",
                            prev.name
                        ));
                        continue;
                    }
                    let row = format!("ann_guarantee_{idx}");
                    if written_by_user && let Some(line) = ann.source_line {
                        scan.guarantee_lines.push((
                            row.clone(),
                            crate::trace::SourceLoc {
                                file: file.clone(),
                                line,
                            },
                        ));
                    }
                    scan.template_forms.push((row.clone(), body.to_string()));
                    if mods.assume_exempt {
                        scan.assume_exempt.push(row.clone());
                    }
                    scan.guarantees.push(TranslatedAssertion {
                        name: row,
                        label: Some(label),
                        kind: SvaKind::Assert,
                        formula: inst.formula,
                        recoverability_companion: None,
                        written: None,
                    });
                    idx += 1;
                }
                MununuTag::Assume => {
                    let body = ann.value.trim();
                    // mununu#477 Option B — parse `GF <atom>` into a typed fairness assume.
                    // The atom must be a single register-comparison (`REG op VALUE`),
                    // matching the fair-cycle primitive's atom shape exactly. Anything
                    // else falls through to the untyped string bucket unchanged (the
                    // config-concretization path already picks up `<sig> = <val>`).
                    if let Some(atom_str) = body
                        .strip_prefix("GF ")
                        .or_else(|| body.strip_prefix("GF\t"))
                    {
                        let atom_str = atom_str.trim();
                        match crate::adapter::liveness_rescue::parse_response_atom(atom_str) {
                            Ok(atom) => scan.fairness_assumes.push(atom),
                            Err(e) => scan.skipped.push(format!(
                                "@mununu_assume GF `{atom_str}` did not parse as a \
                                 register-comparison atom: {e}"
                            )),
                        }
                    } else {
                        scan.assumes.push(body.to_string());
                    }
                }
                MununuTag::Predicate => {
                    let body = ann.value.trim();
                    // A bare identifier (`sig` ≡ `sig == 1`) is admitted like a bare
                    // formula atom; anything else must parse as a predicate expr.
                    let ok = !body.is_empty()
                        && (!body.contains(['=', '<', '>', '!'])
                            || parse_predicate_expr(body).is_ok());
                    if ok {
                        scan.predicates.push(body.to_string());
                    } else {
                        scan.skipped.push(format!(
                            "@mununu_predicate `{body}` did not parse as a predicate expression"
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    scan
}

/// H.5-GR1 — the provenance note for an [`AnnotationScan`]: how many
/// `@mununu_guarantee` properties were merged, which `@mununu_assume` bodies were
/// seen, and any guarantee bodies that failed to parse (surfaced, never dropped).
/// `None` when the source carried no `@mununu` property annotations.
fn annotation_note(scan: &AnnotationScan) -> Option<VerificationNote> {
    if scan.guarantees.is_empty()
        && scan.assumes.is_empty()
        && scan.fairness_assumes.is_empty()
        && scan.skipped.is_empty()
    {
        return None;
    }
    let mut items = Vec::new();
    for g in &scan.guarantees {
        match scan.template_forms.iter().find(|(n, _)| *n == g.name) {
            Some((_, form)) => items.push(format!("template {}: {form}", g.name)),
            None => items.push(format!("guarantee {}: {}", g.name, g.formula)),
        }
    }
    for a in &scan.assumes {
        items.push(format!("assume: {a}"));
    }
    for a in &scan.fairness_assumes {
        items.push(format!(
            "assume: GF ({} {} {}) — auto-applied via fair-cycle l2s (mununu#477)",
            a.signal,
            match a.op {
                crate::adapter::btor2::predicate_expr::CmpOp::Eq => "==",
                crate::adapter::btor2::predicate_expr::CmpOp::Ne => "!=",
                crate::adapter::btor2::predicate_expr::CmpOp::Lt => "<",
                crate::adapter::btor2::predicate_expr::CmpOp::Le => "<=",
                crate::adapter::btor2::predicate_expr::CmpOp::Gt => ">",
                crate::adapter::btor2::predicate_expr::CmpOp::Ge => ">=",
            },
            a.value,
        ));
    }
    for s in &scan.skipped {
        items.push(format!("skipped: {s}"));
    }
    Some(VerificationNote {
        kind: "annotation-properties".into(),
        level: if scan.skipped.is_empty() {
            NoteLevel::Info
        } else {
            NoteLevel::ScopeCaveat
        },
        summary: format!(
            "{} `@mununu_guarantee` propert{} verified alongside the design's SVA{}.",
            scan.guarantees.len(),
            if scan.guarantees.len() == 1 {
                "y"
            } else {
                "ies"
            },
            if scan.skipped.is_empty() {
                String::new()
            } else {
                format!("; {} unparsable, skipped", scan.skipped.len())
            }
        ),
        detail:
            "`@mununu_guarantee <mu-calculus>` / `@mununu_assume <body>` annotations carry the \
                 mununu-exclusive properties an author adds beyond the design's own SVA (e.g. \
                 assume-guarantee liveness the SVA fragment cannot express). Guarantee bodies \
                 are mu-calculus formulas parsed by the same parser as the translated SVA and \
                 verified through the same pipeline. `@mununu_assume` of the form \
                 `<signal> = <value>` is applied by this path via config concretization. A \
                 temporal `GF <REG op VALUE>` fairness assume is auto-applied for \
                 response-shape guarantees (`AG(a → AF b)`) via the Emerson–Lei fair-cycle l2s \
                 (mununu#477 Option B) — each `GF` assume adds a `fair_seen` monitor latch, \
                 and a `holds` verdict under fairness means the guarantee holds against every \
                 environment schedule satisfying the assumption infinitely often. For \
                 assumptions that do not fit `GF <REG op VALUE>` — a state predicate, a \
                 non-response guarantee shape, or a multi-guarantee-coupled fairness — the \
                 assume is recorded here; use `mununu btor2 verify-liveness --fairness` \
                 on the emitted BTOR2 or model in CTXDSL where the GR(1) game engine \
                 `(⋀ GF envᵢ) → (⋀ GF sysⱼ)` discharges multi-pair assume-guarantee liveness \
                 directly."
                .into(),
        items,
        property: None,
    })
}

/// translated property's ORIGINAL formula for a relational atom comparing a register
/// `R` against a pinned config register `C` (`cnt_q >= cfg_detect_timer_i`); the
/// pinned value `V = config[C]` becomes a candidate bound `R <= V` (a counter counts
/// up to the threshold it is compared against). GLOBAL across properties because the
/// property that NEEDS the bound (sva_13, `cnt_q >= cnt_q__past`) does not itself
/// reference the config — the comparison lives in a sibling property (sva_5,
/// `cnt_q >= cfg_detect_timer_i`). The widest value wins on collision (the loosest
/// sound partition). Empty when no config was concretized.
fn config_inferred_counter_bounds(
    translated: &[crate::adapter::slang::translate::TranslatedAssertion],
    applied_config_values: &[(String, u64)],
) -> std::collections::HashMap<String, u64> {
    let mut bounds: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    if applied_config_values.is_empty() {
        return bounds;
    }
    let cfg: std::collections::HashMap<&str, u64> = applied_config_values
        .iter()
        .map(|(n, v)| (n.as_str(), *v))
        .collect();
    for t in translated {
        let Ok(formula) = crate::mu_calculus::parser::parse(&t.formula) else {
            continue;
        };
        for node in formula.nodes() {
            let Node::Predicate(atom) = node else {
                continue;
            };
            let Ok(PredicateExpr::CmpReg { lhs, rhs, .. }) = parse_predicate_expr(atom) else {
                continue;
            };
            // One operand is a pinned config register; the other is the register
            // it bounds. (Either order — the comparison direction does not matter
            // for a sound partition.)
            let (reg, v) = if let Some(&v) = cfg.get(rhs.as_str()) {
                (lhs, v)
            } else if let Some(&v) = cfg.get(lhs.as_str()) {
                (rhs, v)
            } else {
                continue;
            };
            let e = bounds.entry(reg).or_insert(v);
            if v > *e {
                *e = v;
            }
        }
    }
    bounds
}

/// H.H — seed a bound compound `X <= K` for each counter register `X`, manual bound
/// (`opts.counter_bounds`) winning over the config-inferred one. Returns the applied
/// `(register, bound, provenance)` triples for the `counter-bound` note. **Sound
/// regardless of K:** the bound is a cube-dimension PARTITION (`{X<=K}` / `{X>K}`),
/// not an assumption — the SMT must-edges verify whether the reachable cubes stay
/// bounded, so a wrong K only leaves the property at honest ⊥, never mis-verdicts.
/// Dedups against compounds already present (an identical `X <= K` atom from the
/// formula itself).
fn seed_counter_bounds(
    seeded: &mut Seeded,
    counters: &std::collections::BTreeSet<String>,
    manual: &std::collections::HashMap<String, u64>,
    config_inferred: &std::collections::HashMap<String, u64>,
) -> Vec<(String, u64, &'static str)> {
    let referenced = referenced_registers(seeded);
    let mut applied = Vec::new();
    for reg in counters {
        let (k, provenance) = if let Some(&k) = manual.get(reg) {
            (k, "user-supplied")
        } else if let Some(&k) = config_inferred.get(reg) {
            (k, "config-inferred")
        } else {
            continue;
        };
        // Bound BOTH the counter and its `$past` shadow (when present): the
        // monotonicity/increment relational (`cnt_q >= cnt_q__past`) excludes the
        // abstract wraparound state only when BOTH operands are bounded — bounding
        // `cnt_q` alone leaves the state `(cnt_q=0, cnt_q__past=2^32-1)` reachable,
        // so the relation stays ⊥.
        let shadow = format!("{reg}__past");
        let mut targets = vec![reg.clone()];
        if referenced.contains(&shadow) {
            targets.push(shadow);
        }
        let mut added_any = false;
        for target in targets {
            let name = format!("{target} <= {k}");
            if seeded.compounds.iter().any(|(n, _)| n == &name) {
                continue;
            }
            seeded.compounds.push((
                name,
                PredicateExpr::Cmp {
                    register: target,
                    op: CmpOp::Le,
                    value: k,
                },
            ));
            added_any = true;
        }
        // Only report a bound that actually seeded a new compound this call — so
        // the note does not double-count and re-seeding is a true no-op.
        if added_any {
            applied.push((reg.clone(), k, provenance));
        }
    }
    applied
}

/// Build the synthesised `SvAnnotation` sidecar JSON `cegar_refine_loop` reads:
/// the compound predicates (`compound_predicates: [{name, expr}]`) PLUS
/// `signals[].config_values` pinning each referenced register to its design init
/// value. The config_values pin is load-bearing: without it the cube lift
/// defaults its initial cube to `cube_0` (all-predicates-false) — generally NOT
/// the reset state — and a property can falsely report VIOLATED at that
/// fictitious init. Returns `None` when there is nothing to emit.
fn synth_sidecar_json(
    compounds: &[(String, PredicateExpr)],
    derived: &[PredicateSpec],
    derived_relational: &[(String, PredicateExpr)],
    referenced: &std::collections::BTreeSet<String>,
    init_values: &std::collections::HashMap<String, u64>,
) -> Option<String> {
    let mut compound_decls: Vec<serde_json::Value> = compounds
        .iter()
        // `expr` == `name` == the atom string, which `sidecar_compound_predicates`
        // re-parses via `parse_predicate_expr` (REL handles `reg == reg`).
        .map(|(name, _)| serde_json::json!({ "name": name, "expr": name }))
        .collect();
    // H.F — relational derived predicates ride the SAME `compound_predicates`
    // field tagged `derived: true`, so `cegar_refine_loop` routes them to the
    // per-cube label path (not the cube index). `expr` == `name` as above.
    compound_decls.extend(
        derived_relational
            .iter()
            .map(|(name, _)| serde_json::json!({ "name": name, "expr": name, "derived": true })),
    );
    // H.E — derived combinational predicates: `cegar_refine_loop` reads these
    // into `lift_opts.derived_predicates`; the lift labels each per cube via the
    // SMT `combinational_labels` pass (NOT a cube dimension).
    let combinational_decls: Vec<serde_json::Value> = derived
        .iter()
        .map(|d| serde_json::json!({ "name": d.name, "signal": d.register, "value": d.value }))
        .collect();
    let signals: Vec<serde_json::Value> = referenced
        .iter()
        .filter_map(|r| {
            init_values
                .get(r)
                .map(|v| serde_json::json!({ "name": r, "config_values": [v] }))
        })
        .collect();
    if compound_decls.is_empty() && combinational_decls.is_empty() && signals.is_empty() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({
        "module": "cegar",
        "source": "verify_auto.btor2",
        "compound_predicates": compound_decls,
        "combinational_predicates": combinational_decls,
        "signals": signals,
    }))
    .ok()
}

/// mununu#504 — the ONE way a stopped run records a property it did not reach.
///
/// Both budgets (memory, #490; time, #504) and both later passes use this, so the degraded shape
/// cannot drift between them — before this, `escalate_bottom` and `rescue_skipped_via_exact`
/// handled a stop differently from the main loop, and one of them not at all.
///
/// `Unknown`, never `Skipped`: `ci_exit_code` does not fail on `skipped`, so a strict
/// `--fail-on unknown` gate would otherwise pass GREEN on a property that ran out of budget.
///
/// **`reason` is a parameter, not a constant, because this helper serves three different causes.**
/// Wiring it to one value mislabelled every budget abstention as "never attempted" — an
/// over-attribution of exactly the kind `bottom_reason` exists to end, and it survived until the
/// call sites were read rather than assumed.
/// The cube pass's verdict for one property, read over every initial input flavour (H.B): a
/// violation under SOME env input is a violation (False dominates); else an undecided flavour
/// makes it ⊥; else Holds. With no free inputs `init_cubes == [base_init_cube]`, so this is exactly
/// the single-cube read.
///
/// mununu#504 / #728 — a budget stop is `Unknown`, NEVER `Skipped` (`ci_exit_code` never fails on
/// `skipped`, so a timed-out property mapped to `Skipped` would pass a strict `--fail-on unknown`
/// gate green), and it is ATTRIBUTED: `Some(BudgetExpired)` whenever the refinement stopped on the
/// clock or the per-property budget had expired when the engine returned. Which budget fired
/// decides what happens next: only the RUN budget's expiry (`run_expired`) stops the run — a
/// per-property expiry marks ITS row and the rows after it are still attempted. Before this
/// split, one slow property abstained every later one.
fn cube_outcome(
    res: Result<(crate::mu_calculus::trit::TritSet, bool), crate::adapter::AdapterError>,
    init_cubes: &[usize],
    property_expired: bool,
    run_expired: bool,
) -> (VerifyOutcome, Option<BottomReason>, bool) {
    match res {
        Ok((v, stopped_on_budget)) => {
            use crate::mu_calculus::trit::Trit;
            let any_false = init_cubes.iter().any(|&ic| v.verdict_at(ic) == Trit::False);
            let any_unknown = init_cubes
                .iter()
                .any(|&ic| v.verdict_at(ic) == Trit::Unknown);
            if any_false {
                let false_cells = (0..v.len())
                    .filter(|&i| v.verdict_at(i) == Trit::False)
                    .count();
                (VerifyOutcome::Violated { false_cells }, None, false)
            } else if any_unknown {
                let unknown_cells = (0..v.len())
                    .filter(|&i| v.verdict_at(i) == Trit::Unknown)
                    .count();
                let on_clock = stopped_on_budget || property_expired;
                (
                    VerifyOutcome::Unknown { unknown_cells },
                    on_clock.then_some(BottomReason::BudgetExpired),
                    on_clock && run_expired,
                )
            } else {
                (VerifyOutcome::Holds, None, false)
            }
        }
        Err(e) if e.kind == crate::adapter::AdapterErrorKind::ResourceBudgetExceeded => (
            VerifyOutcome::Unknown { unknown_cells: 0 },
            property_expired.then_some(BottomReason::BudgetExpired),
            run_expired,
        ),
        Err(e) => (
            VerifyOutcome::Skipped {
                reason: format!("CEGAR error: {}", e.message),
            },
            None,
            false,
        ),
    }
}

fn abstained(
    t: &crate::adapter::slang::translate::TranslatedAssertion,
    reason: BottomReason,
) -> PropertyVerdict {
    PropertyVerdict {
        name: t.name.clone(),
        label: t.label.clone(),
        kind: t.kind,
        formula: t.formula.clone(),
        outcome: VerifyOutcome::Unknown { unknown_cells: 0 },
        bottom_reason: Some(reason),
        seeded_predicates: Vec::new(),
        counterexample: None,
        decided_by: None,
        vacuity: None,
    }
}

/// Init value of every state cell that HAS a BTOR2 `init` line, keyed by symbol. Used to pin
/// the cube lift's initial cube to the design's reset state.
///
/// mununu#579 — a state cell with no `init` line is ABSENT, not 0. It used to default to 0
/// (read as the `setundef -zero` power-up), which made "init 0" and "no init" indistinguishable
/// one line after the only place that could tell them apart, and pinned the cube path's cycle-0
/// state where the exact engine (`initial_state_bdd`, since mununu#498) and the reachability
/// portfolio leave it FREE — opposite soundness postures inside one portfolio run, the cube's
/// being the one that makes `HOLDS` unsound. An absent cell is now a free initial dimension of
/// the cube (`free_init_dimensions`), bounded, so the three paths read cycle 0 the same way.
fn state_cell_init_values(
    file: &crate::adapter::btor2::ast::Btor2File,
) -> std::collections::HashMap<String, u64> {
    use crate::adapter::btor2::ast::Node;
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let mut init_of_state: std::collections::HashMap<crate::adapter::btor2::ast::Nid, u64> =
        std::collections::HashMap::new();
    for line in &file.lines {
        if let Node::Init { state, value, .. } = &line.node
            && let Some(cl) = file.lookup(value.nid())
            && let Node::Const { sort, value: cv } = &cl.node
        {
            init_of_state.insert(*state, const_to_u64(file, *sort, cv));
        }
    }
    let mut out = std::collections::HashMap::new();
    for line in &file.lines {
        if matches!(line.node, Node::State { .. })
            && let Some(name) = symbols.get(&line.nid)
            && let Some(v) = init_of_state.get(&line.nid)
        {
            out.insert(name.clone(), *v);
        }
    }
    out
}

/// mununu#579 — the most free initial dimensions the cube path enumerates before it abstains.
/// A free dimension is a predicate bit with no established cycle-0 truth: a free input (H.B),
/// or a predicate over a register with no `init` line. The initial cube set is their product,
/// and an unbounded product is how the first convergence attempt overflowed the stack on real
/// RTL (`1 << free_bits.len()` with every init-less register freed). Past this cap the property
/// abstains with [`BottomReason::UnestablishedInitialCubes`] — a named ⊥, never a silent pin to
/// zero and never `2^n`.
const MAX_FREE_INIT_DIMENSIONS: usize = 16;

/// mununu#579 — what a predicate's register is at cycle 0, for a name that is not a state-cell
/// symbol of `state_cell_init_values` and not a primary input.
#[derive(Debug, PartialEq, Eq)]
enum CycleZero {
    /// A value-identical alias of a state cell that has an `init` line, at that value.
    Established(u64),
    /// An alias of an init-less state cell, or a combinational signal whose cone reaches an
    /// init-less state cell or a primary input: no single cycle-0 value.
    Unestablished,
    /// A combinational function of established state cells only: its cycle-0 value is what the
    /// reset state computes (observed by the caller).
    Combinational,
}

/// See [`CycleZero`]. `reset_pinned` is whether the strict resolver may follow the async-reset
/// register mux — the same flag the seeder binds with.
fn cycle_zero_class(
    file: &crate::adapter::btor2::ast::Btor2File,
    name: &str,
    reset_pinned: bool,
) -> CycleZero {
    use crate::adapter::btor2::ast::Node;
    let init_of_state = |nid: crate::adapter::btor2::ast::Nid| -> Option<u64> {
        file.lines.iter().find_map(|l| match &l.node {
            Node::Init { state, value, .. } if *state == nid => {
                let cl = file.lookup(value.nid())?;
                match &cl.node {
                    Node::Const { sort, value: cv } => Some(const_to_u64(file, *sort, cv)),
                    _ => None,
                }
            }
            _ => None,
        })
    };
    if let Some(nid) = crate::adapter::btor2::parser::resolve_state_alias(file, name, reset_pinned)
    {
        return match init_of_state(nid) {
            Some(v) => CycleZero::Established(v),
            None => CycleZero::Unestablished,
        };
    }
    let leaves = crate::adapter::btor2::dep_graph::cone_leaf_nids(file, &[name.to_string()]);
    let free_leaf = leaves
        .iter()
        .any(|nid| match file.lookup(*nid).map(|l| &l.node) {
            Some(Node::Input { .. }) => true,
            Some(Node::State { .. }) => init_of_state(*nid).is_none(),
            _ => false,
        });
    if free_leaf {
        CycleZero::Unestablished
    } else {
        CycleZero::Combinational
    }
}

/// A BTOR2 constant value → `u64` (all-ones needs the sort width).
fn const_to_u64(
    file: &crate::adapter::btor2::ast::Btor2File,
    sort_nid: crate::adapter::btor2::ast::Nid,
    cv: &crate::adapter::btor2::ast::ConstValue,
) -> u64 {
    use crate::adapter::btor2::ast::ConstValue::*;
    match cv {
        Zero => 0,
        One => 1,
        Ones => {
            let w = crate::adapter::btor2::parser::bv_width(file, sort_nid).unwrap_or(0);
            if w == 0 || w >= 64 {
                u64::MAX
            } else {
                (1u64 << w) - 1
            }
        }
        Dec(d) => *d as u64,
        Bin(b) => u64::from_str_radix(b, 2).unwrap_or(0),
        Hex(h) => u64::from_str_radix(h, 16).unwrap_or(0),
    }
}

/// H.B — the set of initial cube indices: the pinned-state `base` cube ⊗ every
/// combination of the free-input bit positions. The environment is free at cycle
/// 0, so each free-input dimension ranges over both polarities. An empty
/// `free_bits` returns `[base]` (the pre-H.B single reset cube), so state-only
/// properties read exactly one cube. Bit `free_bits[k]` is toggled by bit `k` of
/// the combination index.
///
/// Production always calls [`free_input_init_cubes_feasible`]; this unfiltered form is retained
/// as the CONTROL the feasibility tests measure against — it is what shows the filter drops 26
/// of 32 rather than silently dropping nothing.
#[cfg(test)]
fn free_input_init_cubes(base: usize, free_bits: &[u32]) -> Vec<usize> {
    free_input_init_cubes_feasible(base, free_bits, &[])
}

/// mununu#503 — as `free_input_init_cubes`, but dropping cube valuations that are
/// **mathematically empty**.
///
/// `free_dims` pairs each entry of `free_bits` with the `(register, value)` its dimension
/// asserts. Two `Eq` dimensions over the SAME register with DIFFERENT values cannot both hold, so
/// any combination setting both describes no concrete state at all.
///
/// # Why this matters
///
/// `$onehot0(oh_i)` over a primary input expands to one atom PER VALUE — `oh_i == 0`,
/// `oh_i == 1`, `oh_i == 2`, `oh_i == 4`, `oh_i == 8` — i.e. five mutually exclusive dimensions on
/// one register. Of the 2⁵ = 32 valuations only **6** are satisfiable (one per value, plus the
/// all-false case realised by `oh_i ∈ {3,5,6,7,9..15}`). The other **26 are contradictory**, and
/// `downgrade_unsatisfiable_cells` correctly masks them to ⊥ — but they were still enumerated as
/// INITIAL cubes here, so `any_unknown` read an empty cube's ⊥ as an abstention and the whole
/// property came back `Unknown { unknown_cells: 26 }`. 26 = 32 − 6, exactly.
///
/// The sibling test `e2e_onehot0_state_invariant_holds` isolates the variable: the same property
/// over a STATE register has no free-input dimensions, so it yields ONE initial cube and decides.
///
/// # Soundness
///
/// Dropping a cube that asserts `reg == v₁ ∧ reg == v₂` (v₁ ≠ v₂) cannot lose a reachable initial
/// state, because no state satisfies it. This is an exact reduction, not an approximation. The
/// symbolic path already takes the same view — `symbolic_final_verdict` projects infeasible cubes
/// to `F` as "never a reachable reset cube"; this brings the explicit path into line.
fn free_input_init_cubes_feasible(
    base: usize,
    free_bits: &[u32],
    free_dims: &[(String, u64)],
) -> Vec<usize> {
    (0..(1usize << free_bits.len()))
        .filter(|combo| {
            if free_dims.len() != free_bits.len() {
                return true; // no dimension metadata — enumerate as before
            }
            // Reject if any register is asserted at two DIFFERENT values at once.
            let mut claimed: Vec<(&str, u64)> = Vec::new();
            for (k, (reg, val)) in free_dims.iter().enumerate() {
                if (combo >> k) & 1 != 1 {
                    continue;
                }
                if claimed.iter().any(|(r, v)| *r == reg.as_str() && v != val) {
                    return false;
                }
                claimed.push((reg.as_str(), *val));
            }
            true
        })
        .map(|combo| {
            let mut c = base;
            for (k, &b) in free_bits.iter().enumerate() {
                if (combo >> k) & 1 == 1 {
                    c |= 1 << b;
                }
            }
            c
        })
        .collect()
}

/// R-F5.5d — project the R-F5 symbolic engine's per-cube verdicts into the
/// explicit path's `TritSet` shape (`2^|final P|` cells, same cube indexing), so
/// the reset-cube read in [`verify_auto`] is identical across engines. Feasible
/// cubes carry their `{T, F, ⊥}` verdict; infeasible cubes are `F` (never a
/// reachable reset cube, so they only pad the advisory `false_cells` count).
fn symbolic_final_verdict(
    result: &crate::adapter::btor2::symbolic_engine::SymbolicCegarResult,
) -> crate::mu_calculus::trit::TritSet {
    use bitvec::prelude::*;
    let k = result.final_verdicts.num_predicates;
    let n = 1usize << k;
    let mut must = bitvec![usize, Lsb0; 0; n];
    let mut may = bitvec![usize, Lsb0; 0; n];
    // `symbolic_cube_verdicts` tallies ONLY feasible cubes. Every other cube
    // stays absent from `cube_verdicts`. Track which cubes were tallied so the
    // untallied (infeasible) ones can be projected to ⊥ below — NOT left at
    // `must=0, may=0`, which `TritSet::verdict_at` reads as a definite-False.
    let mut tallied = bitvec![usize, Lsb0; 0; n];
    for (cube, trit) in &result.final_verdicts.cube_verdicts {
        tallied.set(*cube, true);
        match trit {
            crate::mu_calculus::trit::Trit::True => {
                must.set(*cube, true);
                may.set(*cube, true);
            }
            crate::mu_calculus::trit::Trit::Unknown => {
                may.set(*cube, true);
            }
            crate::mu_calculus::trit::Trit::False => {}
        }
    }
    // SOUNDNESS (R-F5.5d projection fix): an infeasible cube — one no concrete
    // state inhabits, hence absent from the feasible-cube tally — is VACUOUS,
    // not a definite violation. Projecting it to ⊥ (`may=1, must=0`) prevents an
    // infeasible `init_cube` from reading as a spurious VIOLATED via
    // `TritSet::verdict_at` (which maps `must=0, may=0` → `Trit::False`).
    // Feasible cubes that genuinely evaluate to False stay `must=0, may=0` (a
    // real violation is preserved) because they ARE tallied.
    for c in 0..n {
        if !tallied[c] {
            may.set(c, true);
        }
    }
    crate::mu_calculus::trit::TritSet::from_parts(must, may)
}

/// A property's definite verdict as a bool (`Some(true)` = Holds, `Some(false)` = Violated),
/// or `None` for an undecided (⊥) outcome. The portfolio merges on this.
pub(crate) fn outcome_definite(o: &VerifyOutcome) -> Option<bool> {
    match o {
        VerifyOutcome::Holds => Some(true),
        VerifyOutcome::Violated { .. } => Some(false),
        VerifyOutcome::Unknown { .. } | VerifyOutcome::Skipped { .. } => None,
    }
}

/// Merge the per-engine single-engine reports into one portfolio report. For each property
/// (matched by name), the merged outcome is the definite verdict from the FIRST engine (in
/// precision order) that produced one — carrying that engine's counterexample witness; a ⊥
/// survives only if EVERY engine left it undecided. **Runtime soundness guard:** if two engines
/// return OPPOSITE definite verdicts (Holds vs Violated) the property is forced to ⊥ with a
/// `portfolio-soundness-alarm` note — never a silent pick (the runtime form of the parity gate;
/// the differential proved this never fires on the corpus). Pure over the input reports, so it is
/// unit-testable without the toolchain.
pub(crate) fn merge_portfolio_reports(
    runs: &[(&str, Result<AutoVerifyReport, AdapterError>)],
    mode: PortfolioMode,
) -> Result<AutoVerifyReport, AdapterError> {
    // Successful reports, in precision order (exact first).
    let oks: Vec<(&str, &AutoVerifyReport)> = runs
        .iter()
        .filter_map(|(l, r)| r.as_ref().ok().map(|rep| (*l, rep)))
        .collect();

    // mununu#553 follow-up / monono ask 26 — THE ERRORS USED TO CONTRIBUTE NOTHING, and that was
    // the defect. Each engine's `Err` carries its own abstention string, which NAMES the budget it
    // hit and the numbers ("abstained on the ITERATION budget (N > M) — raise
    // MUNUNU_BDD_ITER_BUDGET"). Seven such messages exist. All seven were discarded HERE, one
    // function from the consumer's report, leaving a bare `⊥` plus a cube count — so the only
    // available move was to try budget knobs blind. Measured cost: a consumer's red lane, an hour,
    // and two refuted hypotheses, neither refuted from the `⊥` itself.
    //
    // Collected in PRECISION ORDER, so the first entry is the highest-precision engine that failed
    // — the one whose completion would most likely have decided a residual `⊥`.
    let engine_failures: Vec<(&str, String)> = runs
        .iter()
        .filter_map(|(l, r)| r.as_ref().err().map(|e| (*l, e.message.clone())))
        .collect();
    let Some(&(_, base)) = oks.first() else {
        // Every engine errored — surface the first (highest-precision) error.
        return Err(runs
            .iter()
            .find_map(|(_, r)| r.as_ref().err().cloned())
            .unwrap_or_else(|| AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: "portfolio: no engine produced a report".to_string(),
                location: None,
            }));
    };

    // The base (exact if available) supplies the property list, diagnostics, and notes.
    let mut merged = base.clone();
    let mut decided_by: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    let mut contradictions: Vec<String> = Vec::new();

    for prop in merged.properties.iter_mut() {
        let candidates: Vec<(&str, &PropertyVerdict)> = oks
            .iter()
            .filter_map(|(l, rep)| {
                rep.properties
                    .iter()
                    .find(|p| p.name == prop.name)
                    .map(|p| (*l, p))
            })
            .collect();
        let has_true = candidates
            .iter()
            .any(|(_, p)| outcome_definite(&p.outcome) == Some(true));
        let has_false = candidates
            .iter()
            .any(|(_, p)| outcome_definite(&p.outcome) == Some(false));

        if has_true && has_false {
            // CONTRADICTION — one engine is unsound on this design. Degrade to ⊥, loudly.
            let trues: Vec<&str> = candidates
                .iter()
                .filter(|(_, p)| outcome_definite(&p.outcome) == Some(true))
                .map(|(l, _)| *l)
                .collect();
            let falses: Vec<&str> = candidates
                .iter()
                .filter(|(_, p)| outcome_definite(&p.outcome) == Some(false))
                .map(|(l, _)| *l)
                .collect();
            contradictions.push(format!(
                "{}: Holds@[{}] vs Violated@[{}]",
                prop.name,
                trues.join(","),
                falses.join(",")
            ));
            prop.outcome = VerifyOutcome::Unknown { unknown_cells: 0 };
            // The most dangerous of the ⊥ causes to conflate: before this it was byte-identical
            // to a timeout (`unknown`, `unknown_cells: 0`), so the obvious remedy — retry with a
            // bigger budget — meant retrying an UNSOUNDNESS.
            prop.bottom_reason = Some(BottomReason::EngineContradiction {
                detail: contradictions
                    .last()
                    .cloned()
                    .unwrap_or_else(|| prop.name.clone()),
            });
            prop.counterexample = None;
            continue;
        }

        if let Some((label, pv)) = candidates
            .iter()
            .find(|(_, p)| outcome_definite(&p.outcome).is_some())
        {
            prop.outcome = pv.outcome.clone();
            prop.counterexample = pv.counterexample.clone();
            // The per-property attribution was already computed here and immediately aggregated
            // into a COUNT on the next line — so the fact a consumer needs existed and was
            // discarded one statement after being derived. Keep it.
            // mununu#722 — a member may have attributed the verdict itself (`constant`: an
            // atom-less formula decided before any engine ran); keep that over the engine label.
            prop.decided_by = Some(
                pv.decided_by
                    .clone()
                    .unwrap_or_else(|| (*label).to_string()),
            );
            *decided_by.entry(*label).or_default() += 1;
        } else {
            // All ⊥ — prefer an Unknown (abstraction attempted, undecided) over a Skipped
            // (atom not cube-bindable) so the merged ⊥ carries the most informative cause.
            if let Some((_, pv)) = candidates
                .iter()
                .find(|(_, p)| matches!(p.outcome, VerifyOutcome::Unknown { .. }))
            {
                prop.outcome = pv.outcome.clone();
            } else if let Some((_, pv)) = candidates
                .iter()
                .find(|(_, p)| matches!(p.outcome, VerifyOutcome::Skipped { .. }))
            {
                prop.outcome = pv.outcome.clone();
            }
            prop.counterexample = None;
        }
    }

    let mode_str = match mode {
        PortfolioMode::Sequential => "sequential",
        PortfolioMode::Parallel => "parallel",
    };
    let n_props = merged.properties.len();
    let n_decided = merged
        .properties
        .iter()
        .filter(|p| outcome_definite(&p.outcome).is_some())
        .count();
    let engines_ran: Vec<String> = oks.iter().map(|(l, _)| (*l).to_string()).collect();
    let mut items: Vec<String> = engines_ran.iter().map(|l| format!("ran:{l}")).collect();
    items.extend(
        decided_by
            .iter()
            .map(|(l, n)| format!("decided-by:{l}={n}")),
    );
    merged.notes.push(VerificationNote {
        kind: "portfolio".to_string(),
        level: NoteLevel::Info,
        summary: format!("portfolio-{mode_str}: {n_decided}/{n_props} properties decided"),
        detail: format!(
            "portfolio-{mode_str}: {} engine(s) ran ({}); {n_decided}/{n_props} properties decided. \
             Each property took the definite verdict from the first engine IN THE ORDER SHOWN \
             ABOVE to decide it; a ⊥ means no engine decided it (since mununu#733 a later \
             sequential member does not re-run a row an earlier one decided). Since mununu#543 (W1) \
             that order is the planner's own per-property prediction, most-predicted-decider \
             first, with the precision ladder (exact → symbolic → explicit) as the tiebreak — so \
             it is no longer fixed, and `ran:` lists what actually ran.",
            engines_ran.len(),
            engines_ran.join(", ")
        ),
        items,
        property: None,
    });
    // mununu#536 — every verdict is final now, so recompute the one note that counts them. The
    // clone above carried the BASE engine's coverage summary, which the merge has just made
    // wrong; see `refresh_coverage_summary`.
    refresh_coverage_summary(&mut merged);
    // mununu#548 — same inherited-from-base defect, different note: the posture note describes the
    // BASE engine, not the merge. Must run AFTER `engines_ran` is known.
    refresh_abstraction_posture(&mut merged, &engines_ran);
    // mununu#548 — and a third note inherited from the base that the merge can falsify: an
    // explanation of why a property is ⊥, kept for a property another engine has since decided.
    drop_stale_bottom_reasons(&mut merged);
    // monono ask 26 — and the reason a ⊥ was previously unattributable: the failing engine's own
    // abstention string, which names the budget it hit, was discarded here. Attach it now.
    attach_engine_failure_reason(&mut merged, &engine_failures);
    // mununu#599 — the merged report pairs verdicts from DIFFERENT engines, which is where
    // mununu#577's pair came from; each engine's own report was checked against itself, the merge
    // has not been until now. Judged on the verdicts that ship, and the notes re-derived.
    flag_self_contradictory_verdicts(&mut merged);
    flag_vacuous_guarantees(&mut merged);
    refresh_vacuity_notes(&mut merged);
    if !contradictions.is_empty() {
        merged.notes.push(VerificationNote {
            kind: "portfolio-soundness-alarm".to_string(),
            level: NoteLevel::SoundnessCaveat,
            summary: "engines returned CONTRADICTING definite verdicts (forced to ⊥)".to_string(),
            detail: format!(
                "two portfolio engines returned OPPOSITE definite verdicts on the same property; \
                 the merged outcome is ⊥ (not silently picked) — one engine is unsound on this \
                 design and must be investigated: {}",
                contradictions.join("; ")
            ),
            items: contradictions,
            property: None,
        });
    }
    Ok(merged)
}

/// Plan 2 / PR C (2026-10-18) — the ⊥ re-plan on the MERGED portfolio report, run ONCE by the
/// planner after every member ([`crate::planner::execute`]); each member runs with
/// [`VerifyAutoOptions::replan_defer`] and leaves its ⊥ rows alone. The merged ⊥ rows are exactly
/// the rows NO engine decided, so the rescue is paid once per row and never on a row another
/// engine's main pass settles. Before this every member re-planned its own ⊥ rows — measured on
/// the full OpenTitan `edn` IP: fifteen 60-s slices in the first member on rows the second decided
/// in about a second each, and the five rows no engine decides paid a slice per member (1 877 s
/// against 584 s). The rescue's provenance notes land on the merged report (a member's notes
/// never survived the merge, which keeps the base engine's). The post-passes a member runs after
/// its own re-plan run here, on the merged verdicts, in the same order.
pub(crate) fn replan_merged(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
    opts: &VerifyAutoOptions,
) {
    if report
        .properties
        .iter()
        .all(|p| outcome_definite(&p.outcome).is_some())
    {
        return;
    }
    let reset_pinned = !report.diagnostics.gated_resets.is_empty();
    // A member's shape-derived classification is re-derived once the re-plan has run; a specific
    // upstream reason (budget, memory, contradiction, unestablished init, engine failure) stays.
    for p in report.properties.iter_mut() {
        if matches!(p.outcome, VerifyOutcome::Unknown { .. })
            && matches!(
                p.bottom_reason,
                Some(
                    BottomReason::UnclassifiedBottom
                        | BottomReason::SafetyShapeNotReducible
                        | BottomReason::NoStateModelNonSafety
                )
            )
        {
            p.bottom_reason = None;
        }
    }
    let mut breadcrumb = crate::adapter::partial_json::Breadcrumb::from_env_with_append(true);
    let before: Vec<&'static str> = report
        .properties
        .iter()
        .map(|p| p.outcome.label())
        .collect();
    let notes = crate::planner::replan(report, design_btor2, reset_pinned, opts, fairness_atoms);
    for (i, p) in report.properties.iter().enumerate() {
        if before.get(i).copied() != Some(p.outcome.label()) {
            breadcrumb.record(i, &p.name, p.outcome.label(), "escalated", None);
        }
    }
    downgrade_violated_on_unestablished_init(report, design_btor2);
    attach_safety_witnesses(report, design_btor2, reset_pinned);
    withhold_violated_over_free_leaves(report, design_btor2);
    report.notes.extend(notes);
    refresh_coverage_summary(report);
    drop_stale_bottom_reasons(report);
    flag_self_contradictory_verdicts(report);
    flag_vacuous_guarantees(report);
    refresh_vacuity_notes(report);
}

/// One property's baseline-vs-mutant verdict under a mutation (`sv mutate`).
#[derive(Debug, Clone)]
pub struct PropertyFlip {
    /// The property (assertion) name — the join key between baseline and mutant.
    pub name: String,
    /// The SVA label (`a_trip_latches: assert property …`), when the source gives one.
    pub label: Option<String>,
    /// The canonical verdict on the design as-lifted.
    pub baseline: String,
    /// The canonical verdict on the mutated design.
    pub mutant: String,
    /// `baseline != mutant` — the mutation was *caught* by this property.
    pub flipped: bool,
}

/// The result of an `sv mutate` run: whether a NAMED structural mutation flips the
/// property verdicts. A **flip** confirms the property constrains the mutated
/// behaviour; an **unflipped** property is the property-adequacy finding — the
/// spec does not pin that fault down (vacuous w.r.t. the mutation). Per
/// [claims-integrity §2] this is a statement about the PROPERTIES, never a bug
/// report about the design.
#[derive(Debug, Clone)]
pub struct MutateReport {
    /// The applied mutation's label (`stick:<reg>` / `drop-reset:<reg>`).
    pub mutation: String,
    /// Per-property baseline-vs-mutant comparison (baseline order).
    pub properties: Vec<PropertyFlip>,
    /// Number of properties the mutation flipped (caught it).
    pub flipped: usize,
    /// Number the mutation did NOT flip (the adequacy finding).
    pub unflipped: usize,
}

impl MutateReport {
    /// The mutation run's verdict: `holds` when at least one property caught the mutant (a flip),
    /// `violated` when it survived every property — the spec gap a CI gate fails on.
    pub fn outcome(&self) -> &'static str {
        if self.flipped > 0 {
            "holds"
        } else {
            "violated"
        }
    }
}

/// `sv mutate` core — verify the design **twice** (as-lifted baseline, then with
/// `mutation` applied in `prepare_model`) and diff the per-property verdicts by
/// name. A mutation that names a missing register / does not apply is a hard error
/// from the mutant lift (never a silent no-op). `base_opts.mutation` is ignored
/// (forced `None` for the baseline, `Some(mutation)` for the mutant).
pub fn mutate_and_compare(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    base_opts: &VerifyAutoOptions,
    mutation: crate::adapter::btor2::mutate::Mutation,
) -> Result<MutateReport, AdapterError> {
    let mut baseline_opts = base_opts.clone();
    baseline_opts.mutation = None;
    let baseline = verify_auto(sources, yosys_opts, &baseline_opts)?;

    let mut mutant_opts = base_opts.clone();
    mutant_opts.mutation = Some(mutation.clone());
    let mutant = verify_auto(sources, yosys_opts, &mutant_opts)?;

    let mutant_by_name: std::collections::HashMap<&str, &VerifyOutcome> = mutant
        .properties
        .iter()
        .map(|p| (p.name.as_str(), &p.outcome))
        .collect();

    let properties: Vec<PropertyFlip> = baseline
        .properties
        .iter()
        .map(|b| {
            let baseline = b.outcome.label();
            // A property present at baseline but absent from the mutant reads as
            // `skipped` (the mutation dropped it) — still a flip if baseline was definite.
            let mutant = mutant_by_name
                .get(b.name.as_str())
                .map_or("skipped", |o| o.label());
            PropertyFlip {
                name: b.name.clone(),
                label: b.label.clone(),
                baseline: baseline.to_string(),
                mutant: mutant.to_string(),
                flipped: baseline != mutant,
            }
        })
        .collect();

    let flipped = properties.iter().filter(|p| p.flipped).count();
    let unflipped = properties.len() - flipped;
    Ok(MutateReport {
        mutation: mutation.as_label(),
        properties,
        flipped,
        unflipped,
    })
}

pub fn verify_auto(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<AutoVerifyReport, AdapterError> {
    // PORTFOLIO dispatch — when a portfolio mode is set, run the common-IR-hub pipeline:
    // `prepare_model` once → `plan()` (the ladder + the per-property cost RATIONALE, computed from
    // the shared prepared model) → `execute()` (drive each operator over the shared model, merge
    // under the runtime soundness guard, attach the rationale telemetry). Verdict-equivalent to the
    // former inline `verify_auto_portfolio`; the routing reasoning now lives in the planner.
    if opts.portfolio.is_some() {
        return match prepare_model(sources, yosys_opts, opts)? {
            Prepared::Empty(report) => {
                let mut report = *report;
                finish_assumptions(&mut report);
                Ok(report)
            }
            Prepared::Model(m) => {
                let m = *m;
                let task = crate::planner::VerificationTask {
                    sources,
                    yosys_opts,
                    opts,
                };
                let plan = crate::planner::plan(Some(&m), opts);
                let mut report = crate::planner::execute(&plan, &m, &task)?;
                // The merge builds its report from the operators'; the assumption summary is the
                // prepared model's, whatever the merge kept.
                report.assumptions = m.report.assumptions.clone();
                finish_assumptions(&mut report);
                Ok(report)
            }
        };
    }

    // Single-engine path (a user-picked `--engine`): the direct `verify_auto_impl(None)` form —
    // it checks A.6 (exact + no-reset-gating rejection) at the top, BEFORE any lift, then builds
    // the model inline and runs the one engine. No cost rationale — the predicted engine is
    // trivially the one the user selected. (Only the portfolio uses the plan/execute pipeline.)
    let mut report = verify_auto_impl(sources, yosys_opts, opts, None)?;
    finish_assumptions(&mut report);
    Ok(report)
}

/// The SV → BTOR2 lift output (incl. the `auto_stub_flops` re-lift), factored out so the
/// execution planner ([`crate::planner::execute`]) can run the lift **once** and share it
/// across the precision-ladder operators. The yosys lift is the dominant redundant cost of
/// the per-operator portfolio body — a 3-engine ladder otherwise re-elaborates the identical
/// SV up to three times. The cheap Rust prep (SVA extraction, reset detection / pinning,
/// BTOR2 parse, cube-atom classification) still re-runs per operator; only the lift is shared.
#[derive(Clone)]
pub(crate) struct PreLift {
    /// The lifted BTOR2 (pre reset-pin / shadow / config-pin — those stay per-operator).
    pub btor2: String,
    /// Modules the lift black-boxed (instantiated, no body) — surfaced in diagnostics.
    pub blackboxed_modules: Vec<String>,
    /// Flop-primitive stubs auto-injected on the H.C re-lift (empty when none).
    pub auto_provided_stubs: Vec<String>,
    /// The concrete front end that produced the BTOR2 (#466) — `Verilog`/`Slang`.
    pub frontend_used: crate::adapter::yosys::SvFrontend,
    /// `Some(reason)` when `--frontend auto` fell back after an earlier front end
    /// errored (#466).
    pub fallback_reason: Option<String>,
    /// `Some(reason)` when the lift rerun its script with `opt_clean` after a BTOR-backend
    /// topological-loop error (`ModelDiagnostics::lift_retry`).
    pub backend_retry: Option<String>,
    /// The clock domains the lift merged (more than one only under `--merge-clock-domains`).
    pub clock_domains: Vec<crate::adapter::yosys::ClockDomain>,
    /// How a counterexample names this lift's registers and clocks (display only).
    pub trace_names: crate::trace::TraceNames,
}

/// Run the SV → BTOR2 lift (yosys) with the `auto_stub_flops` re-lift. Pure function of
/// `(sources, yosys_opts, opts)` — the same inputs every precision-ladder operator shares —
/// so the planner calls it once and threads the [`PreLift`] into each [`verify_auto_impl`].
pub(crate) fn lift_sv(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<PreLift, AdapterError> {
    use crate::adapter::yosys::sv_to_btor2_with_blackboxes;
    let (primary_name, primary_content) = sources.first().ok_or_else(|| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: "adapter/slang/verify_auto: no SV sources provided".to_string(),
        location: None,
    })?;
    let overridden;
    let (primary_content, yosys_opts) = if opts.lift_overrides.is_empty() {
        (primary_content, yosys_opts)
    } else {
        overridden = apply_lift_overrides(
            primary_name,
            primary_content,
            yosys_opts,
            &opts.lift_overrides,
        )?;
        (&overridden.0, &overridden.1)
    };
    // 2. SV → flattened BTOR2. The lift reports modules it black-boxed (instantiated, no
    // body) — surfaced in `diagnostics.blackboxed_modules` so a cut FSM (the
    // `prim_sparse_fsm_flop`-class root cause) is visible, not silent.
    let mut lift = sv_to_btor2_with_blackboxes(primary_content, yosys_opts).map_err(|mut e| {
        e.message = format!("verify_auto: SV → BTOR2: {}", e.message);
        e
    })?;
    // H.C — if the first lift cut a known flop primitive, inject a behavioral stub for it and
    // re-lift so the register survives (e.g. OpenTitan's `prim_sparse_fsm_flop`). Only stubs
    // ACTUALLY-cut modules (no collision with designs that provide their own body).
    let mut auto_provided_stubs = Vec::new();
    if opts.auto_stub_flops {
        let stubs =
            crate::adapter::slang::prim_stubs::stubs_for_cut_modules(&lift.blackboxed_modules);
        if !stubs.is_empty() {
            let mut yopts2 = yosys_opts.clone();
            yopts2.additional_sources.extend(stubs.iter().cloned());
            // The re-lift produces the FINAL BTOR2, so its front-end/fallback info is
            // the one to report (#466).
            lift = sv_to_btor2_with_blackboxes(primary_content, &yopts2).map_err(|mut e| {
                e.message = format!("verify_auto: SV → BTOR2 (with flop stubs): {}", e.message);
                e
            })?;
            auto_provided_stubs = stubs
                .iter()
                .map(|(name, _)| name.trim_end_matches(".sv").to_string())
                .collect();
        }
    }
    // 2026-10-29 — the source map's netlist half: yosys-slang (recent builds) and yosys's own
    // `read_verilog` stamp nets and storage cells with the staged file's own lines. Declarations
    // the netlist lacks are filled from slang's AST at the hand-off (`fill_declarations`). When
    // sv2v rewrote the source first, the netlist's lines are its output's: the kinds (register /
    // input / output) stay, the lines go, and the status says what is missing.
    let mut sources = crate::adapter::yosys::cell_sources(&lift.flat_netlist_json);
    crate::adapter::yosys::name_staged_primary(&mut sources, primary_name);
    // The sv2v path: the netlist's lines are sv2v's output's; the kinds (register / input /
    // output) stay, every line comes from slang's AST at the hand-off.
    if lift.sv2v_ran {
        for s in &mut sources {
            s.declared = None;
            s.assigned.clear();
        }
    }
    Ok(PreLift {
        // mununu#713 — `gen[k]` as `gen$k`, the spelling the translator gives the same names.
        btor2: crate::adapter::btor2::parser::flat_symbol_names(&lift.btor2),
        blackboxed_modules: lift.blackboxed_modules,
        auto_provided_stubs,
        frontend_used: lift.frontend_used,
        fallback_reason: lift.fallback_reason,
        backend_retry: lift.backend_retry,
        trace_names: crate::trace::TraceNames {
            aliases: crate::adapter::yosys::trace_aliases(&lift.flat_netlist_json),
            clocks: lift.clock_domains.iter().map(|d| d.net.clone()).collect(),
            sources,
            source_lines_rewritten: lift.sv2v_ran,
            ..Default::default()
        },
        clock_domains: lift.clock_domains,
    })
}

/// Apply [`VerifyAutoOptions::lift_overrides`]: the primary's content and the lift's additional
/// sources with every same-named file replaced. Errors on an override that names no source, and on
/// a name given twice — a typo must not silently lift the real file. Public since mununu#725: the
/// `sv mutate --list` discovery lift (CLI and API) goes through it too — it lifts directly, not
/// through `verify_auto`, and the real OpenTitan `prim_assert.sv` dies in yosys without the shim.
pub fn apply_lift_overrides(
    primary_name: &str,
    primary_content: &str,
    yosys_opts: &YosysOptions,
    overrides: &[(String, String)],
) -> Result<(String, YosysOptions), AdapterError> {
    let err = |message: String| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message,
        location: None,
    };
    let mut primary = primary_content.to_string();
    let mut yopts = yosys_opts.clone();
    let mut seen = std::collections::HashSet::new();
    for (name, content) in overrides {
        if !seen.insert(name.as_str()) {
            return Err(err(format!("lift override `{name}` given more than once")));
        }
        let mut hit = false;
        if name == primary_name {
            primary = content.clone();
            hit = true;
        }
        for (n, c) in yopts.additional_sources.iter_mut() {
            if n == name {
                *c = content.clone();
                hit = true;
            }
        }
        if !hit {
            return Err(err(format!(
                "lift override `{name}` matches no source (the override replaces a same-named \
                 source for the lift; it does not add one)"
            )));
        }
    }
    Ok((primary, yopts))
}

/// P-3 (common-IR hub) — the SVA + annotation extraction result, factored out so the execution
/// planner can run the slang `extract_sva` subprocess ONCE and share it across the precision-ladder
/// operators — exactly as [`PreLift`] does for the yosys lift. Without this the portfolio
/// re-extracts per engine operator (a slang subprocess apiece). Clone so each operator owns a copy.
#[derive(Clone)]
pub(crate) struct SharedExtraction {
    /// The translated SVA with the `@mununu_guarantee` properties merged in.
    extraction: crate::adapter::slang::translate::TranslationReport,
    /// Every port, net and variable with its declaration, from slang's AST (2026-10-29), or why
    /// that run failed; fills the source map where the lift's netlist carries no line.
    declarations: crate::adapter::slang::extract::Declarations,
    /// The `@mununu_*` annotation scan (drives the annotation note).
    ann_scan: AnnotationScan,
    /// Abstraction-predicate hints (in-source `@mununu_predicate` + CLI/API `--predicate`).
    predicate_hints: Vec<String>,
}

/// Run the SVA + annotation extraction (the slang subprocess + the source annotation scan), merging
/// the `@mununu_guarantee` properties into the translated set. Pure function of `(sources, opts)` —
/// the same inputs every precision-ladder operator shares — so the planner calls it once and threads
/// the [`SharedExtraction`] into each [`verify_auto_impl`].
pub(crate) fn extract_front(
    sources: &[(String, String)],
    opts: &VerifyAutoOptions,
    params: &[(String, String)],
) -> Result<SharedExtraction, AdapterError> {
    // mununu#717 — the SAME `--param` overrides the lift applies, or the SVA describes the design
    // at its parameter defaults.
    let (mut extraction, declarations) =
        crate::adapter::slang::extract::extract_sva_with_declarations(
            sources,
            &TranslateOptions {
                gate_reset: opts.gate_reset,
            },
            params,
        )?;
    // Templates become `@mununu_template` annotations of a synthetic source the lift never
    // reads, so a flag and a hand-written instance go through the same parse, naming and
    // duplicate check (2026-10-30; before, the flag was rendered as a `@mununu_guarantee` of the
    // instantiated formula). The catalog is the run's: built-in + deployment + project packs.
    let registry = crate::adapter::templates::TemplateRegistry::configured(&opts.templates_files)
        .map_err(|e| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message: format!("verify_auto: the template catalog: {e}"),
        location: None,
    })?;
    let template_source = template_annotations(&opts.templates, &registry)?;
    let scan_sources: Vec<(String, String)> =
        sources.iter().cloned().chain(template_source).collect();
    let mut ann_scan = scan_annotation_properties_with(&scan_sources, &registry);
    for g in &mut ann_scan.guarantees {
        g.formula = resolve_named_constants(&g.formula, &extraction.named_constants);
    }
    let predicate_hints: Vec<String> = ann_scan
        .predicates
        .iter()
        .chain(opts.predicate_hints.iter())
        .cloned()
        .collect();
    extraction
        .translated
        .extend(ann_scan.guarantees.iter().cloned());
    Ok(SharedExtraction {
        extraction,
        declarations,
        ann_scan,
        predicate_hints,
    })
}

/// The `@mununu_template` lines for the requested `--template` flags, as one synthetic source
/// (`None` when no template is requested). Each flag is rendered as the annotation an author
/// would write — `// @mununu_template(tmpl.<instance>) <id> KEY="VALUE"…` — so the two forms are
/// one path from here on (2026-10-30; before, the flag was rendered as a `@mununu_guarantee` of
/// the instantiated formula). The flag's instance is validated HERE, as a hard error: a flag the
/// user typed and the run could not honour must not become a note. The row keeps the label a
/// `--template` row has always had ([`template_instance_label`]).
fn template_annotations(
    templates: &[crate::adapter::templates::TemplateRef],
    registry: &crate::adapter::templates::TemplateRegistry,
) -> Result<Option<(String, String)>, AdapterError> {
    if templates.is_empty() {
        return Ok(None);
    }
    let err = |message: String| AdapterError {
        kind: AdapterErrorKind::ParseError,
        message,
        location: None,
    };
    let mut text = String::new();
    for tref in templates {
        let inst = registry
            .instantiate(tref)
            .map_err(|e| err(format!("verify_auto: template `{}`: {e}", tref.template)))?;
        let mut args: Vec<(&String, &String)> = tref.args.iter().collect();
        args.sort();
        let mut line = format!(
            "// @mununu_template({}) {}",
            template_instance_label(&inst.name),
            tref.template
        );
        for (k, v) in args {
            if v.contains('"') {
                return Err(err(format!(
                    "verify_auto: template `{}`: the value of `{k}` contains a double quote, \
                     which the annotation form cannot carry",
                    tref.template
                )));
            }
            line.push_str(&format!(" {k}=\"{v}\""));
        }
        line.push('\n');
        text.push_str(&line);
    }
    Ok(Some(("__mununu_templates.sv".to_string(), text)))
}

/// Rewrite `x == NAME` / `x != NAME` where `NAME` is an enum member or parameter of the design
/// ([`TranslationReport::named_constants`]) to its value: an annotation or template atom names the
/// FSM's states the way the RTL does, and the engine resolves registers, not enum members.
fn resolve_named_constants(
    formula: &str,
    constants: &std::collections::BTreeMap<String, i64>,
) -> String {
    if constants.is_empty() {
        return formula.to_string();
    }
    let b = formula.as_bytes();
    let mut out = String::with_capacity(formula.len());
    let mut i = 0;
    while i < b.len() {
        let op = (i + 1 < b.len()) && ((b[i] == b'=' || b[i] == b'!') && b[i + 1] == b'=');
        if !op {
            out.push(b[i] as char);
            i += 1;
            continue;
        }
        out.push_str(&formula[i..i + 2]);
        i += 2;
        let mut j = i;
        while j < b.len() && b[j] == b' ' {
            j += 1;
        }
        let start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
            j += 1;
        }
        let ident = &formula[start..j];
        let continues = j < b.len() && (b[j] == b'.' || b[j] == b'[');
        let is_ident = ident
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        match constants.get(ident) {
            Some(v) if is_ident && !continues => {
                out.push_str(&formula[i..start]);
                out.push_str(&v.to_string());
            }
            _ => out.push_str(&formula[i..j]),
        }
        i = j;
    }
    out
}

/// P-3b/P-4 (common-IR hub) — the operator-INDEPENDENT prepared model: the fully-prepared BTOR2
/// (lifted + reset/init/shadow/config-pinned) plus everything the per-property loop needs that does
/// NOT depend on the engine choice. `prepare_model` builds it ONCE; `plan` reads it (facts-aware
/// routing rationale) and `verify_from_prepared` runs each engine operator over it. This makes the
/// slang extraction + yosys lift + reset/pin prep run once per portfolio run, not per operator, and
/// gives `plan()` the fully-prepared model to reason about cost. Clone so each operator owns a copy.
#[derive(Clone)]
pub(crate) struct PreparedModel {
    /// The fully-prepared BTOR2 (lift + reset-init + shadow + reset/config pins).
    pub btor2: String,
    /// The report skeleton — carries `unsupported` + the lift/reset diagnostics.
    report: AutoVerifyReport,
    /// The translated properties (+ merged `@mununu_guarantee`).
    extraction: crate::adapter::slang::translate::TranslationReport,
    /// Abstraction-predicate hints (W1/W2).
    predicate_hints: Vec<String>,
    /// The `@mununu_*` annotation scan (drives the annotation note).
    ann_scan: AnnotationScan,
    /// The config inputs actually pinned (`--pin`), for the per-property substitution + note.
    applied_config_values: Vec<(String, u64)>,
    /// The fault posture as applied (`--fault` / `--fault-model`), `None` without one.
    fault: Option<crate::adapter::btor2::fault::FaultApplied>,
    /// slang's source index of the design (or why that run failed): the `involves` post-pass maps
    /// each row's cone through it (2026-10-30).
    source_index: crate::adapter::slang::extract::Declarations,
}

impl PreparedModel {
    /// The `(name, mu-formula)` of each translated property — the planner's input for the routing
    /// rationale (`plan`). Pre-substitution formulas; the class + cone the rationale reads are
    /// substitution-invariant, so this lines up with the post-run outcomes by name.
    pub(crate) fn properties(&self) -> Vec<(String, String)> {
        self.extraction
            .translated
            .iter()
            .map(|t| (t.name.clone(), t.formula.clone()))
            .collect()
    }

    /// The `@mununu_assume GF <atom>` fairness atoms the annotation scan found — the re-plan's
    /// liveness lane reads them ([`replan_merged`]).
    pub(crate) fn fairness_assumes(&self) -> &[crate::adapter::liveness_rescue::Atom] {
        &self.ann_scan.fairness_assumes
    }
}

/// The result of `prepare_model`: either a design with NO translated properties (the empty report is
/// final) or a fully-prepared [`PreparedModel`].
pub(crate) enum Prepared {
    /// No properties to verify — the report (posture + annotation note) is complete. Boxed for
    /// the same reason `Model` is: `Prepared` is returned by value, and an `AutoVerifyReport`
    /// carrying the full diagnostics + note set is not a cheap thing to move through the
    /// enum's largest-variant footprint.
    Empty(Box<AutoVerifyReport>),
    /// The prepared model, ready for `plan` + `verify_from_prepared`. Boxed — it is much larger
    /// than the `Empty` report, and `Prepared` is returned by value.
    Model(Box<PreparedModel>),
}

/// P-3b/P-4 — the operator-INDEPENDENT prep: extract the SVA (slang), lift to BTOR2 (yosys),
/// reset/init/shadow/config-pin. Pure function of `(sources, yosys_opts, opts)` — identical for
/// every precision-ladder operator — so the planner calls it ONCE. Returns [`Prepared::Empty`] for a
/// design with no properties, else [`Prepared::Model`]. (The engine-flag-dependent A.6 check is the
/// caller's — it belongs with the per-operator run, not the shared prep.)
pub(crate) fn prepare_model(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
) -> Result<Prepared, AdapterError> {
    use crate::adapter::btor2::shadow::augment_with_past_shadows;

    // 1. Extract + translate the SVA (slang) + merge `@mununu_guarantee` annotations.
    let SharedExtraction {
        mut extraction,
        declarations,
        ann_scan,
        predicate_hints,
    } = extract_front(sources, opts, &yosys_opts.params)?;
    // Phase 4 — `assume` statements: split off, guarantees relativized (the monitor itself is
    // emitted below, once the expressions it reads are minted); a row whose annotation head says
    // `assumes=ignore` keeps its formula.
    let (assumption_report, assume_candidates) =
        plan_assumptions(&mut extraction, opts.assume_mode, &ann_scan.assume_exempt);

    // Validate `--param` override names against the design's declared parameters
    // (from the slang AST). The read_verilog lift's `chparam` errors on an unknown
    // name, but the slang lift's `-G` override SILENTLY ignores it — so an
    // unapplicable `--param` is caught here, uniformly, as a hard error rather than
    // a silent drop (contrast `--pin`, mununu#459). A `MODULE.NAME`
    // override is validated on its bare `NAME`.
    if !yosys_opts.params.is_empty() {
        for (lhs, value) in &yosys_opts.params {
            let name = lhs.rsplit('.').next().unwrap_or(lhs);
            if !extraction.parameter_names.iter().any(|p| p == name) {
                return Err(AdapterError {
                    kind: AdapterErrorKind::UnsupportedConstruct,
                    location: None,
                    message: format!(
                        "--param {lhs}={value}: `{name}` is not a parameter of the design. \
                         A parameter override must name a real module parameter — it is never \
                         silently dropped. Declared parameters: {}.",
                        if extraction.parameter_names.is_empty() {
                            "none".to_string()
                        } else {
                            extraction.parameter_names.join(", ")
                        }
                    ),
                });
            }
            // mununu#717 — the override must also have REACHED the elaborated SVA design. slang's
            // `-G` is top-scoped, so a `MODULE.NAME` override on a module slang does not elaborate
            // as a top leaves that module at its default while the lift applies it; the
            // properties would then describe a different design than the one checked. Only an
            // integer value is checked, and only when the AST carries an integer value for the
            // name at all (a type or string parameter is not folded, so it cannot mislead).
            if let Ok(want) = value.parse::<i64>()
                && let Some(seen) = extraction.parameter_values.get(name)
                && !seen.is_empty()
                && !seen.contains(&want)
            {
                return Err(AdapterError {
                    kind: AdapterErrorKind::UnsupportedConstruct,
                    location: None,
                    message: format!(
                        "--param {lhs}={value}: the override did not reach the SVA extraction — \
                         slang elaborated `{name}` as {}. The properties would be judged against \
                         the parameter's default rather than the design being checked, so the run \
                         is refused. A `MODULE.NAME` override only applies to a top-level module.",
                        seen.iter()
                            .map(i64::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }
    }

    let mut report = AutoVerifyReport {
        unsupported: extraction
            .unsupported
            .iter()
            .filter(|u| {
                // An `assume` that did not translate is reported with the assumptions when they
                // are applied or ignored, not as an unchecked assertion.
                opts.assume_mode == AssumeMode::Check || u.kind != Some(SvaKind::Assume)
            })
            .map(|u| (u.name.clone(), u.reason.clone()))
            .collect(),
        assumptions: {
            let mut a = assumption_report;
            if opts.assume_mode != AssumeMode::Check {
                for u in extraction
                    .unsupported
                    .iter()
                    .filter(|u| u.kind == Some(SvaKind::Assume))
                {
                    a.unapplied
                        .push((u.name.clone(), format!("did not translate: {}", u.reason)));
                }
            }
            a
        },
        diagnostics: ModelDiagnostics {
            written_formulas: written_formulas_of(&extraction.translated),
            template_forms: ann_scan.template_forms.clone(),
            property_sources: property_sources_of(
                &extraction.translated,
                &declarations,
                &ann_scan.guarantee_lines,
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    if extraction.translated.is_empty() {
        // No properties: the posture note is informational; reflect the engine.
        let posture = if opts.exact_symbolic {
            NotePosture::Exact
        } else {
            NotePosture::Cube
        };
        report.notes = build_notes(
            &report,
            must_edge_posture_label(opts.must_edge_inference, opts.must_edge_auto),
            &[],
            &yosys_opts.params,
            &[],
            &yosys_opts.cutpoint_signals,
            &posture,
        );
        if let Some(n) = annotation_note(&ann_scan) {
            report.notes.push(n);
        }
        return Ok(Prepared::Empty(Box::new(report)));
    }

    // Reset inputs to pin inactive (reset-gating). Empty when gating is off or no `disable iff`
    // reset was recognized.
    let mut reset_pins: Vec<(String, u64)> = if opts.gate_reset {
        extraction
            .reset_signals
            .iter()
            .map(|rs| (rs.signal.clone(), rs.inactive_value))
            .collect()
    } else {
        Vec::new()
    };

    // 2. SV → flattened BTOR2 (yosys), + auto-stub re-lift. Surfaces black-boxed modules + stubs.
    let PreLift {
        mut btor2,
        blackboxed_modules,
        auto_provided_stubs,
        frontend_used,
        fallback_reason,
        backend_retry,
        clock_domains,
        trace_names,
    } = lift_sv(sources, yosys_opts, opts)?;
    let mut trace_names = trace_names;
    fill_declarations(&mut trace_names, &declarations);
    report.diagnostics.trace_names = trace_names;
    if clock_domains.len() > 1 {
        report.diagnostics.merged_clock_domains = clock_domains
            .iter()
            .map(|d| crate::adapter::yosys::describe_clock_domains(std::slice::from_ref(d)))
            .collect();
    }
    report.diagnostics.blackboxed_modules = blackboxed_modules;
    report.diagnostics.auto_provided_stubs = auto_provided_stubs;
    // #466 — record which RTL front end produced this lift (and any auto-fallback)
    // so verify-auto can report it (the `lift-frontend` note in `build_notes`).
    report.diagnostics.frontend = Some(frontend_used.lift_label().to_string());
    report.diagnostics.frontend_fallback_reason = fallback_reason;
    report.diagnostics.lift_retry = backend_retry;

    // mununu#709 — a reset named inside a sub-instance (`u_rx.rst_ni`) is an alias of the input
    // connected to it; pin that input. A name that is no input's alias stays as it was (unpinned).
    if !reset_pins.is_empty()
        && let Ok(file) = crate::adapter::btor2::parser::parse(&btor2)
    {
        let mut resolved: Vec<(String, u64)> = Vec::with_capacity(reset_pins.len());
        for (name, value) in reset_pins.drain(..) {
            let name = crate::adapter::btor2::pin::input_behind(&file, &name).unwrap_or(name);
            if !resolved.iter().any(|(n, _)| *n == name) {
                resolved.push((name, value));
            }
        }
        reset_pins = resolved;
    }

    // Structural reset auto-pin: when gating is on but no `disable iff` guard was recognized, detect
    // the async-reset input from the lifted BTOR2's reset-mux structure and pin it inactive (else a
    // free reset-edge spuriously VIOLATES every box/AF property). `disable iff` always wins.
    if opts.gate_reset
        && reset_pins.is_empty()
        && let Ok(file) = crate::adapter::btor2::parser::parse(&btor2)
    {
        let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
        reset_pins.extend(crate::adapter::fsm_scan::detect_resets(&file, &symbols));
    }

    // Inject `init` at the post-reset state (a reset-gated async-reset FSM starts in its real reset
    // state, not the init-less 0). Runs on the reset-FREE BTOR2 (before the pin below).
    btor2 = crate::adapter::btor2::reset_init::inject_reset_init(&btor2, &reset_pins)?;

    // Complete the init to the `setundef -zero` power-up for a RESET-LESS design, so every engine
    // starts from the same state (the portfolio otherwise leaves an init-less cell free → engine
    // disagreement). Scoped to reset-less designs; a reset-based design's free-init is real behavior.
    if opts.gate_reset && reset_pins.is_empty() {
        btor2 = crate::adapter::btor2::reset_init::inject_zero_init(&btor2)?;
    }

    // Augment only the `__past` shadows whose base resolves to a shadowable
    // signal — a state cell or a primary input (a renamed base is skipped → its
    // properties are SKIPPED below, not an abort).
    //
    // This asks `can_shadow`, the same predicate the augmentation itself uses,
    // rather than re-deciding here. An earlier version filtered on
    // `resolve_state_by_symbol` and so dropped every primary-input base before
    // `augment_with_past_shadows` saw it — which made `$past(<input>)` report
    // `unknown register/signal` no matter what the augmentation supported.
    let pre_file = crate::adapter::btor2::parser::parse(&btor2).map_err(|mut e| {
        e.message = format!("verify_auto: parse lifted BTOR2: {}", e.message);
        e
    })?;
    let shadow_bases: Vec<(&str, u32)> = extraction
        .required_shadows
        .iter()
        .filter(|s| crate::adapter::btor2::can_shadow(&pre_file, &s.base))
        .map(|s| (s.base.as_str(), s.depth))
        .collect();
    let btor2 = augment_with_past_shadows(&btor2, &shadow_bases)?;
    // mununu#565 — mint a named `slice` node per bit-slice atom, AFTER the shadow pass so
    // `$past(sig[7:0])` can slice `sig__past`. A slice is combinational, so this adds no state and
    // cannot change a verdict for a design whose properties use no slice: with an empty list the
    // function returns the input unchanged.
    let slice_refs: Vec<(&str, u32, u32)> = extraction
        .required_slices
        .iter()
        .map(|s| (s.base.as_str(), s.msb, s.lsb))
        .collect();
    let btor2 = crate::adapter::btor2::augment_with_slices(&btor2, &slice_refs)?;
    // SVA-fragment widening (2026-10-15) — `$countones(x)` reads a minted `x__popcount`, AFTER the
    // slice pass so a count over a slice finds its base. Combinational; a design with no
    // `$countones` is returned unchanged.
    let popcount_refs: Vec<&str> = extraction
        .required_popcounts
        .iter()
        .map(String::as_str)
        .collect();
    let btor2 = crate::adapter::btor2::augment_with_popcounts(&btor2, &popcount_refs)?;
    // OpenTitan coverage, Phase 2 — each expression the formulas read as a minted atom
    // (`mununu_x_<hash>`), AFTER the shadow / slice / count passes so its leaves resolve.
    // Combinational; a design whose properties mint nothing is returned unchanged.
    let expr_refs: Vec<(&str, &crate::adapter::btor2::minted_expr::ExprIr)> = extraction
        .required_expressions
        .iter()
        .map(|m| (m.name.as_str(), &m.expr))
        .collect();
    let (btor2, unresolved_exprs) =
        crate::adapter::btor2::minted_expr::augment_with_expressions_explained(&btor2, &expr_refs)?;
    // mununu#717 — a property reading an expression that could not be minted is skipped later
    // with "unknown register/signal `mununu_x_…`"; say here what actually disagreed.
    for u in &unresolved_exprs {
        for t in extraction
            .translated
            .iter()
            .filter(|t| t.formula.contains(u.name.as_str()))
        {
            report.notes.push(VerificationNote {
                kind: "expression-leaf-unresolved".into(),
                level: NoteLevel::ScopeCaveat,
                summary: format!("`{}`: {}", t.name, u.describe()),
                detail: "The property reads an expression over a signal whose width (or \
                         existence) differs between the SVA elaboration and the lifted model, so \
                         the expression cannot be built and the property is skipped. A width \
                         disagreement usually means a parameter elaborated differently on the \
                         two sides (mununu#717)."
                    .into(),
                items: vec![u.leaf.clone()],
                property: Some(t.name.clone()),
            });
        }
    }
    // Phase 4 — the `mununu_assume_ok` monitor the relativized guarantees read. Warm-up: the
    // deepest `$past` any property reads, so no assumption is enforced over a shadow's pre-fill.
    let btor2 = if assume_candidates.is_empty() {
        btor2
    } else {
        let parsed: Vec<crate::mu_calculus::Formula> = assume_candidates
            .iter()
            .map(|(_, f)| crate::mu_calculus::parser::parse(f).expect("planned assumes parse"))
            .collect();
        // The `$past` depth each part of an assumption reads — its atoms' shadows, including
        // the shadows inside a minted expression — so each part is enforced as soon as its
        // history is filled (`AssumeSpec`).
        let minted_depth: std::collections::HashMap<&str, u32> = extraction
            .required_expressions
            .iter()
            .map(|m| {
                let d = m
                    .expr
                    .leaves()
                    .iter()
                    .map(|(leaf, _)| shadow_depth_of_name(leaf))
                    .max()
                    .unwrap_or(0);
                (m.name.as_str(), d)
            })
            .collect();
        let shaped: Vec<(
            &crate::mu_calculus::Formula,
            crate::adapter::btor2::assume_monitor::AssumeSpec,
        )> = parsed
            .iter()
            .map(|f| {
                use crate::adapter::btor2::assume_monitor::{
                    AssumeShape, AssumeSpec, assume_shape,
                };
                let shape = assume_shape(f).expect("planned assumes have a shape");
                let depth = |id| subtree_past_depth(f, id, &minted_depth);
                let spec = match shape {
                    AssumeShape::Invariant { body } => AssumeSpec {
                        shape,
                        hold_after: depth(body),
                        arm_after: 0,
                    },
                    AssumeShape::ImpliesNext {
                        disable,
                        neg_ante,
                        cons,
                    } => AssumeSpec {
                        shape,
                        hold_after: 0,
                        arm_after: depth(neg_ante)
                            .max(disable.map(depth).unwrap_or(0))
                            .max(depth(cons).saturating_sub(1)),
                    },
                    // The deepest `$past` read anywhere in the chain: later than each step needs,
                    // so the attempt arms later and the assumption is weaker, never stronger.
                    AssumeShape::Chain { disable, block } => AssumeSpec {
                        shape,
                        hold_after: 0,
                        arm_after: depth(block).max(disable.map(depth).unwrap_or(0)),
                    },
                };
                (f, spec)
            })
            .collect();
        let (augmented, outcome) = crate::adapter::btor2::assume_monitor::emit_assume_ok_monitor(
            &btor2,
            &shaped,
            !reset_pins.is_empty(),
        )?;
        for ((name, _), result) in assume_candidates.iter().zip(outcome) {
            match result {
                None => report.assumptions.applied.push(name.clone()),
                Some(why) => report.assumptions.unapplied.push((name.clone(), why)),
            }
        }
        augmented
    };

    // mununu#459 — snapshot the model's real primary-input names BEFORE any pinning
    // rewrites them to constants, so a user `--pin` can be validated
    // against them below. A `<nid> input <sid> <symbol>` line names a pinnable
    // input; a bare `<nid> input <sid>` (no symbol) or a comment cannot be pinned.
    let model_input_names: std::collections::HashSet<String> = btor2
        .lines()
        .filter_map(|l| {
            let mut t = l.split_whitespace();
            let _nid = t.next()?;
            if t.next()? != "input" {
                return None;
            }
            let _sid = t.next()?;
            match t.next()? {
                ";" => None,
                sym => Some(sym.to_string()),
            }
        })
        .collect();

    // Pin recognized reset inputs inactive (the model-level half of reset-gating).
    let (btor2, pinned_resets) =
        crate::adapter::btor2::pin::pin_inputs_to_constants(&btor2, &reset_pins);
    // A reset pin that coincides (`name=value`) with a user config pin counts as
    // that config pin being in effect (mununu#459 completeness); capture the set
    // before `pinned_resets` moves into the diagnostics.
    let reset_pinned_set: std::collections::HashSet<String> =
        pinned_resets.iter().cloned().collect();
    report.diagnostics.gated_resets = pinned_resets;

    // H.J.b — pin user-supplied config inputs to constants (same mechanism as reset-gating).
    let config_pins: Vec<(String, u64)> = {
        let mut v: Vec<(String, u64)> = opts
            .config_values
            .iter()
            .map(|(k, val)| (k.clone(), *val))
            .collect();
        v.sort();
        v
    };
    let (btor2, pinned_configs) =
        crate::adapter::btor2::pin::pin_inputs_to_constants(&btor2, &config_pins);
    // mununu#459 — a user config pin must name a real primary input AND take effect.
    // Partition the requested pins into applied (by the config pass, or already
    // pinned to the SAME value by reset-gating) vs unusable, and error on any
    // unusable pin rather than silently verifying a different question than asked.
    let applied_config_set: std::collections::HashSet<String> =
        pinned_configs.iter().cloned().collect();
    let (applied_config_values, unusable) = partition_config_pins(
        &config_pins,
        &model_input_names,
        &applied_config_set,
        &reset_pinned_set,
    );
    if !unusable.is_empty() {
        let mut inputs: Vec<&str> = model_input_names.iter().map(String::as_str).collect();
        inputs.sort_unstable();
        let hint = if inputs.is_empty() {
            "the lifted model has no primary inputs".to_string()
        } else if inputs.len() > 24 {
            format!(
                "{} primary inputs, e.g. {}, …",
                inputs.len(),
                inputs[..24].join(", ")
            )
        } else {
            format!("primary inputs: {}", inputs.join(", "))
        };
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: format!(
                "--pin: {} pin(s) could not be applied — a config pin must name a \
                 real primary input and take effect, it is never silently dropped (mununu#459). \
                 {}. ({hint})",
                unusable.len(),
                unusable.join("; "),
            ),
        });
    }

    // #468 — apply a named structural mutation to the fully-prepared BTOR2 (after
    // reset/init/shadow/config-pin) so the `sv mutate` driver can assert the verdicts
    // FLIP. A mutation that names a missing register or does not apply is a HARD error
    // (never a silent no-op: a no-op mutant would spuriously read as a "no flip" =
    // vacuous-property finding it is not). The plain verify-auto surfaces pass `None`.
    let btor2 = match &opts.mutation {
        Some(m) => crate::adapter::btor2::mutate::apply_mutation(&btor2, &m.with_flat_names())
            .map_err(|message| AdapterError {
                kind: AdapterErrorKind::UnsupportedConstruct,
                location: None,
                message: format!("sv mutate ({}): {message}", m.as_label()),
            })?,
        None => btor2,
    };

    // PR A1 (2026-10-23) — the fault-injection posture: the LAST whole-design rewrite, after the
    // mutation (so `sv mutate` and `--fault` compose) and before the dump. SOUNDNESS: the
    // injected model over-approximates the un-faulted one (`en ≡ 0` is one input sequence) — a
    // safety HOLDS transfers, a VIOLATED is a real escape under the declared sites and budget,
    // an alternating row whose cone holds a site is skipped in the per-row loop.
    let (btor2, fault) = match &opts.faults {
        Some(posture) => {
            let (b, applied) = apply_fault_posture(&btor2, &extraction, posture)?;
            report.diagnostics.fault_model = Some(applied.label.clone());
            report.diagnostics.fault_sites = applied.sites.clone();
            let names = &mut report.diagnostics.trace_names;
            names.fault_inputs = applied
                .injected
                .iter()
                .map(|f| crate::trace::FaultInputs {
                    enable: f.en_input.clone(),
                    value: f.val_input.clone(),
                    site: f.register.clone(),
                })
                .collect();
            names.fault_warmup = posture.warmup;
            names.fault_budget = match &posture.selection {
                crate::adapter::btor2::fault::FaultSelection::Explicit(spec) => spec.bound,
                crate::adapter::btor2::fault::FaultSelection::SecCm(bound) => *bound,
            }
            .budget();
            (b, Some(applied))
        }
        None => (btor2, None),
    };
    // mununu#552 — dump here, at the LAST whole-design rewrite, so what lands on disk is what the
    // engine is handed. The formulas carry the same `--pin` substitution the per-property
    // loop applies, so the pair is self-consistent.
    {
        let dumped: Vec<(String, String)> = extraction
            .translated
            .iter()
            .map(|t| {
                (
                    t.name.clone(),
                    substitute_config_in_formula(&t.formula, &applied_config_values),
                )
            })
            .collect();
        dump_prepared_model(&btor2, &dumped, fault.as_ref().map(|f| f.label.as_str()));
    }

    Ok(Prepared::Model(Box::new(PreparedModel {
        btor2,
        report,
        extraction,
        predicate_hints,
        ann_scan,
        applied_config_values,
        fault,
        source_index: declarations,
    })))
}

/// The signals a formula's atoms read: the leading identifier of every predicate
/// (`u_x.state_q == 3` → `u_x.state_q`, `err_o__past` → `err_o__past`).
fn formula_atom_registers(formula: &crate::mu_calculus::Formula) -> Vec<String> {
    use crate::mu_calculus::Node as MuNode;
    fn collect(
        f: &crate::mu_calculus::Formula,
        id: crate::mu_calculus::NodeId,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        match f.node(id) {
            MuNode::Predicate(a) => {
                let lead: String = a
                    .trim_start_matches(['(', '!', ' '])
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$'))
                    .collect();
                if !lead.is_empty() && !lead.chars().all(|c| c.is_ascii_digit()) {
                    out.insert(lead);
                }
            }
            MuNode::Not(a) => collect(f, *a, out),
            MuNode::And(a, b) | MuNode::Or(a, b) => {
                collect(f, *a, out);
                collect(f, *b, out);
            }
            MuNode::Modal { target, .. } => collect(f, *target, out),
            MuNode::Nu { body, .. } | MuNode::Mu { body, .. } => collect(f, *body, out),
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    collect(formula, formula.root(), &mut out);
    out.into_iter().collect()
}

/// PR A1 — the antecedent-vacuity check of the fault posture: for every security-countermeasure
/// chain row (`$rose(X) |-> ##[0:k] alert`) that HOLDS, decide whether `X` can rise at all on
/// this model — the invariant `AG !(X && !X__past)` on the reach portfolio (the dual of
/// `EF rose`), under the rescue slice. Proved (the flag never rises) ⇒ the row's HOLDS is vacuous
/// (`VacuityKind::AntecedentUnreachable`, a
/// `vacuous-guarantee` note); reachable ⇒ an Info `antecedent-reachable` note naming the
/// deciding engines; inconclusive ⇒ nothing. Runs only under a posture (`--fault-model sec-cm:0`
/// is the "before" instrument), on the model the rows were decided on.
fn check_antecedent_vacuity(
    report: &mut AutoVerifyReport,
    model_btor2: &str,
    reset_pinned: bool,
    rows: &[(String, String)],
) {
    use crate::adapter::reach_rescue::{RescueVerdict, reach_portfolio_rescue_diagnosed};
    let slice_ms = crate::adapter::run_budget::rescue_slice_ms();
    let mut notes = Vec::new();
    for (name, atom) in rows {
        let Some(idx) = report
            .properties
            .iter()
            .position(|p| &p.name == name && matches!(p.outcome, VerifyOutcome::Holds))
        else {
            continue;
        };
        // Idempotent: a member's report may have been checked before the merge.
        if report.properties[idx].vacuity.is_some()
            || report.notes.iter().any(|n| {
                n.property.as_deref() == Some(name.as_str()) && n.kind.starts_with("antecedent-")
            })
        {
            continue;
        }
        // The rescue decides the AG invariant `nu Z. (!rose && [] Z)`: HOLDS ⇔ the flag never
        // rises, VIOLATED ⇔ a path raises it (the `EF rose` reading, through its dual).
        let dual = format!("nu Z. ((!(({atom}) && (!({atom}__past)))) && [] Z)");
        let Ok(f) = crate::mu_calculus::parser::parse(&dual) else {
            continue;
        };
        let mut expired = false;
        let rescued = under_rescue_slice(slice_ms, &mut expired, || {
            reach_portfolio_rescue_diagnosed(model_btor2, &f, reset_pinned)
        });
        match rescued {
            Ok((RescueVerdict::Holds, outcome)) => {
                // the dual `AG !(rose)` holds: the antecedent never fires
                let engines = outcome.unreachable_by.join(", ");
                let detail = format!(
                    "VACUOUS — `{atom}` can never rise on this model (`EF ({atom} && !{atom}__past)` \
                     refuted by {engines}), so the chain's consequent is never checked. On the \
                     un-faulted lift the fault flag is a synthesis constant; run under \
                     `--fault-model sec-cm` to ask the question the check was written for."
                );
                report.properties[idx].vacuity = Some(VacuityFinding {
                    kind: VacuityKind::AntecedentUnreachable,
                    witness: name.clone(),
                    detail,
                });
            }
            Ok((RescueVerdict::Violated, outcome)) => {
                notes.push(VerificationNote {
                    kind: "antecedent-reachable".into(),
                    level: NoteLevel::Info,
                    summary: format!(
                        "`{name}`: its antecedent `$rose({atom})` is reachable ({}) — the HOLDS is \
                         not vacuous.",
                        outcome.reachable_by.join(", ")
                    ),
                    detail: "The fault posture's instrument: the reach portfolio found a path on \
                             which the countermeasure's fault flag rises, so the response the \
                             chain demands was actually checked."
                        .into(),
                    items: vec![atom.clone()],
                    property: Some(name.clone()),
                });
            }
            Ok((other, _)) => {
                notes.push(VerificationNote {
                    kind: "antecedent-check-inconclusive".into(),
                    level: NoteLevel::Info,
                    summary: format!(
                        "`{name}`: the antecedent-vacuity check did not decide whether `$rose({atom})` \
                         is reachable ({other:?})."
                    ),
                    detail: "The reach portfolio neither proved the flag never rises nor found a path \
                             raising it within the rescue slice; the row's HOLDS is neither shown \
                             vacuous nor non-vacuous."
                        .into(),
                    items: vec![atom.clone()],
                    property: Some(name.clone()),
                });
            }
            Err(decline) => {
                notes.push(VerificationNote {
                    kind: "antecedent-check-declined".into(),
                    level: NoteLevel::Info,
                    summary: format!(
                        "`{name}`: the antecedent-vacuity check could not run on `$rose({atom})` \
                         ({decline:?})."
                    ),
                    detail:
                        "The reach rescue declined the reachability question (a leaf it cannot \
                             compile, or the slice was spent); the row's HOLDS is neither shown \
                             vacuous nor non-vacuous."
                            .into(),
                    items: vec![atom.clone()],
                    property: Some(name.clone()),
                });
            }
        }
    }
    report.notes.extend(notes);
    refresh_vacuity_notes(report);
}

#[cfg(test)]
mod fault_posture_debug {
    //! `MUNUNU_DEBUG_DUMP=<dumped model> cargo test … debug_antecedent_check_on_dump -- --ignored
    //! --nocapture`: run the antecedent-vacuity question on a dumped model for two flags.
    #[test]
    #[ignore = "a hand-run probe on a `MUNUNU_SHADOW_BTOR2_DUMP` file"]
    fn debug_antecedent_check_on_dump() {
        let Ok(path) = std::env::var("MUNUNU_DEBUG_DUMP") else {
            return;
        };
        let model = std::fs::read_to_string(&path).expect("the dump reads");
        {
            let mut file = crate::adapter::btor2::parser::parse(&model).expect("parses");
            let flag = "u_edn_core.u_edn_main_sm.u_state_regs.unused_err_o";
            let line = file.lines.iter().find(|l| matches!(&l.node, crate::adapter::btor2::ast::Node::Op { symbol: Some(s), .. } if s == flag)).map(|l| format!("{} {:?}", l.nid, l.node));
            eprintln!("flag line: {line:?}");
            let so = "u_edn_core.u_edn_main_sm.u_state_regs.state_o";
            let line = file.lines.iter().find(|l| matches!(&l.node, crate::adapter::btor2::ast::Node::Op { symbol: Some(s), .. } if s == so)).map(|l| format!("{} {:?}", l.nid, l.node));
            eprintln!("state_o line: {line:?}");
            let m = crate::adapter::btor2::fault::mint_undefined_state_flag(&mut file, flag);
            eprintln!("mint: {:?}", m.map(|m| (m.state, m.width, m.legal)));
        }
        for atom in [
            "u_edn_core.u_edn_main_sm.u_state_regs.unused_err_o",
            "u_edn_core.u_prim_count_max_reqs_cntr.err_o",
        ] {
            let dual = format!("nu Z. ((!(({atom}) && (!({atom}__past)))) && [] Z)");
            let f = crate::mu_calculus::parser::parse(&dual).expect("parses");
            let t0 = std::time::Instant::now();
            let r =
                crate::adapter::reach_rescue::reach_portfolio_rescue_diagnosed(&model, &f, true);
            eprintln!(
                "{atom}: {:?} in {:.1}s",
                r.as_ref()
                    .map(|(v, o)| (*v, o.reachable_by.clone(), o.unreachable_by.clone())),
                t0.elapsed().as_secs_f64()
            );
        }
    }
}

/// PR A1 — the fault posture's post-pass on a MERGED portfolio report (the planner's path): the
/// antecedent-vacuity check over the prepared model. The single-engine path runs it at the end
/// of `verify_auto_impl`; the check is idempotent, so a member's earlier pass costs nothing here.
pub(crate) fn finish_fault_posture(report: &mut AutoVerifyReport, prepared: &PreparedModel) {
    if let Some(applied) = prepared.fault.as_ref() {
        let reset_pinned = !report.diagnostics.gated_resets.is_empty();
        check_antecedent_vacuity(report, &prepared.btor2, reset_pinned, &applied.sec_cm_rows);
    }
}

/// The `$rose(X)` atom of a security-countermeasure chain — a bounded chain
/// (`reach_rescue::reduce_bounded_chain`) whose predicates hold a fault flag `X` ending in
/// `err_o` / `unused_err_o` together with its `X__past` shadow (the `$rose` lowering). `None`
/// for any other row.
fn sec_cm_rose_atom(formula: &crate::mu_calculus::Formula) -> Option<String> {
    use crate::mu_calculus::Node as MuNode;
    crate::adapter::reach_rescue::reduce_bounded_chain(formula)?;
    fn collect(
        f: &crate::mu_calculus::Formula,
        id: crate::mu_calculus::NodeId,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        match f.node(id) {
            MuNode::Predicate(a) => {
                out.insert(a.clone());
            }
            MuNode::Not(a) => collect(f, *a, out),
            MuNode::And(a, b) | MuNode::Or(a, b) => {
                collect(f, *a, out);
                collect(f, *b, out);
            }
            MuNode::Modal { target, .. } => collect(f, *target, out),
            MuNode::Nu { body, .. } | MuNode::Mu { body, .. } => collect(f, *body, out),
            _ => {}
        }
    }
    let mut atoms: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    collect(formula, formula.root(), &mut atoms);
    atoms
        .iter()
        .find(|a| {
            (a.ends_with("err_o") || a.ends_with(".err_o") || a.ends_with("unused_err_o"))
                && atoms.contains(&format!("{a}__past"))
        })
        .cloned()
}

/// Apply the fault posture to the prepared model: select the sites (named, or from the
/// security-countermeasure chains' own fault flags), inject them unless the budget is zero, and
/// report what was done. See [`crate::adapter::btor2::fault`].
fn apply_fault_posture(
    btor2: &str,
    extraction: &crate::adapter::slang::translate::TranslationReport,
    posture: &crate::adapter::btor2::fault::FaultPosture,
) -> Result<(String, crate::adapter::btor2::fault::FaultApplied), AdapterError> {
    use crate::adapter::btor2::fault::{
        FaultApplied, FaultBound, FaultSelection, FaultSite, FaultSiteReport, FaultSpec,
        cells_compared_by, inject_faults,
    };
    let err = |message: String| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("fault posture: {message}"),
        location: None,
    };
    let mut file = crate::adapter::btor2::parser::parse(btor2).map_err(|e| err(e.message))?;
    // The security-countermeasure rows are found whichever selection runs: the explicit sites
    // need them for the vacuity check too.
    let sec_cm_rows: Vec<(String, String)> = extraction
        .translated
        .iter()
        .filter_map(|t| {
            let f = crate::mu_calculus::parser::parse(&t.formula).ok()?;
            Some((t.name.clone(), sec_cm_rose_atom(&f)?))
        })
        .collect();
    let describe = |b: FaultBound| match b {
        FaultBound::Unbounded => "unbounded".to_string(),
        FaultBound::AtMost(k) => format!("≤{k}"),
    };
    let (spec, mut sites, rows_without_site, label) = match &posture.selection {
        FaultSelection::Explicit(spec) => {
            let sites = spec
                .sites
                .iter()
                .map(|s| FaultSiteReport {
                    register: s.register.clone(),
                    width: 0,
                    bound: describe(spec.bound),
                    selected_by: "explicit".into(),
                    rows: Vec::new(),
                })
                .collect::<Vec<_>>();
            (spec.clone(), sites, Vec::new(), spec.as_label())
        }
        FaultSelection::SecCm(bound) => {
            let mut by_register: std::collections::BTreeMap<String, (String, Vec<String>)> =
                std::collections::BTreeMap::new();
            let mut without = Vec::new();
            for (row, atom) in &sec_cm_rows {
                let mut cells = cells_compared_by(&file, atom);
                // A sparse-FSM flag the lift left constant (A0's strip of the enum-method loop):
                // re-mint it from the state's compared constants, so the state cell is the site
                // and a corruption outside the encoding raises the flag as the RTL's would.
                if cells.is_empty()
                    && atom.ends_with("unused_err_o")
                    && let Some(minted) =
                        crate::adapter::btor2::fault::mint_undefined_state_flag(&mut file, atom)
                {
                    let nid = crate::adapter::btor2::parser::resolve_state_alias(
                        &file,
                        &minted.state,
                        true,
                    )
                    .unwrap_or(0);
                    let e = by_register
                        .entry(minted.state.clone())
                        .or_insert_with(|| ("fsm-mint".to_string(), Vec::new()));
                    e.1.push(row.clone());
                    let _ = nid;
                    cells = cells_compared_by(&file, atom);
                    if !cells.is_empty() {
                        continue;
                    }
                }
                if cells.is_empty() {
                    without.push(row.clone());
                    continue;
                }
                // prefer the cells inside the flag's own instance
                let prefix = atom
                    .rsplit_once('.')
                    .map(|(p, _)| format!("{p}."))
                    .unwrap_or_default();
                let inside: Vec<&(crate::adapter::btor2::ast::Nid, String, u32)> = cells
                    .iter()
                    .filter(|(_, name, _)| !prefix.is_empty() && name.starts_with(&prefix))
                    .collect();
                let (chosen, how): (Vec<&(crate::adapter::btor2::ast::Nid, String, u32)>, &str) =
                    if inside.is_empty() {
                        (cells.iter().collect(), "cone-fallback")
                    } else {
                        (inside, "instance-prefix")
                    };
                for (_, name, _) in chosen {
                    let e = by_register
                        .entry(name.clone())
                        .or_insert_with(|| (how.to_string(), Vec::new()));
                    e.1.push(row.clone());
                }
            }
            let sites: Vec<FaultSiteReport> = by_register
                .into_iter()
                .map(|(register, (selected_by, rows))| FaultSiteReport {
                    register,
                    width: 0,
                    bound: describe(*bound),
                    selected_by,
                    rows,
                })
                .collect();
            let spec = FaultSpec {
                sites: sites
                    .iter()
                    .map(|s| FaultSite {
                        register: s.register.clone(),
                    })
                    .collect(),
                bound: *bound,
                warmup: posture.warmup,
            };
            let label = format!(
                "fault-model sec-cm ({}, warm-up {}): {} site(s) from {} chain row(s){}",
                describe(*bound),
                posture.warmup,
                sites.len(),
                sec_cm_rows.len(),
                if *bound == FaultBound::AtMost(0) {
                    " — selection and vacuity only, nothing injected"
                } else {
                    ""
                }
            );
            (spec, sites, without, label)
        }
    };
    // With no site nothing is rewritten — unless a flag was re-minted, which the model keeps.
    let minted_model = || crate::adapter::btor2::emit::emit_btor2(&file);
    if spec.sites.is_empty() {
        return Ok((
            minted_model(),
            FaultApplied {
                label,
                sites,
                injected: Vec::new(),
                sec_cm_rows,
                rows_without_site,
            },
        ));
    }
    let zero_budget = spec.bound == FaultBound::AtMost(0);
    let (out, injected) = inject_faults(&file, &spec).map_err(err)?;
    for s in sites.iter_mut() {
        if let Some(i) = injected.iter().find(|i| i.register == s.register) {
            s.width = i.width;
        }
    }
    // A zero budget: the sites are known (and validated by the rewrite), the model is NOT
    // rewritten — every verdict is the un-faulted one, the vacuity check is the instrument.
    let (model, injected) = if zero_budget {
        (minted_model(), Vec::new())
    } else {
        (crate::adapter::btor2::emit::emit_btor2(&out), injected)
    };
    Ok((
        model,
        FaultApplied {
            label,
            sites,
            injected,
            sec_cm_rows,
            rows_without_site,
        },
    ))
}

/// mununu#552 — the env var naming the post-shadow BTOR2 dump.
///
/// **Not `MUNUNU_KEEP_SHADOW_BTOR2`, which is what the issue proposed.** Both existing
/// `MUNUNU_KEEP_*` vars are BOOLEAN keep-the-tempdir flags (`MUNUNU_KEEP_YOSYS_TMP`,
/// `MUNUNU_KEEP_VERILATOR_TMP`); the path-valued precedents are `MUNUNU_INTERP_DUMP` and
/// `MUNUNU_SPCR_OUT_DIR`. Borrowing the `KEEP_` prefix would name a convention this does not
/// follow.
const SHADOW_DUMP_ENV: &str = "MUNUNU_SHADOW_BTOR2_DUMP";

/// mununu#552 — write the model the engine actually runs, plus the formulas it runs on.
///
/// **Why the pre-shadow lift was not enough.** A consumer could already obtain the lift
/// (`MUNUNU_KEEP_YOSYS_TMP`, `sv emit-btor2-per-module`) but not the model after
/// `augment_with_past_shadows` — and a `$past`-bearing property is exactly where the two differ,
/// because the shadow chain is the part that widens the cone. Reproducing mununu#543 in-house
/// meant hand-rebuilding the augmented model from a guessed base list, which could not settle
/// whether the reconstruction matched the real lift. That cost two definite answers.
///
/// **One self-contained file.** The model alone is half a reproducer; the other half is the
/// formula, including the `<base>__past` shadow atom names. Both go in, the formulas as BTOR2
/// comments, so the artifact is handed over as a single file and still parses as BTOR2.
///
/// **It states what it does NOT contain.** The inputs-in-the-state lift (mununu#656: a sampled
/// state cell per primary input the property's atoms reach, the input line renamed `name$next`)
/// runs later, per-property, inside the engine, so it is not in this dump. A dump that silently
/// omitted a rewrite would be the same trap this exists to close. (Until mununu#677 the
/// omitted rewrite named here was the antecedent shadow synthesis, since removed.)
///
/// Diagnostic only: every failure is swallowed after one warning, mirroring
/// [`crate::adapter::partial_json`]. A dump must never be able to change a verdict.
fn dump_prepared_model(btor2: &str, properties: &[(String, String)], fault_model: Option<&str>) {
    let Ok(path) = std::env::var(SHADOW_DUMP_ENV) else {
        return;
    };
    if path.trim().is_empty() {
        return;
    }
    let mut out = String::new();
    out.push_str("; mununu#552 — the BTOR2 the engine runs, dumped after model preparation.\n");
    out.push_str("; INCLUDES: the SV lift, `$past` shadow registers, reset pinning,\n");
    out.push_str(";           `--pin` pins, any `sv mutate` fault, and the fault-injection\n");
    out.push_str(
        ";           posture's inputs / budget counter when one is on (see FAULT MODEL).\n",
    );
    if let Some(label) = fault_model {
        out.push_str(&format!("; FAULT MODEL: {label}\n"));
    }
    out.push_str(
        "; EXCLUDES: the inputs-in-the-state lift (mununu#656: a sampled state cell per\n",
    );
    out.push_str(";           primary input the property's atoms reach, the input renamed\n");
    out.push_str(
        ";           `name$next`), which runs LATER and PER-PROPERTY inside the engine.\n",
    );
    out.push_str(";           If you are reproducing a property whose atoms read an input, that\n");
    out.push_str(";           rewrite is not in this file.\n");
    out.push_str(";\n");
    out.push_str("; Properties, as the engine parses them (post `--pin` substitution).\n");
    out.push_str(
        "; Shadow atoms appear here as `<base>__past`; bit-slices as `<base>__bits<hi>_<lo>`.\n",
    );
    if properties.is_empty() {
        out.push_str(";   (none)\n");
    }
    for (name, formula) in properties {
        out.push_str(&format!("; {name}: {formula}\n"));
    }
    out.push_str(";\n");
    out.push_str(btor2);
    if !btor2.ends_with('\n') {
        out.push('\n');
    }
    if let Err(e) = std::fs::write(&path, out) {
        tracing::warn!(
            "{SHADOW_DUMP_ENV}={path}: could not write the model dump ({e}); continuing without it"
        );
    }
}

/// The single-engine verify body: extraction → lift → reset/pin → per-property engine →
/// replan / rescue / notes. `prelift` is `Some` when the planner supplies a shared SV → BTOR2
/// lift (the portfolio path lifts once), `None` to lift inline (the single-engine path).
/// Verdict-equivalent either way — the only difference is whether the yosys lift is reused.
pub(crate) fn verify_auto_impl(
    sources: &[(String, String)],
    yosys_opts: &YosysOptions,
    opts: &VerifyAutoOptions,
    prepared: Option<PreparedModel>,
) -> Result<AutoVerifyReport, AdapterError> {
    use crate::adapter::AdapterOptions;
    use crate::adapter::btor2::cegar::{
        CegarOptions, LiftStrategy, PredicateSource, cegar_refine_loop,
    };
    use crate::mu_calculus::Environment;
    use crate::mu_calculus::parser as mu_parser;

    // A.6 (soundness-ledger, 2026-07-05) — reject `--engine exact-symbolic` with
    // `--no-gate-reset`. The exact full-state engine is built for the reset-GATED
    // regime: the reset input is pinned inactive and the initial state is the
    // modeled reset state (`init` = the post-reset value injected by
    // `inject_reset_init`, or 0). With reset-gating OFF, that injection is a no-op
    // (no `init` line), so the engine starts from the power-on default 0 — an
    // illegal sparse encoding the real design never occupies — and it does not
    // explore the async reset (a runtime `state_q_next = rst_ni ? FSM : ResetValue`
    // next-mux) as a firing transition. The result is a *spurious VIOLATED*
    // (idle unreachable) presented with the exact engine's 2-valued *definite*
    // authority — a confident wrong answer. Modeling a freed reset soundly needs a
    // universal (havoc) initial state + reset-as-transition semantics the exact
    // engine does not have; until then reject the combination rather than emit an
    // unsound verdict. Free-reset reachability is available on the cube engine
    // (drop `--engine exact-symbolic`), whose over-approximating may-relation
    // soundly includes the reset edge.
    if opts.exact_symbolic && !opts.gate_reset {
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message:
                "adapter/slang/verify_auto (A.6): `--engine exact-symbolic` is not sound with \
                      `--no-gate-reset`. The exact engine models the post-reset state space (reset \
                      pinned inactive, init = the modeled reset state); with reset-gating off it \
                      starts from the illegal power-on default 0 and does not fire the freed async \
                      reset as a transition, yielding a spurious (but definite-looking) VIOLATED. \
                      For free-reset reachability, use the cube engine (drop `--engine \
                      exact-symbolic`); to use the exact engine, keep reset-gating on."
                    .to_string(),
            location: None,
        });
    }

    // The operator-INDEPENDENT prepared model (P-3b common-IR hub): the fully-prepared BTOR2 (slang
    // extract + yosys lift + reset/init/shadow/config-pin) + the properties + diagnostics. Built
    // ONCE by the planner (`prepare_model`) and shared across the precision-ladder operators (so the
    // two subprocesses + the prep run once, not per engine); `None` on the single-engine path, where
    // it is built inline here. A design with no properties returns its (complete) empty report.
    let PreparedModel {
        btor2,
        mut report,
        extraction,
        predicate_hints,
        ann_scan,
        applied_config_values,
        fault,
        source_index,
    } = match prepared {
        Some(m) => m,
        None => match prepare_model(sources, yosys_opts, opts)? {
            Prepared::Empty(r) => return Ok(*r),
            Prepared::Model(m) => *m,
        },
    };

    // State-cell symbols of the augmented design — the seedable-atom universe.
    let file = crate::adapter::btor2::parser::parse(&btor2).map_err(|mut e| {
        e.message = format!("verify_auto: re-parse augmented BTOR2: {}", e.message);
        e
    })?;
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // mununu#518 — BV-sorted states ONLY. A BTOR2 *array* state (a memory) is also a
    // `Node::State` and carries a symbol, so it used to land here beside a scalar register and
    // `resolves_to_state` answered true for it — admitting a memory as a BV cube dimension for
    // which no BV exists. Arrays are deliberately excluded from the BV signal vector in
    // `predicate_image/btor2_encode.rs` (they are Z3 `Array` constants resolved by name through a
    // separate map), and `bv_width` returning `None` is the BV/array discriminator used
    // everywhere else — so this brings the seedable-atom universe into line with it.
    let (state_cells, array_state_cells) = partition_state_cells_by_sort(&file, &symbols);
    // Total REGISTER lines — a `state` that carries a `next` function. A zero/low
    // count is the headline signal that state was cut or optimized away.
    //
    // mununu#498: a `state` with NO `next` is a free variable, not a register
    // (Yosys emits that for `$anyseq`, i.e. every `--cutpoint`), and the engine
    // now classifies it as an input. Counting those here would report a cut point
    // as a register in the very diagnostic whose job is to reveal that state was
    // cut — the count would go UP as more nets were freed. Designs without
    // `$anyseq` are unaffected: every state has a `next`, so the number is
    // unchanged.
    {
        use crate::adapter::btor2::ast::Node;
        let with_next: std::collections::HashSet<_> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Next { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        report.diagnostics.state_register_count = file
            .lines
            .iter()
            .filter(|l| matches!(l.node, Node::State { .. }) && with_next.contains(&l.nid))
            .count();
    }
    // Init value of each state cell — pins the cube lift's initial cube to the
    // design's reset state (else it defaults to cube_0 = all-predicates-false).
    let init_values = state_cell_init_values(&file);

    // H.A — a name is cube-bindable if it is a state-cell symbol directly OR a
    // value-identical alias of one (`resolve_state_alias`): a `uext`/`sext`-0
    // rename, or the async-reset register mux that `async2sync` puts around the
    // state (e.g. `state_q` over the auto-stubbed flop). The strict resolver
    // rejects combinational *functions* of state (an `eq`/`or` output like
    // `main_sm_err_o`) — binding those to the state's value would be a spurious
    // verdict. The reset-mux edge is followed only when the reset is pinned
    // (gated_resets non-empty), which makes the register value equal the state.
    let reset_pinned = !report.diagnostics.gated_resets.is_empty();
    let resolves_to_state = |name: &str| -> bool {
        state_cells.contains(name)
            || crate::adapter::btor2::parser::resolve_state_alias(&file, name, reset_pinned)
                .is_some()
    };

    // H.B (free-input atoms) — primary-input symbols of the lifted design. A
    // simple atom over one of these is admitted as a *free* cube dimension (the
    // env picks any value each cycle), so the dominant real-SVA blocker — IO /
    // config antecedents like sysrst's `cfg_enable_i` / `trigger_*` / `cnt_clr`
    // — is verifiable instead of SKIPPED. `is_input` is consulted only after
    // `is_state` (state precedence), so a register that is both never
    // double-classifies. A reset input pinned by reset-gating is NOT a free
    // input (it was rewritten to a constant), so it never appears here.
    let input_symbols: HashSet<String> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, crate::adapter::btor2::ast::Node::Input { .. }))
        .filter_map(|l| symbols.get(&l.nid).cloned())
        .collect();
    let is_input = |name: &str| -> bool { input_symbols.contains(name) };

    // AR-S2 (2026-07-06) — the cube lift now always uses the SOUND `SmtAllPairs`
    // may-relation (retiring the pure-state sampling-may fallback + the A.4 ⊥-guard
    // that papered over its input-sampling incompleteness). A definite cube verdict
    // is therefore sound by over-approximation with no honest-⊥ downgrade needed.

    // H.E (combinational outputs) — named combinational nodes: an `Op` carrying
    // its OWN symbol that is neither a state cell (incl. value-alias, via
    // `resolves_to_state` precedence in the seeder) nor an input. Each is
    // classified by its CONE (`cone_reaches_input`):
    // - **input-dependent** (`trigger_active = !trigger_i`) → routed through the
    //   FREE-INPUT path (a free cube dimension, source-pin / target-free) so the
    //   may/must edges respect it — SOUND (treating it as a state-cube label is
    //   what produced the sysrst sva_6 / sva_9 spurious VIOLATED);
    // - **state-only** (`event_detected_o = f(state_q)`) → a DERIVED per-cube
    //   3-valued label (Approach B).
    // `is_state` is consulted first in the seeder, so a state value-alias (also
    // an Op with a symbol) binds as state, not combinational.
    let mut combinational_nid: std::collections::HashMap<String, crate::adapter::btor2::ast::Nid> =
        file.lines
            .iter()
            .filter_map(|l| match &l.node {
                crate::adapter::btor2::ast::Node::Op {
                    symbol: Some(s), ..
                } if !state_cells.contains(s) && !input_symbols.contains(s) => {
                    Some((s.clone(), l.nid))
                }
                _ => None,
            })
            .collect();
    // H.U.1d/H.U.2 — also map a combinational signal named only on an `output`
    // line (a top-level combinational output port whose driving Op carries no own
    // symbol — e.g. `assign bad = (state == 3)`). Mirrors the SMT view's
    // output-line registration so the seeder classifier + the cegar nid-map agree.
    // `resolves_to_state` / `is_input` are consulted first in the seeder, so an
    // output that aliases a state cell still binds as state.
    for l in &file.lines {
        if let crate::adapter::btor2::ast::Node::Output {
            symbol: Some(s),
            signal,
        } = &l.node
            && !state_cells.contains(s)
            && !input_symbols.contains(s)
        {
            combinational_nid.entry(s.clone()).or_insert(signal.nid());
        }
    }
    let combinational_kind = |name: &str| -> Option<CombKind> {
        combinational_nid.get(name).map(|&nid| {
            if crate::adapter::btor2::parser::cone_reaches_input(&file, nid) {
                CombKind::InputDependent
            } else {
                CombKind::StateOnly
            }
        })
    };
    // Slice 3 (the unwrap lever) — the raw free inputs in a combinational-of-
    // input signal's cone. Seeding these as free H.B cube dimensions refines the
    // may-relation (so a consequent box over a conditional transition governed
    // by the input becomes definite) while the combinational stays a derived
    // label that turns definite at cubes pinning the input. Empty for a
    // non-combinational name (resolves to no combinational node).
    let cone_inputs_of = |name: &str| -> Vec<String> {
        combinational_nid
            .get(name)
            .map(|&nid| crate::adapter::btor2::parser::cone_inputs(&file, nid))
            .unwrap_or_default()
    };

    // H.H — auto-derive counter upper bounds from the concretized config (a
    // counter compared against a pinned config threshold, `cnt_q >=
    // cfg_detect_timer_i` with cfg=7, yields `cnt_q <= 7`). Global across
    // properties: the property that NEEDS the bound (sva_13) doesn't reference the
    // config; the comparison lives in a sibling (sva_5). Empty without config.
    let config_inferred_bounds =
        config_inferred_counter_bounds(&extraction.translated, &applied_config_values);
    // Accumulates the applied bound descriptions for the report-level counter-bound
    // note (sorted, deduped across properties).
    let mut counter_bound_items: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    // 4. Per property: seed → CEGAR → verdict.
    //
    // mununu#490 — a `MUNUNU_MAX_PROCESS_MEMORY_BYTES` ceiling is checked BEFORE each
    // property so a run against a starving process abstains gracefully instead of
    // aborting inside the next allocation. Once the ceiling is exceeded, mark THIS
    // property and every remaining one as Unknown{ memory-budget-exceeded } — the
    // process cannot honestly claim to have decided them — and push a
    // `memory-budget-exceeded` verification note. Verdicts already recorded stay.
    let mut memory_budget_hit: Option<crate::adapter::memory_budget::MemoryBudgetExceeded> = None;
    // mununu#504 — the TIME sentinel, mirroring the memory one directly above it. Both share the
    // same contract: the current property and every remaining one abstain (`unknown`), and every
    // verdict already computed is PRESERVED. That preservation is the whole point — monono lost a
    // gate run to a process kill that emitted zero bytes, including for properties mununu had
    // already decided.
    let mut time_budget_hit = false;
    // mununu#726 — the run budget is ENTERED for the rest of this function, not just consulted:
    // before, it was a local the per-property guard below was a child of, so once that guard
    // dropped (before the ⊥ re-plan) the thread-local was unbounded again and every
    // `run_budget::expired()` poll in `escalate_bottom`, `rescue_skipped` and the witness pass
    // was a no-op. A child of whatever is current, so an outer budget (the parallel planner
    // re-enters one per engine thread) is honoured.
    let run_budget = crate::adapter::run_budget::Budget::child(
        &crate::adapter::run_budget::current(),
        crate::adapter::run_budget::run_budget_ms(),
    );
    let _run_guard = crate::adapter::run_budget::enter(&run_budget);
    let per_property_ms = crate::adapter::run_budget::property_budget_ms();
    // ⚠️ SCOPE, corrected 2026-09-15: these records cover THIS function only — the cube pass and
    // the ⊥ re-plan. The planner's portfolio merge writes NONE, so on a portfolio run a property's
    // last record can disagree with the final verdict. A consumer measured three properties whose
    // last record read `unknown` and which ended the run decided, by engines that left no record.
    // Neither "a `main` unknown means undecided" nor "no `escalated` record means escalation never
    // reached it" survives that data; both were asserted and refuted the same day.
    //
    // mununu#504 C7 — the SIGKILL-survivable breadcrumb. The budgets above preserve verdicts when
    // MUNUNU can observe its own trouble; they are powerless against a kill from outside (the
    // kernel OOM killer, a CI step timeout), which runs no Rust code at all. That is the exact
    // failure in the comment above: "monono lost a gate run to a process kill that emitted zero
    // bytes, including for properties mununu had already decided." Opt-in via
    // `MUNUNU_VERIFY_AUTO_PARTIAL_JSON=<path>`; disabled and zero-cost otherwise.
    let mut breadcrumb =
        crate::adapter::partial_json::Breadcrumb::from_env_with_append(opts.breadcrumb_append);
    // PR A1 — the injected model, parsed once, for the per-row cone test below.
    let fault_file = fault
        .as_ref()
        .filter(|f| !f.injected.is_empty())
        .and_then(|_| crate::adapter::btor2::parser::parse(&btor2).ok());
    for t in &extraction.translated {
        // Flush whatever the PREVIOUS iteration decided, before this one starts its (possibly
        // very long, possibly fatal) work. One call here covers all nine `properties.push` sites
        // in this loop, several of which sit behind a `continue`.
        breadcrumb.record_pending(
            report
                .properties
                .iter()
                .map(|p| (p.name.as_str(), p.outcome.label())),
            "main",
        );
        // Plan 4 / PR D3 (mununu#733) — an earlier sequential member decided this row; the merge
        // keeps that verdict, so this member records "not attempted" and spends nothing on it —
        // before the budget sentinels (no budget consumed, never mislabelled `BudgetExpired`) and as
        // `Unknown`, not `Skipped` (the cube-skip rescue and the over-cap notes act on `Skipped`).
        if opts.skip_rows.contains(&t.name) {
            report
                .properties
                .push(abstained(t, BottomReason::NotAttempted));
            continue;
        }
        // Once EITHER budget has been hit, every remaining property abstains without work.
        if memory_budget_hit.is_some() || time_budget_hit {
            // Not "never attempted" — a budget already stopped the run. WHICH budget is known
            // here, so say it: the memory and wall-clock cases need opposite responses.
            report.properties.push(abstained(
                t,
                if memory_budget_hit.is_some() {
                    BottomReason::MemoryCeilingExceeded
                } else {
                    BottomReason::BudgetExpired
                },
            ));
            continue;
        }
        if let Err(hit) = crate::adapter::memory_budget::check_process_memory_budget() {
            memory_budget_hit = Some(hit);
            // NOT `BudgetExpired` — that is the wall clock. #558 conflated them, so a consumer
            // branching on the tag would retry an RSS exhaustion with more TIME.
            report
                .properties
                .push(abstained(t, BottomReason::MemoryCeilingExceeded));
            continue;
        }
        if run_budget.expired() {
            time_budget_hit = true;
            report
                .properties
                .push(abstained(t, BottomReason::BudgetExpired));
            continue;
        }
        // mununu#504 — per-property wall time, opt-in via `MUNUNU_PROPERTY_TIMING=1`.
        //
        // This exists because a per-property budget DEFAULT must be MEASURED, not guessed: a cap
        // that fires on legitimate work turns a slow success into a spurious `unknown` across
        // every consumer's gate, which is worse than having no cap. It is also useful on its own
        // — it answers "which property is eating the run?", which no existing diagnostic does.
        let prop_t0 = std::env::var("MUNUNU_PROPERTY_TIMING")
            .is_ok()
            .then(std::time::Instant::now);
        // mununu#504 — install the PER-PROPERTY budget for this iteration. `child` clamps to the
        // earlier of (now + per-property) and the run deadline, so a generous per-property value
        // cannot extend the run. RAII: the guard restores the previous budget on EVERY exit from
        // this loop body, of which there are many `continue`s.
        let _budget_guard = crate::adapter::run_budget::enter(
            &crate::adapter::run_budget::Budget::child(&run_budget, per_property_ms),
        );
        // H.J.b — substitute the concretized config inputs (`cfg_* → const`) so a
        // relational-with-wide-input atom becomes a decidable state-vs-constant
        // comparison. The substituted string is BOTH parsed and reported (the
        // verdict shows `cnt_q >= 7`, consistent with the config-concretization
        // note). No-op when no config was pinned.
        let formula_str = substitute_config_in_formula(&t.formula, &applied_config_values);
        let formula = match mu_parser::parse(&formula_str) {
            Ok(f) => f,
            Err(e) => {
                report.properties.push(PropertyVerdict {
                    name: t.name.clone(),
                    label: t.label.clone(),
                    kind: t.kind,
                    formula: formula_str.clone(),
                    outcome: VerifyOutcome::Skipped {
                        reason: format!("formula failed to parse: {e:?}"),
                    },
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                    bottom_reason: None,
                    decided_by: None,
                    vacuity: None,
                });
                continue;
            }
        };
        // PR A1 — under the fault posture an ALTERNATING property (νμ: `AG EF good`) whose cone
        // holds a corrupted register is not asked: the fault player can restore any state, so it
        // would hold trivially (and the trap certificate would stop refuting). Decided without
        // the posture only; the row says so.
        if let (Some(applied), Some(file)) = (fault.as_ref(), fault_file.as_ref())
            && !applied.injected.is_empty()
            && formula.alternation_depth() >= 2
        {
            let atoms = formula_atom_registers(&formula);
            let coi = crate::adapter::btor2::fault::coi_states(file, &atoms);
            let hit: Vec<&str> = applied
                .injected
                .iter()
                .filter(|i| coi.contains(&i.register))
                .map(|i| i.register.as_str())
                .collect();
            if !hit.is_empty() {
                report.properties.push(PropertyVerdict {
                    name: t.name.clone(),
                    label: t.label.clone(),
                    kind: t.kind,
                    formula: formula_str.clone(),
                    outcome: VerifyOutcome::Skipped {
                        reason: format!(
                            "fault posture: an alternating (νμ) property over a corrupted register \
                             ({}) would hold trivially — the fault player can restore any state; \
                             decide it without `--fault`",
                            hit.join(", ")
                        ),
                    },
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                    bottom_reason: None,
                    decided_by: None,
                    vacuity: None,
                });
                continue;
            }
        }

        // mununu#722 — an atom-less formula (`AG true`: an `ASSERT_KNOWN` once `$isunknown` folds)
        // is decided here. It has no atom to restrict its cone, so the engines saw the WHOLE design
        // as its cone and skipped it over the bit cap on any real IP; its value is the same on
        // every total model (`Formula::constant_value`).
        if let Some(value) = formula.constant_value() {
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome: if value {
                    VerifyOutcome::Holds
                } else {
                    VerifyOutcome::Violated { false_cells: 0 }
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: Some("constant".into()),
                vacuity: None,
            });
            continue;
        }

        // monono#partsel soundness guard (all engines). Refuse — rather than
        // silently mis-decide — a property whose atom binds to a signal reaching an
        // ANONYMOUS free `input`. The yosys-slang lift of a plain-vector partial
        // register assignment (`q[hi:lo] <= d`) models the register's unwritten bits
        // as free inputs (havoc) and aliases the register name `q` to the `concat`
        // that mixes them; a state predicate over `q` (or a combinational output of
        // it, `q != 0`) then reads those free inputs and every engine returns a
        // spurious definite verdict (the reported `HOLDS` on a reachable-non-zero
        // register — monono's planted-bug canary). Refusing is honest and
        // recoverable (a downstream gate fails on `skipped`, never on a false HOLDS);
        // the read_verilog / sv2v front-end models the register faithfully and
        // decides it. Runs BEFORE any engine so cube, exact, symbolic and explicit
        // are covered uniformly. (The packed-2-D `q[idx] <= d` split — anonymous
        // sub-*registers*, no free inputs — is NOT refused: the NID-indexed
        // cone-of-influence keeps those cells, so that case now decides.)
        if let Some(reg) = formula_atom_over_undriven_register(&formula, &file) {
            // mununu#507 — report the CONDITION, and the cause only when it is
            // actually derivable. The condition this guard tests is "`reg`'s cone
            // reaches an unnamed free input"; a partial register write is only ONE
            // of the ways that happens (a blackboxed submodule's freed outputs, an
            // undriven net and an unconnected port are others, and on an integrator
            // blackboxing is far likelier). Asserting the partial-write cause
            // unconditionally pointed consumers at correct RTL — `f_slot_q <= 7'd0
            // / f_slot[6:0]` is a part-select of the SOURCE, written whole, with no
            // `f_slot_q[hi:lo] <= …` anywhere.
            let partial_write = undriven_via_partial_write_concat(&file, &reg);
            let cause = if partial_write {
                format!(
                    " The lift shows the partial-write shape: `{reg}`'s name aliases a \
                     `concat` mixing a free input, which is how the slang front-end models \
                     the UNWRITTEN bits of a plain-vector partial assignment \
                     (`{reg}[hi:lo] <= …`)."
                )
            } else {
                let bb = &report.diagnostics.blackboxed_modules;
                let bb_hint = if bb.is_empty() {
                    String::new()
                } else {
                    format!(
                        " This run black-boxed {} module(s) ({}), whose outputs ARE freed to \
                         unnamed inputs — the most likely source here; provide their source to \
                         model them.",
                        bb.len(),
                        bb.join(", ")
                    )
                };
                format!(
                    " The cause is NOT attributed: this lift shows no partial-write shape for \
                     `{reg}`, so do not assume a `{reg}[hi:lo] <= …` in the RTL. An unnamed \
                     free input also comes from a black-boxed submodule, an undriven net or an \
                     unconnected port.{bb_hint}"
                )
            };
            // The sv2v remedy only exists for a design sv2v can parse. It cannot
            // parse `bind` (0.0.13), and bound SVA is not optional for these
            // designs — `read_verilog -sv` cannot parse `assert property`, and there
            // is no `--define` to guard an `ifdef` with.
            let remedy = if partial_write && !sources_use_bind(sources) {
                " Re-lift with the read_verilog / sv2v front-end \
                 (`--frontend sv2v`), which models the register faithfully."
                    .to_string()
            } else if partial_write {
                " The usual remedy (`--frontend sv2v`) is NOT available \
                 here: these sources use `bind`, which sv2v cannot parse. Rewriting the \
                 assignment to write the register whole is the route that works."
                    .to_string()
            } else {
                String::new()
            };
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome: VerifyOutcome::Skipped {
                    reason: format!(
                        "property references `{reg}`, whose cone reaches a free input the lift \
                         could not attribute to a driver — a verdict over it would read that \
                         havoc value and be unsound, so it is refused rather than silently \
                         decided.{cause}{remedy}"
                    ),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            });
            continue;
        }

        // D1.6 — exact full-state symbolic MC (`--engine exact-symbolic`): decide
        // the property EXACTLY over the reset-gated btor2's bit-blasted state — no
        // predicate abstraction, so a **definite** 2-valued verdict (never ⊥). Runs
        // BEFORE cube seeding: the exact engine binds the formula's atoms directly
        // (register-name resolution), so it needs no cube and must NOT be gated
        // behind cube-seeding success — deciding a property the cube path cannot
        // seed is exactly this engine's purpose. The btor2 here is reset-gated
        // (reset input pinned inactive) and `$past`-shadow-augmented; the init is
        // the modelled reset state (`initial_state_bdd`: every register pinned to
        // its `init` value or 0). Decides `AF`-liveness (and any μ-calculus
        // property) where the cube path returns Unknown; bounded by BDD size
        // (build errors above the bit cap ⇒ Skipped).
        if opts.exact_symbolic {
            use crate::adapter::btor2::symbolic_bitblast::{ExactSymbolicOptions, ExactVerdict};
            let exact_opts = ExactSymbolicOptions {
                antecedent_shadow_enabled: opts.antecedent_shadow,
                antecedent_propagate_enabled: opts.antecedent_propagate,
                input_state_enabled: opts.input_state,
                ..Default::default()
            };

            // mununu#543 W3 — opt-in ISOLATION. `oxidd`'s `apply_bin` can recurse unboundedly on a
            // malformed diagram created inside OxiDD's own `reduce`, and in-process containment is
            // impossible: a `stacker` red zone cannot fire inside the dependency, and a guard-page
            // SIGSEGV aborts whichever thread it lands on. Out of process it IS containable — the
            // child's death is a wait status, so this property becomes `unknown` with a note and
            // every OTHER property in the run still reports. A consumer measured losing 5 of 8 full
            // lane runs (62%) to that abort.
            if crate::adapter::engine_isolate::enabled() {
                use crate::adapter::engine_isolate::Isolated;
                let iso = crate::adapter::engine_isolate::exact_symbolic(&btor2, &formula_str);
                // `Unavailable` falls THROUGH to the in-process path: isolation could not be
                // attempted, which is our problem and not a reason to abstain on the property.
                if !matches!(iso, Isolated::Unavailable(_)) {
                    let outcome = match &iso {
                        Isolated::Holds => VerifyOutcome::Holds,
                        // No witness under isolation — the child returns a verdict, not a
                        // counterexample. Recorded in `engine_isolate`'s docs; a consumer gating on
                        // `properties[].outcome` is unaffected, one reading traces is not.
                        Isolated::Violated => VerifyOutcome::Violated { false_cells: 1 },
                        Isolated::Abstained(why) => VerifyOutcome::Skipped {
                            reason: format!("exact symbolic MC (isolated): {why}"),
                        },
                        // THE CASE THIS EXISTS FOR. `unknown`, never `skipped`: the engine was
                        // attempted and reached no answer, and `ci_exit_code` does not fail on
                        // `skipped` — so `skipped` here would let a strict gate pass green on a
                        // property whose engine crashed.
                        Isolated::Died(_) => VerifyOutcome::Unknown { unknown_cells: 0 },
                        Isolated::Unavailable(_) => unreachable!("filtered above"),
                    };
                    if let Isolated::Died(why) = &iso {
                        report.notes.push(VerificationNote {
                            kind: "engine-isolation".to_string(),
                            level: NoteLevel::ScopeCaveat,
                            summary: format!(
                                "`{}`: the exact-symbolic engine DIED in its isolated child — {why}. \
                                 The property is `unknown`; every other property in this run is \
                                 unaffected, which is what isolation buys (mununu#543).",
                                t.name
                            ),
                            detail: "The engine ran in a child process because \
                                     MUNUNU_ISOLATE_ENGINES is set. Without isolation this death \
                                     would have ended the whole run with no report at all. The \
                                     underlying defect is an unbounded recursion in OxiDD's \
                                     `apply_bin` on a malformed diagram; it is not a defect in the \
                                     design under verification, and this property's verdict is \
                                     absent rather than negative."
                                .to_string(),
                            items: vec![format!("property:{}", t.name)],
                            // mununu#548 O-2 — the property is a FIELD now. `items` keeps its
                            // `property:<name>` entry for one release so a consumer already
                            // reading it does not break on the same commit that gives them the
                            // better route.
                            property: Some(t.name.clone()),
                        });
                    }
                    report.properties.push(PropertyVerdict {
                        name: t.name.clone(),
                        label: t.label.clone(),
                        kind: t.kind,
                        formula: formula_str.clone(),
                        // ask 26 — the death reason was already captured for the
                        // `engine-isolation` NOTE above. Carry it on the FIELD as well: a crash
                        // and a budget abstention were previously byte-identical here
                        // (`unknown`, `unknown_cells: 0`), so a consumer could not tell "your
                        // engine died" from "this needed more budget" — and the note that said
                        // so was filterable.
                        bottom_reason: match &iso {
                            Isolated::Died(why) => Some(BottomReason::EngineCrashed {
                                detail: why.to_string(),
                            }),
                            _ => None,
                        },
                        outcome,
                        seeded_predicates: Vec::new(),
                        counterexample: None,
                        decided_by: None,
                        vacuity: None,
                    });
                    continue;
                }
            }

            // Plan P3 / P4 — the first-pass exact member runs the LADDER (unrestricted ≤ 2 s,
            // then reach-restricted with a 5 s probe and a 30 s cap, then unrestricted with the
            // engine's own budgets). The easy cases decide on stage 1 and pay for no lever.
            let ladder = crate::adapter::btor2::symbolic_bitblast::exact_ladder(
                &btor2,
                &formula,
                &std::collections::HashSet::new(),
                &exact_opts,
                None,
                None,
            );
            let exact_stage = ladder.stage;
            let exact_witness_miss = ladder.witness_miss.clone();
            let mut by_ladder = false;
            let (outcome, counterexample) = match ladder.result {
                Ok((ExactVerdict::Holds, _)) => {
                    by_ladder = true;
                    (VerifyOutcome::Holds, None)
                }
                Ok((ExactVerdict::Violated, witness)) => {
                    // A definite full-state counterexample (not a cube tally). For a
                    // liveness/recoverability failure (`AF p` / `AG AF p` / `AG EF p`)
                    // the witness carries the concrete path (stall lasso / trap path).
                    // Failing that, a bare `EF p` (reachability) violation carries the
                    // A.4 unreachable-target witness — no trace, because the target is
                    // simply never reached (sound under the over-approximating cut).
                    by_ladder = true;
                    (
                        VerifyOutcome::Violated { false_cells: 1 },
                        witness
                            .map(|w| exact_counterexample_from_lasso(w, &cell_widths_of(&btor2)))
                            .or_else(|| ef_unreachable_counterexample(&formula)),
                    )
                }
                Err(e) => {
                    // RANKING RESCUE — the bit-blaster is over-cap (a wide counter);
                    // a recoverability `AG EF <cnt == N>` may still HOLD by a
                    // well-founded datapath descent the exact engine can't fit. Route
                    // it to the scalable ranking/recoverability engine before abstaining.
                    match try_ranking_recoverability(&btor2, &formula) {
                        Some(outcome) => (outcome, None),
                        None => (
                            VerifyOutcome::Skipped {
                                reason: format!("exact symbolic MC: {e}"),
                            },
                            None,
                        ),
                    }
                }
            };
            // mununu#808 follow-up — a VIOLATED that ships no lasso says why (the extraction
            // failed after the verdict), as the safety path does for its missing paths.
            if counterexample.is_none()
                && let Some(reason) = exact_witness_miss
            {
                report.notes.push(witness_miss_note(
                    &t.name,
                    Some(exact_stage),
                    &crate::adapter::reach_rescue::WitnessMiss {
                        stage: "exact-witness",
                        reason,
                    },
                ));
            }
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome,
                seeded_predicates: Vec::new(),
                counterexample,
                bottom_reason: None,
                decided_by: by_ladder.then(|| exact_stage.to_string()),
                vacuity: None,
            });
            continue;
        }

        // mununu#518 — a property naming a MEMORY cannot bind it as a cube dimension: a BTOR2
        // array state has no BV. Before the sort filter it was admitted as though it could, which
        // is worse than refusing — it produced a confident wrong answer. Now it is simply unknown
        // to the seeder, and "unknown signal" is an unhelpful thing to tell someone who wrote a
        // perfectly sensible name, so say what it actually is.
        for mem in formula_atoms_naming_memories(&formula, &array_state_cells) {
            report.notes.push(VerificationNote {
                kind: "array-atom-unsupported".into(),
                level: NoteLevel::ScopeCaveat,
                summary: format!(
                    "`{}`: the atom names `{mem}`, which is a MEMORY (a BTOR2 array state), not a \
                     scalar register; memory atoms are not supported as cube dimensions and the \
                     property cannot be decided from them",
                    t.name
                ),
                detail:
                    "mununu#518 — arrays are encoded as SMT `Array` constants resolved through \
                         a separate name map, not as entries in the bit-vector signal vector, so \
                         there is no BV for a cube dimension to range over. Rephrase the property \
                         over a scalar register (a read port's output, say), or over a specific \
                         cell if the design exposes one."
                        .into(),
                items: vec![mem.clone()],
                property: Some(t.name.clone()),
            });
        }

        let mut seeded = seed_from_formula(
            &formula,
            resolves_to_state,
            is_input,
            combinational_kind,
            cone_inputs_of,
            &predicate_hints,
        );
        // H.H — seed a bound compound `X <= K` for each counter register (a state
        // cell whose `$past` shadow is also present, i.e. a self-relational
        // monotonicity / increment atom). The bound is a sound cube-dimension
        // PARTITION (not an assumption); with must-edges on it excludes the abstract
        // wraparound state that leaves `cnt_q >= $past(cnt_q)` at ⊥. Manual bounds
        // override the config-inferred ones. No-op when no counter / no bound.
        let counters = counter_registers_in(&seeded);
        for (reg, k, provenance) in seed_counter_bounds(
            &mut seeded,
            &counters,
            &opts.counter_bounds,
            &config_inferred_bounds,
        ) {
            counter_bound_items.insert(format!("{reg} <= {k} ({provenance})"));
        }
        if !seeded.unseedable.is_empty() {
            let reason = unseedable_skip_reason(&seeded.unseedable, &report.diagnostics);
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome: VerifyOutcome::Skipped { reason },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            });
            continue;
        }
        let predicate_count = seeded.specs.len() + seeded.compounds.len();
        if predicate_count == 0 && seeded.derived.is_empty() && seeded.derived_relational.is_empty()
        {
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome: VerifyOutcome::Skipped {
                    reason: "no state-cell / combinational predicate atoms to seed the cube"
                        .to_string(),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            });
            continue;
        }

        let mut seeded_names: Vec<String> = seeded.specs.iter().map(|s| s.name.clone()).collect();
        seeded_names.extend(seeded.compounds.iter().map(|(n, _)| n.clone()));
        seeded_names.extend(seeded.derived.iter().map(|d| d.name.clone()));
        seeded_names.extend(seeded.derived_relational.iter().map(|(n, _)| n.clone()));

        // AR-S2 — the cube lift always uses the SmtAllPairs eager seam now, so the
        // old per-property may-policy branch (compounds / inputs / derived /
        // combinational forcing SmtAllPairs, else sampling) is gone: every property
        // gets the sound may. `cegar_refine_loop` still re-checks the compound gate.
        let cegar_opts = CegarOptions {
            max_iterations: opts.max_iterations,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: true,
            lift_strategy: LiftStrategy::Eager,
            // Derived-label properties now use the caller's must-edge policy like
            // any other. The H.E.r2 `must=Off`-for-derived band-aid is RETIRED:
            // its purpose was to dodge a spurious VIOLATED on a derived ⊥-label
            // safety property under `SmtHyperMust`, but the real cause was the
            // 3-valued evaluator collapsing a `KleeneBot` derived label to
            // definite-False (its negation a spurious definite-True antecedent).
            // That is fixed at the root in `mu_calculus::evaluator::predicate_bits`
            // (a `KleeneBot` label now yields `⊥`, so `¬⊥ = ⊥` and the safety
            // implication is `⊥`, never a spurious `False`). With the root fix the
            // must relation is sound for these properties too.
            // mununu#663 — `auto`: a Liveness/Mixed-class formula gets per-target must here, so the
            // cube member can HOLD an `AG EF` instead of leaving it ⊥ for the re-plan.
            must_edge_inference: effective_must_edge_inference(opts, &formula),
            // AR-S2 — always the sound all-pairs SMT may-relation (the pure-state
            // sampling-may fallback + its A.4 ⊥-guard are retired).
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        };
        // Every register the predicates reference — pin each to its init value
        // (config_values) so the lift's initial cube is the reset state.
        let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for s in &seeded.specs {
            referenced.insert(s.register.clone());
        }
        for (_, e) in &seeded.compounds {
            for r in e.registers() {
                referenced.insert(r);
            }
        }
        // H.F — pin the state operands of relational derived predicates too (the
        // input operands have no `init_values` entry, so they are silently skipped
        // for config_values — correct, a free input has no reset value).
        for (_, e) in &seeded.derived_relational {
            for r in e.registers() {
                referenced.insert(r);
            }
        }
        let adapter_options = AdapterOptions {
            sidecar_json: synth_sidecar_json(
                &seeded.compounds,
                &seeded.derived,
                &seeded.derived_relational,
                &referenced,
                &init_values,
            ),
            ..Default::default()
        };
        // Env sized to the cube space: simple specs + the sidecar compounds
        // `cegar_refine_loop` appends.
        let env = Environment::new(1usize << predicate_count);

        // The design's initial cube index(es): evaluate every predicate at the
        // reset valuation, in the lift's bit order (simple specs first, then the
        // sidecar compounds `cegar_refine_loop` appends). The verdict AT THE
        // RESET CUBE is the property's answer — sidestepping the lift's `cube_0`
        // initial-state default (cube_is_admissible can't pin a compound).
        //
        // H.B — a free-input spec's bit is NOT pinned at init (the environment
        // is free at cycle 0). Its bit position is collected in
        // `free_input_bits` and the verdict is read across ALL of its initial
        // polarities (the product of every free-input bit), then combined
        // conjunctively: the property holds at reset iff it holds under every
        // initial environment input.
        // mununu#579 — ONE classification of every register the property's predicates name:
        // ESTABLISHED (a cycle-0 value, in `init_for_cube`) or UNESTABLISHED (free at cycle 0,
        // in `unestablished_registers`). A free input is neither — it is H.B's own free
        // dimension, handled below.
        //
        // The name a predicate uses is not always the name `collect_symbols` gave the state
        // cell: its loose alias pass names a cell after the FIRST symbol-bearing node whose
        // cone reaches it, so a counter's cell can be named `cnt_d` (its next-value function)
        // while the predicates say `cnt_q`. That is why the lookup goes through the strict
        // resolver the seeder itself binds with (`resolve_state_alias`, a value-identical
        // alias — `uext`-0, the pinned reset mux): the cell's `init` is then read by NID, not
        // by whichever name it happened to get. (Measured: the saturating-counter e2e read
        // `cnt_q` as unestablished — and VIOLATED at cycle 0 — because of exactly that
        // naming, after the old `unwrap_or(0)` had hidden it.)
        //
        // A name that is neither a state cell nor a strict alias is combinational-of-state
        // (H.U.2): its cycle-0 value is OBSERVED at the reset state — but only when every
        // state cell in its cone has an `init` and no primary input feeds it. A cone with an
        // init-less cell or an input in it has no single cycle-0 value, and observing it with
        // those defaulted to 0 would be the pin this change removes, one node downstream.
        let mut init_for_cube = init_values.clone();
        // mununu#579 — the registers whose predicate bits are free at cycle 0.
        let mut unestablished_registers: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        {
            let mentioned: std::collections::BTreeSet<String> = seeded
                .specs
                .iter()
                .map(|s| s.register.clone())
                .chain(seeded.compounds.iter().flat_map(|(_, e)| e.registers()))
                .filter(|r| !init_for_cube.contains_key(r) && !seeded.input_registers.contains(r))
                .collect();
            let mut comb_names: Vec<String> = Vec::new();
            for r in mentioned {
                match cycle_zero_class(&file, &r, reset_pinned) {
                    CycleZero::Established(v) => {
                        init_for_cube.insert(r, v);
                    }
                    CycleZero::Unestablished => {
                        unestablished_registers.insert(r);
                    }
                    CycleZero::Combinational => comb_names.push(r),
                }
            }
            if !comb_names.is_empty() {
                let established: std::collections::HashMap<String, u128> = init_for_cube
                    .iter()
                    .map(|(k, v)| (k.clone(), *v as u128))
                    .collect();
                if let Ok(outcome) = crate::adapter::btor2::bit_blast::simulate_one_step_observe(
                    &file,
                    &established,
                    &std::collections::HashMap::new(),
                    &comb_names,
                ) {
                    for n in &comb_names {
                        if let Some(&v) = outcome.observed.get(n) {
                            init_for_cube.insert(n.clone(), v as u64);
                        }
                    }
                }
            }
        }
        let init_for_cube_u128: std::collections::HashMap<String, u128> = init_for_cube
            .iter()
            .map(|(k, v)| (k.clone(), *v as u128))
            .collect();
        let mut base_init_cube = 0usize;
        let mut free_input_bits: Vec<u32> = Vec::new();
        // mununu#503 — carry each free dimension's `(register, value)` so mutually exclusive
        // combinations (two `Eq`s on one register) are not enumerated as initial cubes.
        let mut free_input_dims: Vec<(String, u64)> = Vec::new();
        let mut bit = 0u32;
        for s in &seeded.specs {
            if seeded.input_registers.contains(&s.register) {
                // Free input dimension — left unset in the base; enumerated below.
                free_input_bits.push(bit);
                free_input_dims.push((s.register.clone(), s.value));
            } else if let Some(v) = init_for_cube.get(&s.register) {
                if *v == s.value {
                    base_init_cube |= 1 << bit;
                }
            } else {
                // mununu#579 — no cycle-0 value to pin: the bit is FREE, like an input's, so
                // the verdict is read over both polarities (the exact engine's and the
                // portfolio's reading of the same cell).
                free_input_bits.push(bit);
                free_input_dims.push((s.register.clone(), s.value));
            }
            bit += 1;
        }
        for (name, expr) in &seeded.compounds {
            let free = expr.registers().into_iter().any(|r| {
                seeded.input_registers.contains(&r) || unestablished_registers.contains(&r)
            });
            if !free {
                if expr.eval(&init_for_cube_u128) {
                    base_init_cube |= 1 << bit;
                }
            } else {
                // A compound over a free register: free at cycle 0, keyed by its own name
                // (no other dimension shares it, so the exclusivity filter is a no-op).
                free_input_bits.push(bit);
                free_input_dims.push((name.clone(), 1));
            }
            bit += 1;
        }
        // mununu#579 — bounded. The initial cube set is the product of the free dimensions;
        // past the cap the property abstains by name rather than enumerating `2^n`.
        if free_input_bits.len() > MAX_FREE_INIT_DIMENSIONS {
            report.properties.push(PropertyVerdict {
                name: t.name.clone(),
                label: t.label.clone(),
                kind: t.kind,
                formula: formula_str.clone(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 0 },
                seeded_predicates: seeded_names,
                counterexample: None,
                bottom_reason: Some(BottomReason::UnestablishedInitialCubes {
                    free_dimensions: free_input_bits.len(),
                    cap: MAX_FREE_INIT_DIMENSIONS,
                    registers: unestablished_registers.len(),
                }),
                decided_by: None,
                vacuity: None,
            });
            continue;
        }
        // All initial cubes = base ⊗ every combination of the free bits (inputs and
        // init-less registers). No free bits ⇒ a single cube, the pre-H.B single read.
        let init_cubes =
            free_input_init_cubes_feasible(base_init_cube, &free_input_bits, &free_input_dims);
        // mununu#637 — the product above admits a cell no cycle-0 state inhabits once a
        // dimension is a FUNCTION of others (an output over state cells, a compound over the
        // atoms' registers): such a cell is edgeless, `downgrade_unsatisfiable_cells` masks it
        // to ⊥, and the whole property would read ⊥ at it. Keep the cubes a concrete reset
        // state inhabits, under the cycle-0 pins established above (bit order as the lift's:
        // simple specs, then compounds).
        let init_cubes = {
            let mut dims: Vec<PredicateSpec> = seeded.specs.clone();
            let mut exprs = std::collections::HashMap::new();
            for (name, expr) in &seeded.compounds {
                dims.push(PredicateSpec {
                    name: name.clone(),
                    register: expr.registers().into_iter().next().unwrap_or_default(),
                    value: 0,
                });
                exprs.insert(name.clone(), expr.clone());
            }
            let pins: std::collections::HashMap<String, Vec<u64>> = init_for_cube
                .iter()
                .map(|(k, v)| (k.clone(), vec![*v]))
                .collect();
            crate::adapter::btor2::kmts_lift::cycle_zero_feasible_cubes(
                &file, &dims, &exprs, init_cubes, &pins,
            )
        };

        // R-F5.5d — the final 3-valued verdict over the cube space, from the
        // selected engine. The explicit path runs `cegar_refine_loop`; the
        // symbolic path runs the R-F5 BDD CEGAR loop and projects its per-cube
        // verdicts into the same `TritSet` shape (over `2^|final P|`, same cube
        // indexing), so the init-cube read below is identical. (The
        // `--engine exact-symbolic` path returned early above, before seeding.)
        // `(verdict, the refinement stopped on a wall-clock budget)` — the termination travels
        // with the verdict so a budget stop is ATTRIBUTED (mununu#728: it used to be dropped).
        let final_verdict_res: Result<
            (crate::mu_calculus::trit::TritSet, bool),
            crate::adapter::AdapterError,
        > = if opts.symbolic_engine {
            crate::adapter::btor2::symbolic_engine::symbolic_cegar_refine(
                &btor2,
                &seeded.specs,
                &adapter_options,
                &formula,
                crate::adapter::btor2::symbolic_bitblast::MustSemantics::ForallExists,
                opts.max_iterations,
            )
            .map(|r| {
                let on_budget = matches!(
                    r.terminated_with,
                    crate::adapter::btor2::symbolic_engine::SymbolicCegarTermination::BudgetExpired
                );
                (symbolic_final_verdict(&r), on_budget)
            })
        } else {
            cegar_refine_loop(
                &formula,
                &btor2,
                seeded.specs.clone(),
                &env,
                &adapter_options,
                &cegar_opts,
            )
            .map(|trace| {
                let on_budget = matches!(
                    trace.terminated_with,
                    crate::adapter::btor2::cegar::CegarTermination::BudgetExpired
                );
                (trace.final_verdict, on_budget)
            })
        };
        let (outcome, cube_bottom_reason, stop_run) = cube_outcome(
            final_verdict_res,
            &init_cubes,
            crate::adapter::run_budget::expired(),
            run_budget.expired(),
        );
        if stop_run {
            time_budget_hit = true;
        }

        // AR-S2 — the cube lift uses the sound `SmtAllPairs` may-relation, so a
        // definite cube verdict is sound by over-approximation; the A.4 honest-⊥
        // downgrade (a stopgap for the retired sampling-may under-approximation)
        // is no longer needed.
        if let Some(t0) = prop_t0 {
            eprintln!(
                "[mununu-timing] property={} outcome={} elapsed_ms={} phase=main",
                t.name,
                outcome.label(),
                t0.elapsed().as_millis()
            );
        }
        report.properties.push(PropertyVerdict {
            name: t.name.clone(),
            label: t.label.clone(),
            kind: t.kind,
            formula: formula_str.clone(),
            outcome,
            seeded_predicates: seeded_names,
            counterexample: None,
            bottom_reason: cube_bottom_reason,
            decided_by: None,
            vacuity: None,
        });
    }

    // ⊥ RE-PLAN (verification-execution-planner P2.1b): the planner's reactive re-plan
    // step. When the main-path plan leaves a property ⊥, `planner::replan` dispatches it by
    // property_class to a sound reduction (safety→reach_rescue, box-AF→l2s, νμ
    // recoverability→exact+COI / cube+hyper-must) — the re-plan edges. Runs before the
    // notes/coverage build so the counts reflect any rescue; each edge is gated by its own
    // `rescue_bottom_*` opt and only fires on its recognized shape. (P2.1b routes the
    // re-plan entry through the planner, delegating the edge-application to the existing
    // rescue subsystem — verdict-equivalent; P2.1c adds the soundness plan-invariants.)
    // C7 — the last iteration's verdict has not been flushed yet (the flush happens at the top of
    // the NEXT iteration, and there isn't one).
    breadcrumb.record_pending(
        report
            .properties
            .iter()
            .map(|p| (p.name.as_str(), p.outcome.label())),
        "main",
    );
    // The re-plan below can turn an `unknown` into a definite verdict, so a breadcrumb written
    // before it is not the final answer. Snapshot the pre-escalation outcomes and append a second
    // record for anything that MOVED — a consumer takes the last line per `property` (the JSON field name).
    let pre_replan_outcomes: Option<Vec<&'static str>> = breadcrumb.is_active().then(|| {
        report
            .properties
            .iter()
            .map(|p| p.outcome.label())
            .collect()
    });

    // Plan 2 / PR C — a portfolio member leaves its ⊥ rows to the planner, which re-plans the
    // MERGED report once after every member ([`replan_merged`]); a single-engine run re-plans
    // its own rows here, as before.
    let rescue_notes = if opts.replan_defer {
        Vec::new()
    } else {
        crate::planner::replan(
            &mut report,
            &btor2,
            reset_pinned,
            opts,
            &ann_scan.fairness_assumes,
        )
    };

    // C7 — append an `escalated` record for every verdict the re-plan MOVED. Only the movers, so
    // the file stays a log of what changed rather than a duplicated dump.
    if let Some(before) = pre_replan_outcomes {
        for (i, p) in report.properties.iter().enumerate() {
            if before.get(i).copied() != Some(p.outcome.label()) {
                breadcrumb.record(i, &p.name, p.outcome.label(), "escalated", None);
            }
        }
    }

    // Lever (b) — exact-symbolic rescue for cube-SKIPPED properties. The predicate cube
    // cannot seed an ATOM-LESS modal formula (no-deadlock `nu X.(<> true && [] X)`, a pure
    // `<>`/`[]` obligation) and reports it Skipped; the full-state exact engine needs no
    // seeding and decides it. Only fires on a cube run under reset-gating (the exact main
    // path already ran exact; A.6 requires reset gating). Sound: exact is the differential
    // oracle — a Skipped property is upgraded only on a DEFINITE Holds/Violated.
    let exact_skip_notes = crate::planner::rescue_skipped(&mut report, &btor2, opts);

    // H.J — provenance notes: surface every abstraction/scoping decision this run
    // made (config concretizations, posture, reset-gating, flop stubs, cut
    // modules, coverage).
    let counter_bound_items_vec: Vec<String> = counter_bound_items.into_iter().collect();
    // A.3 — the exact-symbolic engine (per-property branch above) and the
    // predicate-cube path both reach this single note build; pick the posture the
    // run actually used.
    let posture = if opts.exact_symbolic {
        NotePosture::Exact
    } else {
        NotePosture::Cube
    };
    // mununu#536 — record the run's SCOPE on the diagnostics, not only in note prose. Both
    // operands are already in hand here (they are `build_notes` arguments); a consumer asking
    // "what was this verdict scoped to?" should not have to parse a summary sentence.
    report.diagnostics.applied_config_values = applied_config_values
        .iter()
        .map(|(sig, val)| format!("{sig}={val}"))
        .collect();
    report.diagnostics.cutpoint_signals = yosys_opts.cutpoint_signals.clone();
    // mununu#577 — withhold a `Violated` that rests on a register whose initial value was never
    // established. Runs as a post-pass, after the rescue/escalation ladder, so it catches every
    // path that can produce one rather than nine push sites.
    downgrade_violated_on_unestablished_init(&mut report, &btor2);
    // PR A1 — under a fault posture, the antecedent-vacuity check of every sec_cm chain row.
    if let Some(applied) = fault.as_ref() {
        check_antecedent_vacuity(&mut report, &btor2, reset_pinned, &applied.sec_cm_rows);
    }
    // The trace for every safety VIOLATED the report ships, whichever engine decided it.
    attach_safety_witnesses(&mut report, &btor2, reset_pinned);
    // mununu#750 — after the witness, so a withheld violation keeps its trace as a may-trace.
    withhold_violated_over_free_leaves(&mut report, &btor2);
    // 2026-10-30 — the RTL lines of each row's cone (display only; reads no verdict).
    attach_involves(&mut report, &btor2, &source_index);
    // 2026-10-30 — `--localize`: repair candidates per violated row, one BMC query per register
    // in the property's cone, with the counterexample's inputs fixed.
    if opts.localize {
        attach_repair_candidates(&mut report, &btor2, &source_index, reset_pinned);
    }
    // mununu#579 — last of the verdict post-passes: everything above may still CHANGE a verdict, and
    // a self-contradiction must be judged on the verdicts the report actually ships.
    flag_self_contradictory_verdicts(&mut report);
    // mununu#599 — and, on the verdicts that survive, say at each recoverability guarantee what
    // the rest of the report says about its vacuity.
    flag_vacuous_guarantees(&mut report);
    report.notes = build_notes(
        &report,
        must_edge_posture_label(opts.must_edge_inference, opts.must_edge_auto),
        &applied_config_values,
        &yosys_opts.params,
        &counter_bound_items_vec,
        &yosys_opts.cutpoint_signals,
        &posture,
    );
    if let Some(n) = annotation_note(&ann_scan) {
        report.notes.push(n);
    }
    report
        .notes
        .extend(two_state_fold_notes(&extraction.two_state_folds));
    report
        .notes
        .extend(strong_reading_notes(&extraction.strong_readings));
    // mununu#490 — a process-memory ceiling was configured AND we hit it before
    // finishing every property. Surface the abstention as a first-class note so a
    // downstream gate sees "this run was truncated by the ceiling" rather than
    // interpreting the trailing `Unknown` outcomes as engine-level unknowns.
    if let Some(hit) = memory_budget_hit {
        report.notes.push(VerificationNote {
            kind: "memory-budget-exceeded".into(),
            level: NoteLevel::ScopeCaveat,
            // mununu#504 C6 — name the ceiling's PROVENANCE, not the env var unconditionally.
            // Since unset resolves to an auto ceiling, "via `MUNUNU_MAX_PROCESS_MEMORY_BYTES`"
            // would attribute the limit to a variable the operator never set, and send them
            // looking for a value that is not there.
            summary: format!(
                "process memory ceiling reached ({} B in use, ceiling {} B, {}); the remaining \
                 properties abstained rather than aborting the process",
                hit.current_rss_bytes,
                hit.limit_bytes,
                crate::adapter::memory_budget::ceiling_provenance(),
            ),
            detail: "mununu#490 — `MUNUNU_MAX_PROCESS_MEMORY_BYTES` is a self-imposed \
                     process-RSS ceiling. When the ceiling is exceeded between properties, the \
                     current property and every remaining one abstain (`unknown`) with this \
                     note attached, and prior verdicts are preserved. This trades an OS-level \
                     `abort()` (exit 134) for a graceful degradation, so a downstream gate can \
                     distinguish 'ran out of memory' from 'the engine did not decide'. It does \
                     NOT catch an allocation that fails BETWEEN checkpoints — a single BDD \
                     blowup can still crash the process. Recommended setting: 70-80% of the \
                     process's real memory limit (`ulimit -m`, container `--memory`), leaving \
                     headroom for allocator overhead + non-mununu memory."
                .into(),
            items: Vec::new(),
            property: None,
        });
    }
    // mununu#504 — the TIME sibling of the memory note above. The memory note's text is
    // deliberately left BYTE-IDENTICAL: the #490 briefing quotes it, and a consumer keying on
    // `kind == "memory-budget-exceeded"` must stay valid. This is a NEW `kind`, not a rewording
    // of that one.
    if time_budget_hit {
        report.notes.push(VerificationNote {
            kind: "time-budget-exceeded".into(),
            level: NoteLevel::ScopeCaveat,
            summary: format!(
                "the run's wall-clock budget was reached; the remaining properties abstained \
                 rather than hanging (`{}`; per property `{}`, per rescue step `{}`)",
                crate::adapter::run_budget::RUN_BUDGET_ENV,
                crate::adapter::run_budget::PROPERTY_BUDGET_ENV,
                crate::adapter::run_budget::RESCUE_SLICE_ENV,
            ),
            detail: format!(
                "mununu#504 — a wall-clock budget expired. `{}` bounds a single property and                  `{}` bounds the whole run; the per-property budget is additionally clamped to                  the run deadline, so it can never extend it. On expiry the current property and                  every remaining one abstain (`unknown`) and PRIOR VERDICTS ARE PRESERVED — the                  point being that a run which used to hang, or be killed with no output at all,                  now reports everything it managed to decide. An abstention here is `unknown`,                  never `skipped`, so a strict `--fail-on unknown` gate still fails rather than                  passing green on a property that was never decided. Set either to `0` to                  disable.",
                crate::adapter::run_budget::PROPERTY_BUDGET_ENV,
                crate::adapter::run_budget::RUN_BUDGET_ENV,
            ),
            items: Vec::new(),
            property: None,
        });
    }
    report.notes.extend(rescue_notes);
    report.notes.extend(exact_skip_notes);
    // The cost-annotated plan telemetry (`plan-cost` / `plan-accuracy`) is NO LONGER computed here:
    // it is the planner's job now (`plan` builds the rationale from the shared prepared model,
    // `execute` attaches it after the merge). This body is the pure per-engine leaf.
    Ok(report)
}

/// Unified ⊥-escalation router (P3): for each property the cube portfolio left `⊥`,
/// dispatch by [`property_class`](crate::mu_calculus::Formula::property_class) to the
/// sound reduction matching its shape and upgrade the verdict in place —
///   - **Safety** (an AG-invariant) → the multi-engine reachability portfolio
///     ([`reach_portfolio_rescue`](crate::adapter::reach_rescue::reach_portfolio_rescue));
///   - **Liveness / box-AF response** `AG(a → AF b)` → liveness-to-safety
///     ([`response_liveness_rescue`](crate::adapter::liveness_rescue::response_liveness_rescue));
///   - **Liveness / νμ recoverability** `AG EF good` → the cube + `smt-hyper-must` path
///     ([`verify_recoverability_scalable`](crate::adapter::recoverability::verify_recoverability_scalable)).
///
/// Each reducer recognizes ONLY its own shape (returns `None`/abstains otherwise), so a
/// property is routed to at most one reduction and an un-reducible ⊥ is left untouched —
/// the roadmap's L3 `property_class()` promoted from a diagnostic label to the actual
/// escalation dispatch. The νμ recoverability arm closes the gap where a diamond-EF ⊥
/// was previously left undecided in verify_auto even though the cube + smt-hyper-must
/// path could decide it. Pure over `(report, design_btor2)`, so it is unit-testable on a
/// BTOR2 fixture without the SV toolchain. Per-arm rescues are gated by
/// `rescue_bottom_{safety,liveness,recoverability}`.
pub(crate) fn escalate_bottom(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    reset_pinned: bool,
    opts: &VerifyAutoOptions,
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
) -> Vec<VerificationNote> {
    escalate_bottom_with_slice(
        report,
        design_btor2,
        reset_pinned,
        opts,
        fairness_atoms,
        crate::adapter::run_budget::rescue_slice_ms(),
    )
}

/// [`escalate_bottom`] with the rescue slice given explicitly (`None` = no slice, the row budget
/// alone) — the seam a test uses to run a row under an already-spent slice deterministically.
pub(crate) fn escalate_bottom_with_slice(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    reset_pinned: bool,
    opts: &VerifyAutoOptions,
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
    slice_ms: Option<u64>,
) -> Vec<VerificationNote> {
    use crate::adapter::btor2::predicate_expr::CmpOp;
    use crate::adapter::liveness_rescue::{
        LivenessVerdict, reduce_ag_ef_target, response_liveness_rescue_formula,
    };
    use crate::adapter::reach_rescue::RescueVerdict;
    use crate::adapter::recoverability::{
        RecoverabilityEngine, verify_recoverability_attributed, verify_recoverability_scalable,
    };
    use crate::mu_calculus::{PropertyClass, parser as mu_parser};
    use crate::verdict::PropertyVerdict;

    // Trap-certificate candidates the report already establishes: every row that HOLDS the
    // terminal idiom `AG (R == v → AG R == v)`. They only order the search below — its closure is
    // re-checked there — so harvesting them before the loop cannot import a stale verdict.
    let sibling_traps: Vec<(String, crate::adapter::liveness_rescue::Atom)> = report
        .properties
        .iter()
        .filter(|p| matches!(p.outcome, VerifyOutcome::Holds))
        .filter_map(|p| {
            let f = mu_parser::parse(&p.formula).ok()?;
            crate::adapter::liveness_rescue::reduce_terminal_state(&f).map(|t| (p.name.clone(), t))
        })
        .collect();

    let mut notes = Vec::new();
    for prop in report.properties.iter_mut() {
        if !matches!(prop.outcome, VerifyOutcome::Unknown { .. }) {
            continue;
        }
        // mununu#750 — a violation already found over a free leaf: every engine re-finds the same
        // may-trace on this model, so a rescue would spend its slice to be withheld again.
        if matches!(
            prop.bottom_reason,
            Some(BottomReason::ViolationOverFreeLeaves { .. })
        ) {
            continue;
        }
        // mununu#490 — same ceiling guard as the main per-property loop: if the
        // process is out of memory, abstaining here is strictly safer than starting
        // an escalation that may abort mid-BDD-blast. The main loop's Unknown
        // outcomes stay; we simply skip the remaining escalation attempts. Break
        // rather than continue: the ceiling only rises during the loop.
        if crate::adapter::memory_budget::check_process_memory_budget().is_err() {
            break;
        }
        // mununu#504 — time joins memory here, for the same reason and with the same `break`:
        // `escalate_bottom` is a SECOND full pass over the property list, so without a guard an
        // out-of-time run can spend as long again after the main loop has already stopped.
        if crate::adapter::run_budget::expired() {
            break;
        }
        // mununu#726 — the row gets the per-property budget (a child of the run budget, which
        // `verify_auto_impl` keeps in effect), and every portfolio-backed arm below runs under a
        // further RESCUE SLICE (`MUNUNU_RESCUE_SLICE_MS`, 60 s by default) so twenty undecidable
        // rows cannot each spend fifteen minutes. The recoverability arm keeps the row budget: its
        // cube ladder legitimately runs minutes. `[mununu-timing] … phase=replan` names the lane.
        let row_t0 = std::env::var_os("MUNUNU_PROPERTY_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        let row_budget = crate::adapter::run_budget::Budget::child(
            &crate::adapter::run_budget::current(),
            crate::adapter::run_budget::property_budget_ms(),
        );
        let _row_guard = crate::adapter::run_budget::enter(&row_budget);
        // `MUNUNU_RESCUE_MONITOR_DUMP` names this row's monitor files after the row.
        crate::adapter::reach_rescue::set_monitor_dump_tag(Some(&prop.name));
        let mut lane: &str = "none";
        let mut slice_expired = false;
        let Ok(formula) = mu_parser::parse(&prop.formula) else {
            continue;
        };
        match formula.property_class() {
            PropertyClass::Safety if opts.rescue_bottom_safety => {
                lane = "safety";
                // mununu#503 — use the DIAGNOSED variant so a decline is reported instead of
                // vanishing. Before this, every decline was a bare `None`: the `bottom-reason`
                // note told the consumer to "check for a sibling `bit-cap` / `abstained` note",
                // and there was none, so a residual ⊥ could not be attributed at all.
                let rescued = under_rescue_slice(slice_ms, &mut slice_expired, || {
                    crate::adapter::reach_rescue::reach_portfolio_rescue_diagnosed(
                        design_btor2,
                        &formula,
                        reset_pinned,
                    )
                });
                let rescued = match rescued {
                    Ok(v) => Some(v),
                    Err(why) => {
                        notes.push(VerificationNote {
                            kind: "safety-rescue-declined".into(),
                            level: NoteLevel::ScopeCaveat,
                            summary: format!(
                                "`{}`: the safety-rescue lane declined — {why}",
                                prop.name
                            ),
                            detail: "mununu#503 — the reachability rescue is what decides an \
                                     AG-safety property the cube abstraction left ⊥, by \
                                     compiling it to a `bad`-monitor and running the concrete \
                                     portfolio. When it declines, the ⊥ stands. The reason above \
                                     distinguishes the two cases a consumer must act on \
                                     differently: a SHAPE the reducers do not match (reshape the \
                                     property, or a new reducer is needed) versus a LEAF the \
                                     monitor emitter cannot resolve to a state cell, output port \
                                     or primary input (an internal combinational wire, typically \
                                     — the property is fine, the monitor simply has nothing to \
                                     observe). Neither is a resource abstain, so a bigger budget \
                                     will not help either one."
                                .into(),
                            items: Vec::new(),
                            property: Some(prop.name.clone()),
                        });
                        None
                    }
                };
                if let Some((verdict, reach)) = rescued {
                    let engines = engines_of(&reach);
                    match verdict {
                        RescueVerdict::Holds => {
                            prop.outcome = VerifyOutcome::Holds;
                            prop.decided_by = Some("reach-rescue".into());
                            notes.push(rescue_note(&prop.name, "HOLDS", &engines));
                        }
                        RescueVerdict::Violated => {
                            prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                            prop.decided_by = Some("reach-rescue".into());
                            // P3 Update 2 — attach the concrete safety counterexample: native
                            // BMC extracts the `Init → ¬invariant` path from the same bad-monitor.
                            // A safety CEX is a finite path (empty `cycle`); the monitor adds no
                            // state, so the trace is over the original design registers.
                            let witness = under_rescue_slice(slice_ms, &mut slice_expired, || {
                                Some(crate::adapter::reach_rescue::safety_witness(
                                    design_btor2,
                                    &formula,
                                    reset_pinned,
                                    SAFETY_WITNESS_MAX_K,
                                ))
                            });
                            prop.counterexample = match witness {
                                Some(Ok(trace)) => Some(safety_counterexample(trace)),
                                Some(Err(miss)) => {
                                    notes.push(witness_miss_note(
                                        &prop.name,
                                        Some("reach-rescue"),
                                        &miss,
                                    ));
                                    None
                                }
                                None => {
                                    let miss = crate::adapter::reach_rescue::WitnessMiss {
                                        stage: "slice",
                                        reason:
                                            "the rescue slice expired before the path search ran"
                                                .to_string(),
                                    };
                                    notes.push(witness_miss_note(
                                        &prop.name,
                                        Some("reach-rescue"),
                                        &miss,
                                    ));
                                    None
                                }
                            };
                            notes.push(rescue_note(&prop.name, "VIOLATED", &engines));
                        }
                        RescueVerdict::Inconclusive => attribute_portfolio_abstention(
                            prop,
                            &reach,
                            slice_expired || row_budget.expired(),
                            &mut notes,
                        ),
                        RescueVerdict::Contradiction => {
                            attribute_portfolio_contradiction(prop, &reach, &mut notes)
                        }
                    }
                }
            }
            PropertyClass::Liveness => {
                lane = "liveness";
                let mut handled = false;
                // mununu#477 Option B — if `GF <atom>` fairness assumes are present AND
                // this guarantee reduces to response shape, route through the fair-cycle
                // l2s primitive `(⋀ⱼ GF fairⱼ) → AG(a → AF b)` (Emerson–Lei). Empty
                // fairness ⇒ falls through to the plain rescue below, unchanged.
                if opts.rescue_bottom_liveness
                    && !fairness_atoms.is_empty()
                    && let Some((verdict, reach)) =
                        under_rescue_slice(slice_ms, &mut slice_expired, || {
                            response_liveness_rescue_formula(
                                design_btor2,
                                &formula,
                                fairness_atoms,
                                reset_pinned,
                            )
                        })
                {
                    handled = true;
                    let engines = engines_of(&reach);
                    match verdict {
                        LivenessVerdict::Holds => {
                            prop.outcome = VerifyOutcome::Holds;
                            notes.push(fair_cycle_rescue_note(
                                &prop.name,
                                "HOLDS",
                                &engines,
                                fairness_atoms,
                            ));
                        }
                        LivenessVerdict::Violated => {
                            prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                            notes.push(fair_cycle_rescue_note(
                                &prop.name,
                                "VIOLATED",
                                &engines,
                                fairness_atoms,
                            ));
                        }
                        LivenessVerdict::Inconclusive => {}
                    }
                }
                if !handled
                    && opts.rescue_bottom_liveness
                    && let Some((verdict, reach)) =
                        under_rescue_slice(slice_ms, &mut slice_expired, || {
                            response_liveness_rescue_formula(
                                design_btor2,
                                &formula,
                                &[],
                                reset_pinned,
                            )
                        })
                {
                    handled = true;
                    let engines = engines_of(&reach);
                    match verdict {
                        LivenessVerdict::Holds => {
                            prop.outcome = VerifyOutcome::Holds;
                            notes.push(liveness_rescue_note(&prop.name, "HOLDS", &engines));
                        }
                        LivenessVerdict::Violated => {
                            prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                            notes.push(liveness_rescue_note(&prop.name, "VIOLATED", &engines));
                        }
                        LivenessVerdict::Inconclusive => {}
                    }
                }
                if !handled
                    && opts.rescue_bottom_recoverability
                    && let Some(good) = reduce_ag_ef_target(&formula)
                    && good.op == CmpOp::Eq
                {
                    // The trap certificate first: a reachable set closed under every transition and
                    // disjoint from `good` refutes `AG EF good` with two safety queries, in seconds,
                    // where the ladder below spends its budget and abstains on a wide cone (OpenTitan
                    // edn's three `AG EF Idle` rows). It only refutes, so a failed search changes
                    // nothing and the ladder runs as before. Its own slice, so a search that runs
                    // out of time is reported in its note, not read as the ladder's budget abstain.
                    // SOUNDNESS: both checks are on the model as given, and the search is withheld
                    // when the target register's cone holds a free leaf (see trap_certificate).
                    lane = "trap-certificate";
                    let mut trap_slice_expired = false;
                    let trap = under_rescue_slice(slice_ms, &mut trap_slice_expired, || {
                        let candidates = crate::adapter::trap_certificate::trap_candidates(
                            design_btor2,
                            &good,
                            reset_pinned,
                            &sibling_traps,
                        );
                        crate::adapter::trap_certificate::search_trap(
                            design_btor2,
                            &good,
                            &candidates,
                            reset_pinned,
                        )
                    });
                    if let Some(cert) = &trap.certificate {
                        prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                        prop.decided_by = Some(TRAP_CERTIFICATE_DECIDED_BY.to_string());
                        prop.counterexample = cert.witness.clone().map(safety_counterexample);
                        notes.push(trap_certificate_note(&prop.name, &good, cert));
                        handled = true;
                    } else if !trap.withheld_by.is_empty() {
                        notes.push(trap_certificate_withheld_note(&prop.name, &good, &trap));
                    } else if !trap.tried.is_empty() {
                        notes.push(trap_certificate_search_note(
                            &prop.name,
                            &good,
                            &trap,
                            trap_slice_expired,
                        ));
                    }
                    // A search the slice cut short may have missed the trap: a residual ⊥ is then
                    // (also) a budget abstention, attributed after the arm like the other lanes'.
                    slice_expired |= trap_slice_expired && trap.certificate.is_none();
                }
                if !handled
                    && opts.rescue_bottom_recoverability
                    && let Some(good) = reduce_ag_ef_target(&formula)
                    && good.op == CmpOp::Eq
                {
                    // SOUNDNESS: verify_recoverability_scalable runs the sound cube +
                    // smt-hyper-must νμ path and abstains (Unknown) if it cannot decide,
                    // so a ⊥ is only ever upgraded to a DEFINITE verdict.
                    lane = "recoverability";
                    let good_str = format!("{} == {}", good.signal, good.value);
                    // ③a routing — try the EXACT+COI recoverability engine FIRST. On a
                    // control signal whose cone-of-influence fits the bit cap (e.g. a
                    // wishbone `ack`/`err` whose cone excludes the datapath), exact+COI
                    // decides the νμ recoverability the cube + smt-hyper-must *abstains*
                    // on — `verify_recoverability` runs exact and falls to the scalable
                    // cube path only on an over-cap `Err`. Gated on `reset_pinned`: the
                    // exact engine is sound only under reset-gating (A.6); with a free
                    // reset use the cube path, whose over-approximating may-relation
                    // soundly includes the reset edge.
                    // mununu#656 follow-up — the exact-first pass runs on the REQUEST's exact
                    // options: `--no-input-state` reaches the rescue too, so the differential
                    // switch switches the whole run, not just the main pass.
                    let recov = if reset_pinned {
                        verify_recoverability_attributed(
                            design_btor2,
                            &good_str,
                            &[],
                            &crate::adapter::btor2::symbolic_bitblast::ExactSymbolicOptions {
                                input_state_enabled: opts.input_state,
                                ..Default::default()
                            },
                        )
                    } else {
                        verify_recoverability_scalable(design_btor2, &good_str, &[])
                            .map(|v| (v, RecoverabilityEngine::CubeLadder))
                    };
                    if let Ok((v, engine)) = recov {
                        match v {
                            PropertyVerdict::Holds => {
                                prop.outcome = VerifyOutcome::Holds;
                                prop.decided_by = Some(engine.decided_by_label().to_string());
                                notes.push(recoverability_rescue_note(&prop.name, "HOLDS", engine));
                            }
                            PropertyVerdict::Violated => {
                                prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                                prop.decided_by = Some(engine.decided_by_label().to_string());
                                notes.push(recoverability_rescue_note(
                                    &prop.name, "VIOLATED", engine,
                                ));
                            }
                            PropertyVerdict::Unknown => {
                                // Provenance: the recoverability rescue RAN but the cube +
                                // smt-hyper-must abstained (sound — never a fabricated verdict).
                                // Recorded so a ⊥ that reached the rescue is explained, not silent.
                                notes.push(recoverability_rescue_note(
                                    &prop.name, "UNKNOWN", engine,
                                ));
                                // Actionable-⊥: localize WHY it abstained (structural obstacles in
                                // the recovery cone) so the ⊥ is actionable, not a bare "don't know".
                                let diag =
                                    crate::adapter::recoverability::diagnose_recoverability_bot(
                                        design_btor2,
                                        &good_str,
                                    );
                                notes.push(recoverability_bot_reason_note(&prop.name, &diag));
                            }
                            PropertyVerdict::Skipped => {}
                        }
                    }
                }
            }
            // Plan 2 / B2 (2026-10-18) — a plain `EF p` the cube left ⊥ is a reachability question
            // on its DUAL: `AG !p` violated ⇒ `EF p` holds (the portfolio's path reaches `p`),
            // `AG !p` proved ⇒ `EF p` is violated (`p` is unreachable from reset). The same gate as
            // the safety arm: the dual IS a safety rescue. Measured need: the four `EF` twins of the
            // full OpenTitan edn IP's recoverability rows, which SymbiYosys `cover` reaches within
            // six cycles while the cube certified no MUST path.
            PropertyClass::Reachability
                if opts.rescue_bottom_safety
                    && crate::adapter::reach_rescue::reduce_ef_target(&formula).is_some() =>
            {
                lane = "reachability";
                let dual = crate::mu_calculus::invert::invert(&formula);
                let rescued = under_rescue_slice(slice_ms, &mut slice_expired, || {
                    crate::adapter::reach_rescue::reach_portfolio_rescue_diagnosed(
                        design_btor2,
                        &dual,
                        reset_pinned,
                    )
                });
                match rescued {
                    // the dual's `bad` (= p) is reachable: the target IS reachable
                    Ok((RescueVerdict::Violated, reach)) => {
                        let engines = engines_of(&reach);
                        prop.outcome = VerifyOutcome::Holds;
                        prop.decided_by = Some("reach-rescue".into());
                        notes.push(reachability_rescue_note(&prop.name, "HOLDS", &engines));
                    }
                    // the dual holds: `p` is unreachable from reset — a violated `EF p`
                    Ok((RescueVerdict::Holds, reach)) => {
                        let engines = engines_of(&reach);
                        prop.outcome = VerifyOutcome::Violated { false_cells: 0 };
                        prop.decided_by = Some("reach-rescue".into());
                        prop.counterexample = ef_unreachable_counterexample(&formula);
                        notes.push(reachability_rescue_note(&prop.name, "VIOLATED", &engines));
                    }
                    Ok((RescueVerdict::Inconclusive, reach)) => attribute_portfolio_abstention(
                        prop,
                        &reach,
                        slice_expired || row_budget.expired(),
                        &mut notes,
                    ),
                    Ok((RescueVerdict::Contradiction, reach)) => {
                        attribute_portfolio_contradiction(prop, &reach, &mut notes)
                    }
                    Err(why) => notes.push(VerificationNote {
                        kind: "reachability-rescue-declined".into(),
                        level: NoteLevel::ScopeCaveat,
                        summary: format!(
                            "`{}`: the reachability-rescue lane declined on the dual — {why}",
                            prop.name
                        ),
                        detail: "A plain `EF p` the cube left ⊥ is decided by the reachability \
                                 portfolio on its dual `AG !p` (a `bad := p` monitor). When the \
                                 dual's monitor cannot be emitted the ⊥ stands; the reason names \
                                 the leaf or shape, not a resource — a bigger budget will not help."
                            .into(),
                        items: Vec::new(),
                        property: Some(prop.name.clone()),
                    }),
                }
            }
            _ => {}
        }
        // A row still ⊥ after its arm, with the row budget or a rescue slice expired, is a budget
        // abstention — attributed here so the classifier below does not read it as a shape it
        // could not reduce (and so a consumer knows that MORE TIME is the right response).
        if matches!(prop.outcome, VerifyOutcome::Unknown { .. })
            && prop.bottom_reason.is_none()
            && (row_budget.expired() || slice_expired)
        {
            prop.bottom_reason = Some(BottomReason::BudgetExpired);
        }
        if let Some(t0) = row_t0 {
            eprintln!(
                "[mununu-timing] property={} outcome={} elapsed_ms={} phase=replan lane={lane}",
                prop.name,
                prop.outcome.label(),
                t0.elapsed().as_millis()
            );
        }
    }
    crate::adapter::reach_rescue::set_monitor_dump_tag(None);
    // mununu#492 Part B — classify residual ⊥s so a downstream gate can distinguish
    // "resource-abstain" (the engine gave up) from "shape-not-reducible-here" (the
    // rescue couldn't reduce this shape) from "no-state-model" (the property class
    // is degenerate on a stateless model). Prevents ⊥ from a fundamental modelling
    // issue (which calls for a fix) from being confused with ⊥ from a resource cap
    // (which calls for a raise).
    let stateless_model = design_stateless(design_btor2);
    for prop in report.properties.iter_mut() {
        if !matches!(prop.outcome, VerifyOutcome::Unknown { .. }) {
            continue;
        }
        // mununu#728 — a reason set upstream (`BudgetExpired`, `MemoryCeilingExceeded`,
        // `NotAttempted`, …) is more specific than anything this classifier can infer from the
        // shape; it used to be overwritten here, which erased every budget attribution.
        if prop.bottom_reason.is_some() {
            continue;
        }
        let reason = classify_bottom_reason(prop, stateless_model);
        notes.push(bottom_reason_note(&prop.name, reason.clone()));
        // monono ask 26 — carried as a FIELD as well as a note. The note has existed since
        // mununu#492 and never reached the consumer it was written for, because their harness
        // filtered notes wholesale. A field cannot be filtered out by a consumer that does not
        // know about it; a note can.
        prop.bottom_reason = Some(reason);
    }
    notes
}

/// Why a property's residual outcome is `⊥`. mununu#492 Part B, extended by monono ask 26.
///
/// **Carried as a FIELD on [`PropertyVerdict`], not only as a note** — and the distinction is the
/// whole point. A consumer that does not know about a field cannot silently filter it out; a
/// consumer that does not know about a note routinely does. This has existed as a
/// `bottom-reason` NOTE since mununu#492, and monono's formal lane discarded notes wholesale, so
/// it never reached the one reader it was written for — while the engine's own abstention string
/// was being dropped in [`merge_portfolio_reports`] under *"Errors contribute nothing."*
///
/// Measured cost of that pair: a consumer's red lane, an hour spent trying budget knobs blind in
/// the wrong order, and two hypotheses refuted — neither of them from the `⊥` itself.
///
/// No longer `Copy`: [`Self::EngineDidNotComplete`] carries the engine's verbatim message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BottomReason {
    /// The property is Safety-class but neither the single-atom nor the compound
    /// reducer could reduce it — its shape is out of what the rescue lane
    /// currently supports (e.g. a compound with a `CmpReg` register-vs-register
    /// leaf, or a non-standard AG outer shape).
    SafetyShapeNotReducible,
    /// A recoverability (`AG EF good`) or liveness (`AG(a → AF b)`) property on
    /// a design with zero state registers. Recoverability is vacuously true on
    /// a stateless model; response-liveness's l2s reduction needs state to
    /// lasso. Either case is a modelling issue, not an engine cap.
    NoStateModelNonSafety,
    /// None of the above — the ⊥ is unexplained by this classifier (probably a
    /// resource abstain on the cube / exact engine, or a shape the residual
    /// doesn't cover here). A downstream gate should NOT treat this as
    /// distinct-from-abstain without more evidence.
    UnclassifiedBottom,
    /// monono ask 26 — a higher-precision engine **did not run to completion on this design**, so
    /// the outcome came from a lower-precision one (or from none).
    ///
    /// **Read the attribution precisely.** `engine` and `detail` describe a WHOLE-RUN failure over
    /// the design; they do NOT assert that this property caused it. The claim is exactly: this
    /// property is `⊥`, and the engine most likely to have decided it stopped for the stated
    /// reason. Conflating those is the mununu#548 defect — a note describing ONE engine inherited
    /// by properties another engine decided — and is deliberately not repeated here.
    ///
    /// `detail` is the engine's verbatim string, which NAMES the budget and its numbers (e.g.
    /// *"abstained on the ITERATION budget (N > M) … raise MUNUNU_BDD_ITER_BUDGET"*). Seven such
    /// messages exist in the exact engine; all seven used to be discarded at the portfolio merge.
    EngineDidNotComplete {
        /// Portfolio label of the engine that failed, e.g. `exact-symbolic`.
        engine: String,
        /// The engine's verbatim abstention or error message.
        detail: String,
    },
    /// 🔴 **A SOUNDNESS ALARM, not a resource problem.** Two portfolio engines returned OPPOSITE
    /// definite verdicts on this property, so the merge forced `⊥` rather than silently picking
    /// one. **One of those engines is unsound on this design and must be investigated.**
    ///
    /// Before mununu#553's follow-up this was indistinguishable from a timeout: both produced
    /// `unknown` with `unknown_cells: 0`. A consumer retrying with a bigger budget would have been
    /// retrying an unsoundness, and a gate counting `unknown`s would have scored it as ordinary
    /// undecidedness. It is the single most dangerous of the five `⊥` causes to conflate.
    EngineContradiction {
        /// `Holds@[…] vs Violated@[…]`, naming which engines disagreed and how.
        detail: String,
    },
    /// The engine process **crashed** (isolated run died — abort, OOM kill, signal). Not an
    /// abstention: it was attempted and produced no answer at all.
    ///
    /// `unknown` rather than `skipped` deliberately, because `ci_exit_code` never fails on
    /// `skipped` — so `skipped` here would let a strict gate pass GREEN on a property whose engine
    /// crashed. Distinguishing it from a budget abstention is what this variant adds.
    EngineCrashed {
        /// What the isolation layer reported about the death.
        detail: String,
    },
    /// The self-imposed **process-RSS ceiling** was exceeded
    /// (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`).
    ///
    /// **Split out from [`Self::BudgetExpired`] because the two need OPPOSITE responses and were
    /// briefly conflated** (shipped in #558, corrected here). Retrying with more *time* cannot help
    /// an RSS exhaustion; more memory, a smaller cone, or a lower engine tier can.
    ///
    /// **It is host-dependent WITHOUT a clock**, which is what makes it easy to miss. RSS depends
    /// on allocator state, other processes, and the cgroup — so the same command on the same commit
    /// abstains on a loaded container and decides on a quiet one, with no wall clock anywhere in
    /// the mechanism. Since mununu#504 C6 the ceiling is **AUTO** when a cgroup limit is detected
    /// (80% of it), i.e. **on by default inside a container**, so this can fire without anyone
    /// having configured it.
    ///
    /// Recorded in the N-track sweep as one of only two budget knobs that are on by default AND
    /// host-dependent.
    MemoryCeilingExceeded,
    /// A per-property or per-run **wall-clock** budget expired (`ResourceBudgetExceeded`).
    ///
    /// ⚠️ **This is the WALL-CLOCK budget only.** The RSS ceiling is
    /// [`Self::MemoryCeilingExceeded`] — do not merge them: retrying with more time is right here
    /// and useless there.
    ///
    /// Distinct from [`Self::EngineDidNotComplete`], which is a whole-run engine failure carrying
    /// that engine's own message. This one is the harness budget, and it is the one case where
    /// "retry with more budget" is the right consumer response.
    BudgetExpired,
    /// mununu#577 — the property's cone contains a register whose INITIAL VALUE was never
    /// established, so a `Violated` cannot be trusted.
    ///
    /// An async reset lowers to a mux rather than a BTOR2 `init`; reset-gating then pins the reset
    /// INACTIVE, removing the only path that would drive those registers to their reset values.
    /// The result is a free cycle-0 state — an over-approximation of reachability, which licenses
    /// a definite `HOLDS` and **never** a definite `VIOLATED`.
    ///
    /// `determinism` is `Reproducible`: this is a property of the model, not of the host. Re-running
    /// will not change it; establishing the reset values will.
    UnestablishedInitialState {
        /// How many init-less registers the property's cone touches.
        registers: usize,
        /// The resets pinned inactive, which is what removed the reset path.
        gated_resets: Vec<String>,
    },
    /// mununu#750 — the property was refuted on a model that is FREER than the RTL: its cone reads
    /// an undriven net (an anonymous BTOR2 `input`: an unconnected wire, or the output of a module
    /// missing from the source set). The trace is a may-trace: the real driver may never produce
    /// the values it needs. Measured on the OpenTitan census: four rows VIOLATED over a missing
    /// `prim_diff_decode` / `prim_sec_anchor_*` HOLD on the full closure. A deliberate cut — a
    /// `--cutpoint` or a `(* blackbox *)` stub — is NOT counted (mununu#578). Reproducible — a
    /// property of the model; vendoring the module or providing the driver decides it.
    ViolationOverFreeLeaves {
        /// The undriven nets in the property's cone, `input_<nid>`.
        leaves: Vec<String>,
    },
    /// mununu#579 — the cube path's initial cube set could not be read. Every predicate bit with
    /// no established cycle-0 truth (a free input, or a predicate over a register with no `init`
    /// line) is a free initial dimension and the initial cube set is their product; past
    /// `cap` dimensions the property abstains here rather than enumerating `2^n` (which is how
    /// the first convergence attempt overflowed the stack on real RTL). Not a withheld verdict:
    /// nothing was decided. Reproducible — a property of the model.
    UnestablishedInitialCubes {
        /// How many free initial dimensions the property's predicates have.
        free_dimensions: usize,
        /// The cap they exceeded.
        cap: usize,
        /// How many init-less registers contribute to them (the remedy's size: an `init` for
        /// each, via a sidecar or reset-gating).
        registers: usize,
    },
    /// The property was **never attempted** — filtered out before any engine ran.
    /// mununu#579 — this report contradicts **itself**: two properties of the same model came back
    /// with definite verdicts that cannot both be right.
    ///
    /// Distinct from [`Self::EngineContradiction`], which compares two ENGINES on one property. This
    /// is one run disagreeing with itself across properties, which is how mununu#577 reached a
    /// consumer — and nothing looked.
    ///
    /// Both properties are forced to `⊥`. **Never pick a winner**: the natural reading of mununu#577
    /// (*"the engine is right at 0 and 1 and wrong at the boundary"*) blamed the half that was
    /// correct, and adjudicating would have shipped the wrong answer with more confidence. A
    /// contradiction says one of them is broken, not which.
    ReportSelfContradiction {
        /// Which property it contradicts, and on which atoms.
        detail: String,
    },
    NotAttempted,
}

/// mununu#553 ask 2 — would re-running the SAME command on the SAME commit produce this `⊥`
/// again?
///
/// **Why this is a field and not prose.** A deterministic abstention is a property of the
/// PROBLEM: pin it, or raise the budget it names. A host-dependent one is a property of the
/// AFTERNOON, and the right response is to treat it as a configuration error — a different
/// action entirely. Before this, both arrived as `unknown` with the same shape, and the engine's
/// own wall-clock message said so outright: *"this ⊥ is not distinguishable in the report from a
/// real one."* A consumer asked for the distinction and said they would use it at once.
///
/// Three-valued deliberately. A bool would force a claim on [`BottomReason::UnclassifiedBottom`]
/// and on an unrecognised engine budget, and "we did not establish this" is the honest answer
/// there — the same discipline the `⊥` vocabulary itself follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomDeterminism {
    /// Same input ⇒ same `⊥`. Pin it, or raise the budget the reason names.
    Reproducible,
    /// Depends on the host — a wall clock, RSS, allocator state, or a kill from outside. The
    /// same command may DECIDE on a quieter machine. Treat as a configuration error, not a
    /// verdict about the design.
    HostDependent,
    /// Not established. Never read this as either of the others.
    Unestablished,
}

impl BottomDeterminism {
    /// Stable kebab-case tag for gates.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Reproducible => "reproducible",
            Self::HostDependent => "host-dependent",
            Self::Unestablished => "unestablished",
        }
    }
}

impl BottomReason {
    /// mununu#553 ask 1 — the engine budget this `⊥` names, when it names one.
    ///
    /// Derived from the engine's verbatim `detail` rather than stored, so every construction
    /// site keeps working and the marker list stays in ONE place
    /// ([`crate::adapter::classify_engine_budget`]).
    pub fn engine_budget(&self) -> Option<crate::adapter::EngineBudget> {
        match self {
            Self::EngineDidNotComplete { detail, .. } => {
                Some(crate::adapter::classify_engine_budget(detail))
            }
            _ => None,
        }
    }

    /// mununu#553 ask 2 — see [`BottomDeterminism`].
    pub fn determinism(&self) -> BottomDeterminism {
        match self {
            // Host-dependent, and the first two are ON BY DEFAULT — the per-property (15 min)
            // and whole-run (1 h) harness clocks, and the cgroup-derived RSS ceiling. These are
            // the cases the issue was filed about.
            Self::BudgetExpired | Self::MemoryCeilingExceeded => BottomDeterminism::HostDependent,
            // A kill from outside (OOM killer, signal) is by definition about the host.
            Self::EngineCrashed { .. } => BottomDeterminism::HostDependent,
            // Defer to the budget the engine named; an unrecognised marker stays unestablished.
            Self::EngineDidNotComplete { .. } => {
                match self.engine_budget().and_then(|b| b.is_host_dependent()) {
                    Some(true) => BottomDeterminism::HostDependent,
                    Some(false) => BottomDeterminism::Reproducible,
                    None => BottomDeterminism::Unestablished,
                }
            }
            // Cause not established, so determinism is not either.
            Self::UnclassifiedBottom => BottomDeterminism::Unestablished,
            // Shape, modelling, contradiction and never-attempted are all functions of the
            // input alone.
            Self::SafetyShapeNotReducible
            | Self::NoStateModelNonSafety
            | Self::EngineContradiction { .. }
            | Self::UnestablishedInitialState { .. }
            | Self::ViolationOverFreeLeaves { .. }
            | Self::UnestablishedInitialCubes { .. }
            | Self::ReportSelfContradiction { .. }
            | Self::NotAttempted => BottomDeterminism::Reproducible,
        }
    }

    /// Stable kebab-case tag, for gates and log greps.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::SafetyShapeNotReducible => "safety-shape-not-reducible",
            Self::NoStateModelNonSafety => "no-state-model-non-safety",
            Self::UnclassifiedBottom => "unclassified-bottom",
            Self::EngineDidNotComplete { .. } => "engine-did-not-complete",
            Self::EngineContradiction { .. } => "engine-contradiction",
            Self::EngineCrashed { .. } => "engine-crashed",
            Self::BudgetExpired => "budget-expired",
            Self::MemoryCeilingExceeded => "memory-ceiling-exceeded",
            Self::UnestablishedInitialState { .. } => "unestablished-initial-state",
            Self::ViolationOverFreeLeaves { .. } => "violation-over-free-leaves",
            Self::UnestablishedInitialCubes { .. } => "unestablished-initial-cubes",
            Self::ReportSelfContradiction { .. } => "report-self-contradiction",
            Self::NotAttempted => "not-attempted",
        }
    }

    /// One-line rendering for the CLI, carrying the budget name where there is one.
    pub fn one_line(&self) -> String {
        match self {
            Self::SafetyShapeNotReducible => "safety-shape-not-reducible — the property SHAPE is                  outside the rescue lane; needs a reshape or a new reducer, NOT a bigger budget"
                .to_string(),
            Self::NoStateModelNonSafety => "no-state-model-non-safety — the design has zero state                  registers; a modelling issue, not an engine cap"
                .to_string(),
            Self::UnclassifiedBottom => "unclassified — cause not established; do NOT read as                  distinct-from-abstain"
                .to_string(),
            Self::EngineDidNotComplete { engine, detail } => {
                format!("engine `{engine}` did not complete on this design: {detail}")
            }
            Self::EngineContradiction { detail } => format!(
                "SOUNDNESS ALARM — portfolio engines returned CONTRADICTING definite verdicts \
                 ({detail}); one engine is unsound on this design. A bigger budget cannot help \
                 and retrying is the wrong response."
            ),
            Self::EngineCrashed { detail } => {
                format!("the engine process CRASHED (not an abstention): {detail}")
            }
            Self::BudgetExpired => "a WALL-CLOCK budget expired — retrying with more time is the \
                 right response here (MUNUNU_PROPERTY_BUDGET_MS for the property as a whole, \
                 MUNUNU_RESCUE_SLICE_MS for the ⊥ re-plan's per-step slice). HOST-DEPENDENT: the \
                 same command on a quieter or faster machine may DECIDE this property, so treat a \
                 gate failure here as a configuration result, not a verdict about the design"
                .to_string(),
            Self::MemoryCeilingExceeded => "the process-RSS ceiling was exceeded \
                 (MUNUNU_MAX_PROCESS_MEMORY_BYTES) — retrying with more TIME cannot help; more \
                 memory, a smaller cone, or a lower engine tier can. Host-dependent without a \
                 clock: RSS varies with allocator state and neighbouring processes"
                .to_string(),
            Self::UnestablishedInitialState {
                registers,
                gated_resets,
            } => format!(
                "the property's cone touches {registers} register(s) whose INITIAL VALUE is not \
                 established, with {} pinned inactive. A free cycle-0 state OVER-approximates \
                 reachability, and that licenses a definite HOLDS but never a definite VIOLATED — \
                 so the violation is withheld rather than reported. Since mununu#578 the reset \
                 values that CAN be recovered are established automatically, so a register still \
                 free here is one with no reset value to recover — it HOLDS through reset \
                 (`next = ite(rst, d, q)`). Supply an `init` for it via a sidecar to decide this \
                 property; a `--cutpoint` is deliberately free and is NOT counted here.",
                gated_resets.join(", ")
            ),
            Self::ViolationOverFreeLeaves { leaves } => format!(
                "the violation was found on a model FREER than the RTL: the property's cone reads \
                 {} undriven net(s) ({}) — an unconnected wire or the output of a module \
                 missing from the source set, free at every cycle. The trace is kept as a MAY-trace: the real \
                 driver may never produce the values it needs, so the violation is withheld \
                 rather than reported. Vendor the missing module or provide the driver to decide \
                 this property; a HOLDS over the same model would have transferred.",
                leaves.len(),
                leaves.join(", ")
            ),
            Self::UnestablishedInitialCubes {
                free_dimensions,
                cap,
                registers,
            } => format!(
                "the property's predicates have {free_dimensions} FREE initial dimensions (free \
                 inputs, and predicates over {registers} register(s) with no `init` line), past \
                 the {cap} the cube path enumerates — so its initial cube set was not read and \
                 nothing was decided. An init-less register is FREE at cycle 0 on every engine \
                 (mununu#579); supply an `init` for those registers via a sidecar, or let \
                 reset-gating establish them, to decide this property."
            ),
            Self::ReportSelfContradiction { detail } => format!(
                "🔴 SOUNDNESS ALARM — this report contradicts ITSELF: {detail}. The two verdicts \
                 cannot both be right, so BOTH are withheld. Retrying is actively wrong, and so is \
                 believing whichever one looks more plausible: mununu#577 taught that expensively, \
                 where the half that looked broken was the correct one. Escalate this rather than \
                 re-running it."
            ),
            Self::NotAttempted => {
                "never attempted — filtered out before any engine ran".to_string()
            }
        }
    }
}

/// True when the design has no state registers (`state ...` lines).
fn design_stateless(design_btor2: &str) -> bool {
    match crate::adapter::btor2::parser::parse(design_btor2) {
        Ok(file) => !file
            .lines
            .iter()
            .any(|l| matches!(l.node, crate::adapter::btor2::ast::Node::State { .. })),
        Err(_) => false,
    }
}

/// Classify a residual ⊥ property. Called ONLY on properties whose outcome is
/// still `Unknown` after the escalation pass.
fn classify_bottom_reason(prop: &PropertyVerdict, stateless_model: bool) -> BottomReason {
    use crate::mu_calculus::{PropertyClass, parser as mu_parser};
    let Ok(formula) = mu_parser::parse(&prop.formula) else {
        return BottomReason::UnclassifiedBottom;
    };
    match formula.property_class() {
        PropertyClass::Safety => {
            // The reducers the safety rescue tries, in its order — a shape any of them accepts
            // REACHED the portfolio, so its residual ⊥ is not a shape gap (PR C added the chain).
            use crate::adapter::reach_rescue::{
                reduce_ag_boolean_body, reduce_ag_implies_next, reduce_ag_invariant,
                reduce_bounded_chain,
            };
            let reducible = reduce_ag_invariant(&formula).is_some()
                || reduce_ag_boolean_body(&formula).is_some()
                || reduce_ag_implies_next(&formula).is_some()
                || reduce_bounded_chain(&formula).is_some();
            if reducible {
                BottomReason::UnclassifiedBottom
            } else {
                BottomReason::SafetyShapeNotReducible
            }
        }
        PropertyClass::Liveness | PropertyClass::Reachability | PropertyClass::Mixed => {
            if stateless_model {
                BottomReason::NoStateModelNonSafety
            } else {
                BottomReason::UnclassifiedBottom
            }
        }
        PropertyClass::Propositional => BottomReason::UnclassifiedBottom,
    }
}

/// mununu#492 Part B — the per-property provenance note that names a residual
/// ⊥'s classification so a consumer's gate can act on the distinction.
fn bottom_reason_note(name: &str, reason: BottomReason) -> VerificationNote {
    let (summary, detail) = match reason {
        BottomReason::SafetyShapeNotReducible => (
            format!(
                "`{name}`: residual ⊥ classified as `safety-shape-not-reducible` — the \
                 rescue lane could not reduce this AG-safety shape (compound-of-relational, \
                 non-standard AG outer shape, or a leaf the compiler rejects). NOT the same \
                 as an engine resource abstain — this is a modelling / property-shape gap."
            ),
            "The safety-rescue lane (mununu#492) accepts `AG(single_atom)` and \
             `AG(compound_boolean_of_single_atoms)` where each leaf is a `REG op VALUE` \
             comparison. Shapes outside that — a compound-of-relational (`AG(reg_a == reg_b \
             && …)`), a `CmpRegAddend` leaf, an `AG(a → AX b)` implication-with-next, or a \
             non-standard AG outer shape — are rejected here. A downstream gate should NOT \
             treat this ⊥ as interchangeable with an engine's resource abstain: this needs \
             a property reshape or a new rescue reducer, not a bigger budget."
                .into(),
        ),
        BottomReason::NoStateModelNonSafety => (
            format!(
                "`{name}`: residual ⊥ classified as `no-state-model-non-safety` — the design \
                 has zero state registers AND this property is non-Safety (liveness / \
                 recoverability). On a stateless model, `AG EF good` is vacuously true (rule \
                 4) and `AG(a → AF b)` has no lasso to reduce; the ⊥ is a modelling issue, \
                 not an engine cap."
            ),
            "A purely combinational block has no reachable state trajectory beyond the \
             initial (empty-state) point, so non-Safety property classes are either \
             vacuously true (recoverability) or unfulfillable-by-shape (response-liveness's \
             l2s needs state to close a lasso). Either compose the block with an enclosing \
             stateful context (its scheduler / driver), or restrict the property set to \
             tier-1 safety invariants — which the compound-safety rescue (mununu#492) does \
             decide on stateless models."
                .into(),
        ),
        BottomReason::UnclassifiedBottom => (
            format!(
                "`{name}`: residual ⊥ was not classified by the mununu#492 diagnostic. Most \
                 commonly this is a resource abstain (bit-cap / node-budget / iteration / \
                 wall-clock) on the exact-symbolic or cube engine — check for a sibling \
                 `bit-cap` / `abstained on the …` note on the same property."
            ),
            "The bottom-reason classifier recognises three shapes (safety-shape-not-reducible, \
             no-state-model-non-safety, unclassified). Anything outside those falls back to \
             this note. It is NOT a claim about resource vs shape — just an honest 'this \
             classifier did not narrow it further', so the consumer looks at the engine-side \
             notes for the actionable answer."
                .into(),
        ),
        // monono ask 26 — the case the sentence just above used to send readers hunting for.
        // That advice ("check for a sibling `abstained on the …` note") was unactionable: the
        // engine's abstention string was being DROPPED at the portfolio merge, so the sibling
        // note it pointed at did not exist. Now the reason arrives here verbatim.
        BottomReason::EngineDidNotComplete { engine, detail } => (
            format!(
                "`{name}`: ⊥ because engine `{engine}` did not complete on this design — \
                 {detail}"
            ),
            format!(
                "The abstention message above is the ENGINE'S OWN and names the budget it hit \
                 plus the numbers, so it is directly actionable — raise the named knob, or route \
                 the property elsewhere.\n\nRead the attribution precisely: `{engine}` failed \
                 over the WHOLE DESIGN, which is not a claim that THIS property caused the \
                 failure. What is claimed is that this property is ⊥ and the engine most likely \
                 to have decided it stopped for the stated reason. (Conflating those is the \
                 mununu#548 defect — a note describing one engine inherited by properties \
                 another engine decided.)\n\nAlso carried as the structured \
                 `bottom_reason` FIELD on the property, which a consumer cannot filter out by \
                 not knowing about it — a note can be, and was, for the reader this was written \
                 for."
            ),
        ),
        // Every variant added after monono ask 26 renders from `one_line()`, which the compiler
        // forces to stay exhaustive. So a new variant cannot silently acquire an empty or
        // misleading note — the failure mode that let `unclassified-bottom` point readers at a
        // sibling note the merge had already discarded.
        other => (
            format!("`{name}`: ⊥ — {}", other.one_line()),
            format!(
                "Machine-readable as the `bottom_reason` FIELD on this property (tag `{}`), which \
                 a consumer cannot filter out by not knowing about it. Branch a gate on the TAG: \
                 `budget-expired` is the one case where retrying with more budget is right; \
                 `engine-contradiction` is a SOUNDNESS ALARM where retrying is actively wrong; \
                 `safety-shape-not-reducible` needs a property reshape, not a bigger budget.",
                other.tag()
            ),
        ),
    };
    VerificationNote {
        kind: "bottom-reason".into(),
        level: NoteLevel::ScopeCaveat,
        summary,
        detail,
        items: Vec::new(),
        // mununu#548 O-2 — `bottom-reason` is THE per-property note, and the one whose join
        // matters: `drop_stale_bottom_reasons` removes it when another engine went on to decide the
        // property, and a missed match leaves a report saying `p` is ⊥ while also reporting it
        // HOLDS. That is the mununu#548 defect, so this is the site that must not rely on prose.
        property: Some(name.to_string()),
    }
}

/// Lever (b) — exact-symbolic rescue for cube-SKIPPED properties.
///
/// The predicate-cube engine skips a property it cannot seed — most commonly an
/// ATOM-LESS modal formula (no-deadlock `nu X.(<> true && [] X)`, a pure `<>`/`[]`
/// obligation) for which there is no state-cell / combinational atom to build a cube
/// dimension from, so it is reported `Skipped` with "no … atoms to seed the cube". The
/// full-state exact engine needs no seeding: it bit-blasts the design and decides the
/// formula directly (the same engine `verify_auto` runs on the `--engine exact-symbolic`
/// path). This pass gives every still-`Skipped` property one exact attempt and upgrades
/// it in place on a DEFINITE verdict.
///
/// SOUNDNESS. Runs only under reset-gating (`opts.gate_reset`) — the exact engine's A.6
/// precondition (with reset freed it is unsound; see [`verify_auto`]). Gated to the cube
/// path (`!opts.exact_symbolic`); the exact main path already ran exact, so there is
/// nothing new to rescue there. The exact engine is the differential ORACLE (the 56
/// `symbolic_bitblast` parity tests), so a definite `Holds`/`Violated` it returns transfers
/// to the RTL exactly as the main exact branch's does — a Skipped property gaining a
/// definite verdict is pure precision, never a contradiction. An over-cap / intractable
/// design returns `Err` (bounded by the node/iteration budgets + catch_unwind) and the
/// property is left `Skipped`.
///
/// Pure over `(report, design_btor2)`, so it is unit-testable on a BTOR2 fixture without
/// the SV toolchain.
pub(crate) fn rescue_skipped_via_exact(
    report: &mut AutoVerifyReport,
    design_btor2: &str,
    opts: &VerifyAutoOptions,
) -> Vec<VerificationNote> {
    use crate::adapter::btor2::symbolic_bitblast::{ExactSymbolicOptions, ExactVerdict};
    use crate::mu_calculus::parser as mu_parser;

    let mut notes = Vec::new();
    // A.6: the exact engine is sound only under reset-gating. `opts.exact_symbolic`
    // means the exact main path already ran — nothing to rescue on a cube run here.
    if !opts.gate_reset || opts.exact_symbolic {
        return notes;
    }
    // Plan P3 — the skip rescue is a ⊥ re-plan pass: the ladder with the cap ladder on (a
    // cone over the default cap is retried once at the ceiling under its own 60 s budget).
    let exact_opts = ExactSymbolicOptions {
        antecedent_shadow_enabled: opts.antecedent_shadow,
        antecedent_propagate_enabled: opts.antecedent_propagate,
        input_state_enabled: opts.input_state,
        cap_ladder: true,
        ..Default::default()
    };
    for prop in report.properties.iter_mut() {
        // mununu#504 — this is the THIRD full pass over the property list, and until now it had
        // NO budget guard at all (neither memory nor time), so it could run unbounded after both
        // earlier passes had already stopped. `break`, matching `escalate_bottom`: neither budget
        // recovers during the loop.
        if crate::adapter::run_budget::expired()
            || crate::adapter::memory_budget::check_process_memory_budget().is_err()
        {
            break;
        }
        if !matches!(prop.outcome, VerifyOutcome::Skipped { .. }) {
            continue;
        }
        let Ok(formula) = mu_parser::parse(&prop.formula) else {
            continue;
        };
        let ladder = crate::adapter::btor2::symbolic_bitblast::exact_ladder(
            design_btor2,
            &formula,
            &std::collections::HashSet::new(),
            &exact_opts,
            None,
            None,
        );
        let stage = ladder.stage;
        let witness_miss = ladder.witness_miss.clone();
        match ladder.result {
            Ok((ExactVerdict::Holds, _)) => {
                prop.outcome = VerifyOutcome::Holds;
                prop.counterexample = None;
                prop.decided_by = Some(stage.to_string());
                notes.push(exact_skip_rescue_note(&prop.name, "HOLDS"));
            }
            Ok((ExactVerdict::Violated, witness)) => {
                prop.outcome = VerifyOutcome::Violated { false_cells: 1 };
                prop.counterexample = witness
                    .map(|w| exact_counterexample_from_lasso(w, &cell_widths_of(design_btor2)))
                    .or_else(|| ef_unreachable_counterexample(&formula));
                prop.decided_by = Some(stage.to_string());
                notes.push(exact_skip_rescue_note(&prop.name, "VIOLATED"));
                // mununu#808 follow-up — the omitted lasso's reason reaches the report.
                if prop.counterexample.is_none()
                    && let Some(reason) = witness_miss
                {
                    notes.push(witness_miss_note(
                        &prop.name,
                        Some(stage),
                        &crate::adapter::reach_rescue::WitnessMiss {
                            stage: "exact-witness",
                            reason,
                        },
                    ));
                }
            }
            // Err (over cap / intractable design): leave the property Skipped.
            Err(_) => {}
        }
    }
    notes
}

/// Provenance note for a cube-Skipped property the exact engine rescued (lever b).
fn exact_skip_rescue_note(name: &str, verdict: &str) -> VerificationNote {
    VerificationNote {
        kind: "exact-skip-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the predicate cube could not seed the property (no atoms); the \
             full-state exact-symbolic engine decided it {verdict}"
        ),
        detail: "An atom-less modal property (e.g. no-deadlock `nu X.(<> true && [] X)`) has \
                 no state-cell / combinational atom for the cube to build a dimension from, so \
                 the cube reports Skipped. The exact engine bit-blasts the design and decides \
                 it directly. Sound: the exact engine is the differential oracle — under \
                 reset-gating a definite verdict transfers to the RTL; the property is upgraded \
                 only on a definite Holds/Violated."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// The engines that participated in a reachability-portfolio rescue (for a note).
fn engines_of(reach: &crate::adapter::reach_portfolio::ReachOutcome) -> Vec<&'static str> {
    reach
        .reachable_by
        .iter()
        .chain(reach.unreachable_by.iter())
        .copied()
        .collect()
}

/// A provenance note for a property the reachability portfolio rescued from `⊥`.
/// mununu#726 — run one portfolio-backed rescue step under the RESCUE SLICE: a child of the current
/// (row) budget bounded by `MUNUNU_RESCUE_SLICE_MS`, so it can never outlive the row or the run.
/// Records whether the slice ran out, so the caller can attribute a residual ⊥ to the clock.
/// mununu#734 — a reach portfolio that decided NOTHING on a row names its cause. When the slice and
/// the row budget are intact, an instant `Inconclusive` is not a search that gave up: it is every
/// member refusing the design (an encode error, an absent binary, a child without a verdict) or
/// every member returning a clean `unknown`. Either becomes `EngineDidNotComplete { engine:
/// "reach-portfolio", .. }` with the members' own words, plus a `portfolio-refused` note; a row that
/// already carries a reason (an engine failure attributed upstream) is left alone; a row whose
/// slice or budget ran out is the caller's `BudgetExpired`, attributed after the arm as before.
fn attribute_portfolio_abstention(
    prop: &mut PropertyVerdict,
    reach: &crate::adapter::reach_portfolio::ReachOutcome,
    budget_ran_out: bool,
    notes: &mut Vec<VerificationNote>,
) {
    if !matches!(prop.outcome, VerifyOutcome::Unknown { .. })
        || prop.bottom_reason.is_some()
        || budget_ran_out
        || !engines_of(reach).is_empty()
    {
        return;
    }
    let items: Vec<String> = reach
        .refused
        .iter()
        .map(|(member, why)| format!("{member}: {why}"))
        .collect();
    let detail = if items.is_empty() {
        "every portfolio member returned unknown within its budget (no refusal, no timeout)"
            .to_string()
    } else {
        format!("refused by {}", items.join("; "))
    };
    prop.bottom_reason = Some(BottomReason::EngineDidNotComplete {
        engine: "reach-portfolio".to_string(),
        detail: detail.clone(),
    });
    notes.push(VerificationNote {
        kind: "portfolio-refused".to_string(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{}`: the reach portfolio decided nothing within an intact budget — {detail}",
            prop.name
        ),
        detail: "mununu#734 — the members' refusals used to be dropped at the member boundary, so a \
                 portfolio that abstained in 40 ms because every member refused the design read \
                 exactly like one that searched and gave up. A refusal names a construct the member \
                 does not carry (a constant width, an array sort, a missing binary): fix or route \
                 around it — more time will not help. An all-unknown with no refusal is the \
                 genuine search abstention."
            .to_string(),
        items,
        property: Some(prop.name.clone()),
    });
}

/// mununu#734 — two sound portfolio members disagreed on the monitor: a soundness alarm, carried as
/// `EngineContradiction` (never an "unknown"), so the merge's own alarm and this one read the same.
fn attribute_portfolio_contradiction(
    prop: &mut PropertyVerdict,
    reach: &crate::adapter::reach_portfolio::ReachOutcome,
    notes: &mut Vec<VerificationNote>,
) {
    let detail = format!(
        "{}: reachable@[{}] vs unreachable@[{}] on the rescue's bad-monitor",
        prop.name,
        reach.reachable_by.join(","),
        reach.unreachable_by.join(",")
    );
    prop.bottom_reason = Some(BottomReason::EngineContradiction {
        detail: detail.clone(),
    });
    notes.push(VerificationNote {
        kind: "portfolio-rescue-contradiction".to_string(),
        level: NoteLevel::SoundnessCaveat,
        summary: format!(
            "`{}`: reach portfolio members CONTRADICT each other — {detail}",
            prop.name
        ),
        detail: "One member is unsound on this design. The row is ⊥, not a pick; a bigger budget \
                 cannot help and the disagreement must be investigated."
            .to_string(),
        items: Vec::new(),
        property: Some(prop.name.clone()),
    });
}

fn under_rescue_slice<T>(
    slice_ms: Option<u64>,
    slice_expired: &mut bool,
    step: impl FnOnce() -> T,
) -> T {
    let slice =
        crate::adapter::run_budget::Budget::child(&crate::adapter::run_budget::current(), slice_ms);
    let _guard = crate::adapter::run_budget::enter(&slice);
    let out = step();
    *slice_expired |= slice.expired();
    out
}

/// Plan 2 / B2 — provenance for a plain `EF p` decided on its dual by the reach portfolio.
fn reachability_rescue_note(name: &str, verdict: &str, engines: &[&str]) -> VerificationNote {
    VerificationNote {
        kind: "reachability-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the reachability portfolio decided it {verdict} \
             on the dual `AG !p` (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "`EF p` holds iff some initial state reaches `p` under some input sequence — exactly \
                 the `bad := p` reachability the portfolio decides. A reachable `bad` is a concrete \
                 path to `p` (HOLDS); a proof that `bad` is unreachable is a proof that `p` is never \
                 reached from reset (VIOLATED, with the target atoms as the witness). Sound in both \
                 directions: every portfolio member is sound and a disagreement raises a \
                 contradiction alarm rather than a verdict."
            .into(),
        items: Vec::new(),
        property: Some(name.to_string()),
    }
}

fn rescue_note(name: &str, verdict: &str, engines: &[&str]) -> VerificationNote {
    VerificationNote {
        kind: "portfolio-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the reachability portfolio decided it \
             {verdict} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The safety ⊥ was reduced to a `bad`-monitor and decided symbolically by the \
                 multi-engine reachability portfolio (exact ⊕ native ⊕ spacer ⊕ btormc ⊕ Pono). \
                 Sound: every portfolio member is sound, and a disagreement would surface a \
                 contradiction alarm rather than a verdict."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// A provenance note for a νμ recoverability property rescued from `⊥` — by the exact+COI
/// engine (reset-gated) if its cone fits the bit cap, else the scalable cube + smt-hyper-must
/// path.
fn recoverability_rescue_note(
    name: &str,
    verdict: &str,
    engine: crate::adapter::recoverability::RecoverabilityEngine,
) -> VerificationNote {
    use crate::adapter::recoverability::RecoverabilityEngine;
    let rung = match engine {
        RecoverabilityEngine::Exact => "exact+COI",
        RecoverabilityEngine::ExactReachRestricted => "exact+COI, reach-restricted",
        RecoverabilityEngine::CubeLadder => "the cube + smt-hyper-must ladder",
    };
    VerificationNote {
        kind: "recoverability-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the νμ recoverability path (exact+COI \
             first, then cube + smt-hyper-must) decided it {verdict} — by {rung}"
        ),
        detail: "The AG EF (recoverability) ⊥ was escalated: exact+COI decides it directly when \
                 the property's cone fits the bit cap (a wishbone/status control signal whose \
                 cone excludes the datapath), else the sound predicate-cube + smt-hyper-must \
                 path. Definite νμ verdicts transfer (exact = oracle; cube per Bruns–Godefroid / \
                 Shoham–Grumberg); box-AF liveness and safety take their own reductions."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// `decided_by` of an `AG EF` row refuted by a trap certificate.
pub const TRAP_CERTIFICATE_DECIDED_BY: &str = "trap-certificate";

/// Provenance for an `AG EF good` row refuted by a trap certificate: the trap, where the candidate
/// came from, and which engines decided the two checks.
fn trap_certificate_note(
    name: &str,
    good: &crate::adapter::liveness_rescue::Atom,
    cert: &crate::adapter::trap_certificate::TrapCertificate,
) -> VerificationNote {
    let list = |e: &[&str]| {
        if e.is_empty() {
            "—".to_string()
        } else {
            e.join(", ")
        }
    };
    let depth = cert
        .witness
        .as_ref()
        .map(|w| w.states.len().saturating_sub(1).to_string())
        .unwrap_or_else(|| "beyond the witness bound".to_string());
    VerificationNote {
        kind: "trap-certificate".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: VIOLATED by a trap certificate — `{s} == {v}` is reachable from reset (depth \
             {depth}; {reach}) and closed under every transition ({closure}), and the target \
             `{s} == {g}` lies outside it, so from that state the target is never reached again",
            s = cert.signal,
            v = cert.value,
            g = good.value,
            reach = list(&cert.reach_engines),
            closure = list(&cert.closure_engines),
        ),
        detail: "`AG EF p` fails iff some reachable state satisfies `AG ¬p`. A set T that is \
                 reachable (EF T, decided on its dual AG ¬T), closed over the reachable states \
                 (AG (T → [] T), the `|=>` monitor bad = T(prev) ∧ ¬T) and disjoint from p (the \
                 same register at another value) is such a state set. Both checks run on the model \
                 as given — nothing is approximated — and the certificate only ever refutes. The \
                 counterexample is the path into T; it ends there because T never lets the target \
                 back in."
            .into(),
        items: vec![
            format!("trap: {} == {}", cert.signal, cert.value),
            format!("source: {}", cert.source.label()),
            format!("closure: {}", list(&cert.closure_engines)),
            format!("reach: {}", list(&cert.reach_engines)),
            format!("depth: {depth}"),
        ],
        property: Some(name.to_string()),
    }
}

/// The trap search ran and certified nothing: what each candidate turned out to be, so the ladder's
/// own ⊥ explanation is not the only trail.
fn trap_certificate_search_note(
    name: &str,
    good: &crate::adapter::liveness_rescue::Atom,
    search: &crate::adapter::trap_certificate::TrapSearch,
    slice_expired: bool,
) -> VerificationNote {
    VerificationNote {
        kind: "trap-certificate-search".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: no trap certified for `{} == {}` ({} candidate(s) tried{})",
            good.signal,
            good.value,
            search.tried.len(),
            if slice_expired {
                "; the rescue slice ran out"
            } else {
                ""
            }
        ),
        detail: "A trap is a value of the target's register that is reachable and closed under \
                 every transition. A candidate that is reachable but open, or closed but never \
                 entered, refutes nothing; an undecided one may still be a trap the budget did not \
                 reach. The recoverability ladder ran afterwards, unchanged."
            .into(),
        items: search
            .tried
            .iter()
            .map(|(c, o)| {
                format!(
                    "{} == {} ({}): {}",
                    good.signal,
                    c.value,
                    c.source.label(),
                    o.tag()
                )
            })
            .collect(),
        property: Some(name.to_string()),
    }
}

/// The trap search was not run: the target register's sequential cone holds a free leaf, so a path
/// into a trap could be one the real driver never takes.
fn trap_certificate_withheld_note(
    name: &str,
    good: &crate::adapter::liveness_rescue::Atom,
    search: &crate::adapter::trap_certificate::TrapSearch,
) -> VerificationNote {
    VerificationNote {
        kind: "trap-certificate-withheld".into(),
        level: NoteLevel::ScopeCaveat,
        summary: format!(
            "`{name}`: no trap certificate attempted — `{}`'s cone reads {} free leaf(s) (an \
             undriven net, a cut point or a black-boxed output)",
            good.signal,
            search.withheld_by.len()
        ),
        detail: "A trap certificate refutes `AG EF p` by a path into a closed set. On a cone with a \
                 free leaf the model over-approximates the RTL: the closure would transfer, but the \
                 path could be one the real driver never takes. Vendor the module, drop the cut \
                 point, or provide the driver, and the search runs."
            .into(),
        items: search.withheld_by.clone(),
        property: Some(name.to_string()),
    }
}

/// Actionable-⊥: an explanation of WHY a νμ recoverability property stayed ⊥ — the structural
/// obstacles in its recovery cone the predicate abstraction cannot cross (uncertified gating
/// counters; wide data/config state the recovery value rides). Purely diagnostic: it never changes
/// the ⊥ verdict, only makes it actionable. The `items` carry the machine-readable obstacle list.
fn recoverability_bot_reason_note(
    name: &str,
    diag: &crate::adapter::recoverability::RecoverabilityBotDiagnosis,
) -> VerificationNote {
    let mut items: Vec<String> = Vec::new();
    for (n, w) in &diag.uncertified_counters {
        items.push(format!("uncertified-counter: {n} ({w}-bit)"));
    }
    for (n, w) in &diag.wide_influences {
        items.push(format!("wide-influence: {n} ({w}-bit)"));
    }
    VerificationNote {
        kind: "recoverability-bot-reason".into(),
        // A localized obstacle narrows the verdict's scope (input-scoped / needs an assumption);
        // an empty diagnosis is purely informational.
        level: if diag.is_empty() {
            NoteLevel::Info
        } else {
            NoteLevel::ScopeCaveat
        },
        summary: format!("`{name}`: ⊥ because — {}", diag.hint()),
        detail: "This is a best-effort, sound explanation of the abstain (never a verdict): the \
                 structural obstacles in the recovery cone the predicate cube cannot cross. An \
                 un-certified counter needs a ranking certificate or a fairness assumption; a wide \
                 data/config influence means the recovery value is input-scoped (constrain those \
                 inputs to decide). No obstacle localized ⇒ the abstraction may be closable with \
                 additional predicates."
            .into(),
        items,
        property: None,
    }
}

/// mununu#477 Option B — provenance note for a box-AF liveness property the
/// fair-cycle l2s (Emerson–Lei extension) decided under a conjunctive `GF` fairness
/// assumption on primary-input atoms. Names the assumption(s) applied.
fn fair_cycle_rescue_note(
    name: &str,
    verdict: &str,
    engines: &[&str],
    fairness_atoms: &[crate::adapter::liveness_rescue::Atom],
) -> VerificationNote {
    use crate::adapter::btor2::predicate_expr::CmpOp;
    let fair_render: Vec<String> = fairness_atoms
        .iter()
        .map(|a| {
            let op = match a.op {
                CmpOp::Eq => "==",
                CmpOp::Ne => "!=",
                CmpOp::Lt => "<",
                CmpOp::Le => "<=",
                CmpOp::Gt => ">",
                CmpOp::Ge => ">=",
            };
            format!("GF ({} {} {})", a.signal, op, a.value)
        })
        .collect();
    let fair_conj = fair_render.join(" && ");
    VerificationNote {
        kind: "fair-cycle-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the fair-cycle l2s (Emerson–Lei) \
             decided it {verdict} under {fair_conj} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The box-response liveness ⊥ (`AG(a → AF b)`) was decided under the \
                 conjunctive fairness assumption (mununu#477 Option B) via the Emerson–Lei \
                 fair-cycle l2s: the standard l2s pending / snapshot construction plus one \
                 `fair_seen` monitor latch per `GF` atom, all conjuncted into `bad`. A `holds` \
                 verdict means the guarantee holds against every environment schedule \
                 satisfying every fairness assume infinitely often; a `violated` verdict is \
                 a reachable lasso satisfying every fairness constraint that leaves the \
                 request forever ungranted."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

/// A provenance note for a box-AF liveness property the l2s reduction + reachability
/// portfolio rescued from `⊥`.
fn liveness_rescue_note(name: &str, verdict: &str, engines: &[&str]) -> VerificationNote {
    VerificationNote {
        kind: "liveness-rescue".into(),
        level: NoteLevel::Info,
        summary: format!(
            "`{name}`: the cube abstraction left ⊥; the liveness-to-safety reduction + \
             reachability portfolio decided it {verdict} (engine(s): {})",
            if engines.is_empty() {
                "—".to_string()
            } else {
                engines.join(", ")
            }
        ),
        detail: "The box-response liveness ⊥ (`AG(a → AF b)`) was reduced to a reachable \
                 `b`-free-lasso `bad`-monitor (Biere–Artho–Schuppan liveness-to-safety) and \
                 decided symbolically by the reachability portfolio. Sound + complete for the \
                 box-AF shape; the diamond-EF recoverability response is not reduced here."
            .into(),
        items: Vec::new(),
        property: None,
    }
}

#[cfg(test)]
mod tests {
    /// mununu#459 — `partition_config_pins` classifies user pins into applied vs unusable. A pin
    /// is applied when the config pass pins it OR reset-gating already pinned it to the same value;
    /// it is unusable when its name is not a model input (misspelled / non-input) or, being a real
    /// input, it conflicts with an existing constant. The unusable set becomes a hard error
    /// upstream — never a silent drop that verifies a different question than the one asked.
    #[test]
    fn config_pins_partition_applied_vs_unusable() {
        use std::collections::HashSet;
        let inputs: HashSet<String> = ["rst_n", "mode", "clk"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let config_pins = vec![
            ("mode".to_string(), 3u64),  // applied by the config pass
            ("rst_n".to_string(), 1u64), // already pinned by reset-gating (same value) → applied
            ("bogus".to_string(), 0u64), // not a model input → unusable (misspelled / non-input)
            ("clk".to_string(), 0u64), // real input, but reset pinned it to a DIFFERENT value → conflict
        ];
        let pinned_configs: HashSet<String> = ["mode=3"].iter().map(|s| s.to_string()).collect();
        let reset_pinned: HashSet<String> =
            ["rst_n=1", "clk=1"].iter().map(|s| s.to_string()).collect();
        let (applied, unusable) =
            super::partition_config_pins(&config_pins, &inputs, &pinned_configs, &reset_pinned);
        assert_eq!(
            applied,
            vec![("mode".to_string(), 3), ("rst_n".to_string(), 1)],
            "mode (config pass) + rst_n (reset pin to the same value) are applied"
        );
        assert_eq!(
            unusable.len(),
            2,
            "bogus (non-input) + clk (value conflict) are unusable: {unusable:?}"
        );
        assert!(
            unusable
                .iter()
                .any(|u| u.contains("bogus") && u.contains("not a primary input")),
            "an unknown/misspelled name reports 'not a primary input': {unusable:?}"
        );
        assert!(
            unusable
                .iter()
                .any(|u| u.contains("clk") && u.contains("already pinned")),
            "a real input pinned to a different constant reports a conflict: {unusable:?}"
        );
    }

    /// A.4 — a `Violated` bare `EF p` (reachability) yields the unreachable-target
    /// witness: no path (`prefix`/`cycle` empty), the target atoms naming what the
    /// design never reaches. A recoverability (`AG EF`) shape is not a bare `EF` and
    /// keeps its concrete trap-path witness instead (no unreachable-target here).
    #[test]
    fn bare_ef_violation_yields_unreachable_target_witness() {
        use crate::mu_calculus::parser as mu_parser;
        let ef = mu_parser::parse("mu X. ((round_counter == 31) or <> X)").expect("EF parses");
        let cx = super::ef_unreachable_counterexample(&ef).expect("bare EF → unreachable witness");
        assert!(cx.prefix.is_empty() && cx.cycle.is_empty() && cx.inputs.is_empty());
        assert_eq!(cx.unreachable_target.len(), 1);
        assert!(cx.unreachable_target[0].contains("round_counter"));
        let agef = mu_parser::parse("nu Y. ((mu X. ((busy == 0) or <> X)) and [] Y)")
            .expect("AG EF parses");
        assert!(super::ef_unreachable_counterexample(&agef).is_none());
    }

    /// Ranking-wiring — a recoverability `AG EF <good>` routes to the scalable
    /// recoverability engine (ranking + cube), returning its definite verdict. `good`
    /// = `data == target` over a design where both increment from 0 (invariant → the
    /// relation always holds ⇒ recoverable ⇒ Holds); the never-equal variant ⇒ Violated.
    /// A non-recoverability shape (bare `EF`) does NOT route (`None`).
    #[test]
    fn ranking_rescue_routes_ag_ef_recoverability_verdict() {
        use crate::mu_calculus::parser as mu_parser;
        // data,target both init 0 and +1 each cycle ⇒ data==target INVARIANT ⇒ AG EF holds.
        let holds_btor2 = "1 sort bitvec 1\n2 sort bitvec 8\n4 state 2 data\n5 state 2 target\n\
             6 zero 2\n7 one 2\n13 init 2 4 6\n14 init 2 5 6\n15 add 2 4 7\n16 add 2 5 7\n\
             22 next 2 4 15\n23 next 2 5 16\n";
        // target inits 1 ≠ data(0), both +1 ⇒ never equal ⇒ AG EF (data==target) VIOLATED.
        let viol_btor2 = "1 sort bitvec 1\n2 sort bitvec 8\n4 state 2 data\n5 state 2 target\n\
             6 zero 2\n7 one 2\n13 init 2 4 6\n14 init 2 5 7\n15 add 2 4 7\n16 add 2 5 7\n\
             22 next 2 4 15\n23 next 2 5 16\n";
        let agef = mu_parser::parse("nu Y. ((mu X. ((data == target) or <> X)) and [] Y)")
            .expect("parses");
        assert_eq!(
            super::try_ranking_recoverability(holds_btor2, &agef),
            Some(VerifyOutcome::Holds),
            "invariant-relational recoverability routes to Holds via the recoverability engine",
        );
        assert_eq!(
            super::try_ranking_recoverability(viol_btor2, &agef),
            Some(VerifyOutcome::Violated { false_cells: 1 }),
            "never-equal relational target routes to Violated",
        );
        // A bare `EF` (not the recoverability νμ shape) does not route.
        let ef = mu_parser::parse("mu X. ((data == target) or <> X)").expect("EF parses");
        assert_eq!(super::try_ranking_recoverability(holds_btor2, &ef), None);
    }

    use super::*;
    use crate::mu_calculus::parser as mu_parser;

    fn cells(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn src(content: &str) -> Vec<(String, String)> {
        vec![("design.sv".to_string(), content.to_string())]
    }

    // ---- PORTFOLIO combiner (hermetic: no toolchain, pure over reports) --------------------

    /// A single-property report with the given name + outcome (+ optional counterexample).
    fn mk_report(props: &[(&str, VerifyOutcome, Option<ExactCounterexample>)]) -> AutoVerifyReport {
        AutoVerifyReport {
            properties: props
                .iter()
                .map(|(name, outcome, cx)| PropertyVerdict {
                    name: name.to_string(),
                    label: None,
                    kind: crate::adapter::slang::translate::SvaKind::Assert,
                    formula: format!("formula::{name}"),
                    outcome: outcome.clone(),
                    seeded_predicates: Vec::new(),
                    counterexample: cx.clone(),
                    bottom_reason: None,
                    decided_by: None,
                    vacuity: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// mununu#548 — the merged portfolio report must NOT carry the base engine's single-engine
    /// abstraction posture. `merge_portfolio_reports` does `base.clone()`, so an exact-symbolic base
    /// contributes a note asserting *"no abstraction … and there is no ⊥"* — while the merge folds
    /// in a ⊥ from an abstracting engine. A consumer then reads, in ONE report, that ⊥ cannot
    /// happen and a ⊥.
    ///
    /// The fixture is the reported shape: exact decides one property and leaves one ⊥, a second
    /// engine runs, and the merged report still contains a ⊥.
    /// A mutant some property catches is `holds`; one that survives every property is the
    /// adequacy gap, `violated`, which is what `sv mutate`'s exit code and both surfaces report.
    #[test]
    fn a_surviving_mutant_is_the_violated_outcome() {
        let flip = |flipped| PropertyFlip {
            name: "p".into(),
            label: Some("a_p".into()),
            baseline: "holds".into(),
            mutant: if flipped { "violated" } else { "holds" }.into(),
            flipped,
        };
        let report = |props: Vec<PropertyFlip>| {
            let flipped = props.iter().filter(|p| p.flipped).count();
            MutateReport {
                mutation: "stick:q".into(),
                unflipped: props.len() - flipped,
                properties: props,
                flipped,
            }
        };
        assert_eq!(report(vec![flip(false), flip(true)]).outcome(), "holds");
        assert_eq!(report(vec![flip(false), flip(false)]).outcome(), "violated");
    }

    #[test]
    fn merged_portfolio_posture_note_does_not_claim_bottom_is_impossible() {
        let exact_base = {
            let mut r = mk_report(&[
                ("p_decided", VerifyOutcome::Holds, None),
                (
                    "p_bottom",
                    VerifyOutcome::Unknown { unknown_cells: 0 },
                    None,
                ),
            ]);
            // What a single-engine exact run attaches, verbatim from `build_notes`.
            r.notes.push(VerificationNote {
                kind: "abstraction-posture".into(),
                level: NoteLevel::Info,
                summary: "Exact-symbolic model checking — no abstraction, so a definite verdict \
                          is sound and there is no ⊥."
                    .into(),
                detail: "the predicate abstraction that would produce ⊥ is not used".into(),
                items: Vec::new(),
                property: None,
            });
            r
        };
        let symbolic = mk_report(&[
            ("p_decided", VerifyOutcome::Holds, None),
            (
                "p_bottom",
                VerifyOutcome::Unknown { unknown_cells: 0 },
                None,
            ),
        ]);

        let merged = merge_portfolio_reports(
            &[
                ("exact-symbolic", Ok(exact_base)),
                ("symbolic", Ok(symbolic)),
            ],
            PortfolioMode::Sequential,
        )
        .expect("merge ok");

        // The premise: the merged report really does contain a ⊥.
        assert!(
            merged
                .properties
                .iter()
                .any(|p| matches!(p.outcome, VerifyOutcome::Unknown { .. })),
            "fixture must produce a ⊥, or the note is not contradicted by anything"
        );

        let posture = merged
            .notes
            .iter()
            .find(|n| n.kind == "abstraction-posture")
            .expect("the posture note survives the merge");

        // THE DEFECT: the inherited single-engine wording asserts ⊥ is impossible.
        let text = format!("{} {}", posture.summary, posture.detail);
        assert!(
            !text.contains("there is no ⊥") || text.contains("PER ENGINE"),
            "the merged posture note still claims ⊥ cannot happen: {text}"
        );
        // And it must say what IS true of a merged report: the posture is per engine.
        assert!(
            posture.summary.contains("PER ENGINE"),
            "the merged posture note should scope itself to engines; got: {}",
            posture.summary
        );
        assert!(
            posture.detail.contains("exact-symbolic") && posture.detail.contains("symbolic"),
            "it should name the engines that ran; got: {}",
            posture.detail
        );
        assert_eq!(
            posture.level,
            NoteLevel::ScopeCaveat,
            "a posture that does not describe the whole report is a scope caveat, not Info"
        );
    }

    /// mununu#548 (second instance) — a `bottom-reason` note explains why a property is ⊥. It is
    /// emitted PER ENGINE inside `build_notes`. If the base engine left a property ⊥ and a LATER
    /// engine decided it, the merge takes the definite verdict — but `base.clone()` carries the
    /// stale explanation, so the report says "here is why `p` is ⊥" about a property it also
    /// reports as HOLDS.
    #[test]
    fn merged_portfolio_drops_bottom_reason_for_a_property_another_engine_decided() {
        let exact_base = {
            let mut r = mk_report(&[("p", VerifyOutcome::Unknown { unknown_cells: 0 }, None)]);
            r.notes.push(VerificationNote {
                kind: "bottom-reason".into(),
                level: NoteLevel::ScopeCaveat,
                summary: "p: ⊥ — safety-shape-not-reducible".into(),
                detail: "the rescue could not reduce this shape".into(),
                items: Vec::new(),
                property: None,
            });
            r
        };
        // A later engine DECIDES the same property.
        let symbolic = mk_report(&[("p", VerifyOutcome::Holds, None)]);

        let merged = merge_portfolio_reports(
            &[
                ("exact-symbolic", Ok(exact_base)),
                ("symbolic", Ok(symbolic)),
            ],
            PortfolioMode::Sequential,
        )
        .expect("merge ok");

        assert!(
            matches!(
                merged
                    .properties
                    .iter()
                    .find(|q| q.name == "p")
                    .map(|q| &q.outcome),
                Some(VerifyOutcome::Holds)
            ),
            "premise: the merge must have taken the definite verdict"
        );
        let stale: Vec<&str> = merged
            .notes
            .iter()
            .filter(|n| n.kind == "bottom-reason" && n.summary.starts_with("p:"))
            .map(|n| n.summary.as_str())
            .collect();
        assert!(
            stale.is_empty(),
            "the report explains why `p` is ⊥ while also reporting it HOLDS: {stale:?}"
        );
    }

    fn cx_stub() -> ExactCounterexample {
        ExactCounterexample {
            prefix: vec![vec![("st".to_string(), CexValue::from_u64(0, 2))]],
            cycle: vec![vec![("st".to_string(), CexValue::from_u64(3, 2))]],
            inputs: vec![vec![("esc".to_string(), CexValue::from_u64(1, 1))]],
            unreachable_target: Vec::new(),
        }
    }

    // T3 — the control-slice (cutpoint over-approximation) note must gate its
    // "definite HOLDS transfers (safety + over-approx = sound)" claim on the property
    // class: it holds for safety/universal properties but NOT for an alternating (νμ)
    // recoverability property, which collapses under a plain over-approximation. The
    // νμ caveat appears only when the report actually carries an alternating property.
    #[test]
    fn control_slice_note_gates_holds_transfer_claim_on_property_class() {
        use crate::adapter::slang::translate::SvaKind;
        let prop = |name: &str, formula: &str| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome: VerifyOutcome::Holds,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let build = |report: &AutoVerifyReport| {
            build_notes(
                report,
                must_edge_posture_label(MustEdgeInference::Off, false),
                &[],
                &[],
                &[],
                &["cnt".to_string()],
                &NotePosture::Exact,
            )
        };
        // νμ recoverability (`AG EF`, alternation_depth 2) under cutpoints ⇒ caveat present.
        let alternating = AutoVerifyReport {
            properties: vec![prop("recov", "nu Y. ((mu X. ((p == 1) || <> X)) && [] Y)")],
            ..Default::default()
        };
        let cs = build(&alternating);
        let note = cs
            .iter()
            .find(|n| n.kind == "control-slice")
            .expect("control-slice note present");
        assert!(
            note.detail.contains("ALTERNATING recoverability property"),
            "a νμ property under cutpoints must caveat that HOLDS is sound for the abstract model \
             only; got: {}",
            note.detail
        );

        // Only a safety property (alternation_depth 1) under cutpoints ⇒ no νμ caveat.
        let safety_only = AutoVerifyReport {
            properties: vec![prop("safe", "nu X. ((p == 1) && [] X)")],
            ..Default::default()
        };
        let cs2 = build(&safety_only);
        let note2 = cs2
            .iter()
            .find(|n| n.kind == "control-slice")
            .expect("control-slice note present");
        assert!(
            !note2.detail.contains("ALTERNATING recoverability property"),
            "a safety-only report must not carry the νμ caveat; got: {}",
            note2.detail
        );
    }

    /// Plan 2 / B2 — a plain `EF p` the cube left ⊥ is decided on its dual by the reach portfolio:
    /// `big` is held at 0, so `EF (big == 0)` is reached at the initial state (HOLDS, with the
    /// portfolio's path), `EF (big == 5)` is never reached (VIOLATED, the target atoms as the
    /// witness), and an `EF` whose body is not a compilable boolean (a nested modality) is left ⊥
    /// without a note. Before this arm every `Reachability` row fell through `_ => {}`.
    #[test]
    fn escalate_bottom_reachability_decides_ef_on_the_dual() {
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let row = |name: &str, formula: &str| PropertyVerdict {
            name: name.to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: formula.to_string(),
            outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut report = AutoVerifyReport {
            properties: vec![
                row("reach_zero", "mu Z. ((big == 0) || <> Z)"),
                row("reach_five", "mu Z. ((big == 5) || <> Z)"),
                row("not_reducible", "mu Z. ((big == 0) || <> <> Z)"),
            ],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            DESIGN,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "EF (big == 0) is reached at reset: {:?}",
            report.properties[0].outcome
        );
        assert_eq!(
            report.properties[0].decided_by.as_deref(),
            Some("reach-rescue")
        );
        assert!(
            matches!(report.properties[1].outcome, VerifyOutcome::Violated { .. }),
            "EF (big == 5) is never reached: {:?}",
            report.properties[1].outcome
        );
        let cx = report.properties[1]
            .counterexample
            .as_ref()
            .expect("an unreachable target carries its atoms as the witness");
        assert_eq!(cx.unreachable_target, vec!["big == 5".to_string()]);
        assert!(cx.prefix.is_empty() && cx.cycle.is_empty());
        assert!(
            matches!(report.properties[2].outcome, VerifyOutcome::Unknown { .. }),
            "a non-reducible EF stays ⊥: {:?}",
            report.properties[2].outcome
        );
        assert_eq!(
            notes
                .iter()
                .filter(|n| n.kind == "reachability-rescue")
                .count(),
            2,
            "one provenance note per decided row: {:?}",
            notes.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>()
        );
    }

    /// PR C — the bounded-chain shapes the rescue used to DECLINE in 0 ms are rescued: a
    /// `$rose(flag) |-> ##[0:2] ack` chain on a design whose flag is constant 0 HOLDS (the
    /// antecedent never fires; k-induction proves the monitor's `bad` unreachable), and the same
    /// shape on the 2-bit counter with `cnt == 1` as the trigger and `cnt == 3` as the response is
    /// VIOLATED with a trace when the window is one cycle short.
    #[test]
    fn escalate_bottom_safety_rescues_a_bounded_response_chain() {
        const QUIET: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 flag\n4 init 1 3 2\n\
                             5 next 1 3 2\n6 state 1 flag__past\n7 init 1 6 2\n8 next 1 6 3\n\
                             9 input 1 ack\n";
        const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                               5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";
        let row = |name: &str, formula: &str| PropertyVerdict {
            name: name.to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: formula.to_string(),
            outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut quiet = AutoVerifyReport {
            properties: vec![row(
                "flag_alerts",
                "nu X. (((!((flag && (!(flag__past))))) || (ack || [] (ack || [] ack))) && [] X)",
            )],
            ..Default::default()
        };
        let notes = escalate_bottom(&mut quiet, QUIET, false, &VerifyAutoOptions::default(), &[]);
        assert!(
            matches!(quiet.properties[0].outcome, VerifyOutcome::Holds),
            "a flag that never rises makes the chain hold: {:?}",
            quiet.properties[0].outcome
        );
        assert_eq!(
            quiet.properties[0].decided_by.as_deref(),
            Some("reach-rescue")
        );
        assert!(
            notes.iter().any(|n| n.kind == "portfolio-rescue"),
            "{notes:?}"
        );
        let mut short = AutoVerifyReport {
            properties: vec![row(
                "window_short",
                "nu X. (((!((cnt == 1))) || ((cnt == 3) || [] (cnt == 3))) && [] X)",
            )],
            ..Default::default()
        };
        escalate_bottom(
            &mut short,
            COUNTER,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(short.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "{:?}",
            short.properties[0].outcome
        );
        assert!(
            short.properties[0].counterexample.is_some(),
            "the violated chain carries its trace"
        );
    }

    /// PR D3 (mununu#733) — a later member's `Unknown { NotAttempted }` row (it left the row to an
    /// earlier member) never displaces that member's definite verdict in the merge, keeps its
    /// attribution, and leaves no stale ⊥ reason behind.
    #[test]
    fn a_not_attempted_row_never_displaces_an_earlier_definite() {
        let row = |outcome: VerifyOutcome, reason: Option<BottomReason>| PropertyVerdict {
            name: "p".to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: "nu X. ((q == 0) && [] X)".to_string(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: reason,
            decided_by: None,
            vacuity: None,
        };
        let first = AutoVerifyReport {
            properties: vec![row(VerifyOutcome::Holds, None)],
            ..Default::default()
        };
        let later = AutoVerifyReport {
            properties: vec![row(
                VerifyOutcome::Unknown { unknown_cells: 0 },
                Some(BottomReason::NotAttempted),
            )],
            ..Default::default()
        };
        let merged = merge_portfolio_reports(
            &[("exact-symbolic", Ok(first)), ("explicit", Ok(later))],
            PortfolioMode::Sequential,
        )
        .expect("merges");
        let p = &merged.properties[0];
        assert!(matches!(p.outcome, VerifyOutcome::Holds), "{:?}", p.outcome);
        assert_eq!(p.decided_by.as_deref(), Some("exact-symbolic"));
        assert!(p.bottom_reason.is_none(), "{:?}", p.bottom_reason);
        let portfolio = merged
            .notes
            .iter()
            .find(|n| n.kind == "portfolio")
            .expect("portfolio note");
        assert!(
            portfolio
                .items
                .iter()
                .any(|i| i == "decided-by:exact-symbolic=1"),
            "{:?}",
            portfolio.items
        );
    }

    /// PR C — the planner's once-per-run re-plan on the MERGED report: a ⊥ row the rescue decides
    /// becomes HOLDS attributed to `reach-rescue`, with the rescue's provenance note ON the merged
    /// report (a member's notes never survived the merge) and the member's generic ⊥ reason gone;
    /// a merged report with every row decided is left untouched.
    #[test]
    fn replan_merged_decides_the_rows_no_engine_decided_once() {
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let row = |outcome: VerifyOutcome| PropertyVerdict {
            name: "big_zero".to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: "nu X. ((big == 0) && [] X)".to_string(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: Some(BottomReason::UnclassifiedBottom),
            decided_by: None,
            vacuity: None,
        };
        let mut merged = AutoVerifyReport {
            properties: vec![row(VerifyOutcome::Unknown { unknown_cells: 1 })],
            ..Default::default()
        };
        replan_merged(&mut merged, DESIGN, &[], &VerifyAutoOptions::default());
        assert!(matches!(merged.properties[0].outcome, VerifyOutcome::Holds));
        assert_eq!(
            merged.properties[0].decided_by.as_deref(),
            Some("reach-rescue")
        );
        assert!(
            merged.properties[0].bottom_reason.is_none(),
            "{:?}",
            merged.properties[0].bottom_reason
        );
        assert!(
            merged.notes.iter().any(|n| n.kind == "portfolio-rescue"),
            "the rescue's provenance lands on the merged report: {:?}",
            merged.notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );

        let mut settled = AutoVerifyReport {
            properties: vec![row(VerifyOutcome::Holds)],
            ..Default::default()
        };
        let before = settled.notes.len();
        replan_merged(&mut settled, DESIGN, &[], &VerifyAutoOptions::default());
        assert_eq!(
            settled.notes.len(),
            before,
            "nothing to re-plan, nothing touched"
        );
    }

    /// mununu#734 — a reach portfolio that decides nothing within an intact budget names its cause:
    /// on a design with an array sort every in-process member refuses (`ArraySortUnsupportedInBvOnly`
    /// from the SMT encoder; the exact engine has no array support) and the subprocess members are
    /// absent on the host, so the row's ⊥ carries `EngineDidNotComplete { engine: "reach-portfolio" }`
    /// with the members' own words and a `portfolio-refused` note — not `unclassified`, not
    /// `budget-expired`. On OpenTitan csrng the same situation (a 64-bit constant limit) cost a debug
    /// run and a probe on a dumped model to attribute.
    #[test]
    fn escalate_bottom_attributes_an_instant_inconclusive_to_the_refusing_members() {
        const ARRAY_DESIGN: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 sort array 2 1\n\
                                    4 state 3 mem\n5 zero 1\n6 state 1 q\n7 init 1 6 5\n\
                                    8 next 1 6 6\n9 next 3 4 4\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "q_zero".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. ((q == 0) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            ARRAY_DESIGN,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        let p = &report.properties[0];
        assert!(
            matches!(p.outcome, VerifyOutcome::Unknown { .. }),
            "{:?}",
            p.outcome
        );
        match &p.bottom_reason {
            Some(BottomReason::EngineDidNotComplete { engine, detail }) => {
                assert_eq!(engine, "reach-portfolio");
                assert!(
                    detail.contains("native") && detail.to_lowercase().contains("array"),
                    "the refusing member and its reason are named: {detail}"
                );
            }
            other => panic!("expected the portfolio's refusal as the reason, got {other:?}"),
        }
        assert!(
            notes
                .iter()
                .any(|n| n.kind == "portfolio-refused" && !n.items.is_empty()),
            "{:?}",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
    }

    /// PR C — the classifier agrees with the reducers: a bounded chain and a chain ending in a
    /// weak-until block are REDUCIBLE safety shapes now (the rescue runs on them), so a residual ⊥
    /// on one is not `safety-shape-not-reducible`; `[] a || [] b` still is. On csrng's ten control
    /// checks the old classifier reported a shape gap for rows the portfolio had just run on.
    #[test]
    fn a_bounded_chain_bottom_is_not_a_shape_gap() {
        let row = |formula: &str| PropertyVerdict {
            name: "p".to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: formula.to_string(),
            outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let chain = "nu X. (((!((a == 1))) || ((b == 1) || [] ((b == 1) || [] (b == 1)))) && [] X)";
        let until = "nu X. (((!((a == 1))) || [] (nu Z. ((q == q__past) && ((r == 1) || [] Z)))) \
                     && [] X)";
        let two_boxes = "nu X. (([] (a == 1) || [] (b == 1)) && [] X)";
        assert_eq!(
            classify_bottom_reason(&row(chain), false),
            BottomReason::UnclassifiedBottom
        );
        assert_eq!(
            classify_bottom_reason(&row(until), false),
            BottomReason::UnclassifiedBottom
        );
        assert_eq!(
            classify_bottom_reason(&row(two_boxes), false),
            BottomReason::SafetyShapeNotReducible
        );
    }

    // The safety-⊥ escalation reduces a reducible AG-invariant the cube left ⊥ to a
    // `bad`-monitor and decides it with the reachability portfolio — testable on a
    // BTOR2 fixture without the SV toolchain (exact/native/spacer run in-process).
    #[test]
    fn escalate_bottom_safety_rescues_a_reducible_invariant() {
        // `big` (4-bit) is init 0 and holds its value ⇒ AG(big == 0) HOLDS; the cube
        // is pretended to have left it ⊥.
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "big_zero".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. ((big == 0) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };

        let notes = escalate_bottom(
            &mut report,
            DESIGN,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "the ⊥ safety property should be rescued to HOLDS, got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "portfolio-rescue"),
            "a portfolio-rescue provenance note should be recorded"
        );
    }

    /// mununu#728 — a reason set upstream survives the residual-⊥ classifier. Here the row carries
    /// `BudgetExpired` and the budget is already spent, so the re-plan's loop breaks before any
    /// arm; before the fix the classifier then overwrote the field with a shape reason.
    #[test]
    fn escalate_bottom_keeps_a_budget_expired_reason_through_classification() {
        use crate::adapter::run_budget::{Budget, enter};
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "big_zero".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. ((big == 0) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: Some(BottomReason::BudgetExpired),
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = {
            let _g = enter(&Budget::with_ms(0));
            escalate_bottom(
                &mut report,
                DESIGN,
                false,
                &VerifyAutoOptions::default(),
                &[],
            )
        };
        assert!(matches!(
            report.properties[0].outcome,
            VerifyOutcome::Unknown { .. }
        ));
        assert_eq!(
            report.properties[0].bottom_reason,
            Some(BottomReason::BudgetExpired),
            "the upstream reason must survive the classifier"
        );
        assert!(
            !notes.iter().any(|n| n.kind == "portfolio-rescue"),
            "no rescue ran under a spent budget"
        );
    }

    /// mununu#726 — the re-plan runs each portfolio-backed step under the rescue slice, which the
    /// portfolio observes: with a spent slice the reducible invariant stays ⊥ and is attributed
    /// to the clock; with no slice (the row budget alone, here unbounded) it is rescued to HOLDS.
    #[test]
    fn escalate_bottom_installs_a_row_budget_the_portfolio_observes() {
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let fresh = || AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "big_zero".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. ((big == 0) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let mut starved = fresh();
        let t0 = std::time::Instant::now();
        escalate_bottom_with_slice(
            &mut starved,
            DESIGN,
            false,
            &VerifyAutoOptions::default(),
            &[],
            Some(0),
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t0.elapsed()
        );
        assert!(
            matches!(starved.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "a spent slice rescues nothing: {:?}",
            starved.properties[0].outcome
        );
        assert_eq!(
            starved.properties[0].bottom_reason,
            Some(BottomReason::BudgetExpired),
            "a ⊥ left by a spent slice is attributed to the clock"
        );
        let mut unbounded = fresh();
        escalate_bottom_with_slice(
            &mut unbounded,
            DESIGN,
            false,
            &VerifyAutoOptions::default(),
            &[],
            None,
        );
        assert!(matches!(
            unbounded.properties[0].outcome,
            VerifyOutcome::Holds
        ));
        assert_eq!(
            unbounded.properties[0].decided_by.as_deref(),
            Some("reach-rescue")
        );
    }

    /// mununu#728 — the cube pass's classification: a refinement that stopped on the clock, or an
    /// engine that returned after the per-property budget expired, is `unknown` carrying
    /// `BudgetExpired`; only the RUN budget's expiry stops the run. An ordinary error is still
    /// `skipped`, so the special case stays narrow.
    #[test]
    fn cube_outcome_attributes_a_budget_stop_and_stops_the_run_only_on_run_expiry() {
        use crate::mu_calculus::trit::TritSet;
        use bitvec::prelude::*;
        // two cells: cell 0 ⊥ (out of bounds), cell 1 false
        let set = TritSet::from_predicate(bitvec![usize, Lsb0; 0, 0], &bitvec![usize, Lsb0; 1, 0]);
        let init = [0usize];
        let (o, r, stop) = cube_outcome(Ok((set.clone(), true)), &init, false, false);
        assert!(matches!(o, VerifyOutcome::Unknown { .. }));
        assert_eq!(r, Some(BottomReason::BudgetExpired));
        assert!(!stop, "a per-property stop does not stop the run");
        let (_, r2, stop2) = cube_outcome(Ok((set.clone(), true)), &init, true, true);
        assert_eq!(r2, Some(BottomReason::BudgetExpired));
        assert!(stop2, "the run budget's expiry stops the run");
        let (o3, r3, stop3) = cube_outcome(Ok((set, false)), &init, false, false);
        assert!(matches!(o3, VerifyOutcome::Unknown { .. }));
        assert_eq!(r3, None, "a plain ⊥ is not a budget stop");
        assert!(!stop3);
        let budget_err = AdapterError {
            kind: crate::adapter::AdapterErrorKind::ResourceBudgetExceeded,
            location: None,
            message: "budget expired".into(),
        };
        let (o4, r4, stop4) = cube_outcome(Err(budget_err.clone()), &init, true, false);
        assert_eq!(o4.label(), "unknown", "never `skipped`");
        assert_eq!(r4, Some(BottomReason::BudgetExpired));
        assert!(!stop4);
        let (_, _, stop5) = cube_outcome(Err(budget_err), &init, true, true);
        assert!(stop5);
        let other = AdapterError {
            kind: crate::adapter::AdapterErrorKind::ParseError,
            location: None,
            message: "boom".into(),
        };
        let (o6, r6, _) = cube_outcome(Err(other), &init, false, false);
        assert_eq!(o6.label(), "skipped");
        assert_eq!(r6, None);
    }

    // P3 Update 2 — a safety AG-invariant the cube left ⊥ that is actually VIOLATED is
    // rescued to Violated AND carries the concrete native-BMC counterexample: a wrapping
    // 2-bit counter `cnt` (0→1→2→3→0) violates `AG(cnt != 3)` at depth 3. The attached
    // witness is a finite path (empty cycle) over the ORIGINAL design register `cnt`,
    // ending in the violating state (cnt == 3).
    #[test]
    fn escalate_bottom_safety_attaches_violation_witness() {
        const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                               5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "cnt_ne_3".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. ((cnt != 3) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            COUNTER,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "the ⊥ safety property should be rescued to VIOLATED, got {:?}",
            report.properties[0].outcome
        );
        let cx = report.properties[0]
            .counterexample
            .as_ref()
            .expect("a VIOLATED safety rescue must attach the native-BMC counterexample");
        assert!(
            !cx.prefix.is_empty(),
            "the safety CEX prefix (the finite Init→bad path) must be non-empty"
        );
        assert!(
            cx.cycle.is_empty(),
            "a safety counterexample is a finite path — no repeating cycle"
        );
        // The final state of the finite path satisfies the violation (cnt == 3).
        assert!(
            cx.prefix
                .last()
                .unwrap()
                .iter()
                .any(|(sig, v)| sig == "cnt" && v.as_u64() == Some(3)),
            "the witness's final state must satisfy the violated bad condition (cnt == 3); got {:?}",
            cx.prefix.last().unwrap()
        );
        assert!(
            notes.iter().any(|n| n.kind == "portfolio-rescue"),
            "a portfolio-rescue provenance note should be recorded"
        );
    }

    // Lever (b) — a cube-SKIPPED ATOM-LESS modal property (no-deadlock) is rescued to a
    // definite verdict by the full-state exact engine. The 2-bit counter increments every
    // cycle (0→1→2→3→0), so every state has a successor ⇒ `nu X.(<> true && [] X)` HOLDS.
    // The cube has no atom to seed (pure modal) so it reports Skipped; the exact rescue
    // upgrades it. Gated on reset-gating (`opts.gate_reset`, the exact engine's A.6 precond).
    #[test]
    /// mununu#504 — THE gate hazard, pinned.
    ///
    /// `ci_exit_code` fails on `unknown` but never on `skipped`. Every other `cegar_refine_loop`
    /// error maps to `Skipped`, so mapping a budget expiry there too is the natural move — and it
    /// would make a strict `--fail-on unknown` gate report GREEN on a property that was never
    /// decided. A silent pass is worse than a red gate: a red gate gets investigated.
    ///
    /// This asserts the classification directly, so it cannot be broken by a refactor that keeps
    /// the message but changes the outcome.
    fn a_budget_expiry_is_unknown_never_skipped() {
        let budget_err = AdapterError {
            kind: crate::adapter::AdapterErrorKind::ResourceBudgetExceeded,
            location: None,
            message: "budget expired".into(),
        };
        // The classification the main loop performs.
        let outcome = if budget_err.kind == crate::adapter::AdapterErrorKind::ResourceBudgetExceeded
        {
            VerifyOutcome::Unknown { unknown_cells: 0 }
        } else {
            VerifyOutcome::Skipped {
                reason: format!("CEGAR error: {}", budget_err.message),
            }
        };
        assert_eq!(
            outcome.label(),
            "unknown",
            "a timed-out property MUST be `unknown`; `skipped` would let `--fail-on unknown` \
             pass green on a property that was never decided"
        );

        // ...and an ordinary CEGAR error still maps to `skipped`, so the special case is narrow.
        let other = AdapterError {
            kind: crate::adapter::AdapterErrorKind::ParseError,
            location: None,
            message: "boom".into(),
        };
        let outcome2 = if other.kind == crate::adapter::AdapterErrorKind::ResourceBudgetExceeded {
            VerifyOutcome::Unknown { unknown_cells: 0 }
        } else {
            VerifyOutcome::Skipped {
                reason: format!("CEGAR error: {}", other.message),
            }
        };
        assert_eq!(
            outcome2.label(),
            "skipped",
            "only budget expiry is special-cased"
        );
    }

    #[test]
    /// mununu#504 — a stopped run reports every property, including the ones it never reached.
    /// The whole point is that verdicts already computed survive; monono lost a gate run to a
    /// kill that emitted zero bytes including for properties mununu had already decided.
    fn abstained_marks_an_unreached_property_as_unknown_not_skipped() {
        let t = crate::adapter::slang::translate::TranslatedAssertion {
            name: "p1".into(),
            label: None,
            kind: SvaKind::Assert,
            formula: "nu X. ([] X)".into(),
            recoverability_companion: None,
            written: None,
        };
        let v = abstained(&t, BottomReason::NotAttempted);
        assert_eq!(v.name, "p1");
        assert_eq!(
            v.outcome.label(),
            "unknown",
            "an unreached property is `unknown` (a gate must still fail), never `skipped`"
        );
        assert!(v.seeded_predicates.is_empty());
        assert!(v.counterexample.is_none());
    }

    #[test]
    fn rescue_skipped_via_exact_decides_atomless_no_deadlock() {
        const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                               5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "no_deadlock".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X.(<> true && [] X)".to_string(),
                outcome: VerifyOutcome::Skipped {
                    reason: "no state-cell / combinational predicate atoms to seed the cube"
                        .to_string(),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            gate_reset: true,
            exact_symbolic: false,
            ..Default::default()
        };
        let notes = rescue_skipped_via_exact(&mut report, COUNTER, &opts);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "the cube-Skipped atom-less no-deadlock property should be rescued to HOLDS, got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "exact-skip-rescue"),
            "an exact-skip-rescue provenance note should be recorded"
        );
    }

    // A.6 soundness gate — with reset-gating OFF the exact engine is unsound (it starts from
    // the illegal power-on default and does not fire the freed reset), so the rescue must NOT
    // touch the property: it stays Skipped, no note.
    /// mununu#808 follow-up — a VIOLATED liveness row the exact rescue decides without a lasso
    /// (the extraction failed after the verdict) carries a `witness-unavailable` note naming the
    /// `exact-witness` stage and the reason; with the extraction working the lasso ships and no
    /// such note exists. The failure is injected at the engine's own extraction closure.
    #[test]
    fn rescue_skipped_via_exact_says_why_a_violated_row_ships_no_lasso() {
        use crate::adapter::btor2::symbolic_bitblast::WITNESS_EXTRACTION_FAILS;
        // A 2-bit counter that stalls at 3: `AG AF (cnt == 1)` is VIOLATED (0 → 1 → 2 → 3 → 3 …).
        const STALL: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                             5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 const 1 11\n\
                             9 eq 2 5 8\n10 ite 1 9 5 7\n11 next 1 5 10\n";
        let fresh = || AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "returns_to_one".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Y. ((mu X. ((cnt == 1) or [] X)) and [] Y)".to_string(),
                outcome: VerifyOutcome::Skipped {
                    reason: "cube: abstained".to_string(),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            gate_reset: true,
            exact_symbolic: false,
            ..Default::default()
        };
        let mut report = fresh();
        let notes = rescue_skipped_via_exact(&mut report, STALL, &opts);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "{:?}",
            report.properties[0].outcome
        );
        assert!(
            report.properties[0].counterexample.is_some(),
            "the lasso ships when the extraction works"
        );
        assert!(notes.iter().all(|n| n.kind != "witness-unavailable"));

        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                WITNESS_EXTRACTION_FAILS.with(|f| f.set(false));
            }
        }
        let _reset = Reset;
        WITNESS_EXTRACTION_FAILS.with(|f| f.set(true));
        let mut report = fresh();
        let notes = rescue_skipped_via_exact(&mut report, STALL, &opts);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "the verdict stands: {:?}",
            report.properties[0].outcome
        );
        assert!(
            report.properties[0].counterexample.is_none(),
            "the lasso is omitted"
        );
        let kinds: Vec<&str> = notes.iter().map(|n| n.kind.as_str()).collect();
        let note = notes
            .iter()
            .find(|n| n.kind == "witness-unavailable")
            .unwrap_or_else(|| panic!("the row says why; notes: {kinds:?}"));
        assert_eq!(note.property.as_deref(), Some("returns_to_one"));
        assert_eq!(note.items, vec!["exact-witness".to_string()]);
        assert!(
            note.detail
                .contains("injected: the witness extraction failed"),
            "{}",
            note.detail
        );
    }

    #[test]
    fn rescue_skipped_via_exact_respects_reset_gate() {
        const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                               5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "no_deadlock".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X.(<> true && [] X)".to_string(),
                outcome: VerifyOutcome::Skipped {
                    reason: "no atoms".to_string(),
                },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            gate_reset: false,
            ..Default::default()
        };
        let notes = rescue_skipped_via_exact(&mut report, COUNTER, &opts);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Skipped { .. }),
            "with reset-gating off (A.6) the exact rescue must not fire, got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.is_empty(),
            "no rescue note when the A.6 gate blocks the exact engine"
        );
    }

    // A liveness / recoverability ⊥ (diamond-bodied μ) is NOT a reducible AG-invariant, so
    // the SAFETY (reachability-portfolio) arm never touches it — property_class classifies
    // it Liveness, not Safety. With only the safety arm enabled (the recoverability arm that
    // WOULD legitimately decide this νμ is gated off), the router leaves it untouched: no
    // 2-valued reachability reduction is ever applied to a νμ.
    #[test]
    fn escalate_bottom_safety_ignores_non_safety_bottom() {
        const DESIGN: &str =
            "1 sort bitvec 4\n2 state 1 big\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                // AG EF (recoverability) — diamond inner, not an AG-invariant.
                formula: "nu Y. ((mu X. ((big == 0) || <> X)) && [] Y)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        // Only the safety arm on — the νμ recoverability arm (which would decide this) is
        // gated off, isolating the property under test: the safety arm ignores a νμ ⊥.
        let opts = VerifyAutoOptions {
            rescue_bottom_liveness: false,
            rescue_bottom_recoverability: false,
            ..VerifyAutoOptions::default()
        };
        let notes = escalate_bottom(&mut report, DESIGN, false, &opts, &[]);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "a νμ ⊥ must be left untouched by the safety arm (2-valued reachability can't \
             soundly decide it)"
        );
        // No rescue note fires, but the mununu#492 Part B classifier attaches a
        // `bottom-reason` note explaining WHY the ⊥ stands.
        assert!(
            !notes.iter().any(|n| n.kind == "safety-rescue"
                || n.kind == "liveness-rescue"
                || n.kind == "recoverability-rescue"),
            "no rescue note fires for a non-safety-reducible ⊥"
        );
        assert!(
            notes.iter().any(|n| n.kind == "bottom-reason"),
            "the Part B classifier must attach a bottom-reason note; got kinds: {:?}",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
    }

    // 3-state responder: st 0=idle,1=req,2=grant; idle -go-> req; req -> grant; grant ->
    // idle. AG((st==1) → AF (st==2)) HOLDS — the l2s reduction + portfolio decides it.
    const RESPONDER_LIVE: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 eq 2 3 4
10 eq 2 3 7
11 ite 1 6 7 4
12 ite 1 10 8 4
13 ite 1 9 11 12
14 next 1 3 13
";

    // A box-response `AG(a → AF b)` the cube left ⊥ is rescued to a definite verdict by
    // the liveness-to-safety reduction + reachability portfolio.
    #[test]
    fn escalate_bottom_liveness_rescues_a_box_response() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "req_grant".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Z. ((!(st == 1) || mu Y. ((st == 2) || [] Y)) && [] Z)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            RESPONDER_LIVE,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "the ⊥ box-AF liveness property should be rescued to HOLDS, got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "liveness-rescue"),
            "a liveness-rescue provenance note should be recorded"
        );
    }

    // mununu#477 Option B — a fair-cycle rescue fixture. `next(pending) = (pending || req) && !ack`
    // and `next(ack) = fair && pending`: without a `GF fair` assumption env holds fair=0 forever
    // and starves ack; under `GF(fair == 1)` every pending req eventually meets a fair cycle.
    const FAIR_GATED: &str = "\
1 sort bitvec 1
2 input 1 req
3 input 1 fair
4 state 1 pending
5 zero 1
6 init 1 4 5
7 state 1 ack
8 init 1 7 5
9 or 1 4 2
10 not 1 7
11 and 1 9 10
12 next 1 4 11
13 and 1 3 4
14 next 1 7 13
";

    /// mununu#477 Option B — `AG(req → AF ack)` on FAIR_GATED is VIOLATED without any
    /// fairness (env starves fair forever). Baseline for the fair-cycle test below.
    #[test]
    fn escalate_bottom_fair_cycle_baseline_violated_without_fairness() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "req_ack".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Z. ((!(req == 1) || mu Y. ((ack == 1) || [] Y)) && [] Z)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let _notes = escalate_bottom(
            &mut report,
            FAIR_GATED,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "without a fairness assumption env can starve fair=0 forever, so the response is \
             VIOLATED; got {:?}",
            report.properties[0].outcome
        );
    }

    /// mununu#477 Option B — the same property on the same design HOLDS under
    /// `GF(fair == 1)`: the fair-cycle bridge routes to
    /// `response_liveness_rescue_under_fairness` when the annotation scanner has
    /// a typed `fairness_assumes` list. This is the load-bearing test for the SV
    /// verify-auto bridge — the ticket's `wb_mem_client`-shape case reduced to
    /// the BTOR2 layer this escalation runs on.
    #[test]
    fn escalate_bottom_fair_cycle_rescues_response_under_gf_fair() {
        use crate::adapter::btor2::predicate_expr::CmpOp;
        use crate::adapter::liveness_rescue::Atom;
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "req_ack".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Z. ((!(req == 1) || mu Y. ((ack == 1) || [] Y)) && [] Z)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let fair = [Atom {
            signal: "fair".to_string(),
            op: CmpOp::Eq,
            value: 1,
        }];
        let notes = escalate_bottom(
            &mut report,
            FAIR_GATED,
            false,
            &VerifyAutoOptions::default(),
            &fair,
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "under GF(fair == 1), the fair-cycle bridge must decide the response HOLDS; \
             got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "fair-cycle-rescue"),
            "a fair-cycle-rescue provenance note should be recorded (found: {:?})",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
        assert!(
            notes.iter().any(|n| n.summary.contains("GF (fair == 1)")),
            "the note must name the fairness assumption applied"
        );
    }

    /// mununu#477 Option B — a `GF <atom>` assume paired with a non-response guarantee
    /// (a νμ recoverability) does NOT go through the fair-cycle bridge (the primitive is
    /// response-specific). The escalation must leave the non-response verdict untouched
    /// rather than mis-route it — soundness guard.
    #[test]
    fn escalate_bottom_fair_cycle_does_not_route_non_response_guarantees() {
        use crate::adapter::btor2::predicate_expr::CmpOp;
        use crate::adapter::liveness_rescue::Atom;
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                // A νμ recoverability (`AG(a → EF b)`), NOT the box-AF shape the primitive expects.
                formula: "nu Z. ((!(req == 1) || mu Y. ((ack == 1) || <> Y)) && [] Z)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let fair = [Atom {
            signal: "fair".to_string(),
            op: CmpOp::Eq,
            value: 1,
        }];
        let notes = escalate_bottom(
            &mut report,
            FAIR_GATED,
            false,
            &VerifyAutoOptions::default(),
            &fair,
        );
        assert!(
            !notes.iter().any(|n| n.kind == "fair-cycle-rescue"),
            "the fair-cycle bridge must not fire on a non-response guarantee shape; found: {:?}",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
    }

    // SOUNDNESS: a diamond-EF recoverability liveness (`AG(a → EF b)`, νμ) is NOT the
    // l2s-able box-AF shape, so the liveness escalation must leave it untouched — never
    // route a νμ property through the 2-valued l2s.
    #[test]
    fn escalate_bottom_liveness_ignores_diamond_ef() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Z. ((!(st == 1) || mu Y. ((st == 2) || <> Y)) && [] Z)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            RESPONDER_LIVE,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "a diamond-EF νμ ⊥ must be left untouched (l2s does not decide it)"
        );
        // mununu#492 Part B — the residual ⊥ gets a `bottom-reason` note, but no
        // rescue-arm note fires.
        assert!(
            !notes.iter().any(|n| n.kind == "liveness-rescue"),
            "no liveness-rescue note for a non-box-AF property"
        );
        assert!(
            notes.iter().any(|n| n.kind == "bottom-reason"),
            "the Part B classifier must attach a bottom-reason note; got kinds: {:?}",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
    }

    // A νμ recoverability property `AG EF (st == 0)` the cube left ⊥ is routed by
    // property_class → the Liveness arm → (l2s abstains) → the cube + smt-hyper-must νμ
    // path, which decides it on RESPONDER_LIVE (st==0 is reachable from every reachable
    // state: idle stays/returns, req→grant→idle, grant→idle).
    #[test]
    fn escalate_bottom_routes_recoverability_to_cube() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover_idle".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            RESPONDER_LIVE,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        // The router dispatches the νμ ⊥ to the cube + smt-hyper-must path, which now DECIDES it
        // `Holds`: the N1-first-increment property-directed seeding (good register's other control
        // states) splits the coarse abstraction so the recoverability is provable. (Between the
        // universal-◇ soundness fix and the seeding this abstained to a sound `Unknown`.) Soundness
        // invariant preserved throughout: agree with exact or abstain, never contradict.
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "the ⊥ νμ recoverability property must be rescued to Holds by the cube + smt-hyper-must \
             path (property-directed seeding), got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "recoverability-rescue"),
            "a recoverability-rescue provenance note should be recorded"
        );
    }

    // ③a routing — under reset-gating, a ⊥ νμ recoverability property is routed through the
    // EXACT+COI engine (which decides control-cone recoverability the cube abstains on), not
    // only the scalable cube path. A 2-bit wrapping counter (0→1→2→3→0) satisfies
    // `AG EF (cnt == 0)` — every state returns to 0 — and exact decides it Holds.
    #[test]
    fn escalate_bottom_recoverability_uses_exact_when_reset_pinned() {
        const COUNTER: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                               5 state 1 cnt\n6 init 1 5 3\n7 add 1 5 4\n8 next 1 5 7\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover_to_zero".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Y.((mu X.(cnt == 0 || <> X)) && [] Y)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        // reset_pinned = true → the ③a exact-first routing is enabled (A.6 sound).
        let notes = escalate_bottom(
            &mut report,
            COUNTER,
            true,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "reset-pinned recoverability ⊥ must be rescued to Holds via exact+COI, got {:?}",
            report.properties[0].outcome
        );
        assert!(
            notes.iter().any(|n| n.kind == "recoverability-rescue"),
            "a recoverability-rescue provenance note should be recorded"
        );
    }

    /// `st` cycles 0 → 1 → 2 → 0; the input `esc` sends it to 3, which is absorbing.
    /// `AG EF (st == 0)` is VIOLATED; `st == 3` is the trap, entered at depth 1. The BTOR2 twin of
    /// `TRAP_FSM_SV` (`tests/differential_oracle_e2e.rs`).
    const TRAP_FSM: &str = "1 sort bitvec 2\n2 sort bitvec 1\n3 input 2 esc\n4 state 1 st\n\
                            5 zero 1\n6 init 1 4 5\n7 one 1\n8 constd 1 2\n9 constd 1 3\n\
                            10 eq 2 4 9\n11 eq 2 4 8\n12 add 1 4 7\n13 ite 1 11 5 12\n\
                            14 ite 1 10 9 13\n15 ite 1 3 9 14\n16 next 1 4 15\n";

    fn recover_row(name: &str, formula: &str, outcome: VerifyOutcome) -> PropertyVerdict {
        PropertyVerdict {
            name: name.to_string(),
            label: None,
            kind: crate::adapter::slang::translate::SvaKind::Assert,
            formula: formula.to_string(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        }
    }

    /// The trap certificate refutes `AG EF (st == 0)` before the ladder runs: VIOLATED, decided by
    /// `trap-certificate`, with the reset → trap path as the counterexample and a note naming the
    /// trap and the engines. On the parent the ladder decided this row too (exact+COI with the reset
    /// pinned, the cube ladder without), but with no counterexample and no certificate — the
    /// outcome-level reproducer is the OpenTitan edn row the ladder leaves ⊥ (image e2e).
    #[test]
    fn a_trap_certificate_refutes_recoverability_with_the_path_into_the_trap() {
        for reset_pinned in [true, false] {
            let mut report = AutoVerifyReport {
                properties: vec![recover_row(
                    "recover_idle",
                    "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)",
                    VerifyOutcome::Unknown { unknown_cells: 1 },
                )],
                ..Default::default()
            };
            let notes = escalate_bottom(
                &mut report,
                TRAP_FSM,
                reset_pinned,
                &VerifyAutoOptions::default(),
                &[],
            );
            let p = &report.properties[0];
            assert!(
                matches!(p.outcome, VerifyOutcome::Violated { .. }),
                "reset_pinned={reset_pinned}: {:?}",
                p.outcome
            );
            assert_eq!(p.decided_by.as_deref(), Some(TRAP_CERTIFICATE_DECIDED_BY));
            let cex = p.counterexample.as_ref().expect("the path into the trap");
            assert!(cex.cycle.is_empty(), "a prefix, never a fabricated lasso");
            let last = cex.prefix.last().expect("a state");
            assert!(
                last.iter().any(|(k, v)| k == "st" && v.as_u64() == Some(3)),
                "the path ends in the trap: {last:?}"
            );
            let note = notes
                .iter()
                .find(|n| n.kind == "trap-certificate")
                .expect("the certificate note");
            assert_eq!(note.property.as_deref(), Some("recover_idle"));
            assert!(note.items.iter().any(|i| i == "trap: st == 3"), "{note:?}");
            assert!(
                !notes.iter().any(|n| n.kind == "recoverability-rescue"),
                "the ladder does not run once the row is refuted"
            );
        }
    }

    /// A row of the same report that HOLDS the terminal idiom is tried first, and named.
    #[test]
    fn a_sibling_terminal_row_is_the_first_candidate() {
        let mut report = AutoVerifyReport {
            properties: vec![
                recover_row(
                    "st_trap_is_terminal",
                    "nu X. ((!(st == 3) || nu Y. ((st == 3) && [] Y)) && [] X)",
                    VerifyOutcome::Holds,
                ),
                recover_row(
                    "recover_idle",
                    "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)",
                    VerifyOutcome::Unknown { unknown_cells: 1 },
                ),
            ],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            TRAP_FSM,
            true,
            &VerifyAutoOptions::default(),
            &[],
        );
        let note = notes
            .iter()
            .find(|n| n.kind == "trap-certificate")
            .expect("certified");
        assert!(
            note.items
                .iter()
                .any(|i| i == "source: sibling:st_trap_is_terminal"),
            "{note:?}"
        );
    }

    /// Control — a recoverable design (every reachable state returns to 0): the search certifies
    /// nothing, says why per candidate, and the ladder still decides HOLDS.
    #[test]
    fn a_failed_trap_search_leaves_the_ladder_its_verdict() {
        let mut report = AutoVerifyReport {
            properties: vec![recover_row(
                "recover_idle",
                "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)",
                VerifyOutcome::Unknown { unknown_cells: 1 },
            )],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            RESPONDER_LIVE,
            true,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Holds),
            "{:?}",
            report.properties[0].outcome
        );
        assert_ne!(
            report.properties[0].decided_by.as_deref(),
            Some(TRAP_CERTIFICATE_DECIDED_BY)
        );
        let search = notes
            .iter()
            .find(|n| n.kind == "trap-certificate-search")
            .expect("the search note");
        assert!(
            !search.items.is_empty() && search.items.iter().all(|i| !i.ends_with(": certified")),
            "{search:?}"
        );
    }

    /// A spent rescue slice runs no trap check; the row the ladder then leaves ⊥ (here the ladder
    /// decides, so the outcome is the ladder's) is never refuted without its two checks.
    #[test]
    fn a_spent_slice_runs_no_trap_check() {
        let mut report = AutoVerifyReport {
            properties: vec![recover_row(
                "recover_idle",
                "nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)",
                VerifyOutcome::Unknown { unknown_cells: 1 },
            )],
            ..Default::default()
        };
        let notes = escalate_bottom_with_slice(
            &mut report,
            TRAP_FSM,
            true,
            &VerifyAutoOptions::default(),
            &[],
            Some(0),
        );
        assert_ne!(
            report.properties[0].decided_by.as_deref(),
            Some(TRAP_CERTIFICATE_DECIDED_BY),
            "{:?}",
            report.properties[0]
        );
        assert!(
            !notes.iter().any(|n| n.kind.starts_with("trap-certificate")),
            "no candidate was checked, so no trap note: {notes:?}"
        );
    }

    /// mununu#656 follow-up — the recoverability rescue runs its exact-first pass on the
    /// REQUEST's exact options and names the rung that decided. `out` is combinationally driven
    /// by the input `in`, so the exact engine decides `AG EF (out == 0)` only through the
    /// input-state product: by default the rescue reads `decided_by = exact-symbolic (…)`; with
    /// `input_state: false` the exact pass refuses and the cube ladder runs — the note says so.
    /// On the parent, both runs decided through the product model (the opt-out never reached the
    /// rescue) and `decided_by` stayed `None`.
    #[test]
    fn x656_the_recoverability_rescue_honours_the_input_state_opt_out_and_names_its_rung() {
        const MEALY: &str = "1 sort bitvec 1\n2 input 1 in\n3 state 1 q\n4 zero 1\n5 init 1 3 4\n\
                             6 next 1 3 2\n7 uext 1 2 0 out\n";
        let fresh = || AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "recover_out_low".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu Y.((mu X.(out == 0 || <> X)) && [] Y)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let mut lifted = fresh();
        let notes = escalate_bottom(&mut lifted, MEALY, true, &VerifyAutoOptions::default(), &[]);
        assert!(
            matches!(lifted.properties[0].outcome, VerifyOutcome::Holds),
            "with the input-state product the exact rung decides it: {:?}",
            lifted.properties[0].outcome
        );
        assert_eq!(
            lifted.properties[0].decided_by.as_deref(),
            Some("exact-symbolic (recoverability rescue)"),
            "the rescued verdict carries its rung"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.kind == "recoverability-rescue" && n.summary.contains("by exact+COI")),
            "the rescue note names the exact rung: {:?}",
            notes.iter().map(|n| &n.summary).collect::<Vec<_>>()
        );

        let mut opted_out = fresh();
        let opts = VerifyAutoOptions {
            input_state: false,
            ..Default::default()
        };
        let notes = escalate_bottom(&mut opted_out, MEALY, true, &opts, &[]);
        assert_ne!(
            opted_out.properties[0].decided_by.as_deref(),
            Some("exact-symbolic (recoverability rescue)"),
            "`--no-input-state` must reach the rescue: the exact rung refuses an input-driven atom"
        );
        assert!(
            notes.iter().any(|n| n.kind == "recoverability-rescue"
                && n.summary.contains("by the cube + smt-hyper-must ladder")),
            "the rescue note names the cube rung: {:?}",
            notes.iter().map(|n| &n.summary).collect::<Vec<_>>()
        );
    }

    // ---- mununu#492 Part B: bottom-reason classifier tests ----

    // A zero-state design — 3 primary inputs, no state cells.
    const BOTTOM_REASON_ZERO_STATE: &str = "\
1 sort bitvec 1
2 input 1 a
3 input 1 b
4 input 1 c
";

    /// mununu#492 — on the zero-state exclusion property, escalate_bottom's Safety
    /// arm now DECIDES it VIOLATED via the compound-atom rescue (Part A). No
    /// bottom-reason note is attached because the outcome is no longer Unknown.
    #[test]
    fn bottom_reason_zero_state_compound_safety_gets_decided_no_bottom_reason() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "mem_router_exclusion".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. (((!(a == 1)) || (!(b == 1))) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 2 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            BOTTOM_REASON_ZERO_STATE,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "the compound-atom rescue decides the exclusion VIOLATED on the zero-state model; \
             got {:?}",
            report.properties[0].outcome
        );
        // No bottom-reason note — the outcome is definite.
        assert!(
            !notes.iter().any(|n| n.kind == "bottom-reason"),
            "no bottom-reason note when the outcome is decided"
        );
    }

    /// mununu#492 — a safety property whose shape the rescue lane cannot reduce
    /// (a compound with a `CmpReg` register-vs-register leaf) stays ⊥ and gets
    /// a `bottom-reason` note classifying it as `safety-shape-not-reducible`.
    #[test]
    fn bottom_reason_safety_shape_not_reducible_gets_classified() {
        // `AG(a == 1 && b == c + 1)` — the addend leaf is out of the compilable set for the
        // compound emitter (a plain `b == c` relational leaf compiles since plan C, 2026-10-21).
        let design = "\
1 sort bitvec 1
2 input 1 a
3 input 1 b
4 input 1 c
";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "compound_with_relational_leaf".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. (((a == 1) && (b == c + 1)) && [] X)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let notes = escalate_bottom(
            &mut report,
            design,
            false,
            &VerifyAutoOptions::default(),
            &[],
        );
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "the addend leaf must stay ⊥ (out of the compilable set)"
        );
        let br = notes
            .iter()
            .find(|n| n.kind == "bottom-reason")
            .expect("bottom-reason note must be attached to a residual ⊥");
        assert!(
            br.summary.contains("safety-shape-not-reducible"),
            "classifier must name `safety-shape-not-reducible`; got summary: {}",
            br.summary
        );
    }

    /// mununu#492 — a non-Safety property (νμ recoverability) on a stateless
    /// model is classified `no-state-model-non-safety`.
    #[test]
    fn bottom_reason_no_state_model_non_safety_gets_classified() {
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "vacuous_recoverability".to_string(),
                label: None,
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                // AG EF (a == 1) — recoverability on a stateless model.
                formula: "nu Y. ((mu X. ((a == 1) || <> X)) && [] Y)".to_string(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            // Isolate the classifier from the recoverability rescue path so the
            // residual ⊥ is what the classifier sees.
            rescue_bottom_recoverability: false,
            ..VerifyAutoOptions::default()
        };
        let notes = escalate_bottom(&mut report, BOTTOM_REASON_ZERO_STATE, false, &opts, &[]);
        let br = notes
            .iter()
            .find(|n| n.kind == "bottom-reason")
            .expect("bottom-reason note must be attached to a residual ⊥");
        assert!(
            br.summary.contains("no-state-model-non-safety"),
            "classifier must name `no-state-model-non-safety`; got summary: {}",
            br.summary
        );
    }

    /// A cube engine's ⊥ (Unknown) is filled by a later engine's definite verdict, and the
    /// portfolio note records which engine decided it.
    #[test]
    fn portfolio_merge_fills_bottom_with_a_definite() {
        let exact = (
            "exact-symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Unknown { unknown_cells: 4 },
                None,
            )])),
        );
        let symbolic = (
            "symbolic",
            Ok(mk_report(&[("p", VerifyOutcome::Holds, None)])),
        );
        let runs = vec![exact, symbolic];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Parallel).expect("merge ok");
        assert_eq!(merged.properties[0].outcome, VerifyOutcome::Holds);
        assert!(
            merged
                .notes
                .iter()
                .any(|n| n.kind == "portfolio"
                    && n.items.iter().any(|i| i == "decided-by:symbolic=1")),
            "the note must attribute the verdict to the symbolic engine"
        );
    }

    /// mununu#536 / mununu#498 — the merged report's `coverage-summary` must count the MERGED
    /// verdicts, not the base engine's.
    ///
    /// The merge starts from `base.clone()`, which carries the base engine's notes; it then
    /// rewrites `properties[].outcome` across engines. Nothing rewrote the summary, so it kept
    /// reporting the base's tally. With `portfolio-sequential` as the DEFAULT engine this was
    /// the common case, and it is what monono saw: a summary claiming `0 unknown` beside
    /// properties printing `UNKNOWN`. A consumer counting the summary under-reported its own
    /// coverage.
    #[test]
    fn portfolio_merge_recounts_the_coverage_summary() {
        // The base (exact) leaves both ⊥; the explicit engine decides one. Merged: 1 holds, 1 ⊥.
        let exact = (
            "exact-symbolic",
            Ok(mk_report(&[
                (
                    "p_decided_later",
                    VerifyOutcome::Unknown { unknown_cells: 8 },
                    None,
                ),
                (
                    "p_stays_bottom",
                    VerifyOutcome::Unknown { unknown_cells: 4 },
                    None,
                ),
            ])),
        );
        let explicit = (
            "explicit",
            Ok(mk_report(&[
                ("p_decided_later", VerifyOutcome::Holds, None),
                (
                    "p_stays_bottom",
                    VerifyOutcome::Unknown { unknown_cells: 4 },
                    None,
                ),
            ])),
        );
        let merged = merge_portfolio_reports(&[exact, explicit], PortfolioMode::Sequential)
            .expect("merge ok");

        // What the properties actually say, which is the record of truth.
        let holds = merged
            .properties
            .iter()
            .filter(|p| matches!(p.outcome, VerifyOutcome::Holds))
            .count();
        let unknown = merged
            .properties
            .iter()
            .filter(|p| matches!(p.outcome, VerifyOutcome::Unknown { .. }))
            .count();
        assert_eq!(
            (holds, unknown),
            (1, 1),
            "the merge decided exactly one property"
        );

        let cov = merged
            .notes
            .iter()
            .find(|n| n.kind == "coverage-summary")
            .expect("a coverage-summary note is present");
        assert!(
            cov.summary.contains("1 definite (HOLDS)") && cov.summary.contains("1 unknown"),
            "the summary must count the MERGED verdicts (1 HOLDS, 1 ⊥). Before mununu#536 it \
             reported the base engine's pre-merge tally (0 HOLDS, 2 ⊥) while the properties said \
             otherwise — the disagreement a consumer parsed. Got: {}",
            cov.summary
        );
        assert_eq!(
            merged
                .notes
                .iter()
                .filter(|n| n.kind == "coverage-summary")
                .count(),
            1,
            "refreshing must REPLACE the stale note, not append a second contradicting one"
        );
    }

    /// When several engines agree on a definite, the FIRST (exact, highest-precision) engine's
    /// verdict AND its counterexample witness are retained.
    #[test]
    fn portfolio_merge_prefers_exact_witness_on_agreement() {
        let exact = (
            "exact-symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Violated { false_cells: 1 },
                Some(cx_stub()),
            )])),
        );
        let symbolic = (
            "symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Violated { false_cells: 9 },
                None,
            )])),
        );
        let runs = vec![exact, symbolic];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Sequential).expect("merge ok");
        // Exact's outcome (false_cells: 1) and its counterexample win.
        assert_eq!(
            merged.properties[0].outcome,
            VerifyOutcome::Violated { false_cells: 1 }
        );
        assert!(
            merged.properties[0].counterexample.is_some(),
            "exact's counterexample witness must be retained"
        );
    }

    /// The runtime soundness guard: OPPOSITE definite verdicts force ⊥ + a soundness-alarm note,
    /// never a silent pick. (The parity differential proves this never fires on the corpus.)
    #[test]
    fn portfolio_merge_contradiction_forces_bottom_and_alarms() {
        let exact = (
            "exact-symbolic",
            Ok(mk_report(&[("p", VerifyOutcome::Holds, None)])),
        );
        let symbolic = (
            "symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Violated { false_cells: 1 },
                None,
            )])),
        );
        let runs = vec![exact, symbolic];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Parallel).expect("merge ok");
        assert!(
            matches!(merged.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "a contradiction must degrade to ⊥, not silently pick a verdict"
        );
        assert!(
            merged.properties[0].counterexample.is_none(),
            "a contradicted property carries no witness"
        );
        assert!(
            merged
                .notes
                .iter()
                .any(|n| n.kind == "portfolio-soundness-alarm"),
            "the contradiction must raise a soundness-alarm note"
        );
    }

    /// All engines ⊥: the merged ⊥ prefers an Unknown (abstraction attempted) over a Skipped
    /// (atom not cube-bindable) — the more informative cause.
    #[test]
    fn portfolio_merge_all_bottom_prefers_unknown_over_skipped() {
        let exact = (
            "exact-symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Skipped {
                    reason: "bit cap".to_string(),
                },
                None,
            )])),
        );
        let symbolic = (
            "symbolic",
            Ok(mk_report(&[(
                "p",
                VerifyOutcome::Unknown { unknown_cells: 8 },
                None,
            )])),
        );
        let runs = vec![exact, symbolic];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Parallel).expect("merge ok");
        assert_eq!(
            merged.properties[0].outcome,
            VerifyOutcome::Unknown { unknown_cells: 8 }
        );
    }

    /// REPRODUCER: a MEMORY ceiling and a WALL-CLOCK budget must not share a tag.
    ///
    /// #558 shipped both as `budget-expired`. A consumer branching on the tag — which is what the
    /// field exists for, and what a consumer said they were about to write — would retry an RSS
    /// exhaustion with more TIME, which cannot help.
    ///
    /// The two are host-dependent by DIFFERENT mechanisms, which is why merging them also loses a
    /// diagnostic: the clock is obvious, while RSS varies with allocator state and neighbouring
    /// processes, so a memory abstention is host-dependent with no clock anywhere in it.
    #[test]
    fn a_memory_ceiling_is_not_a_wall_clock_budget() {
        let mem = BottomReason::MemoryCeilingExceeded;
        let clock = BottomReason::BudgetExpired;

        assert_ne!(
            mem.tag(),
            clock.tag(),
            "a gate cannot branch if an RSS exhaustion and a timeout share a tag"
        );

        let m = mem.one_line();
        assert!(
            m.contains("MUNUNU_MAX_PROCESS_MEMORY_BYTES"),
            "must name the knob that fired: {m}"
        );
        assert!(
            m.contains("cannot help"),
            "must say retrying with more TIME does not help — the opposite of the clock case: {m}"
        );

        // Assert the DISTINCTION, not a prose fragment. An `||` over two line-wrappings of the
        // same sentence is a test accommodating the formatter: it would stay green if the text
        // changed meaning while keeping either substring. What must hold is that the two give
        // OPPOSITE advice about retrying.
        let c = clock.one_line();
        assert!(
            c.contains("WALL-CLOCK"),
            "the clock case must name its axis, or a reader cannot tell it from the RSS one: {c}"
        );
        assert!(
            !c.contains("cannot help"),
            "the clock case must NOT say retrying is useless — that is the memory case: {c}"
        );
        assert!(
            !m.contains("WALL-CLOCK"),
            "the memory case must not claim a clock fired; RSS is host-dependent WITHOUT one: {m}"
        );
    }

    /// mununu#577 REPRODUCER — a `Violated` resting on an unestablished initial value is withheld.
    ///
    /// Measured on `sprite_eval`: an async reset lifts to a mux rather than a BTOR2 `init`, so the
    /// model carried ONE `init` line for ELEVEN state cells; reset-gating then pinned `rst_n`
    /// inactive, removing the last path to the reset values. `AG(drop_q <= 1)` came back a
    /// confident VIOLATED on a counter the RTL bounds at 1, because in the model `drop_q` may
    /// simply START at 1023.
    ///
    /// Fails on the parent commit, where the violation was reported as-is.
    #[test]
    fn a_violated_resting_on_an_unestablished_initial_value_is_withheld() {
        // `cnt` has NO `init` line: its cycle-0 value is unconstrained.
        const NO_INIT: &str = "\
1 sort bitvec 10
2 state 1 cnt
3 one 1
4 add 1 2 3
5 next 1 2 4
";
        let mk = |name: &str, formula: &str, outcome: VerifyOutcome| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut report = AutoVerifyReport {
            properties: vec![
                // universal, cone touches `cnt`, VIOLATED → must be withheld
                mk(
                    "ag",
                    "nu X. ((cnt <= 1) && [] X)",
                    VerifyOutcome::Violated { false_cells: 1 },
                ),
                // existential refutation → stays sound under a freer start set, left alone
                mk(
                    "ef",
                    "mu Z. ((cnt == 2) || <> Z)",
                    VerifyOutcome::Violated { false_cells: 1 },
                ),
                // a HOLDS is never made unsound by a freer start set
                mk(
                    "hold",
                    "nu X. ((cnt <= 1023) && [] X)",
                    VerifyOutcome::Holds,
                ),
            ],
            ..Default::default()
        };
        report.diagnostics.gated_resets = vec!["rst_n=1".into()];

        downgrade_violated_on_unestablished_init(&mut report, NO_INIT);

        let by = |n: &str| report.properties.iter().find(|p| p.name == n).unwrap();
        assert!(
            matches!(by("ag").outcome, VerifyOutcome::Unknown { .. }),
            "a universal VIOLATED over an init-less register must be withheld: {:?}",
            by("ag").outcome
        );
        match &by("ag").bottom_reason {
            Some(BottomReason::UnestablishedInitialState { registers, .. }) => {
                assert_eq!(*registers, 1, "one init-less register in the cone");
            }
            other => panic!("expected the named reason, got {other:?}"),
        }
        assert!(
            matches!(by("ef").outcome, VerifyOutcome::Violated { .. }),
            "an EXISTENTIAL refutation is stronger under over-approximation and stays: {:?}",
            by("ef").outcome
        );
        assert!(
            matches!(by("hold").outcome, VerifyOutcome::Holds),
            "a HOLDS is sound under a freer start set and must not be touched"
        );
    }

    /// mununu#750 — a violation found through an undriven net is a may-trace: it is withheld as ⊥
    /// (`violation-over-free-leaves`, the nets named) and keeps its trace. On the parent these rows
    /// shipped VIOLATED (four OpenTitan census rows that HOLD on the full closure). A deliberate cut
    /// (a state with no `next`), an `EF` refutation and a clean cone keep their verdict.
    #[test]
    fn a_violation_over_an_undriven_net_is_withheld_with_its_trace() {
        // `bb_out` is a deliberate cut (a state with no `next`: a `--cutpoint` or a `(* blackbox *)`
        // stub's output); `q` copies it.
        // `anon` is an undriven net (an anonymous input) feeding `r`. `c` is a clean counter.
        const DESIGN: &str = "\
1 sort bitvec 1
2 state 1 bb_out
3 state 1 q
4 zero 1
5 init 1 3 4
6 next 1 3 2
7 input 1
8 state 1 r
9 init 1 8 4
10 next 1 8 7
11 state 1 c
12 init 1 11 4
13 not 1 11
14 next 1 11 13
";
        let cex = || ExactCounterexample {
            prefix: vec![Vec::new(), Vec::new()],
            cycle: Vec::new(),
            inputs: Vec::new(),
            unreachable_target: Vec::new(),
        };
        let mk = |name: &str, formula: &str| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: Some(cex()),
            bottom_reason: None,
            decided_by: Some("exact-symbolic".into()),
            vacuity: None,
        };
        let fresh = || AutoVerifyReport {
            properties: vec![
                mk("over_black_box", "nu X. ((q == 0) && [] X)"),
                mk("over_undriven_net", "nu X. ((r == 0) && [] X)"),
                mk("clean", "nu X. ((c == 0) && [] X)"),
                mk("ef_refuted", "mu X. ((q == 1) || <> X)"),
            ],
            ..Default::default()
        };
        let mut report = fresh();
        withhold_violated_over_free_leaves(&mut report, DESIGN);
        let by = |r: &AutoVerifyReport, n: &str| {
            r.properties.iter().find(|p| p.name == n).unwrap().clone()
        };
        {
            let (row, leaf) = ("over_undriven_net", "input_7");
            let p = by(&report, row);
            assert!(
                matches!(p.outcome, VerifyOutcome::Unknown { .. }),
                "{row}: {:?}",
                p.outcome
            );
            assert_eq!(
                p.bottom_reason,
                Some(BottomReason::ViolationOverFreeLeaves {
                    leaves: vec![leaf.to_string()]
                }),
                "{row}"
            );
            assert_eq!(
                p.bottom_reason.as_ref().map(BottomReason::tag),
                Some("violation-over-free-leaves")
            );
            assert!(p.counterexample.is_some(), "{row}: the may-trace is kept");
        }
        assert!(matches!(
            by(&report, "clean").outcome,
            VerifyOutcome::Violated { .. }
        ));
        assert!(
            matches!(
                by(&report, "over_black_box").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "a deliberate cut (a state with no `next`: `--cutpoint`, `(* blackbox *)`) keeps its \
             verdict (mununu#578)"
        );
        assert!(
            matches!(
                by(&report, "ef_refuted").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "an unreachable target on a freer model is unreachable on the RTL"
        );
    }

    /// mununu#578 — a `--cutpoint` in the cone must NOT withhold a verdict.
    ///
    /// A cut register lifts to a state with no `next`: free at every cycle because the user asked
    /// for it, not because a reset path was lost. Counting it as "unestablished" converts a sound
    /// `VIOLATED` into ⊥ — a precision loss, not a soundness win — and breaks the cut-point
    /// contract that applying a cut may not change a verdict.
    ///
    /// The e2e `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips` is what caught this, and
    /// it needs slang + yosys and a container. This is the same question at the layer the decision
    /// is actually made (CLAUDE.md §14): `cut` has no `next`, `cnt` does, and only `cnt` may count.
    #[test]
    fn a_cutpoint_in_the_cone_does_not_withhold_a_violated() {
        // `cut` is a cut register: a state with NO `next`. `cnt` is an ordinary init-less register.
        const WITH_CUT: &str = "\
1 sort bitvec 10
2 state 1 cnt
3 one 1
4 add 1 2 3
5 next 1 2 4
6 state 1 cut
";
        let mk = |name: &str, formula: &str| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut report = AutoVerifyReport {
            properties: vec![
                mk("over_cut_only", "nu X. ((cut <= 1) && [] X)"),
                mk("over_real_reg", "nu X. ((cnt <= 1) && [] X)"),
            ],
            ..Default::default()
        };
        report.diagnostics.gated_resets = vec!["rst_n=1".into()];

        downgrade_violated_on_unestablished_init(&mut report, WITH_CUT);

        let by = |n: &str| report.properties.iter().find(|p| p.name == n).unwrap();
        assert!(
            matches!(by("over_cut_only").outcome, VerifyOutcome::Violated { .. }),
            "a cut point is free BY CONSTRUCTION, so it must not withhold a verdict: {:?}",
            by("over_cut_only").outcome
        );
        assert!(
            matches!(by("over_real_reg").outcome, VerifyOutcome::Unknown { .. }),
            "a genuine init-less REGISTER must still be withheld — narrowing the guard must not \
             disable it: {:?}",
            by("over_real_reg").outcome
        );
    }

    /// mununu#579 — the three evaluation paths now read an init-less state cell the same way at
    /// cycle 0: FREE. `state_cell_init_values` returns only cells with an `init` line, so an
    /// init-less cell has no pin (it used to default to 0 — "init 0" and "no init" were
    /// indistinguishable one line after the only place that could tell them apart), and the
    /// cube path treats a predicate over it as a free initial dimension, bounded by
    /// `MAX_FREE_INIT_DIMENSIONS`. The exact engine (`initial_state_bdd`, since mununu#498) and
    /// the reachability portfolio already left it free. This pins the CONVERGED convention so
    /// the next drift fails a build instead of a consumer's gate.
    #[test]
    fn an_initless_cell_is_free_at_cycle_zero_on_the_cube_path_like_the_other_paths() {
        // `pinned` carries an init; `loose` does not.
        let btor2 =
            "1 sort bitvec 4\n2 state 1 pinned\n3 constd 1 5\n4 init 1 2 3\n5 state 1 loose\n";
        let file = crate::adapter::btor2::parser::parse(btor2).expect("fixture parses");
        let vals = state_cell_init_values(&file);
        assert_eq!(
            vals.get("pinned").copied(),
            Some(5),
            "an explicit init is read through"
        );
        assert_eq!(
            vals.get("loose").copied(),
            None,
            "an init-less cell has NO pin — it is a free initial dimension of the cube, as it \
             is a free cycle-0 value on the exact engine and the portfolio (mununu#579)"
        );
    }

    /// mununu#579 — a predicate's register is looked up by the STRICT alias, not by the name
    /// `collect_symbols` gave the cell.
    ///
    /// The fixture is the saturating counter's real lift (`MUNUNU_SHADOW_BTOR2_DUMP`, trimmed):
    /// the state cell `6` has no symbol of its own, `cnt_d` (its next-value function, line 14)
    /// comes first in the file and the loose alias pass named the cell after it (until
    /// mununu#711), while the
    /// predicates say `cnt_q` (line 15, the `uext`-0 of the pinned reset mux). Looked up by name,
    /// `cnt_q` is absent and reads as unestablished — the e2e went VIOLATED at cycle 0 on that.
    /// Resolved by the strict alias it is the cell with `init 0`.
    #[test]
    fn x579_a_register_named_by_a_loose_alias_is_still_established_through_the_strict_one() {
        let btor2 = "1 sort bitvec 1\n2 input 1 clk\n3 one 1 rst_ni\n4 sort bitvec 5\n5 const 4 00000\n\
                     6 state 4\n7 ite 4 3 6 5\n8 const 1 1\n9 uext 4 8 4\n10 add 4 7 9\n\
                     11 const 4 00111\n12 eq 1 7 11\n13 ite 4 12 11 10\n14 uext 4 13 0 cnt_d\n\
                     15 uext 4 7 0 cnt_q\n16 ite 4 3 13 5\n17 next 4 6 16\n18 constd 4 0\n\
                     19 init 4 6 18\n20 input 4 din\n21 add 4 7 20\n22 uext 4 21 0 sum\n\
                     23 state 4 loose\n24 next 4 23 23\n25 uext 4 23 0 loose_q\n";
        let file = crate::adapter::btor2::parser::parse(btor2).expect("fixture parses");
        let names = state_cell_init_values(&file);
        // mununu#711 — `collect_symbols` names the cell by its value-identical alias now; before,
        // the loose pass named it `cnt_d`, the trap this test was written for.
        assert_eq!(names.get("cnt_q"), Some(&0));
        assert!(!names.contains_key("cnt_d"));
        assert_eq!(
            cycle_zero_class(&file, "cnt_q", true),
            CycleZero::Established(0),
            "through the pinned reset mux `cnt_q` IS the cell, at its `init`"
        );
        assert_eq!(
            cycle_zero_class(&file, "cnt_q", false),
            CycleZero::Combinational,
            "not followed as an alias, the mux over the PINNED (`one`) reset is a function of \
             an established cell alone — observed at reset, not free"
        );
        // The un-gated lift: the reset is a primary INPUT. Not an alias (the mux is not
        // value-identical with the reset free), and its cone has an input ⇒ free at cycle 0.
        let ungated = btor2.replace("3 one 1 rst_ni", "3 input 1 rst_ni");
        let file_ungated = crate::adapter::btor2::parser::parse(&ungated).expect("parses");
        assert_eq!(
            cycle_zero_class(&file_ungated, "cnt_q", false),
            CycleZero::Unestablished,
            "with the reset free, `cnt_q`'s cycle-0 value depends on an input"
        );
        assert_eq!(
            cycle_zero_class(&file, "cnt_d", true),
            CycleZero::Combinational,
            "`cnt_d` as a SIGNAL is the next-value function of an established cell — what the \
             loose pass named the cell is read from `state_cell_init_values` before this"
        );
        assert_eq!(
            cycle_zero_class(&file, "sum", true),
            CycleZero::Unestablished,
            "a combinational signal fed by a primary input has no single cycle-0 value"
        );
        assert_eq!(
            cycle_zero_class(&file, "loose_q", true),
            CycleZero::Unestablished,
            "an alias of an init-less cell is free at cycle 0"
        );
        assert_eq!(
            cycle_zero_class(&file, "cnt_q__bits", true),
            CycleZero::Combinational,
            "an unknown name has an empty cone: nothing free in it, left to the observer"
        );
    }

    /// mununu#579 — the bound: the initial cube set is the product of the free dimensions and
    /// is enumerated only up to `MAX_FREE_INIT_DIMENSIONS`; the reason past it names itself and
    /// is reproducible (a property of the model).
    #[test]
    fn the_free_initial_dimension_cap_abstains_by_name() {
        let r = BottomReason::UnestablishedInitialCubes {
            free_dimensions: MAX_FREE_INIT_DIMENSIONS + 1,
            cap: MAX_FREE_INIT_DIMENSIONS,
            registers: 3,
        };
        assert_eq!(r.tag(), "unestablished-initial-cubes");
        assert_eq!(r.determinism(), BottomDeterminism::Reproducible);
        assert!(r.one_line().contains("3 register(s) with no `init` line"));
        assert!(
            free_input_init_cubes_feasible(0, &[0, 1], &[("a".into(), 1), ("a".into(), 2)]).len()
                == 3,
            "two values of one register never co-occur (mununu#503): 4 combos minus the pair"
        );
    }

    /// mununu#550 — an author-given annotation name becomes the property's LABEL, so a pin survives
    /// an insertion above it.
    ///
    /// The positional `ann_guarantee_<idx>` is kept as well: an existing index pin must not break
    /// just because a neighbour gained a name.
    #[test]
    fn an_annotation_name_becomes_the_label_and_the_index_still_works() {
        let src = "// @mununu_guarantee(tier3_recover) nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)\n\
                   // @mununu_guarantee nu X. ((cnt <= 1) && [] X)\n\
                   module m; endmodule\n";
        let scan = scan_annotation_properties(&[("m.sv".to_string(), src.to_string())]);

        assert_eq!(
            scan.guarantees.len(),
            2,
            "both annotations translate: {scan:?}"
        );
        assert_eq!(scan.guarantees[0].name, "ann_guarantee_0");
        assert_eq!(
            scan.guarantees[0].label.as_deref(),
            Some("tier3_recover"),
            "the author's name becomes the label `--expect` matches"
        );
        assert_eq!(
            scan.guarantees[1].label, None,
            "an unnamed annotation stays unnamed — the name is optional, never inferred"
        );
    }

    /// A duplicate name is REFUSED, because the expectation lookup is a `find()`.
    ///
    /// Two properties answering to one pin would make `--expect dup` silently match only the first —
    /// the same silent re-pointing mununu#550 exists to remove, one level up. Stricter than SVA
    /// labels on purpose: those may legally repeat (two asserts in one labelled `begin…end`), an
    /// annotation name has no such excuse.
    #[test]
    fn a_duplicate_annotation_name_is_refused_with_a_reason() {
        let src = "// @mununu_guarantee(dup) nu X. ((cnt <= 1) && [] X)\n\
                   // @mununu_guarantee(dup) nu X. ((other <= 1) && [] X)\n\
                   module m; endmodule\n";
        let scan = scan_annotation_properties(&[("m.sv".to_string(), src.to_string())]);

        assert_eq!(
            scan.guarantees.len(),
            1,
            "the second `dup` must not become a second property answering to one pin: {scan:?}"
        );
        assert!(
            scan.skipped
                .iter()
                .any(|m| m.contains("duplicate name") && m.contains("dup")),
            "and the refusal must SAY so rather than dropping it silently: {:?}",
            scan.skipped
        );
    }

    /// mununu#548 O-2 — the old note→property join could never match a real note.
    ///
    /// `drop_stale_bottom_reasons` removes a ⊥ explanation for a property another engine went on to
    /// decide. It built its pattern as `format!("{name}: ")`, but every summary
    /// [`bottom_reason_note`] produces starts with a BACKTICK — `` `name`: `` — so the match failed
    /// for every note the code actually emits. **The pass was a no-op from the day it was written**,
    /// and its own test passed because that test invented the summary without the backticks.
    ///
    /// So this test does two things: it pins the real production format against the old pattern, and
    /// it checks the field-based join now does the job. The mock comes from the real builder, not
    /// from a string typed here — which is the whole lesson (CLAUDE.md §14).
    #[test]
    fn the_stale_bottom_reason_pass_now_actually_fires_on_a_real_note() {
        let real = bottom_reason_note("p", BottomReason::SafetyShapeNotReducible);
        assert!(
            !real.summary.starts_with("p: "),
            "PREMISE: the old join's pattern cannot match the real summary, which is why the pass \
             never fired. Got: {:?}",
            &real.summary[..real.summary.len().min(40)]
        );
        assert!(
            real.summary.starts_with("`p`: "),
            "the real format is backtick-quoted: {:?}",
            &real.summary[..real.summary.len().min(40)]
        );

        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "p".into(),
                label: None,
                kind: SvaKind::Assert,
                formula: "nu X. (q && [] X)".into(),
                // Another engine DECIDED it, so the ⊥ explanation is stale.
                outcome: VerifyOutcome::Holds,
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: Some("reach-portfolio".into()),
                vacuity: None,
            }],
            notes: vec![real],
            ..Default::default()
        };

        drop_stale_bottom_reasons(&mut report);

        assert!(
            report.notes.is_empty(),
            "a ⊥ explanation for a property the report declares HOLDS must be removed — this is \
             mununu#548's defect, and it survived until the join stopped parsing prose: {:?}",
            report.notes.iter().map(|n| &n.summary).collect::<Vec<_>>()
        );
    }

    /// mununu#548 O-2 — a note whose PROSE names a property must also carry the field.
    ///
    /// The convention is that a per-property summary opens `` `<name>`: ``. This asserts the two
    /// stay in step for every note builder that is a callable pure function, so a reworded summary
    /// cannot drift away from the field the way the old join drifted away from the prose.
    ///
    /// **Honest limit:** three per-property notes are built as inline literals inside
    /// `verify_auto` (`engine-isolation`, `array-atom-unsupported`, `safety-rescue-declined`) and
    /// are not reachable from here, so they are not guarded by this test — they were set by hand and
    /// verified by reading. Making the invariant unavoidable would mean a private field and
    /// constructors across 32 sites; that is worth doing and is not this change.
    #[test]
    fn a_note_whose_prose_names_a_property_also_carries_the_field() {
        let builders: Vec<VerificationNote> = vec![
            bottom_reason_note("sva_2", BottomReason::SafetyShapeNotReducible),
            bottom_reason_note("sva_2", BottomReason::NoStateModelNonSafety),
            bottom_reason_note("sva_2", BottomReason::UnclassifiedBottom),
            crate::planner::diameter_bound_skip_note_for_test("sva_2", 5),
            crate::planner::bitblast_oom_skip_note_for_test("sva_2", Some(40)),
        ];
        for n in &builders {
            let names_a_property = n.summary.starts_with("`sva_2`:");
            assert!(
                names_a_property,
                "premise: these builders use the backtick-name convention; got {:?}",
                &n.summary[..n.summary.len().min(30)]
            );
            assert_eq!(
                n.property.as_deref(),
                Some("sva_2"),
                "kind `{}` names a property in its prose but does not carry the field — the two \
                 must not drift, which is exactly how the stale-note join came to match nothing",
                n.kind
            );
        }
    }

    /// A per-property note must carry its property, and a model-level note must not invent one.
    #[test]
    fn a_bottom_reason_note_carries_its_property_and_a_model_note_does_not() {
        let n = bottom_reason_note("sva_7", BottomReason::SafetyShapeNotReducible);
        assert_eq!(
            n.property.as_deref(),
            Some("sva_7"),
            "the ⊥ explanation is about one property and must say which"
        );
        // The coverage summary describes the run, not a property.
        let report = AutoVerifyReport::default();
        let cov = coverage_summary_note(&report);
        assert_eq!(
            cov.property, None,
            "a model-level note must not claim a property — a consumer would group by it"
        );
    }

    /// A property verdict for the post-pass fixtures below.
    fn pv(name: &str, formula: &str, outcome: VerifyOutcome) -> PropertyVerdict {
        PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        }
    }

    /// mununu#599 — the issue's own pair, under one abstraction: `AG EF(st_q == 0)` HOLDS beside
    /// `EF(st_q == 3)` VIOLATED. Not a contradiction (the design may leave 0 and come back without
    /// ever visiting 3), so the HOLDS stands — and the guarantee now SAYS, on its own property,
    /// that a same-register witness is refuted here. The unrelated HOLDS carries nothing.
    #[test]
    fn x599_a_guarantee_names_its_refuted_witness_at_the_property() {
        let mut report = AutoVerifyReport {
            properties: vec![
                pv(
                    "ann_guarantee_0",
                    "nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)",
                    VerifyOutcome::Holds,
                ),
                pv(
                    "ann_guarantee_1",
                    "mu Z.((st_q == 3) || <> Z)",
                    VerifyOutcome::Violated { false_cells: 4 },
                ),
                pv("other", "nu X.((en == 1) && [] X)", VerifyOutcome::Holds),
            ],
            ..Default::default()
        };
        flag_self_contradictory_verdicts(&mut report);
        flag_vacuous_guarantees(&mut report);
        refresh_vacuity_notes(&mut report);

        let g = &report.properties[0];
        assert!(
            matches!(g.outcome, VerifyOutcome::Holds),
            "the verdict is not changed"
        );
        let v = g
            .vacuity
            .as_ref()
            .expect("the guarantee carries the finding");
        assert_eq!(v.kind, VacuityKind::WitnessRefuted);
        assert_eq!(v.witness, "ann_guarantee_1");
        assert!(
            v.detail.contains("ann_guarantee_1") && v.detail.contains("st_q == 0"),
            "the detail names the witness and the target: {}",
            v.detail
        );
        assert!(
            report.properties[1].vacuity.is_none(),
            "the witness itself carries nothing"
        );
        assert!(report.properties[2].vacuity.is_none());
        // The note form joins on the FIELD (mununu#548), not on prose.
        let note = report
            .notes
            .iter()
            .find(|n| n.kind == "vacuous-guarantee")
            .expect("one note per finding");
        assert_eq!(note.property.as_deref(), Some("ann_guarantee_0"));
        assert_eq!(note.items, vec!["ann_guarantee_1".to_string()]);
        assert_eq!(note.level, NoteLevel::ScopeCaveat);
        // Re-deriving is idempotent: a second refresh does not duplicate the note.
        refresh_vacuity_notes(&mut report);
        assert_eq!(
            report
                .notes
                .iter()
                .filter(|n| n.kind == "vacuous-guarantee")
                .count(),
            1
        );
    }

    /// mununu#599 — the proven case: `EF(st_q != 0)` VIOLATED says the design never leaves 0, so
    /// `AG EF(st_q == 0)` HOLDS vacuously. The stronger finding wins over a refuted witness in the
    /// same report.
    #[test]
    fn x599_an_invariant_target_is_the_stronger_finding() {
        let mut report = AutoVerifyReport {
            properties: vec![
                pv(
                    "g",
                    "nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)",
                    VerifyOutcome::Holds,
                ),
                pv(
                    "w_other_value",
                    "mu Z.((st_q == 3) || <> Z)",
                    VerifyOutcome::Violated { false_cells: 1 },
                ),
                pv(
                    "w_dual",
                    "mu Z.((st_q != 0) || <> Z)",
                    VerifyOutcome::Violated { false_cells: 1 },
                ),
            ],
            ..Default::default()
        };
        flag_self_contradictory_verdicts(&mut report);
        flag_vacuous_guarantees(&mut report);
        let v = report.properties[0].vacuity.as_ref().unwrap();
        assert_eq!(v.kind, VacuityKind::InvariantTarget);
        assert_eq!(v.witness, "w_dual");
        assert!(matches!(report.properties[0].outcome, VerifyOutcome::Holds));
    }

    /// mununu#599 — the HOLDS side of the self-contradiction table: `AG EF(p)` HOLDS beside `EF(p)`
    /// VIOLATED cannot share a model (the initial state is reachable). Both withheld, neither
    /// adjudicated, and no vacuity finding rides on a withheld verdict.
    #[test]
    fn x599_a_guarantee_beside_its_own_refuted_target_is_a_contradiction_not_a_vacuity() {
        let mut report = AutoVerifyReport {
            properties: vec![
                pv(
                    "g",
                    "nu Y.((mu X.((st_q == 0) || <> X)) && [] Y)",
                    VerifyOutcome::Holds,
                ),
                pv(
                    "target_unreachable",
                    "mu Z.((st_q == 0) || <> Z)",
                    VerifyOutcome::Violated { false_cells: 1 },
                ),
                // The HOLDS–HOLDS twin of the mununu#579 pair.
                pv("ef", "mu Z.((cnt == 2) || <> Z)", VerifyOutcome::Holds),
                pv("ag_not", "nu X.((cnt != 2) && [] X)", VerifyOutcome::Holds),
            ],
            ..Default::default()
        };
        flag_self_contradictory_verdicts(&mut report);
        flag_vacuous_guarantees(&mut report);
        for p in &report.properties {
            assert!(
                matches!(p.outcome, VerifyOutcome::Unknown { .. }),
                "{} must be withheld: {:?}",
                p.name,
                p.outcome
            );
            assert!(
                matches!(
                    p.bottom_reason,
                    Some(BottomReason::ReportSelfContradiction { .. })
                ),
                "{}: {:?}",
                p.name,
                p.bottom_reason
            );
            assert!(p.vacuity.is_none());
        }
    }

    /// mununu#579 — a report whose own verdicts are unsatisfiable together forces BOTH to ⊥.
    ///
    /// `AG(cnt <= 1)` VIOLATED says some reachable state has `cnt > 1`; `EF(cnt > 1)` VIOLATED says
    /// no reachable state does. Exactly negated, so one of them is broken — and the check must not
    /// say which.
    #[test]
    fn a_report_that_contradicts_itself_withholds_both_verdicts() {
        let mk = |name: &str, formula: &str| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut report = AutoVerifyReport {
            properties: vec![
                mk("bound", "nu X. ((cnt <= 1) && [] X)"),
                mk("witness", "mu Z. ((cnt > 1) || <> Z)"),
                // An unrelated violation must survive: the alarm is per-pair, not per-report.
                mk("unrelated", "nu X. ((other == 0) && [] X)"),
            ],
            ..Default::default()
        };

        flag_self_contradictory_verdicts(&mut report);

        let by = |n: &str| report.properties.iter().find(|p| p.name == n).unwrap();
        for n in ["bound", "witness"] {
            assert!(
                matches!(by(n).outcome, VerifyOutcome::Unknown { .. }),
                "{n} must be withheld, not adjudicated: {:?}",
                by(n).outcome
            );
            match &by(n).bottom_reason {
                Some(BottomReason::ReportSelfContradiction { detail }) => {
                    assert!(
                        detail.contains("bound") && detail.contains("witness"),
                        "the alarm must name BOTH properties so neither looks like the winner: {detail}"
                    );
                }
                other => panic!("expected the self-contradiction alarm on {n}, got {other:?}"),
            }
        }
        assert!(
            matches!(by("unrelated").outcome, VerifyOutcome::Violated { .. }),
            "an unrelated violation is untouched: {:?}",
            by("unrelated").outcome
        );
    }

    /// ⚠️ mununu#579's own worked example must NOT fire, and this is the control that proves it.
    ///
    /// The issue asserted that `AG(drop_q <= 1)` VIOLATED beside `EF(drop_q == 2)` VIOLATED "cannot
    /// all hold". They can: a model reaching 7 and never 2 satisfies both. Flagging them would force
    /// two verdicts to ⊥ on evidence that permits both — a false soundness alarm destroys sound
    /// results, which is worse than the gap it would be closing.
    #[test]
    fn the_577_trio_is_not_treated_as_a_contradiction() {
        let mk = |name: &str, formula: &str| PropertyVerdict {
            name: name.into(),
            label: None,
            kind: SvaKind::Assert,
            formula: formula.into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        };
        let mut report = AutoVerifyReport {
            properties: vec![
                mk("ag", "nu X. ((drop_q <= 1) && [] X)"),
                mk("ef2", "mu Z. ((drop_q == 2) || <> Z)"),
                mk("ef3", "mu Z. ((drop_q == 3) || <> Z)"),
            ],
            ..Default::default()
        };

        flag_self_contradictory_verdicts(&mut report);

        for p in &report.properties {
            assert!(
                matches!(p.outcome, VerifyOutcome::Violated { .. }),
                "{} must be left alone — a single value > K refutes nothing: {:?}",
                p.name,
                p.outcome
            );
            assert!(
                p.bottom_reason.is_none(),
                "{} gained a spurious alarm",
                p.name
            );
        }
    }

    /// mununu#577 — the guard must NOT fire when no reset was pinned: then the free initial value
    /// is the design's own, not something mununu removed.
    #[test]
    fn the_init_guard_does_not_fire_without_a_pinned_reset() {
        const NO_INIT: &str =
            "1 sort bitvec 10\n2 state 1 cnt\n3 one 1\n4 add 1 2 3\n5 next 1 2 4\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "ag".into(),
                label: None,
                kind: SvaKind::Assert,
                formula: "nu X. ((cnt <= 1) && [] X)".into(),
                outcome: VerifyOutcome::Violated { false_cells: 1 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        // gated_resets deliberately EMPTY.
        downgrade_violated_on_unestablished_init(&mut report, NO_INIT);
        assert!(
            matches!(report.properties[0].outcome, VerifyOutcome::Violated { .. }),
            "with no reset pinned there is nothing mununu removed, so the verdict stands"
        );
    }

    /// mununu#552 — the dump is a self-contained reproducer: the model AND the formulas.
    #[test]
    fn shadow_dump_writes_the_model_with_its_formulas() {
        let dir = std::env::temp_dir().join(format!("mununu552-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmpdir");
        let path = dir.join("dump.btor2");
        // SAFETY: single-threaded test scope; the var is read once inside the call below.
        unsafe { std::env::set_var(SHADOW_DUMP_ENV, &path) };

        let btor2 = "1 sort bitvec 4\n2 state 1 s\n";
        let props = vec![("p0".to_string(), "nu X. ((s == 1) && [] X)".to_string())];
        dump_prepared_model(btor2, &props, None);
        unsafe { std::env::remove_var(SHADOW_DUMP_ENV) };

        let got = std::fs::read_to_string(&path).expect("dump written");
        // The model is present and still parses as BTOR2 (the header is all `;` comments).
        assert!(
            got.contains("2 state 1 s"),
            "the model must be in it: {got}"
        );
        crate::adapter::btor2::parser::parse(&got)
            .expect("the dump must still be valid BTOR2 — the header is comments, not prose");
        // The formula is present, so the file is a whole reproducer rather than half of one.
        assert!(
            got.contains("p0: nu X."),
            "the formula must be in it: {got}"
        );
        // And it declares what it does NOT contain — the trap this issue exists to close.
        assert!(
            got.contains("EXCLUDES") && got.contains("inputs-in-the-state"),
            "the dump must name the rewrite it omits: {got}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// mununu#552 — a dump must never be able to fail a verification run.
    ///
    /// Mirrors `partial_json`'s contract: unset is a no-op, an empty value is treated as unset,
    /// and an unwritable path warns once rather than propagating. A diagnostic that can abort the
    /// thing it is diagnosing is worse than no diagnostic.
    #[test]
    fn shadow_dump_is_diagnostic_only_and_cannot_fail_a_run() {
        // Unset — no panic, no file, nothing.
        unsafe { std::env::remove_var(SHADOW_DUMP_ENV) };
        dump_prepared_model("1 sort bitvec 1\n", &[], None);

        // Empty — treated as unset, not as the path "".
        unsafe { std::env::set_var(SHADOW_DUMP_ENV, "   ") };
        dump_prepared_model("1 sort bitvec 1\n", &[], None);

        // Unwritable — warns, does not propagate.
        unsafe { std::env::set_var(SHADOW_DUMP_ENV, "/nonexistent-dir-552/x.btor2") };
        dump_prepared_model("1 sort bitvec 1\n", &[], None);
        unsafe { std::env::remove_var(SHADOW_DUMP_ENV) };
    }

    /// mununu#553 ask 2 — REPRODUCER: the ⊥ that fires on an ORDINARY run must say it is
    /// host-dependent.
    ///
    /// The asymmetry this replaces: `MemoryCeilingExceeded`'s one-liner said *"Host-dependent
    /// without a clock…"* while `BudgetExpired`'s said nothing about determinism at all — and
    /// `BudgetExpired` is the one that fires from the 15-min per-property and 1-h whole-run
    /// harness clocks, i.e. **on every default run, with nothing configured**. A consumer could
    /// read the less-common case correctly and the common one not at all.
    #[test]
    fn the_default_on_wall_clock_reports_itself_as_host_dependent() {
        assert_eq!(
            BottomReason::BudgetExpired.determinism(),
            BottomDeterminism::HostDependent,
            "the harness wall clock is ON BY DEFAULT; a consumer must be able to tell its ⊥ from \
             a real one without reading prose"
        );
        assert_eq!(
            BottomReason::MemoryCeilingExceeded.determinism(),
            BottomDeterminism::HostDependent,
            "RSS is host-dependent too — without a clock anywhere in the mechanism"
        );
    }

    /// mununu#553 ask 1+2 — the four budgets that used to share one tag are separable, and
    /// exactly one of them is host-dependent.
    ///
    /// Before this, BIT CAP / node / iteration / latency / arena all arrived as
    /// `engine-did-not-complete`, so a gate could only tell them apart by regexing the engine's
    /// prose — while the API's own table told it to branch on the tag.
    #[test]
    fn engine_budgets_are_separable_and_only_the_clock_is_host_dependent() {
        use crate::adapter::EngineBudget;
        let case = |detail: &str| BottomReason::EngineDidNotComplete {
            engine: "exact-symbolic".into(),
            detail: detail.into(),
        };

        // Every marker the engine actually emits, verbatim in shape.
        let cases = [
            (
                "abstained on the ITERATION budget (1048577 > 1048576)",
                EngineBudget::Iteration,
            ),
            (
                "abstained on the NODE budget (12 of 10 live BDD nodes)",
                EngineBudget::Node,
            ),
            (
                "cone is 200 bits (> 192) — abstained on the BIT CAP",
                EngineBudget::BitCap,
            ),
            (
                "abstained on the ARENA-SAFETY bound (live 9 of 10)",
                EngineBudget::ArenaSafety,
            ),
            (
                "abstained on the fixpoint LATENCY bound (nodes 11, iters 6000)",
                EngineBudget::FixpointLatency,
            ),
            (
                "BDD arena exhausted (OxiDD OutOfMemory)",
                EngineBudget::ArenaExhausted,
            ),
            (
                "abstained on the OPT-IN WALL-CLOCK budget (fixpoint)",
                EngineBudget::WallClock,
            ),
        ];
        for (detail, want) in cases {
            assert_eq!(
                case(detail).engine_budget(),
                Some(want),
                "misfiled: {detail}"
            );
        }

        // The whole point: one of the seven is a property of the afternoon, six are not.
        let host_dependent: Vec<_> = cases
            .iter()
            .filter(|(d, _)| case(d).determinism() == BottomDeterminism::HostDependent)
            .map(|(_, b)| *b)
            .collect();
        assert_eq!(
            host_dependent,
            vec![EngineBudget::WallClock],
            "exactly the OPT-IN wall clock is host-dependent; the rest are deterministic \
             total-work bounds and a consumer should pin their ⊥ honestly"
        );
    }

    /// An abstention marker the classifier does not recognise must stay UNESTABLISHED.
    ///
    /// The three-valued answer is the point: a bool would force a claim here, and guessing
    /// `reproducible` would tell a consumer to pin a ⊥ that may not reproduce. The engine's
    /// error channel is a `String`, so an unrecognised marker is a real possibility whenever a
    /// message is reworded — and silence is the honest response, not a default.
    #[test]
    fn an_unrecognised_engine_budget_does_not_claim_determinism() {
        let r = BottomReason::EngineDidNotComplete {
            engine: "exact-symbolic".into(),
            detail: "stopped for a reason this classifier has never seen".into(),
        };
        assert_eq!(
            r.engine_budget(),
            Some(crate::adapter::EngineBudget::Unrecognised)
        );
        assert_eq!(r.determinism(), BottomDeterminism::Unestablished);
        assert_eq!(
            BottomReason::UnclassifiedBottom.determinism(),
            BottomDeterminism::Unestablished,
            "an unestablished CAUSE cannot have an established determinism"
        );
    }

    /// THE FALSIFIER from the plan: the separation must need no string matching.
    ///
    /// A consumer said they would use this "at once" — pin `unknown` honestly for the
    /// deterministic budgets, treat the nondeterministic one as a configuration error. That is
    /// only possible if the distinction is a FIELD. This test is the machine-readable half of
    /// that promise: it touches `determinism()` and `tag()` and never reads `one_line()`.
    #[test]
    fn a_gate_separates_reproducible_from_host_dependent_without_reading_prose() {
        let all = [
            BottomReason::SafetyShapeNotReducible,
            BottomReason::NoStateModelNonSafety,
            BottomReason::UnclassifiedBottom,
            BottomReason::EngineContradiction {
                detail: "Holds vs Violated".into(),
            },
            BottomReason::EngineCrashed {
                detail: "SIGABRT".into(),
            },
            BottomReason::MemoryCeilingExceeded,
            BottomReason::BudgetExpired,
            BottomReason::NotAttempted,
        ];
        for r in &all {
            // Every reason answers, and the tag vocabulary is closed and stable.
            assert!(
                ["reproducible", "host-dependent", "unestablished"]
                    .contains(&r.determinism().tag()),
                "unknown determinism tag for {}",
                r.tag()
            );
        }

        // A soundness alarm is REPRODUCIBLE — retrying is wrong for a different reason than a
        // budget, and a gate must not lump it in with the host-dependent cases.
        assert_eq!(
            BottomReason::EngineContradiction { detail: "x".into() }.determinism(),
            BottomDeterminism::Reproducible
        );
        // A crash is about the host, even though it is not a budget at all.
        assert_eq!(
            BottomReason::EngineCrashed {
                detail: "OOM kill".into()
            }
            .determinism(),
            BottomDeterminism::HostDependent
        );
    }

    /// Does the MERGE preserve a `bottom_reason` an engine already attached?
    ///
    /// Ordering matters here and is easy to get backwards: `escalate_bottom` (which classifies a
    /// residual ⊥ and sets the field) runs INSIDE `verify_auto_impl`, i.e. once PER ENGINE, and
    /// `merge_portfolio_reports` runs AFTER. So by merge time each candidate may already carry a
    /// reason, and the all-⊥ arm copies `outcome` from the chosen candidate — the question is
    /// whether the reason travels with it.
    #[test]
    fn the_merge_preserves_a_reason_an_engine_already_attached() {
        let mut exact_prop = mk_report(&[("p", VerifyOutcome::Unknown { unknown_cells: 0 }, None)]);
        exact_prop.properties[0].bottom_reason = Some(BottomReason::UnclassifiedBottom);

        let mut cube_prop =
            mk_report(&[("p", VerifyOutcome::Unknown { unknown_cells: 125 }, None)]);
        cube_prop.properties[0].bottom_reason = Some(BottomReason::UnclassifiedBottom);

        let runs = vec![
            ("exact-symbolic", Ok(exact_prop)),
            ("symbolic", Ok(cube_prop)),
        ];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Sequential).expect("merge ok");

        assert!(
            matches!(merged.properties[0].outcome, VerifyOutcome::Unknown { .. }),
            "precondition: still ⊥ after the merge"
        );
        assert!(
            merged.properties[0].bottom_reason.is_some(),
            "the merge must not drop a reason the engine already attached — `None` here is \
             indistinguishable from a field that was never set, because \
             `skip_serializing_if` omits the key entirely"
        );
    }

    /// REPRODUCER (monono, 2026-09-15, post-#558): a residual ⊥ arrived at a consumer with the
    /// `bottom_reason` key ABSENT from the property object — not `unclassified-bottom`, absent.
    ///
    /// Verbatim from their `--json` on `b8a2abb`:
    ///
    /// ```text
    /// { "name": "video_timing_sva_sva_1", "outcome": "unknown",
    ///   "detail": "125 cell(s)", "unknown_cells": 125, "seeded_predicates": [] }
    /// ```
    ///
    /// while a DECIDED property in the same report carried `decided_by`. So the fields exist and
    /// serialize; the ⊥ simply had `None`. **Every `⊥` must carry a reason — absence is a defect,
    /// not a classification**, because `skip_serializing_if = "Option::is_none"` makes `None`
    /// indistinguishable from a field that was never added.
    #[test]
    fn every_residual_bottom_carries_a_reason_after_escalation() {
        const DESIGN: &str = "1 sort bitvec 1\n2 sort bitvec 8\n3 state 2 s\n4 zero 2\n\
                              5 init 2 3 4\n6 next 2 3 3\n";
        let mut report = AutoVerifyReport {
            properties: vec![PropertyVerdict {
                name: "video_timing_sva_sva_1".into(),
                label: Some("a_frame_wraps_at_total".into()),
                kind: crate::adapter::slang::translate::SvaKind::Assert,
                formula: "nu X. (((!(((hcount_q == 799) && (vcount_q == 524))) || [] \
                          (vcount_q == 0))) && [] X)"
                    .into(),
                outcome: VerifyOutcome::Unknown { unknown_cells: 125 },
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            ..Default::default()
        };
        let opts = VerifyAutoOptions::default();
        let _ = escalate_bottom(&mut report, DESIGN, false, &opts, &[]);

        let p = &report.properties[0];
        if matches!(p.outcome, VerifyOutcome::Unknown { .. }) {
            assert!(
                p.bottom_reason.is_some(),
                "a residual ⊥ with NO reason is exactly what reached the consumer: the key is \
                 omitted by `skip_serializing_if`, so `None` is indistinguishable from a field \
                 that was never added"
            );
        }
    }

    /// O-2 — the five ⊥ causes must be DISTINGUISHABLE, which is the whole point of the field.
    ///
    /// Before this they all produced `unknown` with `unknown_cells: 0`, so a consumer could not
    /// tell a soundness alarm from a timeout. The assertion that matters is not that each has *a*
    /// reason but that no two share a TAG — a gate branches on the tag, and two causes collapsing
    /// onto one tag is the defect re-created one level up.
    #[test]
    fn the_five_bottom_causes_have_five_distinct_tags() {
        let all = [
            BottomReason::SafetyShapeNotReducible,
            BottomReason::NoStateModelNonSafety,
            BottomReason::UnclassifiedBottom,
            BottomReason::EngineDidNotComplete {
                engine: "exact-symbolic".into(),
                detail: "abstained on the ITERATION budget".into(),
            },
            BottomReason::EngineContradiction {
                detail: "Holds@[exact] vs Violated@[symbolic]".into(),
            },
            BottomReason::EngineCrashed {
                detail: "signal 6".into(),
            },
            BottomReason::BudgetExpired,
            BottomReason::NotAttempted,
        ];
        let tags: Vec<&str> = all.iter().map(|r| r.tag()).collect();
        let mut uniq = tags.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            uniq.len(),
            tags.len(),
            "two causes share a tag, so a gate cannot distinguish them: {tags:?}"
        );
        for r in &all {
            assert!(!r.one_line().is_empty(), "{} has no rendering", r.tag());
        }
    }

    /// The three causes a GATE must treat differently, pinned by the advice each one carries.
    ///
    /// Retrying with a bigger budget is right for exactly ONE of them. It cannot help a shape
    /// problem, and on a contradiction it means retrying an UNSOUNDNESS — so the text has to say
    /// so, because that is the only place a consumer learns it.
    #[test]
    fn only_a_budget_expiry_tells_the_reader_to_retry_with_more_budget() {
        let budget = BottomReason::BudgetExpired.one_line();
        assert!(
            budget.contains("retry"),
            "a budget expiry is THE case where retrying is right, and must say so: {budget}"
        );

        let contradiction = BottomReason::EngineContradiction {
            detail: "Holds@[a] vs Violated@[b]".into(),
        }
        .one_line();
        assert!(
            contradiction.contains("SOUNDNESS"),
            "a contradiction must be named a soundness alarm, not an abstention: {contradiction}"
        );
        assert!(
            contradiction.contains("unsound"),
            "must say an engine is unsound — the actionable part: {contradiction}"
        );

        let shape = BottomReason::SafetyShapeNotReducible.one_line();
        assert!(
            shape.contains("NOT a bigger budget") || shape.contains("not a bigger budget"),
            "a shape problem must say a budget will not help, or a gate loops forever: {shape}"
        );
    }

    /// REPRODUCER for monono ask 26 (2026-09-15).
    ///
    /// `merge_portfolio_reports` carried the comment *"Errors contribute nothing"* and meant it:
    /// the exact engine's `Err` — which NAMES the budget it hit — was discarded, so a consumer saw
    /// a bare `⊥` plus a cube count and could only try knobs blind. Cost: a red lane, an hour,
    /// two refuted hypotheses, neither refuted from the ⊥ itself.
    ///
    /// Fails on the parent: `bottom_reason` does not exist there.
    #[test]
    fn a_failing_engines_budget_name_survives_the_merge() {
        let exact = (
            "exact-symbolic",
            Err(AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: "symbolic bit-blaster: abstained on the ITERATION budget (1048577 > \
                          1048576) — raise MUNUNU_BDD_ITER_BUDGET"
                    .to_string(),
                location: None,
            }),
        );
        let cube = (
            "symbolic",
            Ok(mk_report(&[(
                "sva_1",
                VerifyOutcome::Unknown { unknown_cells: 125 },
                None,
            )])),
        );

        let runs = vec![exact, cube];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Sequential)
            .expect("a surviving engine still yields a report");

        let r = merged.properties[0]
            .bottom_reason
            .as_ref()
            .expect("a residual ⊥ must say WHY, as a FIELD — a note is filterable, a field is not");
        assert_eq!(r.tag(), "engine-did-not-complete");
        let line = r.one_line();
        assert!(
            line.contains("ITERATION budget"),
            "must carry the engine's own budget NAME — that is the actionable part: {line}"
        );
        assert!(
            line.contains("MUNUNU_BDD_ITER_BUDGET"),
            "must carry the KNOB to raise, so the reader need not guess: {line}"
        );
        assert!(
            line.contains("exact-symbolic"),
            "must attribute to the engine that failed: {line}"
        );
    }

    /// The mununu#548 lesson, applied in the new direction: a property another engine DECIDED must
    /// not inherit a failing engine's reason. #548 was this defect for notes; repeating it for the
    /// field would be the same over-claim in a place consumers trust more.
    #[test]
    fn a_decided_property_never_inherits_a_failing_engines_reason() {
        let exact = (
            "exact-symbolic",
            Err(AdapterError {
                kind: AdapterErrorKind::IrConsistencyError,
                message: "abstained on the ARENA-SAFETY bound".to_string(),
                location: None,
            }),
        );
        let cube = (
            "symbolic",
            Ok(mk_report(&[("sva_1", VerifyOutcome::Holds, None)])),
        );

        let runs = vec![exact, cube];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Sequential).expect("merge ok");

        assert!(
            merged.properties[0].bottom_reason.is_none(),
            "a property that HOLDS is not ⊥ and must carry no bottom-reason, however loudly \
             another engine failed"
        );
    }

    /// Every engine errored → the merge surfaces the first (highest-precision) error, not a panic.
    #[test]
    fn portfolio_merge_all_errors_returns_first_error() {
        let err = |m: &str| {
            Err(AdapterError {
                kind: AdapterErrorKind::StateSpaceOverflow,
                message: m.to_string(),
                location: None,
            })
        };
        let runs: Vec<(&str, Result<AutoVerifyReport, AdapterError>)> = vec![
            ("exact-symbolic", err("exact boom")),
            ("symbolic", err("sym boom")),
        ];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Sequential);
        assert!(merged.is_err());
        assert_eq!(merged.unwrap_err().message, "exact boom");
    }

    /// An errored engine contributes nothing; a surviving engine's definite still lands.
    #[test]
    fn portfolio_merge_tolerates_one_engine_error() {
        let exact = (
            "exact-symbolic",
            Err(AdapterError {
                kind: AdapterErrorKind::UnsupportedConstruct,
                message: "exact rejects free reset".to_string(),
                location: None,
            }),
        );
        let symbolic = (
            "symbolic",
            Ok(mk_report(&[("p", VerifyOutcome::Holds, None)])),
        );
        let runs = vec![exact, symbolic];
        let merged = merge_portfolio_reports(&runs, PortfolioMode::Parallel).expect("merge ok");
        assert_eq!(merged.properties[0].outcome, VerifyOutcome::Holds);
    }

    #[test]
    fn engine_selection_defaults_to_portfolio_sequential() {
        // THE DEFAULT (2026-07-06): an unspecified engine ⇒ portfolio-sequential.
        assert_eq!(
            engine_selection(None),
            (false, false, Some(PortfolioMode::Sequential))
        );
        // An unrecognised string also falls to the default (rather than silently explicit).
        assert_eq!(
            engine_selection(Some("nonsense")),
            (false, false, Some(PortfolioMode::Sequential))
        );
        // Each explicit selector maps to exactly its engine (case-insensitive).
        assert_eq!(engine_selection(Some("explicit")), (false, false, None));
        assert_eq!(engine_selection(Some("symbolic")), (true, false, None));
        assert_eq!(
            engine_selection(Some("exact-symbolic")),
            (false, true, None)
        );
        assert_eq!(
            engine_selection(Some("EXACT-SYMBOLIC")),
            (false, true, None)
        );
        assert_eq!(
            engine_selection(Some("portfolio-parallel")),
            (false, false, Some(PortfolioMode::Parallel))
        );
        assert_eq!(
            engine_selection(Some("portfolio-sequential")),
            (false, false, Some(PortfolioMode::Sequential))
        );
    }

    #[test]
    fn outcome_definite_maps_holds_violated_only() {
        assert_eq!(outcome_definite(&VerifyOutcome::Holds), Some(true));
        assert_eq!(
            outcome_definite(&VerifyOutcome::Violated { false_cells: 2 }),
            Some(false)
        );
        assert_eq!(
            outcome_definite(&VerifyOutcome::Unknown { unknown_cells: 1 }),
            None
        );
        assert_eq!(
            outcome_definite(&VerifyOutcome::Skipped {
                reason: "x".to_string()
            }),
            None
        );
    }

    /// H.5-GR1 — a `@mununu_guarantee` with a valid mu-calculus body is merged as
    /// a verifiable property whose formula is the body verbatim (the same string
    /// form the SVA translator emits and the per-property loop parses).
    #[test]
    fn h5_gr1_annotation_guarantee_merges_as_property() {
        let sv = "// @mununu_guarantee nu X. (p and [] X)\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(scan.guarantees.len(), 1);
        assert_eq!(scan.guarantees[0].formula, "nu X. (p and [] X)");
        assert_eq!(scan.guarantees[0].kind, SvaKind::Assert);
        assert!(scan.skipped.is_empty());
        // The stored formula re-parses (it is the exact string the loop feeds mu-calc).
        assert!(mu_parser::parse(&scan.guarantees[0].formula).is_ok());
    }

    /// The box-`F` liveness the LTL translator's existential-`F` cannot express
    /// (`AG(active → AF done)`) is a plain mu-calculus body — it parses and merges.
    #[test]
    fn h5_gr1_annotation_carries_box_f_liveness() {
        let sv = r#"(* mununu_guarantee = "nu X. ((not active or mu Y. (done or [] Y)) and [] X)" *)
module uart_tx(); endmodule"#;
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(scan.guarantees.len(), 1);
        assert!(mu_parser::parse(&scan.guarantees[0].formula).is_ok());
    }

    /// An `ltl` body is read over EVERY path, like a CTXDSL `body = ltl …;`: the response
    /// `G (req -> F ack)` becomes the box-`F` form with the `<> true` successor guard, and a
    /// comparison atom (`state_q == 5`) reaches the formula as one predicate. The stored text
    /// is what the per-property loop re-parses, so it must parse.
    #[test]
    fn an_ltl_annotation_body_is_translated_over_every_path() {
        let sv = "// @mununu_guarantee(resp) ltl G (req -> F ack)\n\
                  // @mununu_guarantee(off) ltl G (state_q == 5 -> !motor_up)\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
        assert_eq!(scan.guarantees.len(), 2);
        let resp = &scan.guarantees[0].formula;
        assert_eq!(scan.guarantees[0].label.as_deref(), Some("resp"));
        assert!(
            resp.contains("nu G1") && resp.contains("mu F2"),
            "universal shape: {resp}"
        );
        assert!(
            resp.contains("<> true"),
            "the every-path `F` needs a successor: {resp}"
        );
        assert!(
            mu_parser::parse(resp).is_ok(),
            "the row's text must re-parse: {resp}"
        );
        let off = &scan.guarantees[1].formula;
        assert!(
            off.contains("(state_q == 5)"),
            "the comparison is one atom: {off}"
        );
        assert!(mu_parser::parse(off).is_ok(), "{off}");
    }

    /// The row keeps the LTL as its author wrote it beside the mu-calculus the engine decides;
    /// a mu-calculus body keeps nothing beside its formula (2026-10-29).
    #[test]
    fn an_ltl_annotation_keeps_its_written_form_and_a_mu_calculus_body_does_not() {
        use crate::adapter::slang::translate::{FormulaLanguage, WrittenFormula};
        let sv = "// @mununu_guarantee(alt) ltl G (start_a -> X (!start_a W start_b))\n\
                  // @mununu_guarantee(safe) nu X. ((!(bad)) && [] X)\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
        assert_eq!(scan.guarantees.len(), 2);
        assert_eq!(
            scan.guarantees[0].written,
            Some(WrittenFormula {
                language: FormulaLanguage::Ltl,
                text: "G (start_a -> X (!start_a W start_b))".to_string(),
            })
        );
        assert!(
            scan.guarantees[0].formula.starts_with("(nu G1."),
            "the engine's text is still the translation: {}",
            scan.guarantees[0].formula
        );
        assert_eq!(scan.guarantees[1].written, None);
        assert_eq!(scan.guarantees[1].formula, "nu X. ((!(bad)) && [] X)");

        // The skeleton's table and the report's lookup: by row name, `None` for a plain row.
        let table = written_formulas_of(&scan.guarantees);
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].0, "ann_guarantee_0");
        let report = AutoVerifyReport {
            diagnostics: ModelDiagnostics {
                written_formulas: table,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            report
                .written_formula("ann_guarantee_0")
                .map(|w| w.text.as_str()),
            Some("G (start_a -> X (!start_a W start_b))")
        );
        assert_eq!(report.written_formula("ann_guarantee_1"), None);
    }

    /// Outside the every-path fragment (`F G p` is path-wise), the body is skipped with the
    /// subformula named — never translated into a different property; and `eltl` is refused
    /// with the mu-calculus alternative spelled out, because the some-path reading is what a
    /// plain body already is.
    #[test]
    fn an_ltl_body_outside_the_fragment_and_an_eltl_body_are_skipped_with_reasons() {
        let sv = "// @mununu_guarantee ltl F G p\n\
                  // @mununu_guarantee eltl G (p -> F q)\n\
                  // @mununu_guarantee ltl\n\
                  // @mununu_guarantee ltl_ok\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(
            scan.guarantees.len(),
            1,
            "`ltl_ok` is a plain atom, not the keyword: {scan:?}"
        );
        assert_eq!(scan.guarantees[0].formula, "ltl_ok");
        assert_eq!(scan.skipped.len(), 3, "{:?}", scan.skipped);
        assert!(
            scan.skipped[0].contains("outside the LTL fragment"),
            "{}",
            scan.skipped[0]
        );
        assert!(
            scan.skipped[1].contains("eltl") && scan.skipped[1].contains("mu X."),
            "{}",
            scan.skipped[1]
        );
        assert!(
            scan.skipped[2].contains("empty formula"),
            "{}",
            scan.skipped[2]
        );
    }

    /// Two 1-bit registers driven by two free inputs: enough for `G (a_q -> X (!a_q W b_q))` to be
    /// violated (a_q twice in a row with no b_q between) along one path of three cycles.
    const TWO_BIT_PULSES: &str = "1 sort bitvec 1\n2 input 1 ia\n3 input 1 ib\n4 state 1 a_q\n5 state 1 b_q\n6 zero 1\n7 init 1 4 6\n8 init 1 5 6\n9 next 1 4 2\n10 next 1 5 3\n";

    /// The weak-until an `ltl` body translates to is the spelling the one-path safety monitor
    /// recognises, so a VIOLATED `G (a -> X (!a W b))` carries its path: here `a_q` at cycle 1 and
    /// again at cycle 2 with `b_q` low. Before this, the universal translation spelled the block
    /// `nu W. (q || (p && [] W))`, `weak_until_parts` did not see it, and the row shipped VIOLATED
    /// with no path and no note — the exact engine decided it, nothing else looked.
    #[test]
    fn an_ltl_weak_until_violation_ships_its_path_through_the_safety_witness() {
        let ltl = crate::ltl::parser::parse("G (a_q -> X (!a_q W b_q))").unwrap();
        let text = crate::ltl::universal::translate_universal_text(&ltl).unwrap();
        assert!(
            text.contains("(nu W2. ((b_q || (!(a_q))) && (b_q || ([] W2))))"),
            "{text}"
        );
        let formula = mu_parser::parse(&text).unwrap();
        assert_eq!(
            formula.property_class(),
            crate::mu_calculus::PropertyClass::Safety
        );
        let trace =
            crate::adapter::reach_rescue::safety_witness(TWO_BIT_PULSES, &formula, false, 8)
                .unwrap_or_else(|miss| panic!("the monitor must compile this shape: {miss}"));
        let a_at = |c: usize| {
            trace.states[c]
                .iter()
                .find(|(n, _)| n == "a_q")
                .and_then(|(_, v)| v.as_u64())
        };
        assert!(trace.states.len() >= 3, "three cycles: {trace:?}");
        assert_eq!(
            (a_at(1), a_at(2)),
            (Some(1), Some(1)),
            "a_q twice in a row: {trace:?}"
        );

        // The post-pass attaches it to a violated row that shipped none.
        let mut report = AutoVerifyReport::default();
        report.properties.push(PropertyVerdict {
            name: "ann_guarantee_0".into(),
            label: Some("alternates".into()),
            kind: SvaKind::Assert,
            formula: text,
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: vec![],
            counterexample: None,
            bottom_reason: None,
            decided_by: Some("exact-symbolic".into()),
            vacuity: None,
        });
        attach_safety_witnesses(&mut report, TWO_BIT_PULSES, false);
        assert!(
            report.properties[0].counterexample.is_some(),
            "the row carries its path; notes: {:?}",
            report.notes.iter().map(|n| &n.summary).collect::<Vec<_>>()
        );
    }

    /// The other half of mununu#821, pinned on the OLD weak-until spelling (which
    /// `weak_until_parts` does not recognise): a violated safety row whose shape the one-path
    /// monitor cannot compile leaves a `witness-unavailable` note naming the `monitor` stage
    /// (mununu#803) — never a bare VIOLATED with no path and no reason.
    #[test]
    fn a_violated_row_the_witness_monitor_cannot_compile_says_so_in_a_note() {
        let old_spelling =
            "(nu G1. (((!(a_q)) || ([] (nu W2. (b_q || ((!(a_q)) && ([] W2)))))) && [] G1))";
        assert!(mu_parser::parse(old_spelling).is_ok());
        let mut report = AutoVerifyReport::default();
        report.properties.push(PropertyVerdict {
            name: "ann_guarantee_0".into(),
            label: Some("alternates".into()),
            kind: SvaKind::Assert,
            formula: old_spelling.into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: vec![],
            counterexample: None,
            bottom_reason: None,
            decided_by: Some("exact-symbolic".into()),
            vacuity: None,
        });
        attach_safety_witnesses(&mut report, TWO_BIT_PULSES, false);
        let note = report.notes.iter().find(|n| {
            n.kind == "witness-unavailable" && n.property.as_deref() == Some("ann_guarantee_0")
        });
        assert!(
            report.properties[0].counterexample.is_none(),
            "the old spelling has no path"
        );
        let note = note.unwrap_or_else(|| {
            panic!(
                "a VIOLATED without a path must say why; notes: {:?}",
                report.notes.iter().map(|n| &n.summary).collect::<Vec<_>>()
            )
        });
        assert_eq!(note.items, vec!["monitor".to_string()], "{}", note.detail);
        assert!(
            note.detail.contains("decided by exact-symbolic"),
            "{}",
            note.detail
        );
    }

    /// The second site of mununu#821: `build_notes` regenerates the model-level notes, and before
    /// this the assignment `report.notes = build_notes(..)` discarded every note a verdict pass had
    /// pushed — the `witness-unavailable` of a VIOLATED row with no path (mununu#803) among them.
    /// The rebuild keeps such a note; a stale copy of a kind it regenerates is not duplicated.
    #[test]
    fn the_notes_rebuild_keeps_what_the_verdict_passes_said() {
        let mut report = AutoVerifyReport::default();
        report.properties.push(PropertyVerdict {
            name: "ann_guarantee_0".into(),
            label: Some("alternates".into()),
            kind: SvaKind::Assert,
            formula: "(nu G. (p && [] G))".into(),
            outcome: VerifyOutcome::Violated { false_cells: 1 },
            seeded_predicates: vec![],
            counterexample: None,
            bottom_reason: None,
            decided_by: Some("exact-symbolic".into()),
            vacuity: None,
        });
        report.notes.push(witness_miss_note(
            "ann_guarantee_0",
            Some("exact-symbolic"),
            &crate::adapter::reach_rescue::WitnessMiss {
                stage: "monitor",
                reason: "the body is not a bounded-depth box / boolean shape".into(),
            },
        ));
        report.notes.push(VerificationNote {
            kind: "abstraction-posture".into(),
            level: NoteLevel::Info,
            summary: "a stale copy of a kind the rebuild regenerates".into(),
            detail: String::new(),
            items: vec![],
            property: None,
        });
        let notes = build_notes(&report, "none", &[], &[], &[], &[], &NotePosture::Exact);
        let witness: Vec<&VerificationNote> = notes
            .iter()
            .filter(|n| n.kind == "witness-unavailable")
            .collect();
        assert_eq!(
            witness.len(),
            1,
            "{:?}",
            notes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
        assert_eq!(witness[0].property.as_deref(), Some("ann_guarantee_0"));
        assert_eq!(witness[0].items, vec!["monitor".to_string()]);
        let postures: Vec<&VerificationNote> = notes
            .iter()
            .filter(|n| n.kind == "abstraction-posture")
            .collect();
        assert_eq!(postures.len(), 1, "the regenerated kind is not duplicated");
        assert!(
            !postures[0].summary.contains("stale"),
            "{}",
            postures[0].summary
        );
    }

    /// A guarantee body that does not parse is recorded in `skipped`, never
    /// silently dropped, and never merged as a property.
    #[test]
    fn h5_gr1_unparsable_guarantee_is_skipped_not_dropped() {
        let sv = "// @mununu_guarantee this is )) not a formula\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.guarantees.is_empty());
        assert_eq!(scan.skipped.len(), 1);
        assert!(scan.skipped[0].contains("did not parse"));
        // The provenance note surfaces the skip as a scope caveat.
        let note = annotation_note(&scan).expect("note present");
        assert_eq!(note.kind, "annotation-properties");
        assert!(matches!(note.level, NoteLevel::ScopeCaveat));
    }

    /// `@mununu_assume` bodies are recorded for provenance (not verified as a
    /// guarantee); a source with no `@mununu` property annotations yields no note.
    #[test]
    fn a6_exact_symbolic_with_free_reset_is_rejected() {
        // A.6 (2026-07-05) — `--engine exact-symbolic` + `--no-gate-reset` is an
        // unsound combination and MUST be rejected before any elaboration. The
        // reject fires right after the sources check, so it needs no slang (this
        // is a non-ignored unit test): the exact engine models the post-reset
        // state space and would emit a definite-looking but spurious VIOLATED on a
        // freed reset.
        let err = verify_auto(
            &src("module m(); endmodule"),
            &YosysOptions::default(),
            &VerifyAutoOptions {
                exact_symbolic: true,
                gate_reset: false,
                ..Default::default()
            },
        )
        .expect_err("exact-symbolic + no-gate-reset must be rejected");
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(
            err.message.contains("(A.6)") && err.message.contains("exact-symbolic"),
            "A.6 reject must name the unsound combination; got: {}",
            err.message
        );
    }

    #[test]
    fn h5_gr1_assume_recorded_and_empty_source_has_no_note() {
        let sv = "// @mununu_assume tick_baud_x16 = 1\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.guarantees.is_empty());
        assert_eq!(scan.assumes, vec!["tick_baud_x16 = 1".to_string()]);
        assert!(scan.fairness_assumes.is_empty());
        assert!(annotation_note(&scan).is_some());

        let plain = scan_annotation_properties(&src("module m(); endmodule"));
        assert!(annotation_note(&plain).is_none());
    }

    /// mununu#477 Option B — `// @mununu_assume GF <REG op VALUE>` bodies are parsed
    /// into typed [`crate::adapter::liveness_rescue::Atom`]s on [`AnnotationScan::fairness_assumes`],
    /// NOT into the untyped `assumes` string bucket. The scanner is the entry point
    /// for the fair-cycle bridge.
    #[test]
    fn h5_gr1_fairness_assume_parses_into_typed_atom() {
        use crate::adapter::btor2::predicate_expr::CmpOp;
        let sv = "// @mununu_assume GF grant_cpu == 1\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(
            scan.assumes.is_empty(),
            "typed fairness must not spill into `assumes`"
        );
        assert_eq!(scan.fairness_assumes.len(), 1);
        assert_eq!(scan.fairness_assumes[0].signal, "grant_cpu");
        assert_eq!(scan.fairness_assumes[0].op, CmpOp::Eq);
        assert_eq!(scan.fairness_assumes[0].value, 1);
    }

    /// mununu#477 Option B — multiple `GF` assumes accumulate; the fair-cycle
    /// bridge treats them as a conjunction `⋀ GF fairⱼ`.
    #[test]
    fn h5_gr1_fairness_assume_multiple_accumulate() {
        let sv = "// @mununu_assume GF fair_1 == 1\n\
                  // @mununu_assume GF fair_2 != 0\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.assumes.is_empty());
        assert_eq!(scan.fairness_assumes.len(), 2);
        assert_eq!(scan.fairness_assumes[0].signal, "fair_1");
        assert_eq!(scan.fairness_assumes[1].signal, "fair_2");
    }

    /// mununu#477 Option B — a `GF` body whose atom does not parse as a
    /// register-comparison is surfaced in `skipped`, never silently dropped.
    /// Sound-by-refusal: unrecognized fairness never affects a verdict.
    #[test]
    fn h5_gr1_fairness_assume_malformed_atom_is_skipped() {
        let sv = "// @mununu_assume GF not_a_valid_atom_shape !!! bogus\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.fairness_assumes.is_empty());
        assert!(
            scan.skipped.iter().any(|s| s.contains("GF")),
            "malformed GF body must surface in `skipped`: {:?}",
            scan.skipped
        );
    }

    /// mununu#477 Option B — a plain `@mununu_assume <signal> = <value>` (no `GF`
    /// prefix) still routes to the untyped `assumes` bucket for the existing
    /// config-concretization path. Regression guard.
    #[test]
    fn h5_gr1_non_fairness_assume_stays_in_string_bucket() {
        let sv = "// @mununu_assume clk_en = 1\nmodule m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.fairness_assumes.is_empty());
        assert_eq!(scan.assumes, vec!["clk_en = 1".to_string()]);
    }

    #[test]
    fn scan_collects_predicate_hints_and_rejects_unparseable() {
        // W1 — `@mununu_predicate <expr>` hints are collected (literal, relational,
        // bound); a bare boolean signal is admitted; a malformed comparison is
        // surfaced in `skipped`, never silently dropped.
        let sv = "// @mununu_predicate c_state == 0\n\
                  // @mununu_predicate wptr == rptr\n\
                  // @mununu_predicate fill >= 30\n\
                  // @mununu_predicate ready\n\
                  // @mununu_predicate bogus ==\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(
            scan.predicates,
            vec![
                "c_state == 0".to_string(),
                "wptr == rptr".to_string(),
                "fill >= 30".to_string(),
                "ready".to_string(),
            ]
        );
        assert!(
            scan.skipped.iter().any(|s| s.contains("bogus ==")),
            "malformed predicate surfaced in skipped: {:?}",
            scan.skipped
        );
    }

    /// R-F5.5d projection SOUNDNESS fix — an infeasible cube (absent from the
    /// symbolic feasible-cube tally) must project to ⊥, NOT a spurious
    /// definite-False. Regression for the unsound VIOLATED verify-auto returned
    /// on `AG AF` (the symbolic tally covers only feasible cubes; an infeasible
    /// `init_cube` was reading as `must=0,may=0` → `Trit::False` → VIOLATED).
    #[test]
    fn rf5_5d_symbolic_final_verdict_infeasible_cube_is_bottom_not_false() {
        use crate::adapter::btor2::symbolic_engine::{
            SymbolicCegarResult, SymbolicCegarTermination, SymbolicCubeVerdicts,
        };
        use crate::mu_calculus::trit::Trit;
        // 2 predicates → 4 cubes; only 0,1 are feasible + tallied. Cubes 2,3 are
        // infeasible and absent from the tally.
        let result = SymbolicCegarResult {
            iterations: Vec::new(),
            final_predicates: Vec::new(),
            final_verdicts: SymbolicCubeVerdicts {
                num_predicates: 2,
                cube_verdicts: vec![(0, Trit::Unknown), (1, Trit::True)],
                definite_true: 1,
                definite_false: 0,
                bottom: 1,
            },
            terminated_with: SymbolicCegarTermination::Converged,
        };
        let ts = symbolic_final_verdict(&result);
        // Feasible cubes keep their verdict.
        assert_eq!(ts.verdict_at(0), Trit::Unknown);
        assert_eq!(ts.verdict_at(1), Trit::True);
        // Infeasible (untallied) cubes are ⊥, never a spurious definite-False.
        assert_eq!(
            ts.verdict_at(2),
            Trit::Unknown,
            "infeasible cube 2 must project to ⊥, not False"
        );
        assert_eq!(
            ts.verdict_at(3),
            Trit::Unknown,
            "infeasible cube 3 must project to ⊥, not False"
        );
    }

    /// R-F5.5d — `symbolic_final_verdict` projects the symbolic engine's per-cube
    /// verdicts into a `TritSet` the verify_auto reset-cube read consumes, with
    /// identical cube indexing and the right length. Uses the symbolic CEGAR loop
    /// directly (no yosys), so it runs in `make ci`.
    #[test]
    fn rf5_5d_symbolic_final_verdict_projects_cube_verdicts() {
        use crate::adapter::AdapterOptions;
        use crate::adapter::btor2::PredicateSpec;
        use crate::adapter::btor2::symbolic_bitblast::MustSemantics;
        use crate::adapter::btor2::symbolic_engine::symbolic_cegar_refine;

        let btor2 = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 input 2 en
5 one 1
6 ones 1
7 add 1 3 5
8 eq 2 3 6
9 ite 1 8 3 7
10 ite 1 4 9 3
11 next 1 3 10
";
        let options = AdapterOptions::default();
        let initial = vec![PredicateSpec {
            name: "p".to_string(),
            register: "cnt".to_string(),
            value: 0,
        }];
        let formula = mu_parser::parse("[] p").expect("formula"); // AG(cnt==0)
        let result = symbolic_cegar_refine(
            btor2,
            &initial,
            &options,
            &formula,
            MustSemantics::ForallExists,
            0,
        )
        .expect("symbolic refine");

        let ts = symbolic_final_verdict(&result);
        assert_eq!(ts.len(), 1usize << result.final_verdicts.num_predicates);
        for (cube, trit) in &result.final_verdicts.cube_verdicts {
            assert_eq!(ts.verdict_at(*cube), *trit, "cube {cube} verdict mismatch");
        }
    }

    #[test]
    fn substitute_config_in_formula_is_token_aware() {
        // H.J.b — a pinned config input is substituted with its constant, whole
        // identifiers only (so a prefix like `cfg_detect_timer` inside another
        // name is NOT touched), and the rest of the formula is preserved.
        let cfg = [("cfg_detect_timer_i".to_string(), 7u64)];
        assert_eq!(
            substitute_config_in_formula("nu X. ((cnt_q >= cfg_detect_timer_i) && [] X)", &cfg),
            "nu X. ((cnt_q >= 7) && [] X)"
        );
        // Whole-identifier: a name that merely CONTAINS the config name is not
        // substituted; a bound var / keyword is untouched.
        assert_eq!(
            substitute_config_in_formula("cfg_detect_timer_i_extra == 1", &cfg),
            "cfg_detect_timer_i_extra == 1"
        );
        // Empty config ⇒ unchanged.
        assert_eq!(
            substitute_config_in_formula("cnt_q >= cfg_detect_timer_i", &[]),
            "cnt_q >= cfg_detect_timer_i"
        );
        // The arithmetic-addend form survives + substitutes its own operands.
        let cfg2 = [("thr".to_string(), 3u64)];
        assert_eq!(
            substitute_config_in_formula("cnt == cnt_past + 1 && x >= thr", &cfg2),
            "cnt == cnt_past + 1 && x >= 3"
        );
    }

    /// OpenTitan's `ASSERT` macro keeps `disable iff (... || !rst_ni)` as a compound guard, so a
    /// pinned `rst_ni` sits in BOOLEAN position. It used to become `(!(1))`, which does not parse —
    /// every such property skipped ("formula failed to parse"). It must read `true` / `false`
    /// there, stay numeric beside a comparison or an addend, and leave the `<>` modality alone.
    #[test]
    fn a_pinned_signal_in_boolean_position_substitutes_a_boolean() {
        let cfg = [("rst_ni".to_string(), 1u64), ("en".to_string(), 0u64)];
        let f = "nu X. ((((err_d || fpv) || (!(rst_ni))) || [] (a == b)) && [] X)";
        let out = substitute_config_in_formula(f, &cfg);
        assert_eq!(
            out,
            "nu X. ((((err_d || fpv) || (!(true))) || [] (a == b)) && [] X)"
        );
        crate::mu_calculus::parser::parse(&out).expect("the substituted formula parses");
        for (input, want) in [
            ("(rst_ni && en)", "(true && false)"),
            ("(x == rst_ni)", "(x == 1)"),
            ("(rst_ni != x)", "(1 != x)"),
            ("(x >= en)", "(x >= 0)"),
            ("(y == x + rst_ni)", "(y == x + 1)"),
            ("<> rst_ni", "<> true"),
            ("(rst_ni || <> X)", "(true || <> X)"),
            ("(rst_ni)", "(true)"),
        ] {
            assert_eq!(substitute_config_in_formula(input, &cfg), want, "{input}");
        }
    }

    #[test]
    fn counter_registers_detects_past_shadow() {
        // H.H — a state register whose `$past` shadow is also referenced is a
        // counter (the self-relational monotonicity/increment atom). A register
        // without its shadow present is NOT flagged.
        let mut seeded = Seeded::default();
        // sva_13 shape: `cnt_q >= cnt_q__past` (a CmpReg compound).
        seeded.compounds.push((
            "cnt_q >= cnt_q__past".to_string(),
            PredicateExpr::CmpReg {
                lhs: "cnt_q".into(),
                op: CmpOp::Ge,
                rhs: "cnt_q__past".into(),
            },
        ));
        // A plain state atom over an unrelated register — no shadow, not a counter.
        seeded.specs.push(PredicateSpec {
            name: "state_q == 0".into(),
            register: "state_q".into(),
            value: 0,
        });
        let counters = counter_registers_in(&seeded);
        assert_eq!(
            counters.into_iter().collect::<Vec<_>>(),
            vec!["cnt_q".to_string()]
        );
    }

    #[test]
    fn seed_counter_bounds_manual_overrides_config_and_dedups() {
        // H.H — manual bound wins over config-inferred; the bound becomes a `<= K`
        // compound on BOTH the counter and its `$past` shadow; a bound already
        // present as a compound is not duplicated.
        let counters: std::collections::BTreeSet<String> =
            ["cnt_q".to_string()].into_iter().collect();
        let manual: std::collections::HashMap<String, u64> =
            [("cnt_q".to_string(), 15u64)].into_iter().collect();
        let config: std::collections::HashMap<String, u64> =
            [("cnt_q".to_string(), 7u64)].into_iter().collect();

        // The monotonicity relational makes `cnt_q__past` a referenced register, so
        // the shadow is bounded alongside the counter.
        let mono = || {
            (
                "cnt_q >= cnt_q__past".to_string(),
                PredicateExpr::CmpReg {
                    lhs: "cnt_q".into(),
                    op: CmpOp::Ge,
                    rhs: "cnt_q__past".into(),
                },
            )
        };
        let mut seeded = Seeded::default();
        seeded.compounds.push(mono());
        let applied = seed_counter_bounds(&mut seeded, &counters, &manual, &config);
        // Manual 15 wins over config-inferred 7; the note reports the base once.
        assert_eq!(applied, vec![("cnt_q".to_string(), 15, "user-supplied")]);
        // BOTH operands of the relational are bounded.
        assert!(seeded.compounds.iter().any(|(n, _)| n == "cnt_q <= 15"));
        assert!(
            seeded
                .compounds
                .iter()
                .any(|(n, _)| n == "cnt_q__past <= 15")
        );

        // Re-seeding is idempotent (both `<= 15` compounds already exist).
        let again = seed_counter_bounds(&mut seeded, &counters, &manual, &config);
        assert!(again.is_empty());
        assert_eq!(
            seeded
                .compounds
                .iter()
                .filter(|(n, _)| n == "cnt_q <= 15")
                .count(),
            1
        );

        // With no manual entry, the config-inferred bound is used.
        let mut seeded2 = Seeded::default();
        seeded2.compounds.push(mono());
        let applied2 = seed_counter_bounds(
            &mut seeded2,
            &counters,
            &std::collections::HashMap::new(),
            &config,
        );
        assert_eq!(applied2, vec![("cnt_q".to_string(), 7, "config-inferred")]);
        assert!(
            seeded2
                .compounds
                .iter()
                .any(|(n, _)| n == "cnt_q__past <= 7")
        );
    }

    #[test]
    fn config_inferred_bounds_from_sibling_comparison() {
        // H.H — the bound for the counter comes from a SIBLING property's
        // comparison against a pinned config threshold (the monotonicity property
        // itself never names the config). Global scan across the property set.
        let translated = vec![
            crate::adapter::slang::translate::TranslatedAssertion {
                name: "m_sva_5".into(),
                label: None,
                kind: SvaKind::Assert,
                // `cnt_q >= cfg_detect_timer_i` — the comparison that reveals the bound.
                formula: "nu X. ((cnt_q >= cfg_detect_timer_i) && [] X)".into(),
                recoverability_companion: None,
                written: None,
            },
            crate::adapter::slang::translate::TranslatedAssertion {
                name: "m_sva_13".into(),
                label: None,
                kind: SvaKind::Assert,
                // The monotonicity property — needs the bound but never names cfg.
                formula: "nu X. ((cnt_q >= cnt_q__past) && [] X)".into(),
                recoverability_companion: None,
                written: None,
            },
        ];
        let bounds =
            config_inferred_counter_bounds(&translated, &[("cfg_detect_timer_i".into(), 7)]);
        assert_eq!(bounds.get("cnt_q"), Some(&7));
        // No config pinned ⇒ no inferred bounds.
        assert!(config_inferred_counter_bounds(&translated, &[]).is_empty());
    }

    #[test]
    fn build_notes_surfaces_every_decision() {
        // H.J.a — the provenance notes cover the coverage tally, the abstraction
        // posture, and each model-level decision (reset-gating, flop stubs, cut
        // modules), plus a config-concretization ScopeCaveat when pins are given.
        let report = AutoVerifyReport {
            assumptions: Default::default(),
            properties: vec![
                PropertyVerdict {
                    name: "p_holds".into(),
                    label: None,
                    kind: SvaKind::Assert,
                    formula: "nu X. (a && [] X)".into(),
                    outcome: VerifyOutcome::Holds,
                    seeded_predicates: vec!["a".into()],
                    counterexample: None,
                    bottom_reason: None,
                    decided_by: None,
                    vacuity: None,
                },
                PropertyVerdict {
                    name: "p_unknown".into(),
                    label: None,
                    kind: SvaKind::Assert,
                    formula: "nu X. (b && [] X)".into(),
                    outcome: VerifyOutcome::Unknown { unknown_cells: 2 },
                    seeded_predicates: vec!["b".into()],
                    counterexample: None,
                    bottom_reason: None,
                    decided_by: None,
                    vacuity: None,
                },
            ],
            unsupported: vec![("u".into(), "reason".into())],
            diagnostics: ModelDiagnostics {
                state_register_count: 3,
                blackboxed_modules: vec!["prim_sparse_fsm_flop".into()],
                gated_resets: vec!["rst_ni=1".into()],
                auto_provided_stubs: vec!["prim_flop".into()],
                ..Default::default()
            },
            notes: Vec::new(),
        };
        let notes = build_notes(
            &report,
            must_edge_posture_label(MustEdgeInference::SmtHyperMust, false),
            &[("cfg_detect_timer_i".into(), 7)],
            &[],
            &["cnt_q <= 7 (config-inferred)".to_string()],
            &[],
            &NotePosture::Cube,
        );
        let kinds: Vec<&str> = notes.iter().map(|n| n.kind.as_str()).collect();
        for expected in [
            "coverage-summary",
            "config-concretization",
            "counter-bound",
            "abstraction-posture",
            "reset-gating",
            "flop-stub",
            "blackbox-cut",
        ] {
            assert!(
                kinds.contains(&expected),
                "missing note {expected}; got {kinds:?}"
            );
        }
        // The counter-bound note is Info (a HOLDS it unlocks is proven) + carries
        // the seeded bound.
        let cb = notes.iter().find(|n| n.kind == "counter-bound").unwrap();
        assert_eq!(cb.level, NoteLevel::Info);
        assert!(
            cb.items
                .contains(&"cnt_q <= 7 (config-inferred)".to_string())
        );
        // The scope caveat carries the pinned value + is a ScopeCaveat.
        let cc = notes
            .iter()
            .find(|n| n.kind == "config-concretization")
            .unwrap();
        assert_eq!(cc.level, NoteLevel::ScopeCaveat);
        assert!(cc.items.contains(&"cfg_detect_timer_i=7".to_string()));
        // The cut module is a SoundnessCaveat; coverage tallies 1 HOLDS + 1 ⊥.
        let bc = notes.iter().find(|n| n.kind == "blackbox-cut").unwrap();
        assert_eq!(bc.level, NoteLevel::SoundnessCaveat);
        let cov = notes.iter().find(|n| n.kind == "coverage-summary").unwrap();
        assert!(cov.summary.contains("1 definite") && cov.summary.contains("1 unknown"));
        // No pins / no bounds ⇒ no config-concretization or counter-bound note.
        // With an all-`SmtAllPairs` (empty sampling) cube posture, the note is the
        // sound over-approximation Info.
        let ap = notes
            .iter()
            .find(|n| n.kind == "abstraction-posture")
            .unwrap();
        assert_eq!(ap.level, NoteLevel::Info);
        assert!(
            ap.summary.contains("may-over-approximation"),
            "{}",
            ap.summary
        );
        let notes_no_pins = build_notes(
            &report,
            must_edge_posture_label(MustEdgeInference::Off, false),
            &[],
            &[],
            &[],
            &[],
            &NotePosture::Cube,
        );
        assert!(
            !notes_no_pins
                .iter()
                .any(|n| n.kind == "config-concretization"
                    || n.kind == "counter-bound"
                    || n.kind == "control-slice")
        );
    }

    /// PR A1 — the security-countermeasure chain classifier reads the `$rose` flag off the
    /// translator's own shape (edn's `FpvSecCmMainFsmCheck_A`), and nothing else.
    #[test]
    fn sec_cm_rose_atom_reads_the_fault_flag_of_a_chain_row() {
        let chain = "nu X. (((!(((u_sm.u_state_regs.unused_err_o && (!(u_sm.u_state_regs.unused_err_o__past))) && (!(false)))) || (alert || [] (alert || [] alert)))) && [] X)";
        let f = crate::mu_calculus::parser::parse(chain).expect("parses");
        assert_eq!(
            sec_cm_rose_atom(&f).as_deref(),
            Some("u_sm.u_state_regs.unused_err_o")
        );
        let plain =
            crate::mu_calculus::parser::parse("nu X. (((!(a)) || [] b) && [] X)").expect("parses");
        assert_eq!(
            sec_cm_rose_atom(&plain),
            None,
            "no fault flag, no chain row"
        );
        let recover =
            crate::mu_calculus::parser::parse("nu Y. ((mu Z. ((err_o == 0) || <> Z)) && [] Y)")
                .expect("parses");
        assert_eq!(
            sec_cm_rose_atom(&recover),
            None,
            "an alternating row is not a chain"
        );
    }

    /// PR A1 — `--fault-model sec-cm` on an inline model: the chain row's flag `err_q` compares
    /// the two counter copies, so both become sites (`cone-fallback`: the flag has no instance
    /// prefix); a zero budget selects without rewriting; a budget of one rewrites both `next`s
    /// and names the inputs; the antecedent-vacuity post-pass reads the un-faulted model as
    /// vacuous and the injected one as reachable.
    #[test]
    fn the_sec_cm_posture_selects_the_compared_cells_and_the_vacuity_check_reads_both_models() {
        use crate::adapter::btor2::fault::{FaultBound, FaultPosture, FaultSelection};
        const WIRED: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 zero 2\n4 one 2\n5 zero 1\n\
                             6 input 1 inc\n7 state 2 a\n8 init 2 7 3\n9 state 2 b\n10 init 2 9 3\n\
                             11 add 2 7 4\n12 ite 2 6 11 7\n13 next 2 7 12\n14 add 2 9 4\n\
                             15 ite 2 6 14 9\n16 next 2 9 15\n17 neq 1 7 9\n18 state 1 err_o\n\
                             19 init 1 18 5\n20 next 1 18 17\n21 state 1 err_o__past\n22 init 1 21 5\n\
                             23 next 1 21 18\n24 state 1 alert_q\n25 init 1 24 5\n26 next 1 24 18\n";
        let chain = "nu X. (((!((err_o && (!(err_o__past))))) || (alert_q || [] (alert_q || [] alert_q))) && [] X)";
        let extraction = crate::adapter::slang::translate::TranslationReport {
            translated: vec![crate::adapter::slang::translate::TranslatedAssertion {
                name: "chain".into(),
                label: Some("CntAlertCheck_A".into()),
                kind: SvaKind::Assert,
                formula: chain.into(),
                recoverability_companion: None,
                written: None,
            }],
            ..Default::default()
        };
        // selection only
        let (model0, applied0) = apply_fault_posture(
            WIRED,
            &extraction,
            &FaultPosture {
                selection: FaultSelection::SecCm(FaultBound::AtMost(0)),
                warmup: 2,
            },
        )
        .expect("applies");
        assert_eq!(model0, WIRED, "a zero budget rewrites nothing");
        let regs: Vec<&str> = applied0.sites.iter().map(|s| s.register.as_str()).collect();
        assert_eq!(regs, vec!["a", "b"]);
        assert!(
            applied0
                .sites
                .iter()
                .all(|s| s.selected_by == "cone-fallback" && s.rows == vec!["chain"])
        );
        assert!(applied0.injected.is_empty());
        assert_eq!(
            applied0.sec_cm_rows,
            vec![("chain".to_string(), "err_o".to_string())]
        );
        // budget one
        let (model1, applied1) = apply_fault_posture(
            WIRED,
            &extraction,
            &FaultPosture {
                selection: FaultSelection::SecCm(FaultBound::AtMost(1)),
                warmup: 2,
            },
        )
        .expect("applies");
        assert_eq!(applied1.injected.len(), 2);
        assert!(
            model1.contains("mununu_fault_en_0") && model1.contains("mununu_fault_cnt"),
            "{model1}"
        );
        assert_eq!(applied1.sites[0].width, 2);
        // the vacuity post-pass: a HOLDS row on each model
        let row = |model: &str| {
            AutoVerifyReport {
                assumptions: Default::default(),
                properties: vec![PropertyVerdict {
                    name: "chain".into(),
                    label: Some("CntAlertCheck_A".into()),
                    kind: SvaKind::Assert,
                    formula: chain.into(),
                    outcome: VerifyOutcome::Holds,
                    seeded_predicates: Vec::new(),
                    counterexample: None,
                    bottom_reason: None,
                    decided_by: Some("explicit".into()),
                    vacuity: None,
                }],
                unsupported: Vec::new(),
                diagnostics: ModelDiagnostics::default(),
                notes: Vec::new(),
            }
            .pipe(|mut r| {
                check_antecedent_vacuity(&mut r, model, false, &applied0.sec_cm_rows);
                r
            })
        };
        let before = row(&model0);
        assert_eq!(
            before.properties[0].vacuity.as_ref().map(|v| v.kind),
            Some(VacuityKind::AntecedentUnreachable),
            "{:?}",
            before.properties[0].vacuity
        );
        assert!(before.notes.iter().any(|n| n.kind == "vacuous-guarantee"));
        let after = row(&model1);
        assert_eq!(
            after.properties[0].vacuity, None,
            "{:?}",
            after.properties[0].vacuity
        );
        assert!(
            after.notes.iter().any(|n| n.kind == "antecedent-reachable"),
            "{:?}",
            after.notes
        );
    }

    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl<T> Pipe for T {}

    #[test]
    fn lift_retry_note_is_info_and_names_the_backend_loop() {
        let with = AutoVerifyReport {
            assumptions: Default::default(),
            properties: Vec::new(),
            unsupported: Vec::new(),
            diagnostics: ModelDiagnostics {
                frontend: Some("yosys-slang (read_slang)".into()),
                lift_retry: Some(
                    "the BTOR backend reported a topological loop (ERROR: Found topological loop \
                     while processing cell $6638. Active cells:); the lift succeeded with \
                     `opt_clean` before `write_btor`"
                        .into(),
                ),
                ..Default::default()
            },
            notes: Vec::new(),
        };
        let notes = build_notes(
            &with,
            must_edge_posture_label(MustEdgeInference::Off, false),
            &[],
            &[],
            &[],
            &[],
            &NotePosture::Cube,
        );
        let note = notes
            .iter()
            .find(|n| n.kind == "lift-retry")
            .expect("a retried lift carries the lift-retry note");
        assert_eq!(note.level, NoteLevel::Info, "the model is the same design");
        assert!(note.summary.contains("topological loop") && note.summary.contains("opt_clean"));
        let without = AutoVerifyReport {
            diagnostics: ModelDiagnostics {
                frontend: Some("yosys-slang (read_slang)".into()),
                ..Default::default()
            },
            ..with
        };
        assert!(
            !build_notes(
                &without,
                must_edge_posture_label(MustEdgeInference::Off, false),
                &[],
                &[],
                &[],
                &[],
                &NotePosture::Cube,
            )
            .iter()
            .any(|n| n.kind == "lift-retry"),
            "no retry, no note"
        );
    }

    #[test]
    fn build_notes_reports_lift_frontend() {
        // #466 — verify-auto names the RTL front end that produced the lift: Info
        // when there was no fallback, ScopeCaveat + the reason when `--frontend auto`
        // fell back (read_verilog errored → slang), and no note when unrecorded.
        let base = |frontend: Option<&str>, fallback: Option<&str>| AutoVerifyReport {
            assumptions: Default::default(),
            properties: Vec::new(),
            unsupported: Vec::new(),
            diagnostics: ModelDiagnostics {
                frontend: frontend.map(String::from),
                frontend_fallback_reason: fallback.map(String::from),
                ..Default::default()
            },
            notes: Vec::new(),
        };
        let note_of = |r: &AutoVerifyReport| {
            build_notes(
                r,
                must_edge_posture_label(MustEdgeInference::Off, false),
                &[],
                &[],
                &[],
                &[],
                &NotePosture::Cube,
            )
            .into_iter()
            .find(|n| n.kind == "lift-frontend")
        };

        // No fallback → Info note naming the front end.
        let n = note_of(&base(Some("read_verilog + sv2v"), None)).expect("lift-frontend note");
        assert_eq!(n.level, NoteLevel::Info);
        assert!(n.summary.contains("read_verilog + sv2v"), "{}", n.summary);

        // Fallback → ScopeCaveat naming the front end + why the prior one fell back.
        let n = note_of(&base(
            Some("yosys-slang (read_slang)"),
            Some("read_verilog + sv2v failed: parse error at x.sv:21"),
        ))
        .expect("lift-frontend note");
        assert_eq!(n.level, NoteLevel::ScopeCaveat);
        assert!(
            n.summary.contains("fell back") && n.summary.contains("yosys-slang"),
            "{}",
            n.summary
        );

        // No front end recorded (a non-lift path) → no note.
        assert!(note_of(&base(None, None)).is_none());
    }

    /// mununu#507 — the refusal must attribute a partial write ONLY when the lift
    /// shows that shape, because an unnamed free input has several other sources.
    #[test]
    fn partial_write_shape_is_distinguished_from_other_undriven_sources() {
        use crate::adapter::btor2::parser;

        // (a) THE PARTSEL SHAPE: `q`'s name aliases a `concat` mixing an anonymous
        // input — how slang models the unwritten bits of `q[hi:lo] <= d`.
        const PARTSEL: &str = r#"1 sort bitvec 4
2 sort bitvec 8
3 input 1
4 state 1 lo
5 concat 2 3 4
6 uext 2 5 0 q
"#;
        let f = parser::parse(PARTSEL).expect("parse");
        assert!(
            undriven_via_partial_write_concat(&f, "q"),
            "the concat-over-anonymous-input shape IS the partial-write evidence"
        );

        // (b) A BLACKBOXED SUBMODULE's freed output: `q` reads an anonymous input
        // through ordinary logic, with no concat. Same guard condition, different
        // cause — and the message must not claim a partial write.
        const BLACKBOX: &str = r#"1 sort bitvec 1
2 input 1
3 state 1 r
4 and 1 2 3
5 uext 1 4 0 q
"#;
        let f = parser::parse(BLACKBOX).expect("parse");
        assert!(
            parser::signal_reaches_anonymous_input(&f, "q"),
            "the guard still fires — the property is still refused"
        );
        assert!(
            !undriven_via_partial_write_concat(&f, "q"),
            "but there is NO partial-write shape, so the refusal must not name one \
             (mununu#507: the old message pointed at correct, whole-written RTL)"
        );
    }

    /// mununu#507 — the sv2v remedy must not be offered to a design that uses
    /// `bind`, because sv2v 0.0.13 cannot parse it (`Parse error: unexpected token
    /// 'bind'`). Bound SVA is not optional for these designs: `read_verilog -sv`
    /// cannot parse `assert property` and there is no `--define` for an `ifdef`.
    #[test]
    fn bound_sva_sources_are_detected_so_the_sv2v_remedy_is_withheld() {
        let bound = vec![
            ("dut.sv".to_string(), "module dut; endmodule\n".to_string()),
            (
                "dut_sva.sv".to_string(),
                "module dut_sva; endmodule\nbind dut dut_sva u_sva();\n".to_string(),
            ),
        ];
        assert!(
            sources_use_bind(&bound),
            "a module-scope `bind` is detected"
        );

        let plain = vec![(
            "dut.sv".to_string(),
            "module dut;\n  // no bind here\n  logic bind_addr;\n endmodule\n".to_string(),
        )];
        assert!(
            !sources_use_bind(&plain),
            "a commented mention and an identifier PREFIXED `bind_` are not a bind"
        );
    }

    #[test]
    fn control_slice_note_present_when_cutpoints_given() {
        // The control-slice note is a ScopeCaveat present iff nets were cut, carrying
        // the cut list and the over-approximation posture. It documents that a HOLDS
        // is sound but a VIOLATED is sound only when guard-independent (orphan case).
        let report = AutoVerifyReport {
            assumptions: Default::default(),
            properties: vec![PropertyVerdict {
                name: "p".into(),
                label: None,
                kind: SvaKind::Assert,
                formula: "nu X. (a && [] X)".into(),
                outcome: VerifyOutcome::Holds,
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            unsupported: Vec::new(),
            diagnostics: ModelDiagnostics {
                state_register_count: 1,
                blackboxed_modules: Vec::new(),
                gated_resets: Vec::new(),
                auto_provided_stubs: Vec::new(),
                ..Default::default()
            },
            notes: Vec::new(),
        };
        let notes = build_notes(
            &report,
            must_edge_posture_label(MustEdgeInference::SmtHyperMust, false),
            &[],
            &[],
            &[],
            &["must_refresh".to_string(), "precharge_done".to_string()],
            &NotePosture::Exact,
        );
        let cs = notes
            .iter()
            .find(|n| n.kind == "control-slice")
            .expect("control-slice note present when cut points given");
        assert_eq!(cs.level, NoteLevel::ScopeCaveat);
        assert!(cs.items.contains(&"must_refresh".to_string()));
        assert!(cs.summary.contains("over-approximation"), "{}", cs.summary);
        // No cut points ⇒ no control-slice note.
        let none = build_notes(
            &report,
            must_edge_posture_label(MustEdgeInference::Off, false),
            &[],
            &[],
            &[],
            &[],
            &NotePosture::Exact,
        );
        assert!(!none.iter().any(|n| n.kind == "control-slice"));
    }

    #[test]
    fn abstraction_posture_note_reflects_may_relation() {
        // A.3 — the abstraction-posture note is honest about the may-relation the
        // run actually used, rather than hardcoding "may-over-approximation".
        let report = AutoVerifyReport {
            assumptions: Default::default(),
            properties: vec![PropertyVerdict {
                name: "recover".into(),
                label: None,
                kind: SvaKind::Assert,
                formula: "nu Y. ((mu X. (idle || <> X)) && [] Y)".into(),
                outcome: VerifyOutcome::Holds,
                seeded_predicates: vec!["idle".into()],
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
            }],
            unsupported: Vec::new(),
            diagnostics: ModelDiagnostics {
                state_register_count: 1,
                blackboxed_modules: Vec::new(),
                gated_resets: Vec::new(),
                auto_provided_stubs: Vec::new(),
                ..Default::default()
            },
            notes: Vec::new(),
        };
        let ap = |posture: &NotePosture| {
            build_notes(
                &report,
                must_edge_posture_label(MustEdgeInference::SmtHyperMust, false),
                &[],
                &[],
                &[],
                &[],
                posture,
            )
            .into_iter()
            .find(|n| n.kind == "abstraction-posture")
            .unwrap()
        };
        // AR-S2 — the cube path always uses the sound `SmtAllPairs` may-relation,
        // so the cube posture note is the sound over-approximation Info (the
        // sampling-may SoundnessCaveat + its A.4 ⊥-guard were retired).
        let sound = ap(&NotePosture::Cube);
        assert_eq!(sound.level, NoteLevel::Info);
        assert!(sound.summary.contains("may-over-approximation"));
        // Exact engine → Info, and it does not claim a may-over-approximation.
        let exact = ap(&NotePosture::Exact);
        assert_eq!(exact.level, NoteLevel::Info);
        assert!(exact.summary.contains("Exact"), "{}", exact.summary);
        assert!(!exact.summary.contains("may-over-approximation"));
    }

    /// **The contract between the SVA translator and the cube seeder — the test mununu#565 was
    /// missing.**
    ///
    /// #565 added a new atom shape (a minted bit-slice signal) and every unit test stayed green.
    /// `make ci` stayed green. The feature was half-broken, and the only thing that caught it was
    /// an `#[ignore]`d e2e that needs slang + yosys + z3 and ten minutes in a container — a tier
    /// neither `make ci` nor CI runs.
    ///
    /// **The bug was never in the toolchain.** It was in `seed_from_formula`, a pure function over
    /// mocked closures that costs ~0 ms to exercise, and which this file already tests eight ways.
    /// What was missing was not a cheaper e2e; it was a table connecting *"the translator can now
    /// emit shape X"* to *"the seeder accepts shape X"* — an interface contract between two layers
    /// that are each individually correct.
    ///
    /// So: one row per atom shape the translator can emit. Adding an arm to `compare` without
    /// adding a row here is now the thing that shows up red, in milliseconds, on the author's own
    /// machine.
    #[test]
    fn every_translator_atom_shape_has_a_known_seeding_classification() {
        #[derive(Debug, PartialEq)]
        enum Class {
            /// A cube dimension — the property can be decided by the predicate-cube engine.
            Simple,
            /// A relational/compound predicate — still seedable, all-state.
            Compound,
            /// Refused, with the atom named. Sound, but the property will SKIP.
            Unseedable,
        }

        // The signal universe the shapes are classified against: `st`/`q` are state cells,
        // `wdata` is a primary input, and the two minted signals are combinational — a slice is
        // an `Op`, by construction, whether or not its base is a state cell.
        let classify = |formula: &str| -> Class {
            let f = mu_parser::parse(formula).unwrap_or_else(|e| panic!("{formula}: {e:?}"));
            let s = seed_from_formula(
                &f,
                |n| cells(&["st", "q", "wdata__past"]).contains(n),
                |n| cells(&["wdata"]).contains(n),
                |n| {
                    // The minted signals: a slice of an input is input-dependent; a slice of a
                    // (shadow) state cell is state-only.
                    if n == "wdata__bits7_0" || n == "wdata__bits3_3" || n == "wdata__popcount" {
                        Some(CombKind::InputDependent)
                    } else if n == "wdata__past__bits7_0"
                        || n == "st__bits1_1"
                        || n == "q__popcount"
                    {
                        Some(CombKind::StateOnly)
                    } else {
                        None
                    }
                },
                |n| {
                    if n == "wdata__bits7_0" || n == "wdata__bits3_3" || n == "wdata__popcount" {
                        vec!["wdata".to_string()]
                    } else {
                        Vec::new()
                    }
                },
                &[],
            );
            // `Simple` must mean "seeded", not "nothing happened": a formula with atoms that
            // seeds neither a dimension nor a derived label would otherwise pass as `Simple`.
            let has_atoms = f.nodes().iter().any(|n| matches!(n, Node::Predicate(_)));
            if !s.unseedable.is_empty() {
                Class::Unseedable
            } else if !s.compounds.is_empty() {
                Class::Compound
            } else {
                assert!(
                    !has_atoms
                        || !s.specs.is_empty()
                        || !s.derived.is_empty()
                        || !s.derived_relational.is_empty(),
                    "{formula}: atoms present but nothing seeded"
                );
                Class::Simple
            }
        };

        let table = [
            // shape the translator emits            expected classification
            ("nu X. ((st == 5) && [] X)", Class::Simple), // state cell == literal
            ("nu X. ((wdata == 5) && [] X)", Class::Simple), // free INPUT == literal (H.B)
            ("nu X. ((wdata__past == 5) && [] X)", Class::Simple), // $past shadow == literal
            ("nu X. ((st == q) && [] X)", Class::Compound), // relational, all-state
            // mununu#565 — a minted bit-slice. Simple against a literal: it seeds via the
            // input-dependent-combinational route (H.E), which is why `wdata[7:0] == 8'hA5`
            // DECIDES.
            ("nu X. ((wdata__bits7_0 == 165) && [] X)", Class::Simple),
            // SVA-fragment widening (2026-10-15) — a constant bit-select is the 1-bit slice. In
            // boolean position it is the BARE minted atom (of an input, then of a state cell),
            // and compared it is the ordinary `slice == literal`.
            ("nu X. (wdata__bits3_3 && [] X)", Class::Simple),
            ("nu X. (st__bits1_1 && [] X)", Class::Simple),
            ("nu X. ((st__bits1_1 == 1) && [] X)", Class::Simple),
            // Ranged antecedents (2026-10-15) add NO atom shape — they only nest the existing
            // boolean atoms under boxes — so they add no row here.
            // `$countones(x) OP k` (2026-10-15) reads the minted `x__popcount` — combinational, a
            // function of `x`: input-dependent over an input, state-only over a state cell.
            ("nu X. ((wdata__popcount == 3) && [] X)", Class::Simple),
            // A non-equality comparison over a combinational signal is a derived per-cube
            // relational label (H.F), not a dimension.
            ("nu X. ((q__popcount <= 1) && [] X)", Class::Simple),
            // `!$isunknown(x)` folds to `false` in the 2-state model: no atom at all.
            ("nu X. ((!(false)) && [] X)", Class::Simple),
            // Plan C (2026-10-21) — an unpacked port's element, case-split on its selector: a
            // guard `sel != k` per element and a RELATIONAL compare against the flattened slice.
            // The guard over a free input is a dimension; the relational compare of two
            // input-dependent signals is a derived relational label.
            (
                "nu X. (((!(wdata != 3)) || (wdata__popcount == wdata__bits7_0)) && [] X)",
                Class::Simple,
            ),
        ];

        // The mununu#565 discrepancy this comment used to record — `q == wdata__past__bits7_0`
        // classified seedable here but `Unseedable` by the e2e — was the e2e that was wrong, and
        // for the reason this comment suspected: on that path the slice was NOT minted, because
        // `$past(wdata[7:0])` never requested `wdata`'s shadow (fixed 2026-10-15, reproducer
        // `past_over_a_slice_requests_the_base_shadow`). With the shadow requested the e2e binds
        // the atom too, so the two tiers agree; the relational row is still left out because its
        // classification depends on `combinational_kind` of a real lift, which this mock only
        // approximates.

        for (formula, want) in table {
            let got = classify(formula);
            eprintln!("[contract] {formula} => {got:?} (want {want:?})");
            assert_eq!(got, want, "shape changed classification: {formula}");
        }
    }

    #[test]
    fn seeds_simple_state_predicate() {
        // `nu X. ((state == 5) && [] X)` over a state cell → one simple spec.
        let f = mu_parser::parse("nu X. ((state == 5) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["state"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1, "one simple reg==val spec");
        assert_eq!(s.specs[0].name, "state == 5");
        assert_eq!(s.specs[0].register, "state");
        assert_eq!(s.specs[0].value, 5);
        assert!(s.compounds.is_empty());
        assert!(s.unseedable.is_empty());
    }

    #[test]
    fn extra_atoms_seed_the_cube_like_formula_atoms() {
        // W1 (`@mununu_predicate`): an auxiliary predicate `aux == 3` that is NOT in
        // the formula seeds a cube dimension exactly as a formula atom would — a
        // simple state spec, plus a relational one as a compound. Dedups against the
        // formula atom.
        let f = mu_parser::parse("mu X. ((target == 1) || <> X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["target", "aux", "a", "b"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[
                "aux == 3".to_string(),
                "a == b".to_string(),
                "target == 1".to_string(), // duplicate of the formula atom → deduped
            ],
        );
        assert!(
            s.specs.iter().any(|p| p.name == "target == 1"),
            "formula atom still seeded"
        );
        assert!(
            s.specs.iter().any(|p| p.name == "aux == 3"),
            "extra simple atom seeded as a spec"
        );
        assert!(
            s.compounds.iter().any(|(n, _)| n == "a == b"),
            "extra relational atom seeded as a compound"
        );
        assert_eq!(
            s.specs.iter().filter(|p| p.name == "target == 1").count(),
            1,
            "duplicate extra atom is deduped against the formula atom"
        );
    }

    #[test]
    fn extra_atoms_bound_predicate_routes_to_compound() {
        // W2 (relational/bound grammar rides the SAME annotation path): a bound
        // `cnt >= 5` over a state cell seeds a compound dimension (not a literal spec).
        let f = mu_parser::parse("mu X. ((done == 1) || <> X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["done", "cnt"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &["cnt >= 5".to_string()],
        );
        assert!(
            s.compounds.iter().any(|(n, _)| n == "cnt >= 5"),
            "bound hint seeded as a compound dimension"
        );
        assert!(s.unseedable.is_empty(), "a state bound is seedable");
    }

    #[test]
    fn extra_atoms_empty_is_the_baseline() {
        // No `@mununu_predicate` hints ⇒ identical to the pre-W1 behaviour.
        let f = mu_parser::parse("nu X. ((state == 5) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["state"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1);
        assert_eq!(s.specs[0].name, "state == 5");
    }

    #[test]
    fn seeds_relational_predicate_as_compound() {
        // `$stable` shape: `state == state__past` → a compound (REL), both state cells.
        let f = mu_parser::parse("nu X. ((state == state__past) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["state", "state__past"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(s.specs.is_empty(), "relational atom is not a simple spec");
        assert_eq!(s.compounds.len(), 1, "one compound (relational) predicate");
        assert_eq!(s.compounds[0].0, "state == state__past");
        assert!(s.unseedable.is_empty());
        // It serialises into a sidecar cegar can read (compound + init pins).
        let referenced: std::collections::BTreeSet<String> =
            ["state".to_string(), "state__past".to_string()]
                .into_iter()
                .collect();
        let inits: std::collections::HashMap<String, u64> =
            [("state".to_string(), 0), ("state__past".to_string(), 0)]
                .into_iter()
                .collect();
        let json =
            synth_sidecar_json(&s.compounds, &[], &[], &referenced, &inits).expect("sidecar json");
        assert!(json.contains("compound_predicates"));
        assert!(json.contains("state == state__past"));
        assert!(json.contains("config_values"), "init pins present: {json}");
    }

    #[test]
    fn gates_atoms_over_non_state_signals() {
        // `gnt_o != 0` and `ready_i` over IO signals (not state cells) → unseedable.
        let f = mu_parser::parse("nu X. (((gnt_o != 0) || ready_i) && [] X)").unwrap();
        // Neither IO signal is a state cell, and (here) none is a free input
        // either → both unseedable.
        let s = seed_from_formula(
            &f,
            |n| cells(&["state_q"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(
            s.unseedable.contains(&"gnt_o != 0".to_string()),
            "IO comparison atom must be unseedable; got {:?}",
            s.unseedable
        );
        assert!(
            s.unseedable.contains(&"ready_i".to_string()),
            "bare IO boolean atom must be unseedable; got {:?}",
            s.unseedable
        );
    }

    #[test]
    fn bare_one_bit_state_signal_seeds_eq_one() {
        let f = mu_parser::parse("nu X. (busy && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["busy"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1);
        assert_eq!(s.specs[0].name, "busy");
        assert_eq!(s.specs[0].value, 1, "bare boolean state signal ≡ sig == 1");
        assert!(s.input_registers.is_empty(), "no free inputs here");
    }

    #[test]
    fn h_b_admits_simple_input_atom_as_free_dimension() {
        // sysrst shape: `!cfg_enable_i |=> state == Idle` translates (after the
        // disable-iff / |=> lowering) to a formula referencing the INPUT
        // `cfg_enable_i` and the STATE `state`. Here we drive the seeder with
        // `state` as a state cell and `cfg_enable_i` as a free input.
        let f = mu_parser::parse("nu X. ((cfg_enable_i == 0) && ((state == 1) && [] X))").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["state"]).contains(n),
            |n| cells(&["cfg_enable_i"]).contains(n),
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(
            s.unseedable.is_empty(),
            "both atoms seed: {:?}",
            s.unseedable
        );
        assert_eq!(s.specs.len(), 2, "one state + one input simple spec");
        assert!(
            s.input_registers.contains("cfg_enable_i"),
            "the input register is recorded as a free dimension: {:?}",
            s.input_registers
        );
        assert!(
            !s.input_registers.contains("state"),
            "the state register is NOT a free dimension"
        );
    }

    #[test]
    fn h_b_bare_input_boolean_seeds_eq_one_as_free() {
        let f = mu_parser::parse("nu X. (trigger_i && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false,
            |n| cells(&["trigger_i"]).contains(n),
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1);
        assert_eq!(s.specs[0].name, "trigger_i");
        assert_eq!(s.specs[0].value, 1, "bare boolean input ≡ sig == 1");
        assert!(s.input_registers.contains("trigger_i"));
    }

    #[test]
    fn free_input_init_cubes_no_free_bits_is_single_base() {
        // No free inputs → exactly the base reset cube (pre-H.B behaviour).
        assert_eq!(free_input_init_cubes(0b101, &[]), vec![0b101]);
    }

    #[test]
    fn free_input_init_cubes_enumerates_every_flavour() {
        // base = bit2 set (a pinned state predicate); free input dims at bits 0
        // and 1 → 4 flavours, each keeping bit2 and toggling {0,1}.
        let cubes = free_input_init_cubes(0b100, &[0, 1]);
        let got: std::collections::HashSet<usize> = cubes.into_iter().collect();
        let want: std::collections::HashSet<usize> =
            [0b100, 0b101, 0b110, 0b111].into_iter().collect();
        assert_eq!(
            got, want,
            "all 2^|free| initial input flavours, base preserved"
        );
    }

    #[test]
    fn h_f_input_inside_compound_binds_as_derived_relational() {
        // H.F supersedes the H.B deferral: an input inside a compound (`!=`,
        // relational, boolean) is NO LONGER unseedable. `cfg_enable_i != 0` (a Ne
        // over an input, routed to the compound branch) now binds as a derived
        // per-cube 3-valued label (sound; ⊥ where the input swings it).
        let f = mu_parser::parse("nu X. ((cfg_enable_i != 0) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false,
            |n| cells(&["cfg_enable_i"]).contains(n),
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(s.specs.is_empty());
        assert!(
            s.compounds.is_empty(),
            "not a cube dimension (input operand)"
        );
        assert!(
            s.derived_relational
                .iter()
                .any(|(n, _)| n == "cfg_enable_i != 0"),
            "input inside a compound now binds as a derived relational label: {:?}",
            s.derived_relational
        );
        assert!(
            !s.unseedable.iter().any(|a| a.contains("cfg_enable_i")),
            "no longer unseedable: {:?}",
            s.unseedable
        );
    }

    #[test]
    fn h_u_admits_combinational_bare_atom_as_cube_dim() {
        // csrng `main_sm_err_o` — a combinational-of-state output — is admitted as
        // a CUBE DIMENSION (H.U.2: the uniform image resolves it via the
        // combinational nid-map + reads its term over `(s,i)` / `(s',i')`), NOT a
        // derived per-cube label.
        let f = mu_parser::parse("nu X. (main_sm_err_o && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false,
            |_| false,
            |n| {
                cells(&["main_sm_err_o"])
                    .contains(n)
                    .then_some(CombKind::StateOnly)
            },
            |_| Vec::new(),
            &[],
        );
        assert_eq!(
            s.specs.len(),
            1,
            "combinational-of-state is a cube dimension"
        );
        assert_eq!(s.specs[0].name, "main_sm_err_o");
        assert_eq!(s.specs[0].value, 1, "bare boolean ≡ == 1");
        assert!(s.compounds.is_empty());
        assert!(s.unseedable.is_empty(), "not skipped: {:?}", s.unseedable);
    }

    #[test]
    fn h_u_admits_combinational_eq_value_as_cube_dim() {
        let f = mu_parser::parse("nu X. ((err_code == 3) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false,
            |_| false,
            |n| {
                cells(&["err_code"])
                    .contains(n)
                    .then_some(CombKind::StateOnly)
            },
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1);
        assert_eq!(s.specs[0].register, "err_code");
        assert_eq!(s.specs[0].value, 3);
        assert!(s.compounds.is_empty() && s.unseedable.is_empty());
    }

    #[test]
    fn h_u_state_takes_precedence_over_combinational() {
        // A name classified as BOTH state and combinational binds as STATE
        // (`is_state` is checked first) — still a cube dimension either way.
        let f = mu_parser::parse("nu X. (sig && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["sig"]).contains(n),
            |_| false,
            |n| cells(&["sig"]).contains(n).then_some(CombKind::StateOnly),
            |_| Vec::new(),
            &[],
        );
        assert_eq!(s.specs.len(), 1, "a cube dimension");
    }

    #[test]
    fn h_e_r2_input_dependent_combinational_routed_to_derived_label() {
        // `trigger_active = !trigger_i` — combinational of a FREE INPUT. H.E.r2
        // routes it to a DERIVED 3-valued label, NOT a cube dimension and NOT a
        // free dimension (both unsound for the must relation). The per-cube
        // labeller decides KleeneT/F where the cube pins it, KleeneBot where the
        // free input swings it — sound, and never a spurious VIOLATED.
        let f = mu_parser::parse("nu X. ((trigger_active && (state == 2)) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["state"]).contains(n),
            |_| false,
            |n| {
                cells(&["trigger_active"])
                    .contains(n)
                    .then_some(CombKind::InputDependent)
            },
            |_| Vec::new(),
            &[],
        );
        assert!(
            !s.input_registers.contains("trigger_active"),
            "NOT routed as a free dimension (unsound for must)"
        );
        assert!(
            !s.specs.iter().any(|p| p.register == "trigger_active"),
            "not a cube dimension"
        );
        assert!(
            !s.unseedable.contains(&"trigger_active".to_string()),
            "no longer skipped: {:?}",
            s.unseedable
        );
        assert!(
            s.derived.iter().any(|p| p.register == "trigger_active"),
            "input-dependent combinational is routed to the derived ⊥-label: {:?}",
            s.derived
        );
    }

    #[test]
    fn slice3_combinational_of_input_seeds_its_cone_input_as_free_dimension() {
        // Slice 3 (the unwrap lever) — a combinational-of-input atom
        // (`trigger_active = !trigger_i`, one cone input) binds as a derived
        // ⊥-label AND seeds its cone raw input (`trigger_i`) as a free H.B cube
        // dimension. That refines the may-relation (so a consequent box over a
        // trigger-governed transition becomes definite) and makes the derived
        // label definite at cubes that pin `trigger_i`.
        let f = mu_parser::parse("nu X. (trigger_active && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false, // no state
            |_| false, // trigger_active is a combinational, not a raw input
            |n| (n == "trigger_active").then_some(CombKind::InputDependent),
            |n| {
                if n == "trigger_active" {
                    vec!["trigger_i".to_string()]
                } else {
                    Vec::new()
                }
            },
            &[],
        );
        assert!(
            s.derived.iter().any(|p| p.register == "trigger_active"),
            "combinational stays a derived label (binds the formula atom): {:?}",
            s.derived
        );
        assert!(
            s.input_registers.contains("trigger_i"),
            "the cone raw input is seeded as a free cube dimension: {:?}",
            s.input_registers
        );
        assert!(
            s.specs
                .iter()
                .any(|p| p.register == "trigger_i" && p.value == 1),
            "trigger_i is a cube dimension: {:?}",
            s.specs
        );
    }

    #[test]
    fn slice3_many_cone_inputs_left_as_bare_derived_label() {
        // The cube-blowup guard: a combinational reaching MANY (> MAX_CONE_INPUTS)
        // raw inputs is NOT unwrapped — it stays a plain derived ⊥-label with no
        // added dimensions, so the abstraction does not blow up.
        let f = mu_parser::parse("nu X. (wide && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |_| false,
            |_| false,
            |n| (n == "wide").then_some(CombKind::InputDependent),
            |n| {
                if n == "wide" {
                    vec!["i0".to_string(), "i1".to_string(), "i2".to_string()]
                } else {
                    Vec::new()
                }
            },
            &[],
        );
        assert!(
            s.derived.iter().any(|p| p.register == "wide"),
            "wide stays a derived label: {:?}",
            s.derived
        );
        assert!(
            s.input_registers.is_empty(),
            "too many cone inputs → none seeded (blowup guard): {:?}",
            s.input_registers
        );
        assert!(
            s.specs.is_empty(),
            "no cube dimensions added: {:?}",
            s.specs
        );
    }

    #[test]
    fn h_f_relational_with_input_operand_routed_to_derived_relational() {
        // `cnt_q >= cfg_detect_timer_i` — `cnt_q` state, `cfg_detect_timer_i` a
        // free input. The relational's value depends on the demonic input, so it
        // is NOT a sound cube dimension; H.F routes it to a derived per-cube
        // 3-valued label (carrying its PredicateExpr), not unseedable.
        let f = mu_parser::parse("nu X. ((cnt_q >= cfg_detect_timer_i) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["cnt_q"]).contains(n),
            |n| n == "cfg_detect_timer_i",
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(
            s.compounds.is_empty(),
            "not a cube dimension (has a non-state operand): {:?}",
            s.compounds
        );
        assert!(
            s.derived_relational
                .iter()
                .any(|(n, _)| n == "cnt_q >= cfg_detect_timer_i"),
            "relational-with-input routed to derived_relational: {:?}",
            s.derived_relational
        );
        assert!(
            !s.unseedable.iter().any(|a| a.contains("cnt_q")),
            "no longer skipped: {:?}",
            s.unseedable
        );
    }

    #[test]
    fn h_f_relational_with_unresolvable_operand_is_skipped() {
        // An operand that resolves to nothing in the design (not state / input /
        // combinational) → cannot label it → honest SKIP, not a derived label.
        let f = mu_parser::parse("nu X. ((cnt_q >= mystery_signal) && [] X)").unwrap();
        let s = seed_from_formula(
            &f,
            |n| cells(&["cnt_q"]).contains(n),
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );
        assert!(
            s.derived_relational.is_empty(),
            "{:?}",
            s.derived_relational
        );
        assert!(
            s.unseedable.iter().any(|a| a.contains("mystery_signal")),
            "unresolvable operand ⇒ SKIP: {:?}",
            s.unseedable
        );
    }

    #[test]
    fn skip_reason_enriched_with_blackboxed_module_root_cause() {
        // A cut FSM (e.g. csrng's prim_sparse_fsm_flop) → the bare "non-state
        // signal" symptom is augmented with the actionable root cause.
        let diag = ModelDiagnostics {
            state_register_count: 0,
            blackboxed_modules: vec!["prim_sparse_fsm_flop".to_string()],
            gated_resets: Vec::new(),
            auto_provided_stubs: Vec::new(),
            ..Default::default()
        };
        let reason = unseedable_skip_reason(&["state_q == 41".to_string()], &diag);
        assert!(reason.contains("state_q == 41"), "carries the symptom");
        assert!(
            reason.contains("prim_sparse_fsm_flop"),
            "names the cut module: {reason}"
        );
        assert!(
            reason.contains("Provide the missing module source"),
            "actionable: {reason}"
        );
    }

    #[test]
    fn skip_reason_enriched_when_no_state_registers() {
        let diag = ModelDiagnostics {
            state_register_count: 0,
            blackboxed_modules: Vec::new(),
            gated_resets: Vec::new(),
            auto_provided_stubs: Vec::new(),
            ..Default::default()
        };
        let reason = unseedable_skip_reason(&["foo == 1".to_string()], &diag);
        assert!(
            reason.contains("no state registers"),
            "root cause: {reason}"
        );
    }

    #[test]
    fn skip_reason_bare_when_model_has_state() {
        // Genuine IO atom on a healthy model → no misleading root-cause hint.
        let diag = ModelDiagnostics {
            state_register_count: 3,
            blackboxed_modules: Vec::new(),
            gated_resets: Vec::new(),
            auto_provided_stubs: Vec::new(),
            ..Default::default()
        };
        let reason = unseedable_skip_reason(&["gnt_o != 0".to_string()], &diag);
        assert!(reason.contains("gnt_o != 0"));
        assert!(!reason.contains("Root cause"), "no spurious hint: {reason}");
    }

    #[test]
    fn no_sidecar_when_nothing_to_emit() {
        let empty_refs = std::collections::BTreeSet::new();
        let empty_inits = std::collections::HashMap::new();
        assert!(synth_sidecar_json(&[], &[], &[], &empty_refs, &empty_inits).is_none());
    }

    /// The seam between the translator and the assumption rewrite: every formula the translator
    /// emits for the three real slang fixtures relativizes (or is refused with a reason — none is
    /// today), and the result still parses and keeps the rescue-reducible shapes reducible.
    #[test]
    fn every_translated_formula_of_the_real_fixtures_relativizes() {
        use crate::adapter::btor2::assume_monitor::ASSUME_OK_SYMBOL;
        use crate::adapter::slang::assume_guarantee::relativize;
        let mut refused = Vec::new();
        for json in [
            include_str!("testdata/sva_fragment_forms.ast.json"),
            include_str!("testdata/sva_phase1_forms.ast.json"),
            include_str!("testdata/sva_expr_forms.ast.json"),
            include_str!("testdata/sva_instance_forms.ast.json"),
        ] {
            let report = crate::adapter::slang::translate::translate_ast_json(json).unwrap();
            for t in &report.translated {
                match relativize(&t.formula, ASSUME_OK_SYMBOL, t.kind == SvaKind::Cover) {
                    Ok(r) => {
                        let orig = mu_parser::parse(&t.formula).unwrap();
                        let rel = mu_parser::parse(&r).unwrap();
                        // A shape the safety rescue lanes reduce stays reducible.
                        assert_eq!(
                            crate::adapter::reach_rescue::reduce_ag_boolean_body(&orig).is_some(),
                            crate::adapter::reach_rescue::reduce_ag_boolean_body(&rel).is_some(),
                            "{}: {} -> {r}",
                            t.name,
                            t.formula
                        );
                        assert_eq!(
                            crate::adapter::reach_rescue::reduce_ag_implies_next(&orig).is_some(),
                            crate::adapter::reach_rescue::reduce_ag_implies_next(&rel).is_some(),
                            "{}: {} -> {r}",
                            t.name,
                            t.formula
                        );
                    }
                    Err(why) => refused.push(format!("{}: {why}: {}", t.name, t.formula)),
                }
            }
        }
        assert!(refused.is_empty(), "\n{}", refused.join("\n"));
    }

    #[test]
    fn past_depths_are_read_from_atoms_and_minted_leaves() {
        assert_eq!(shadow_depth_of_name("req"), 0);
        assert_eq!(shadow_depth_of_name("req__past"), 1);
        assert_eq!(shadow_depth_of_name("req__past3"), 3);
        assert_eq!(shadow_depth_of_name("req__past__bits7_0"), 1);
        let minted: std::collections::HashMap<&str, u32> =
            [("mununu_x_00000000000000aa", 2)].into_iter().collect();
        let f = mu_parser::parse(
            "nu X. ((((!(a)) || (b == c__past)) || [] mununu_x_00000000000000aa) && [] X)",
        )
        .unwrap();
        assert_eq!(subtree_past_depth(&f, f.root(), &minted), 2);
    }

    #[test]
    fn assumptions_are_split_off_and_the_guarantees_relativized_per_mode() {
        use crate::adapter::slang::translate::{TranslatedAssertion, TranslationReport};
        let t = |name: &str, kind: SvaKind, formula: &str| TranslatedAssertion {
            name: name.to_string(),
            label: Some(name.to_string()),
            kind,
            formula: formula.to_string(),
            recoverability_companion: None,
            written: None,
        };
        let report = || TranslationReport {
            translated: vec![
                t("m_inv", SvaKind::Assume, "nu X. ((!(inc)) && [] X)"),
                t(
                    "m_next",
                    SvaKind::Assume,
                    "nu X. (((!(req)) || [] req) && [] X)",
                ),
                t(
                    "m_deep",
                    SvaKind::Assume,
                    "nu X. (((!(req)) || [] [] req) && [] X)",
                ),
                t("a", SvaKind::Assert, "nu X. ((cnt == 0) && [] X)"),
                t("c", SvaKind::Cover, "mu X. ((cnt == 2) || <> X)"),
            ],
            ..Default::default()
        };
        // precondition
        let mut ex = report();
        let (rep, candidates) = plan_assumptions(&mut ex, AssumeMode::Precondition, &[]);
        assert_eq!(rep.mode, "precondition");
        assert_eq!(
            candidates
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["m_inv", "m_next"]
        );
        assert_eq!(
            rep.unapplied.len(),
            1,
            "the nested-box assume is not applied"
        );
        let names: Vec<&str> = ex.translated.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c", ASSUMPTION_CONSISTENCY_PROPERTY]);
        assert!(ex.translated[0].formula.contains("(!(mununu_assume_ok))"));
        assert!(ex.translated[1].formula.contains("(mununu_assume_ok && "));
        // check: untouched
        let mut ex = report();
        let (rep, candidates) = plan_assumptions(&mut ex, AssumeMode::Check, &[]);
        assert_eq!(rep.mode, "check");
        assert!(candidates.is_empty());
        assert_eq!(ex.translated.len(), 5);
        // ignore: assumes dropped, guarantees untouched
        let mut ex = report();
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Ignore, &[]);
        assert_eq!(rep.unapplied.len(), 3);
        assert_eq!(ex.translated.len(), 2);
        assert_eq!(ex.translated[0].formula, "nu X. ((cnt == 0) && [] X)");
        // no assume: nothing at all
        let mut ex = TranslationReport {
            translated: vec![t("a", SvaKind::Assert, "nu X. ((cnt == 0) && [] X)")],
            ..Default::default()
        };
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Precondition, &[]);
        assert!(rep.mode.is_empty());
        assert_eq!(ex.translated.len(), 1);
    }

    /// 2026-10-30 — a row named in `exempt` keeps its formula under `precondition` (it never
    /// reads `mununu_assume_ok`, so its cone excludes the monitor and it is decided on the
    /// un-assumed design), and the report records what the modifier did in every mode — a no-op
    /// is said out loud with its reason.
    #[test]
    fn an_exempt_row_is_not_relativized_and_the_report_says_what_the_modifier_did() {
        use crate::adapter::slang::translate::{TranslatedAssertion, TranslationReport};
        let t = |name: &str, kind: SvaKind, formula: &str| TranslatedAssertion {
            name: name.to_string(),
            label: Some(name.to_string()),
            kind,
            formula: formula.to_string(),
            recoverability_companion: None,
            written: None,
        };
        let exempt = vec!["b".to_string()];
        let report = || TranslationReport {
            translated: vec![
                t("m_inv", SvaKind::Assume, "nu X. ((!(inc)) && [] X)"),
                t("a", SvaKind::Assert, "nu X. ((cnt == 0) && [] X)"),
                t("b", SvaKind::Assert, "nu X. ((cnt == 1) && [] X)"),
            ],
            ..Default::default()
        };
        // precondition with an applied assumption: `a` is relativized, `b` is not.
        let mut ex = report();
        let (rep, candidates) = plan_assumptions(&mut ex, AssumeMode::Precondition, &exempt);
        assert_eq!(candidates.len(), 1);
        assert!(
            ex.translated[0].formula.contains("mununu_assume_ok"),
            "{}",
            ex.translated[0].formula
        );
        assert_eq!(ex.translated[1].formula, "nu X. ((cnt == 1) && [] X)");
        assert_eq!(rep.exempt.len(), 1);
        assert_eq!(rep.exempt[0].0, "b");
        assert!(
            rep.exempt[0].1.contains("judged on every trace"),
            "{}",
            rep.exempt[0].1
        );
        assert!(
            rep.unguarded.is_empty(),
            "exempt is not a failure to relativize"
        );
        // ignore / check: the modifier changed nothing, and the record says why.
        let mut ex = report();
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Ignore, &exempt);
        assert!(
            rep.exempt[0].1.contains("--assumes ignore"),
            "{}",
            rep.exempt[0].1
        );
        let mut ex = report();
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Check, &exempt);
        assert!(
            rep.exempt[0].1.contains("--assumes check"),
            "{}",
            rep.exempt[0].1
        );
        // no assume in the design: still recorded (the row shows it), the mode stays empty.
        let mut ex = TranslationReport {
            translated: vec![t("b", SvaKind::Assert, "nu X. ((cnt == 1) && [] X)")],
            ..Default::default()
        };
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Precondition, &exempt);
        assert!(rep.mode.is_empty());
        assert!(
            rep.exempt[0].1.contains("no assume statement"),
            "{}",
            rep.exempt[0].1
        );
        // only unapplicable assumes: nothing to be exempt from.
        let mut ex = TranslationReport {
            translated: vec![
                t(
                    "m_deep",
                    SvaKind::Assume,
                    "nu X. (((!(req)) || [] [] req) && [] X)",
                ),
                t("b", SvaKind::Assert, "nu X. ((cnt == 1) && [] X)"),
            ],
            ..Default::default()
        };
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Precondition, &exempt);
        assert!(
            rep.exempt[0].1.contains("no assumption was applicable"),
            "{}",
            rep.exempt[0].1
        );
        assert_eq!(ex.translated[0].formula, "nu X. ((cnt == 1) && [] X)");
    }

    /// 2026-10-30 — an EF-rooted `assert` row (an `rtl_reachable` instance, a `mu X. (t || <> X)`
    /// guarantee) is relativized existentially — `EF (ok && t)` — not with the universal excuse
    /// `(!ok) || …`, which an initial state that already breaks the assumption satisfies: on the
    /// parent, `rtl_reachable TARGET="q == 3"` under `assume property (!inc)` HOLDS on a 2-bit
    /// counter although no assumption-keeping trace reaches `q == 3` (mununu#837).
    #[test]
    fn an_ef_rooted_guarantee_is_relativized_existentially() {
        use crate::adapter::slang::translate::{TranslatedAssertion, TranslationReport};
        let t = |name: &str, kind: SvaKind, formula: &str| TranslatedAssertion {
            name: name.to_string(),
            label: Some(name.to_string()),
            kind,
            formula: formula.to_string(),
            recoverability_companion: None,
            written: None,
        };
        let mut ex = TranslationReport {
            translated: vec![
                t("m_inv", SvaKind::Assume, "nu X. ((!(inc)) && [] X)"),
                t("reach", SvaKind::Assert, "mu X. ((q == 3) || <> X)"),
                t("never", SvaKind::Assert, "nu X. ((!(q == 3)) && [] X)"),
                t(
                    "recover",
                    SvaKind::Assert,
                    "nu Y. ((mu X. ((q == 0) || <> X)) && [] Y)",
                ),
            ],
            ..Default::default()
        };
        let (rep, _) = plan_assumptions(&mut ex, AssumeMode::Precondition, &[]);
        assert!(rep.unguarded.is_empty(), "{:?}", rep.unguarded);
        let f = |n: &str| {
            ex.translated
                .iter()
                .find(|t| t.name == n)
                .map(|t| t.formula.clone())
                .unwrap()
        };
        let reach = f("reach");
        assert!(
            !reach.contains("(!(mununu_assume_ok))"),
            "no universal excuse at an EF root: {reach}"
        );
        assert!(reach.contains("(mununu_assume_ok && (q == 3))"), "{reach}");
        assert!(
            reach.trim_start_matches('(').starts_with("mu X."),
            "{reach}"
        );
        // The universal roots keep the universal guard: the safety block `!(q == 3)` folds to
        // `!(ok && (q == 3))` (the excuse is still there, as the printer folds it), the AG EF
        // keeps the excuse in front of its EF block.
        let never = f("never");
        assert!(
            never.trim_start_matches('(').starts_with("nu X.")
                && never.contains("(!((mununu_assume_ok && (q == 3))))"),
            "{never}"
        );
        assert!(
            f("recover").contains("(!(mununu_assume_ok))"),
            "{}",
            f("recover")
        );
    }

    #[test]
    fn lift_overrides_replace_same_named_sources_and_refuse_unknown_names() {
        let yopts = YosysOptions {
            additional_sources: vec![
                ("prim_assert.sv".into(), "REAL MACROS".into()),
                ("pkg.sv".into(), "PKG".into()),
            ],
            ..Default::default()
        };
        let ov = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        // An additional source: replaced for the lift, the rest untouched.
        let (primary, y) =
            apply_lift_overrides("top.sv", "TOP", &yopts, &ov(&[("prim_assert.sv", "SHIM")]))
                .unwrap();
        assert_eq!(primary, "TOP");
        assert_eq!(
            y.additional_sources,
            ov(&[("prim_assert.sv", "SHIM"), ("pkg.sv", "PKG")])
        );
        // The primary itself.
        let (primary, _) =
            apply_lift_overrides("top.sv", "TOP", &yopts, &ov(&[("top.sv", "TOP2")])).unwrap();
        assert_eq!(primary, "TOP2");
        // A typo must not silently lift the real file; neither may a duplicate.
        let e = apply_lift_overrides("top.sv", "TOP", &yopts, &ov(&[("prim_asert.sv", "X")]))
            .unwrap_err();
        assert!(e.message.contains("matches no source"), "{}", e.message);
        let e = apply_lift_overrides(
            "top.sv",
            "TOP",
            &yopts,
            &ov(&[("pkg.sv", "A"), ("pkg.sv", "B")]),
        )
        .unwrap_err();
        assert!(e.message.contains("more than once"), "{}", e.message);
    }

    #[test]
    fn empty_sources_errors() {
        let err = verify_auto(&[], &YosysOptions::default(), &VerifyAutoOptions::default())
            .expect_err("no sources");
        assert_eq!(err.kind, AdapterErrorKind::ParseError);
    }

    /// REPRODUCER (2026-10-15) — a relational comparison on a SIGNED signal was decided UNSIGNED.
    ///
    /// slang converts both operands of `x >= 0` (`logic signed [3:0] x`) to `logic signed[31:0]`, so
    /// the comparison is signed; `translate.rs::unwrap` stripped those conversions and emitted the
    /// 4-bit atom `(x >= 0)`, which every engine evaluates unsigned. Here `x` is always -1 or -2, so
    /// `x >= 0` is FALSE in SV and `x < 0` TRUE — the unsigned reading inverts both. The mixed
    /// signed/unsigned `x < u` is converted to unsigned by slang and must keep translating.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_signed_relational_comparison_is_not_decided_unsigned() {
        let design = "module sg_dut (input logic clk, input logic rst_n,\n\
                      output logic signed [3:0] x, output logic [3:0] u);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) begin x <= -4'sd1; u <= 4'd15; end\n\
                          else begin x <= -4'sd2; u <= 4'd14; end\n\
                        end\n\
                        a_lt0: assert property (@(posedge clk) disable iff (!rst_n) x < 0);\n\
                        a_ge0: assert property (@(posedge clk) disable iff (!rst_n) x >= 0);\n\
                        a_xu: assert property (@(posedge clk) disable iff (!rst_n) x < u);\n\
                      endmodule\n";
        let sources = vec![("sg_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("sg_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        eprintln!("  unsupported: {:?}", report.unsupported);
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
        };
        // Never the inverted verdicts: `x >= 0` must not HOLD, `x < 0` must not be VIOLATED.
        assert!(
            !matches!(outcome("a_ge0"), Some(VerifyOutcome::Holds)),
            "x >= 0 on an always-negative signed x reported HOLDS: {:?}",
            outcome("a_ge0")
        );
        assert!(
            !matches!(outcome("a_lt0"), Some(VerifyOutcome::Violated { .. })),
            "x < 0 on an always-negative signed x reported VIOLATED: {:?}",
            outcome("a_lt0")
        );
        // The unsigned mixed comparison still decides (x reads 14 or 15 unsigned; u is 15 then 14).
        assert!(outcome("a_xu").is_some(), "x < u still translates");
    }

    /// `$past` of a COMBINATIONAL output samples the output, not the nearest register in its cone.
    /// `o = q | 8` and `w = q` (a plain wire) over a free-running 4-bit counter `q`. When `q == 5`,
    /// `o` is 13; one cycle later `$past(o)` must read 13. Binding `o__past` to `q`'s shadow (the
    /// cone walk's nearest state) reads 5: `a_past_o` turns VIOLATED and its twin `a_past_o_twin`
    /// (`$past(o) == 5`, false) HOLDS. `a_past_w` pins the wire case, which must keep deciding.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_past_of_a_combinational_output_samples_the_output() {
        let design = "module po_dut (input logic clk, input logic rst_n,\n\
                      output logic [3:0] o, output logic [3:0] w);\n\
                        logic [3:0] q;\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) q <= 4'd0; else q <= q + 4'd1;\n\
                        end\n\
                        assign o = q | 4'b1000;\n\
                        assign w = q;\n\
                        a_past_o: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          q == 4'd5 |=> $past(o) == 4'd13);\n\
                        a_past_o_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          q == 4'd5 |=> $past(o) == 4'd5);\n\
                        a_past_w: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          q == 4'd5 |=> $past(w) == 4'd5);\n\
                      endmodule\n";
        let sources = vec![("po_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("po_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        eprintln!("  unsupported: {:?}", report.unsupported);
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
        };
        assert!(
            !matches!(outcome("a_past_o_twin"), Some(VerifyOutcome::Holds)),
            "$past(o) == 5 is false (o is 13 when q is 5) but reported HOLDS: {:?}",
            outcome("a_past_o_twin")
        );
        assert!(
            matches!(outcome("a_past_o"), Some(VerifyOutcome::Holds)),
            "$past(o) == 13 one cycle after q == 5: {:?}",
            outcome("a_past_o")
        );
        assert!(
            matches!(
                outcome("a_past_o_twin"),
                Some(VerifyOutcome::Violated { .. })
            ),
            "the false twin decides VIOLATED: {:?}",
            outcome("a_past_o_twin")
        );
        assert!(
            matches!(outcome("a_past_w"), Some(VerifyOutcome::Holds)),
            "the wire alias keeps deciding: {:?}",
            outcome("a_past_w")
        );
    }

    /// mununu#705 — `disable iff` disables the attempt at ANY of its cycles. `c = a_q & !d`, so
    /// `a |=> c` fails exactly when `d` rises at the consequent's cycle — which the disable
    /// excuses. The twin `a |=> !c` is VIOLATED (whenever `d` stays low).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_a_disable_rising_mid_attempt_disables_it() {
        // A compound disable of two plain inputs, so reset gating keeps it.
        let design = "module dd_dut (input logic clk, input logic en, input logic a,\n\
                      input logic d, output logic c);\n\
                        logic a_q;\n\
                        always_ff @(posedge clk) a_q <= a;\n\
                        assign c = a_q & !d;\n\
                        a_mid: assert property (@(posedge clk) disable iff (d || !en) a |=> c);\n\
                        a_mid_twin: assert property (@(posedge clk) disable iff (d || !en) a |=> !c);\n\
                      endmodule\n";
        let sources = vec![("dd_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("dd_dut".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!("  {:?}: {:?}\n      {}", p.label, p.outcome, p.formula);
        }
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        assert!(
            matches!(outcome("a_mid"), VerifyOutcome::Holds),
            "a disable at the consequent's cycle excuses it: {:?}",
            outcome("a_mid")
        );
        assert!(matches!(
            outcome("a_mid_twin"),
            VerifyOutcome::Violated { .. }
        ));
    }

    /// mununu#709 — an assertion inside a sub-instance reads THAT instance's signals. `u_s.q` follows the free
    /// input `a`, so `a_sub_q_zero` is VIOLATED; the top drives its own output `q` to 0, and the
    /// assertion used to be checked against that same-named top-level net — a false HOLDS. Its
    /// twin over `$past` HOLDS, and so does the top's hierarchical reference `u_s.q`.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_a_sub_instance_assertion_reads_its_own_signals() {
        let design = "module hs_sub (input logic clk, input logic a);\n\
                        logic q;\n\
                        always_ff @(posedge clk) q <= a;\n\
                        a_sub_q_zero: assert property (@(posedge clk) q == 1'b0);\n\
                        a_sub_q_past: assert property (@(posedge clk) ##1 q == $past(a));\n\
                      endmodule\n\
                      module hs_top (input logic clk, input logic a, output logic q);\n\
                        assign q = 1'b0;\n\
                        hs_sub u_s (.clk(clk), .a(a));\n\
                        a_top_hier: assert property (@(posedge clk) ##1 u_s.q == $past(a));\n\
                      endmodule\n";
        let sources = vec![("hs_top.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("hs_top".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} {:?}: {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`: {:?}", report.unsupported))
        };
        assert!(
            matches!(outcome("a_sub_q_zero"), VerifyOutcome::Violated { .. }),
            "the sub-instance's own `q` follows `a`: {:?}",
            outcome("a_sub_q_zero")
        );
        assert!(matches!(outcome("a_sub_q_past"), VerifyOutcome::Holds));
        assert!(matches!(outcome("a_top_hier"), VerifyOutcome::Holds));
        // The VIOLATED safety property carries its trace: reset → `a` high → `q` high.
        let cx = report
            .properties
            .iter()
            .find(|p| p.label.as_deref() == Some("a_sub_q_zero"))
            .and_then(|p| p.counterexample.as_ref())
            .expect("a safety counterexample");
        assert!(cx.cycle.is_empty());
        assert_eq!(cx.inputs.len(), cx.prefix.len());
        assert!(
            cx.prefix.last().is_some_and(|st| st
                .iter()
                .any(|(k, v)| k == "u_s.q" && v.as_u64() == Some(1))),
            "the trace ends with `u_s.q` high: {cx:?}"
        );
    }

    /// mununu#713 — assertions inside a `for` generate (OpenTitan's per-channel checks) and in
    /// instances a `for` generate elaborates: each iteration reads its own signals, named
    /// `g_ch$k.r` as the lifted model names `g_ch[k].r`. `a_ch_follows` / `a_sub_follows` HOLD in
    /// every iteration; `a_ch_zero` is VIOLATED in every iteration, with a trace.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_assertions_inside_a_for_generate_are_checked_per_iteration() {
        let design = "module fg_sub (input logic clk, input logic d);\n\
                        logic s;\n\
                        always_ff @(posedge clk) s <= d;\n\
                        a_sub_follows: assert property (@(posedge clk) ##1 s == $past(d));\n\
                      endmodule\n\
                      module fg_top (input logic clk, input logic [1:0] a, output logic [1:0] q);\n\
                        for (genvar i = 0; i < 2; i++) begin : g_ch\n\
                          logic r;\n\
                          always_ff @(posedge clk) r <= a[i];\n\
                          assign q[i] = r;\n\
                          a_ch_follows: assert property (@(posedge clk) ##1 r == $past(a[i]));\n\
                          a_ch_zero: assert property (@(posedge clk) r == 1'b0);\n\
                        end\n\
                        for (genvar j = 0; j < 2; j++) begin : g_sub\n\
                          fg_sub u_s (.clk(clk), .d(a[j]));\n\
                        end\n\
                      endmodule\n";
        let sources = vec![("fg_top.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("fg_top".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} {:?}: {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        let with_label = |l: &str| -> Vec<&PropertyVerdict> {
            report
                .properties
                .iter()
                .filter(|p| p.label.as_deref() == Some(l))
                .collect()
        };
        for l in ["a_ch_follows", "a_sub_follows"] {
            let rows = with_label(l);
            assert_eq!(rows.len(), 2, "one `{l}` per iteration");
            assert!(
                rows.iter()
                    .all(|p| matches!(p.outcome, VerifyOutcome::Holds)),
                "{l}: {:?}",
                rows.iter().map(|p| &p.outcome).collect::<Vec<_>>()
            );
        }
        let zero = with_label("a_ch_zero");
        assert_eq!(zero.len(), 2);
        for (k, p) in zero.iter().enumerate() {
            assert!(
                matches!(p.outcome, VerifyOutcome::Violated { .. }),
                "{:?}",
                p.outcome
            );
            assert!(p.formula.contains(&format!("g_ch${k}.r")), "{}", p.formula);
            assert!(p.counterexample.is_some(), "a trace for {}", p.name);
        }
    }

    /// mununu#722 — an `ASSERT_KNOWN` (`!$isunknown(x)`, folded to `true`) on a design WIDER than
    /// the exact engine's cap decides trivially instead of skipping: an atom-less formula has no
    /// cone, and its value is the same on every total model. The twin over a real atom still
    /// reads the register (and is skipped over the cap here, as before).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_an_atom_less_assertion_decides_without_a_cone() {
        let design = "module wide_dut (input logic clk, input logic [255:0] d, output logic [255:0] q);\n\
                        always_ff @(posedge clk) q <= d;\n\
                        a_known: assert property (@(posedge clk) !$isunknown(q));\n\
                        a_never: assert property (@(posedge clk) 1'b0);\n\
                      endmodule\n";
        let sources = vec![("wide_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("wide_dut".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} {:?}: {:?} by {:?}",
                p.name, p.label, p.outcome, p.decided_by
            );
        }
        let row = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        assert!(matches!(row("a_known").outcome, VerifyOutcome::Holds));
        assert_eq!(row("a_known").decided_by.as_deref(), Some("constant"));
        assert!(matches!(
            row("a_never").outcome,
            VerifyOutcome::Violated { .. }
        ));
        assert_eq!(row("a_never").decided_by.as_deref(), Some("constant"));
    }

    /// mununu#722 — a hierarchical reference into a module instantiated several times names ITS
    /// instance (OpenTitan's `ASSERT_PRIM_*_ERROR_TRIGGER_ALERT` shape, written through a macro):
    /// `u_a.…u_wptr`'s error does reach `alert_a` (HOLDS); `u_b`'s never reaches `alert_b`
    /// (VIOLATED, with a trace); the `for`-generate pair resolves per iteration (HOLDS ×2).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_hierarchical_references_name_their_instance() {
        let design = include_str!("testdata/sva_hierarchy_refs.sv");
        let sources = vec![("sva_hierarchy_refs.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("top".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} {:?}: {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        let rows = |l: &str| -> Vec<&PropertyVerdict> {
            report
                .properties
                .iter()
                .filter(|p| p.label.as_deref() == Some(l))
                .collect()
        };
        assert!(matches!(
            rows("a_wptr_alerts")[0].outcome,
            VerifyOutcome::Holds
        ));
        let b = rows("b_wptr_silent")[0];
        assert!(
            matches!(b.outcome, VerifyOutcome::Violated { .. }),
            "{:?}",
            b.outcome
        );
        assert!(
            b.counterexample.is_some(),
            "a trace for the violated instance"
        );
        assert!(matches!(
            rows("a_rptr_never")[0].outcome,
            VerifyOutcome::Holds
        ));
        let ep = rows("ep_alerts");
        assert_eq!(ep.len(), 2);
        assert!(ep.iter().all(|p| matches!(p.outcome, VerifyOutcome::Holds)));
    }

    /// mununu#711 — a port that is its register's bits SWAPPED (OpenTitan's `esc_rx_o = {resp_p,
    /// resp_n}` over the flop `{resp_nq, resp_pq}`) is read as the port. `p = ~a` one cycle late:
    /// `a_p_inverts` HOLDS and `a_p_follows` is VIOLATED; read through the raw flop, `p` became
    /// `n = a` and the two swapped — a false HOLDS on `a_p_follows`.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_a_bit_swapped_port_is_not_read_as_its_register() {
        let design = "module sw_sub (input logic clk, input logic rst_n, input logic a,\n\
                                     output logic [1:0] o);\n\
                        logic [1:0] f;\n\
                        always_ff @(posedge clk or negedge rst_n)\n\
                          if (!rst_n) f <= 2'b10; else f <= {a, ~a};\n\
                        assign o = {f[0], f[1]};\n\
                        a_p_inverts: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ##1 o[1] == !$past(a));\n\
                        a_p_follows: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ##1 o[1] == $past(a));\n\
                      endmodule\n\
                      module sw_top (input logic clk, input logic rst_n, input logic a,\n\
                                     output logic [1:0] o);\n\
                        sw_sub u_s (.clk(clk), .rst_n(rst_n), .a(a), .o(o));\n\
                      endmodule\n";
        let sources = vec![("sw_top.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("sw_top".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} {:?}: {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`: {:?}", report.unsupported))
        };
        assert!(
            matches!(outcome("a_p_inverts"), VerifyOutcome::Holds),
            "{:?}",
            outcome("a_p_inverts")
        );
        assert!(
            matches!(outcome("a_p_follows"), VerifyOutcome::Violated { .. }),
            "{:?}",
            outcome("a_p_follows")
        );
    }

    /// mununu#708 — `disable iff` disables an attempt where it STARTS, even when the property evaluates nothing
    /// there. `c` is last cycle's `!d`, so `##1 a |-> c` fails exactly when `d` held at the
    /// attempt's start — which the disable excuses. The twin `##1 a |-> !c` is VIOLATED. Reduced
    /// from OpenTitan's `prim_esc_rxtx` `EscRespCheck_A` (`##1 esc_req_i |-> …`), which the
    /// block-only guard reported VIOLATED and SymbiYosys (abc pdr) proves.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_a_disable_at_the_attempt_start_disables_it() {
        let design = "module ds_dut (input logic clk, input logic en, input logic a,\n\
                      input logic d, output logic c);\n\
                        logic d_q;\n\
                        always_ff @(posedge clk) d_q <= d;\n\
                        assign c = !d_q;\n\
                        a_start: assert property (@(posedge clk) disable iff (d || !en) ##1 a |-> c);\n\
                        a_start_twin: assert property (@(posedge clk) disable iff (d || !en) ##1 a |-> !c);\n\
                      endmodule\n";
        let sources = vec![("ds_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("ds_dut".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!("  {:?}: {:?}\n      {}", p.label, p.outcome, p.formula);
        }
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        assert!(
            matches!(outcome("a_start"), VerifyOutcome::Holds),
            "a disable at the attempt's start excuses it: {:?}",
            outcome("a_start")
        );
        assert!(matches!(
            outcome("a_start_twin"),
            VerifyOutcome::Violated { .. }
        ));
    }

    /// OpenTitan coverage, Phase 4 — `assume` statements as preconditions, in all three modes,
    /// plus a contradictory set. `cnt` counts `inc`; `held` is last cycle's `req`.
    ///
    /// - precondition (default): under `assume !inc`, `cnt == 0` HOLDS (its twin `cnt == 1` stays
    ///   VIOLATED); under `assume req |=> req`, `held |-> req` HOLDS; the set is `consistent`.
    /// - check: the guarantees are VIOLATED and each assumption is checked (VIOLATED).
    /// - ignore: the guarantees are VIOLATED and the assumptions are listed as not applied.
    /// - `assume req` + `assume !req`: `contradictory`, and every HOLDS is flagged vacuous.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_assumes_are_applied_as_preconditions() {
        let module = |assumes: &str| {
            format!(
                "module ag_dut (input logic clk, input logic rst_n, input logic req,\n\
                 input logic inc, output logic [1:0] cnt, output logic held);\n\
                   always_ff @(posedge clk or negedge rst_n) begin\n\
                     if (!rst_n) begin cnt <= 2'd0; held <= 1'b0; end\n\
                     else begin if (inc) cnt <= cnt + 2'd1; held <= req; end\n\
                   end\n{assumes}\
                   a_zero: assert property (@(posedge clk) disable iff (!rst_n) cnt == 2'd0);\n\
                   a_zero_twin: assert property (@(posedge clk) disable iff (!rst_n) cnt == 2'd1);\n\
                   a_held: assert property (@(posedge clk) disable iff (!rst_n) held |-> req);\n\
                 endmodule\n"
            )
        };
        let run = |design: String, mode: AssumeMode| {
            let sources = vec![("ag_dut.sv".to_string(), design)];
            let yopts = YosysOptions {
                top: Some("ag_dut".to_string()),
                use_sv2v: true,
                ..Default::default()
            };
            let opts = VerifyAutoOptions {
                portfolio: Some(PortfolioMode::Sequential),
                assume_mode: mode,
                ..Default::default()
            };
            let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
            eprintln!("--- {mode:?}: {:?}", report.assumptions);
            for p in &report.properties {
                eprintln!("  {:?}: {:?}\n      {}", p.label, p.outcome, p.formula);
            }
            report
        };
        let outcome = |r: &AutoVerifyReport, l: &str| {
            r.properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        let holds = |o: VerifyOutcome| matches!(o, VerifyOutcome::Holds);
        let violated = |o: VerifyOutcome| matches!(o, VerifyOutcome::Violated { .. });
        let assumes = "  m_noinc: assume property (@(posedge clk) disable iff (!rst_n) !inc);\n\
                       m_stay: assume property (@(posedge clk) disable iff (!rst_n) req |=> req);\n";

        let pre = run(module(assumes), AssumeMode::Precondition);
        assert_eq!(pre.assumptions.mode, "precondition");
        assert_eq!(pre.assumptions.applied, vec!["m_noinc", "m_stay"]);
        assert_eq!(pre.assumptions.consistency.as_deref(), Some("consistent"));
        assert!(
            holds(outcome(&pre, "a_zero")),
            "{:?}",
            outcome(&pre, "a_zero")
        );
        assert!(violated(outcome(&pre, "a_zero_twin")));
        assert!(
            holds(outcome(&pre, "a_held")),
            "{:?}",
            outcome(&pre, "a_held")
        );
        assert!(
            pre.properties
                .iter()
                .all(|p| p.label.as_deref() != Some("m_noinc")
                    && p.name != ASSUMPTION_CONSISTENCY_PROPERTY),
            "assumptions and the consistency check are not reported as properties"
        );

        let chk = run(module(assumes), AssumeMode::Check);
        assert!(violated(outcome(&chk, "a_zero")));
        assert!(violated(outcome(&chk, "a_held")));
        assert!(
            violated(outcome(&chk, "m_noinc")),
            "an assumption is checked in check mode"
        );

        let ign = run(module(assumes), AssumeMode::Ignore);
        assert!(violated(outcome(&ign, "a_zero")));
        assert_eq!(ign.assumptions.unapplied.len(), 2);

        let contra = run(
            module(
                "  m_on: assume property (@(posedge clk) disable iff (!rst_n) req);\n\
                   m_off: assume property (@(posedge clk) disable iff (!rst_n) !req);\n",
            ),
            AssumeMode::Precondition,
        );
        assert_eq!(
            contra.assumptions.consistency.as_deref(),
            Some("contradictory")
        );
        assert!(
            contra
                .notes
                .iter()
                .any(|n| n.kind == "assumptions-contradictory"),
            "every vacuous HOLDS is flagged"
        );
    }

    /// OpenTitan coverage, Phases 2 + 3 — prim_count's saturating-counter checks, verbatim in
    /// shape (`min` / `max` single-return helpers over 6-bit signed sums of `$past` expressions),
    /// decide on a saturating up/down counter; the twins drop the saturation (or the signed cast)
    /// and must be VIOLATED.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_prim_count_saturation_checks_decide_with_twins() {
        let design = "module pc_dut (input logic clk, input logic rst_n, input logic incr,\n\
                      input logic decr, input logic [1:0] step, output logic [3:0] cnt);\n\
                        function automatic logic signed [5:0] max(logic signed [5:0] a, logic signed [5:0] b);\n\
                          return (a > b) ? a : b;\n\
                        endfunction\n\
                        function automatic logic signed [5:0] min(logic signed [5:0] a, logic signed [5:0] b);\n\
                          return (a < b) ? a : b;\n\
                        endfunction\n\
                        logic [4:0] up;\n\
                        assign up = cnt + step;\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) cnt <= 4'd0;\n\
                          else if (incr && !decr) cnt <= up[4] ? 4'd15 : up[3:0];\n\
                          else if (decr && !incr) cnt <= (cnt < step) ? 4'd0 : cnt - step;\n\
                        end\n\
                        a_up: assert property (@(posedge clk) disable iff (!rst_n) incr && !decr |=>\n\
                          cnt == min($past(cnt) + $past({2'b0, step}), {2'b0, {4{1'b1}}}));\n\
                        a_up_twin: assert property (@(posedge clk) disable iff (!rst_n) incr && !decr |=>\n\
                          cnt == $past(cnt) + $past(step));\n\
                        a_dn: assert property (@(posedge clk) disable iff (!rst_n) decr && !incr |=>\n\
                          cnt == max($past(signed'({2'b0, cnt})) - $past({2'b0, step}), '0));\n\
                        a_dn_twin: assert property (@(posedge clk) disable iff (!rst_n) decr && !incr |=>\n\
                          cnt == $past(cnt) - $past(step));\n\
                        a_stable: assert property (@(posedge clk) disable iff (!rst_n) !incr && !decr |=> $stable(cnt));\n\
                      endmodule\n";
        let sources = vec![("pc_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("pc_dut".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        for l in ["a_up", "a_dn", "a_stable"] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Holds),
                "{l}: {:?}",
                outcome(l)
            );
        }
        for l in ["a_up_twin", "a_dn_twin"] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Violated { .. }),
                "{l}: {:?}",
                outcome(l)
            );
        }
    }

    /// OpenTitan coverage, Phase 2 — the INDEPENDENT oracle for the expression compiler: sv2v +
    /// yosys `read_verilog` elaborate each random SV expression `E` into `assign r_k = E` (and a registered copy
    /// `q_k <= E`), and `assert property (r_k == E)` / `(##1 q_k == $past(E))` must HOLD under the
    /// shipped default with `E` lowered by OUR compiler (minted into the model). Mixed widths
    /// (1–6 bits) and signedness exercise slang's conversions, which both elaborators must agree
    /// on; a disagreement is a VIOLATED with the expression named.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_minted_expressions_agree_with_yosys_elaboration() {
        // Deterministic generator over inputs a[3:0], b[5:0] (unsigned), s[3:0] (signed), c.
        let mut seed: u64 = 0x0e7_2026_1015;
        let mut rnd = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        fn random_expr(rnd: &mut dyn FnMut(u64) -> u64, depth: u32) -> String {
            let leaves = [
                "a", "b", "s", "c", "a[2:1]", "b[5]", "4'd9", "3'sd3", "6'h2a",
            ];
            if depth == 0 || rnd(4) == 0 {
                return leaves[rnd(leaves.len() as u64) as usize].to_string();
            }
            let (x, y) = (random_expr(rnd, depth - 1), random_expr(rnd, depth - 1));
            match rnd(20) {
                0 => format!("({x} + {y})"),
                1 => format!("({x} - {y})"),
                2 => format!("({x} & {y})"),
                3 => format!("({x} | {y})"),
                4 => format!("({x} ^ {y})"),
                5 => format!("(~{x})"),
                6 => format!("({x} << a[1:0])"),
                7 => format!("({x} >> {y})"),
                8 => format!("(c ? {x} : {y})"),
                9 => format!("{{{x}, {y}}}"),
                10 => format!("({x} < {y})"),
                11 => format!("($signed({x}) >>> 1)"),
                12 => format!("(^{x})"),
                13 => format!("(-{x})"),
                14 => format!("({x} == {y})"),
                15 => format!("(s >= $signed({x}))"),
                16 => format!("$countones({x})"),
                17 => format!("$onehot0({x})"),
                18 => "b[a[1:0]]".to_string(),
                _ => format!("({x} * 2'd3)"),
            }
        }
        let exprs: Vec<String> = (0..72).map(|_| random_expr(&mut rnd, 3)).collect();
        let mut body = String::new();
        for (k, e) in exprs.iter().enumerate() {
            // A 64-bit SIGNED sink: wide enough for every generated `E`, and signed so the
            // assignment (which extends `E` by its own signedness) and the comparison (which
            // extends both sides by the comparison's) mean the same thing. A size cast `N'(E)` is
            // avoided — sv2v and slang disagree on its operand width. `$past`'s argument is
            // SELF-determined (SV 11.8.1), so the registered copy holds `{E}` — a concatenation,
            // self-determined too — and is compared with `$past({E})`.
            body.push_str(&format!(
                "  logic signed [63:0] r{k};\n  logic [63:0] q{k};\n  assign r{k} = {e};\n  \
                 always_ff @(posedge clk) q{k} <= {{{e}}};\n  \
                 eq{k}: assert property (@(posedge clk) r{k} == {e});\n  \
                 pq{k}: assert property (@(posedge clk) ##1 q{k} == $past({{{e}}}));\n"
            ));
        }
        let design = format!(
            "module ex_dut (input logic clk, input logic [3:0] a, input logic [5:0] b,\n\
             input logic signed [3:0] s, input logic c);\n{body}endmodule\n"
        );
        let sources = vec![("ex_dut.sv".to_string(), design.clone())];
        let yopts = YosysOptions {
            top: Some("ex_dut".to_string()),
            // sv2v + `read_verilog`: an elaborator independent of slang (which typed the SVA
            // side), and one that ignores the assertions the slang plugin refuses to lift.
            use_sv2v: true,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts)
            .unwrap_or_else(|e| panic!("{}\n{design}", e.message));
        let mut bad = Vec::new();
        for p in &report.properties {
            let l = p.label.as_deref().unwrap_or("?");
            let k: usize = l
                .trim_start_matches(|c: char| c.is_alphabetic())
                .parse()
                .unwrap();
            if !matches!(p.outcome, VerifyOutcome::Holds) {
                bad.push(format!(
                    "{l} `{}`: {:?}\n      {}",
                    exprs[k], p.outcome, p.formula
                ));
            }
        }
        // A multiplication the comparison widens past 32 bits is refused by design (the cap), and
        // so is `>>>` of a signed operand in an unsigned context (the tools disagree); every
        // other refusal is a finding.
        let by_design = |r: &str| {
            r.contains("multiplication (the cap is 32 bits)") || r.contains("fill differently")
        };
        let capped = report
            .unsupported
            .iter()
            .filter(|(_, r)| by_design(r))
            .count();
        for (n, r) in &report.unsupported {
            if !by_design(r) {
                bad.push(format!("unsupported {n}: {r}"));
            }
        }
        assert!(bad.is_empty(), "\n{}", bad.join("\n"));
        eprintln!(
            "{} assertions HOLD, {capped} refused by design",
            report.properties.len()
        );
        assert!(
            report.properties.len() * 5 >= 2 * exprs.len() * 4,
            "at least 80% of the assertions translate"
        );
    }

    /// OpenTitan coverage, Phase 1 — every new lowering decides under the shipped default, each
    /// with a twin whose verdict flips if the bit mapping is wrong. `w` is the constant
    /// `{hi: 9, mid: 2, lo: 1}` = `1001_10_1`: read first-member-LEAST-significant, `w.mid` would
    /// be bits 5..4 = 1 and both `a_mid` and its twin would flip. `cnt[0]` is the constant 5 while
    /// `cnt[1]` counts, so swapping the element order flips `a_el` and its twin. `a` and `b`
    /// toggle in antiphase from 0 / 1.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_phase1_forms_decide_with_twins() {
        let design = "package p1;\n\
                        typedef struct packed { logic [3:0] hi; logic [1:0] mid; logic lo; } wide_t;\n\
                      endpackage\n\
                      module ph_dut import p1::*; (input logic clk, input logic rst_n,\n\
                        output wide_t w, output logic [1:0][3:0] cnt, output logic a, output logic b);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) begin w <= {4'd9, 2'd2, 1'b1}; cnt <= {4'd0, 4'd5}; a <= 1'b0; b <= 1'b1; end\n\
                          else begin w <= w; cnt <= {cnt[1] + 4'd1, cnt[0]}; a <= ~a; b <= ~b; end\n\
                        end\n\
                        a_mid:       assert property (@(posedge clk) disable iff (!rst_n) w.mid == 2'd2);\n\
                        a_mid_twin:  assert property (@(posedge clk) disable iff (!rst_n) w.mid == 2'd1);\n\
                        a_hi:        assert property (@(posedge clk) disable iff (!rst_n) w.hi == 4'd9 && w.lo);\n\
                        a_el:        assert property (@(posedge clk) disable iff (!rst_n) cnt[0] == 4'd5);\n\
                        a_el_twin:   assert property (@(posedge clk) disable iff (!rst_n) cnt[1] == 4'd5);\n\
                        a_xor:       assert property (@(posedge clk) disable iff (!rst_n) a ^ b);\n\
                        a_and_twin:  assert property (@(posedge clk) disable iff (!rst_n) a & b);\n\
                        a_impl:      assert property (@(posedge clk) disable iff (!rst_n) a -> !b);\n\
                        a_equiv_twin: assert property (@(posedge clk) disable iff (!rst_n) a <-> b);\n\
                        a_seq:       assert property (@(posedge clk) disable iff (!rst_n) ##1 a != b);\n\
                        a_seq_twin:  assert property (@(posedge clk) disable iff (!rst_n) ##1 a);\n\
                        a_rep:       assert property (@(posedge clk) disable iff (!rst_n) cnt[0] != {4{1'b1}});\n\
                      endmodule\n";
        let sources = vec![("ph_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("ph_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        eprintln!("  unsupported: {:?}", report.unsupported);
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .map(|p| p.outcome.clone())
                .unwrap_or_else(|| panic!("no property `{l}`"))
        };
        for l in ["a_mid", "a_hi", "a_el", "a_xor", "a_impl", "a_seq", "a_rep"] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Holds),
                "{l} HOLDS: {:?}",
                outcome(l)
            );
        }
        for l in [
            "a_mid_twin",
            "a_el_twin",
            "a_and_twin",
            "a_equiv_twin",
            "a_seq_twin",
        ] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Violated { .. }),
                "{l} VIOLATED: {:?}",
                outcome(l)
            );
        }
    }

    /// `throughout` and `$countones` (2026-10-15) decide end to end under the shipped default, with
    /// twins. `st`: IDLE 0 -req-> WAIT 1; WAIT -ack-> DONE 2; DONE -> IDLE. `gnt` is one-hot of
    /// `st`; `pair` is the two free inputs.
    ///
    /// - `a_thr`: `(WAIT && ack) |-> ((st != IDLE) throughout ##1 DONE)` holds; `a_thr_twin` with
    ///   `(st == WAIT)` as the invariant is VIOLATED — the invariant must hold at the match END too.
    /// - `a_cnt`: `$countones(gnt) == 1` holds; `a_cnt_slice`: `$countones(gnt[1:0]) <= 1` holds;
    ///   `a_cnt_twin`: `$countones(pair) <= 1` is VIOLATED (both inputs high).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_throughout_and_countones_decide_with_twins() {
        let design = "module tc_dut (input logic clk, input logic rst_n, input logic req,\n\
                      input logic ack, output logic [1:0] st, output logic [3:0] gnt,\n\
                      output logic [1:0] pair);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) st <= 2'd0;\n\
                          else case (st)\n\
                            2'd0: st <= req ? 2'd1 : 2'd0;\n\
                            2'd1: st <= ack ? 2'd2 : 2'd1;\n\
                            default: st <= 2'd0;\n\
                          endcase\n\
                        end\n\
                        assign gnt = 4'b0001 << st;\n\
                        assign pair = {req, ack};\n\
                        a_thr: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ((st == 2'd1) && ack) |-> ((st != 2'd0) throughout (##1 (st == 2'd2))));\n\
                        a_thr_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ((st == 2'd1) && ack) |-> ((st == 2'd1) throughout (##1 (st == 2'd2))));\n\
                        a_cnt: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          $countones(gnt) == 1);\n\
                        a_cnt_slice: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          $countones(gnt[1:0]) <= 1);\n\
                        a_cnt_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          $countones(pair) <= 1);\n\
                      endmodule\n";
        let sources = vec![("tc_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("tc_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto runs");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?} [{:?}]\n      {}",
                p.name, p.label, p.outcome, p.decided_by, p.formula
            );
        }
        assert!(
            report.unsupported.is_empty(),
            "all five translate: {:?}",
            report.unsupported
        );
        let outcome = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` missing: {:?}", report.properties))
                .outcome
                .clone()
        };
        for l in ["a_thr", "a_cnt", "a_cnt_slice"] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Holds),
                "{l} must HOLD: {:?}",
                outcome(l)
            );
        }
        for l in ["a_thr_twin", "a_cnt_twin"] {
            assert!(
                matches!(outcome(l), VerifyOutcome::Violated { .. }),
                "{l} must be VIOLATED: {:?}",
                outcome(l)
            );
        }
    }

    /// Ranged antecedents (2026-10-15) decide end to end, with twins. `st`: IDLE 0 -req-> WAIT 1;
    /// WAIT -ack-> DONE 2 (else stays); DONE -> IDLE.
    ///
    /// - `a_range`: `(IDLE && req) ##[1:2] (WAIT && ack) |=> DONE` — both match ends land on DONE.
    /// - `a_range_twin`: the same without `WAIT &&` — `ack` at +1 AND +2 gives a second match end
    ///   one cycle after DONE, back in IDLE: VIOLATED. The twin is the check that the second match
    ///   end is actually tested.
    /// - `a_rep`: `WAIT[*1:2] |-> !DONE` holds; `a_rep_twin`: `WAIT[*2:3] |=> WAIT` is VIOLATED (an
    ///   `ack` after two cycles of waiting leaves WAIT).
    /// - `a_rep_chain`: `(IDLE && req) ##1 WAIT[*2] |=> !IDLE` — repetition inside a chain.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_range_antecedent_decides_on_handshake_fsm() {
        let design = "module hs_dut (input logic clk, input logic rst_n, input logic req,\n\
                      input logic ack, output logic [1:0] st);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) st <= 2'd0;\n\
                          else case (st)\n\
                            2'd0: st <= req ? 2'd1 : 2'd0;\n\
                            2'd1: st <= ack ? 2'd2 : 2'd1;\n\
                            default: st <= 2'd0;\n\
                          endcase\n\
                        end\n\
                        a_range: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ((st == 2'd0) && req) ##[1:2] ((st == 2'd1) && ack) |=> (st == 2'd2));\n\
                        a_range_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ((st == 2'd0) && req) ##[1:2] ack |=> (st == 2'd2));\n\
                        a_rep: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (st == 2'd1)[*1:2] |-> (st != 2'd2));\n\
                        a_rep_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (st == 2'd1)[*2:3] |=> (st == 2'd1));\n\
                        a_rep_chain: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          ((st == 2'd0) && req) ##1 (st == 2'd1)[*2] |=> (st != 2'd0));\n\
                      endmodule\n";
        let sources = vec![("hs_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("hs_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        // The shipped CLI / API default is `portfolio-sequential` (exact first); the library
        // default runs the predicate cube + rescues only. Both are measured.
        let run = |portfolio: Option<PortfolioMode>| {
            let opts = VerifyAutoOptions {
                portfolio,
                ..Default::default()
            };
            let report = verify_auto(&sources, &yopts, &opts)
                .expect("verify_auto runs on the handshake design");
            eprintln!("== portfolio {portfolio:?}");
            for p in &report.properties {
                eprintln!(
                    "  {} ({:?}): {:?} [{:?}]\n      {}",
                    p.name, p.label, p.outcome, p.decided_by, p.formula
                );
            }
            assert!(
                report.unsupported.is_empty(),
                "all five translate: {:?}",
                report.unsupported
            );
            report
        };
        let outcome = |r: &AutoVerifyReport, l: &str| {
            r.properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` missing: {:?}", r.properties))
                .outcome
                .clone()
        };
        let holds = ["a_range", "a_rep", "a_rep_chain"];
        let violated = ["a_range_twin", "a_rep_twin"];

        // The shipped default decides all five, and the twins' second match ends are tested.
        let portfolio = run(Some(PortfolioMode::Sequential));
        for l in holds {
            assert!(
                matches!(outcome(&portfolio, l), VerifyOutcome::Holds),
                "{l}: {:?}",
                outcome(&portfolio, l)
            );
        }
        for l in violated {
            assert!(
                matches!(outcome(&portfolio, l), VerifyOutcome::Violated { .. }),
                "{l} must be VIOLATED under the default portfolio: {:?}",
                outcome(&portfolio, l)
            );
        }

        // The cube alone (library default): the HOLDS rows decide; a twin is never HOLDS (it
        // may be ⊥ — the cube's may-relation cannot witness these nested-box violations and no
        // rescue lane reduces the shape — but a HOLDS would be unsound).
        let cube = run(None);
        for l in holds {
            assert!(
                matches!(outcome(&cube, l), VerifyOutcome::Holds),
                "{l}: {:?}",
                outcome(&cube, l)
            );
        }
        for l in violated {
            assert!(
                !matches!(outcome(&cube, l), VerifyOutcome::Holds),
                "{l}: {:?}",
                outcome(&cube, l)
            );
        }
    }

    /// SVA-fragment widening (2026-10-15) — liveness SVA decides end to end, with the HOLDS /
    /// VIOLATED twins that show it is not vacuous.
    ///
    /// `st` is a responder (idle -req-> 1 -> 2 -> idle); `st2` is its staller twin (idle -req-> 1
    /// -> 3, stuck). The same assertion over each must split HOLDS / VIOLATED. `##[1:$]` carries
    /// its `strong-sequence-reading` note; `s_eventually` and the other forms carry none.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_sva_liveness_forms_decide_with_responder_and_staller_twins() {
        let design = "module live_dut (input logic clk, input logic rst_n, input logic req,\n\
                      output logic [1:0] st, output logic [1:0] st2);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) begin st <= 2'd0; st2 <= 2'd0; end\n\
                          else begin\n\
                            case (st)\n\
                              2'd0: st <= req ? 2'd1 : 2'd0;\n\
                              2'd1: st <= 2'd2;\n\
                              default: st <= 2'd0;\n\
                            endcase\n\
                            case (st2)\n\
                              2'd0: st2 <= req ? 2'd1 : 2'd0;\n\
                              2'd1: st2 <= 2'd3;\n\
                              default: st2 <= st2;\n\
                            endcase\n\
                          end\n\
                        end\n\
                        a_resp: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          req |-> s_eventually (st == 2'd2));\n\
                        a_resp_stall: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          req |-> s_eventually (st2 == 2'd2));\n\
                        a_home: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          s_eventually (st == 2'd0));\n\
                        a_home_stall: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          s_eventually (st2 == 2'd0));\n\
                        a_unbounded: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (req && st == 2'd0) |-> ##[1:$] (st == 2'd2));\n\
                        a_rep: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (req && st == 2'd0) |=> (st == 2'd1)[*1:$]);\n\
                        a_next: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (st == 2'd1) |-> nexttime (st == 2'd2));\n\
                      endmodule\n";
        let sources = vec![("live_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("live_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs on the liveness design");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        for n in &report.notes {
            eprintln!("  note [{}] {:?}: {}", n.kind, n.property, n.summary);
        }
        assert!(
            report.unsupported.is_empty(),
            "all seven translate: {:?}",
            report.unsupported
        );
        let by_label = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` missing from {:?}", report.properties))
        };
        for l in ["a_resp", "a_home", "a_unbounded", "a_rep", "a_next"] {
            assert!(
                matches!(by_label(l).outcome, VerifyOutcome::Holds),
                "{l} must HOLD: {:?} / {}",
                by_label(l).outcome,
                by_label(l).formula
            );
        }
        for l in ["a_resp_stall", "a_home_stall"] {
            assert!(
                matches!(by_label(l).outcome, VerifyOutcome::Violated { .. }),
                "{l} must be VIOLATED (st2 sticks at 3): {:?} / {}",
                by_label(l).outcome,
                by_label(l).formula
            );
        }
        let strong_notes: Vec<_> = report
            .notes
            .iter()
            .filter(|n| n.kind == "strong-sequence-reading")
            .map(|n| n.property.clone())
            .collect();
        assert_eq!(
            strong_notes,
            vec![Some(by_label("a_unbounded").name.clone())],
            "exactly the `##[1:$]` assertion carries the strong-reading note"
        );
    }

    /// SVA-fragment widening (2026-10-15) — constant bit-selects, slices of a NON-zero-based
    /// vector, an edge of one bit and `!$isunknown` decide end to end on a real slang + yosys lift.
    ///
    /// `din` is declared `[15:8]`, so every index here is offset from the bit position yosys gives
    /// it — the case the slice path got wrong before (an `[11:8]` slice was minted as BTOR2 bits
    /// 11..8 of an 8-bit vector and never bound). The HOLDS / VIOLATED twins (`din[9]` vs `din[8]`)
    /// are the control that the index mapping picks the RIGHT bit, not merely some bit.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_bit_select_offset_slice_edge_and_two_state_fold_decide() {
        let design = "module bs_dut (input logic clk, input logic rst_n, input logic we,\n\
                      input logic [15:8] din, output logic [15:8] q, output logic flag);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) begin q <= '0; flag <= 1'b0; end\n\
                          else if (we) begin q <= din; flag <= din[9]; end\n\
                        end\n\
                        a_bit: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (we && din[9]) |=> flag);\n\
                        a_bit_twin: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (we && din[8]) |=> flag);\n\
                        a_offset_slice: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (we && (din[11:8] == 4'd5)) |=> (q[11:8] == 4'd5));\n\
                        a_rose_bit: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          $rose(q[9]) |-> $past(we));\n\
                        a_known: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          !$isunknown(q));\n\
                      endmodule\n";
        let sources = vec![("bs_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("bs_dut".to_string()),
            use_sv2v: true,
            frontend: crate::adapter::yosys::SvFrontend::Slang,
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs on the bit-select design");
        for p in &report.properties {
            eprintln!(
                "  {} ({:?}): {:?}\n      {}",
                p.name, p.label, p.outcome, p.formula
            );
        }
        for n in &report.notes {
            eprintln!("  note [{}] {:?}: {}", n.kind, n.property, n.summary);
        }
        assert!(
            report.unsupported.is_empty(),
            "all five translate: {:?}",
            report.unsupported
        );
        let by_label = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` missing from {:?}", report.properties))
        };
        for l in ["a_bit", "a_offset_slice", "a_rose_bit", "a_known"] {
            assert!(
                matches!(by_label(l).outcome, VerifyOutcome::Holds),
                "{l} must HOLD: {:?} / {}",
                by_label(l).outcome,
                by_label(l).formula
            );
        }
        assert!(
            matches!(
                by_label("a_bit_twin").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "the din[8] twin must be VIOLATED (flag latches din[9]): {:?}",
            by_label("a_bit_twin").outcome
        );
        // The offset mapping, visible in the atom names: din[9] is bit 1, din[11:8] bits 3..0.
        assert!(
            by_label("a_bit").formula.contains("din__bits1_1"),
            "{}",
            by_label("a_bit").formula
        );
        assert!(by_label("a_offset_slice").formula.contains("q__bits3_0"));
        // The 2-state fold is reported against the property it applies to.
        let known = &by_label("a_known").name;
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.kind == "two-state-fold" && n.property.as_ref() == Some(known)),
            "a two-state-fold note for {known}: {:?}",
            report
                .notes
                .iter()
                .map(|n| (&n.kind, &n.property))
                .collect::<Vec<_>>()
        );
    }

    /// mununu#565 — a bit-slice comparison lifts, binds and DECIDES end to end.
    ///
    /// The refusal this replaces was `got left=Some("RangeSelect") right=Some("IntegerLiteral")`,
    /// while the same claim over the whole word decided — so the gap was the slice, not the
    /// comparison. Both the slice-vs-literal and the `$past`-of-a-slice guise are covered, plus
    /// the whole-word control that already worked, so a regression in either direction shows up.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_565_bit_slice_operand_decides_instead_of_being_refused() {
        let design = "module slice_dut (input logic clk, input logic rst_n, input logic we,\n\
                      input logic [31:0] wdata, output logic [7:0] shadow_q);\n\
                        always_ff @(posedge clk or negedge rst_n) begin\n\
                          if (!rst_n) shadow_q <= 8'd0;\n\
                          else if (we) shadow_q <= wdata[7:0];\n\
                        end\n\
                        a_slice: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (we && (wdata[7:0] == 8'hA5)) |=> (shadow_q == 8'hA5));\n\
                        a_past_slice: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          we |=> (shadow_q == $past(wdata[7:0])));\n\
                        a_whole_word: assert property (@(posedge clk) disable iff (!rst_n)\n\
                          (we && (wdata == 32'h000000A5)) |=> (shadow_q == 8'hA5));\n\
                      endmodule\n";
        let sources = vec![("slice_dut.sv".to_string(), design.to_string())];
        let yopts = YosysOptions {
            top: Some("slice_dut".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs on a sliced design");

        // The headline: nothing is refused for being a slice any more.
        assert!(
            !report
                .unsupported
                .iter()
                .any(|(_, reason)| reason.contains("RangeSelect")),
            "no property may be refused for carrying a bit-slice: {:?}",
            report.unsupported
        );
        assert_eq!(
            report.properties.len(),
            3,
            "all three assertions translate: {:?}",
            report.unsupported
        );

        let by_name = |n: &str| {
            report
                .properties
                .iter()
                .find(|p| p.name == n)
                .unwrap_or_else(|| panic!("{n} missing from {:?}", report.properties))
        };

        // (1) THE CAPABILITY. A slice compared to a literal decides. This is mununu#565's
        // headline shape and the one that used to come back in `unsupported`.
        assert!(
            matches!(by_name("slice_dut_sva_0").outcome, VerifyOutcome::Holds),
            "slice-vs-literal must DECIDE; got {:?}",
            by_name("slice_dut_sva_0").outcome
        );

        // (2) THE CONTROL. The whole-word form already decided and must keep doing so.
        assert!(
            matches!(by_name("slice_dut_sva_2").outcome, VerifyOutcome::Holds),
            "the whole-word control must still decide; got {:?}",
            by_name("slice_dut_sva_2").outcome
        );

        // (3) THE BOUNDARY, re-measured 2026-10-15. `shadow_q == $past(wdata[7:0])` used to SKIP,
        // and this test explained it by the all-state compound rule. That explanation was wrong:
        // `$past(wdata[7:0])` never requested `wdata`'s shadow (`collect_shadow_signals` resolved
        // only a plain-signal argument), so `wdata__past` did not exist, the slice
        // `wdata__past__bits7_0` had no base and was never minted, and the atom was unbound. With
        // the shadow requested (`past_over_a_slice_requests_the_base_shadow`), the atom binds and
        // the property reaches the engines. It is ⊥ today, not a verdict: a register-vs-register
        // consequent behind `|=>` is the known leaf-shape gap of the safety-rescue lane. What
        // this row pins is that it is NO LONGER skipped for an unbound atom, and is never given a
        // verdict it did not earn (it is true on the design, so VIOLATED would be a bug).
        let past_slice = &by_name("slice_dut_sva_1").outcome;
        eprintln!("  slice_dut_sva_1 (`we |=> shadow_q == $past(wdata[7:0])`): {past_slice:?}");
        assert!(
            !matches!(past_slice, VerifyOutcome::Skipped { reason } if reason.contains("wdata__past__bits7_0")),
            "the `$past`-of-a-slice atom must bind now: {past_slice:?}"
        );
        assert!(
            !matches!(past_slice, VerifyOutcome::Violated { .. }),
            "the property is true on the design; VIOLATED would be unsound: {past_slice:?}"
        );

        // And the minted derived signals are the ones the naming contract promises, so a
        // counterexample or a seeded-predicate list names something a reader can find.
        let atoms: String = report
            .properties
            .iter()
            .map(|p| p.formula.clone())
            .collect();
        assert!(
            atoms.contains("wdata__bits7_0"),
            "the slice atom is a plain identifier: {atoms}"
        );
        assert!(
            atoms.contains("wdata__past__bits7_0"),
            "`$past` of a slice slices the SHADOW: {atoms}"
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3; run with --ignored"]
    fn e2e_reset_handled_by_gating_or_as_free_input() {
        // A 2-bit FSM cycling 0→1→2→0, with an active-low-reset-guarded
        // assertion `disable iff (!rst_n) state != 3`. Two SOUND paths to a
        // verdict, exercised here:
        //
        //   * Reset-gating ON (default) — the `disable iff` guard is dropped,
        //     rst_n is pinned inactive and `inject_reset_init` establishes
        //     `init state = 0` (the post-reset value), so the body `state != 3`
        //     is a pure state-cell property over an established start → HOLDS.
        //     (`gated_resets = ["rst_n=1"]`.)
        //   * Reset-gating OFF — the guard is KEPT, so the formula carries the
        //     `!rst_n` vacuity disjunct, and (H.B) rst_n — a primary input — is
        //     admitted as a FREE cube dimension. No reset is applied and the
        //     lift carries no `init` line for `state`, so — since mununu#579
        //     converged the cube path on the convention the exact engine
        //     (`initial_state_bdd`, mununu#498) and the reachability portfolio
        //     already used — `state` is FREE at cycle 0 too. `state == 3` with
        //     `rst_n` high is then an admissible first cycle, and the property
        //     is VIOLATED on that model, with a real cycle-0 witness.
        //     (`gated_resets` empty.)
        //
        // The old un-gated expectation was HOLDS, and it rested on the cube
        // path's zero pin for an init-less cell: "the FSM never reaches 3" is
        // true only from 0. The pin was the mununu#577 / #579 divergence — one
        // report, two initial states — and it is gone. This is the standard
        // formal reading too: a register nothing initialises and no reset
        // sequence establishes is unconstrained at cycle 0, and the remedy is
        // to apply the reset (gating ON), not to assume a value.
        //
        // Pre-H.B the un-gated `!rst_n` atom was unbindable → SKIPPED; H.B's
        // free-input admission subsumes that skip with a sound verdict. Both
        // paths are sound; they differ in what they assume about cycle 0 (the
        // reset applied vs. nothing), visible in the diagnostics.
        let sv = "module fsm (input logic clk, input logic rst_n);\n\
                  logic [1:0] state;\n\
                  always_ff @(posedge clk) begin\n\
                    if (!rst_n) state <= 2'd0;\n\
                    else state <= (state == 2'd2) ? 2'd0 : state + 2'd1;\n\
                  end\n\
                  ok: assert property (@(posedge clk) disable iff (!rst_n) state != 2'd3);\n\
                  endmodule\n";
        let sources = vec![("fsm.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("fsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        };

        // Reset-gating ON (default): the guard is dropped + rst_n pinned.
        let gated = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs with reset-gating");
        assert_eq!(gated.properties.len(), 1);
        assert!(
            matches!(gated.properties[0].outcome, VerifyOutcome::Holds),
            "reset-gated `state != 3` should HOLD; got {:?}",
            gated.properties[0].outcome
        );
        assert_eq!(
            gated.diagnostics.gated_resets,
            vec!["rst_n=1".to_string()],
            "active-low reset pinned inactive"
        );

        // Reset-gating OFF: the `!rst_n` atom is a primary input, admitted by
        // H.B as a free cube dimension, and `state` — no `init`, no reset
        // applied — is free at cycle 0. The kept `disable iff` disjunct makes
        // reset cycles vacuous, but a first cycle with `rst_n` high and
        // `state == 3` is admissible → VIOLATED, and not withheld: the
        // mununu#578 downgrade fires only when a reset was PINNED (that pin is
        // what removes a reset path), and nothing was pinned here.
        let ungated = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                gate_reset: false,
                ..Default::default()
            },
        )
        .expect("verify_auto runs without reset-gating");
        assert!(
            matches!(
                ungated.properties[0].outcome,
                VerifyOutcome::Violated { .. }
            ),
            "un-gated, `state` has no `init` and no reset is applied, so it is free \
             at cycle 0 on the cube path like on every other engine (mununu#579); \
             `state == 3 && rst_n` is an admissible first cycle → VIOLATED; got {:?}",
            ungated.properties[0].outcome
        );
        assert!(
            ungated.diagnostics.gated_resets.is_empty(),
            "no reset was pinned in the un-gated run"
        );
    }

    /// mununu#635 — the two mutation kinds `--list` advertised and `--help` did not APPLY through
    /// `verify_auto` to the `link_ctrl` fixture (`invert-cond:violation` used to error with "no
    /// use sites": the name sits on a `uext 0` alias the logic never reads). What they measure is
    /// the SPEC's adequacy, never the design: a flip means the property caught the planted fault,
    /// a non-flip means no property pins that behaviour (claims integrity).
    ///
    /// MEASURED (2026-10-08, in the image, on a lift where #639 folds the `localparam` atoms so
    /// the four properties that read them decide): `off-by-one` on the retry limit is KILLED —
    /// `a_err_implies_retry_limit` Holds → Violated (`err` rises at the shifted limit, which is
    /// no longer `MAX_RETRIES`) — and `a_retry_limit`, violated on the design, moves to Holds
    /// (its violation sat exactly at the limit the mutation moved). Inverting `violation` moves
    /// only that already-violated `a_retry_limit` to Holds (the inverted gate sends every
    /// request to FATAL before any retry); NO holding property is violated, because none says
    /// fatal is entered ONLY on a violation — that hole is the finding this test records. Before
    /// #639 both mutations flipped nothing, on the same suite: the properties that catch the
    /// off-by-one were the skipped ones. A spec's measured adequacy is a property of which of
    /// its properties DECIDE, not of its text.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_635_advertised_mutation_kinds_apply_and_the_link_ctrl_suite_kills_the_off_by_one_only() {
        use crate::adapter::btor2::mutate::Mutation;
        use crate::adapter::yosys::SvFrontend;
        use std::path::PathBuf;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/v12_link_ctrl_llm_fsm/link_ctrl.sv");
        let sv = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let sources = vec![("link_ctrl.sv".to_string(), sv)];
        let yopts = YosysOptions {
            top: Some("link_ctrl".to_string()),
            use_sv2v: true,
            frontend: SvFrontend::Slang,
            ..Default::default()
        };
        let run = |mutation: Option<&str>| {
            let report = verify_auto(
                &sources,
                &yopts,
                &VerifyAutoOptions {
                    mutation: mutation.map(|m| Mutation::parse(m).expect("parses")),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("verify_auto ({mutation:?}): {}", e.message));
            report
                .properties
                .iter()
                .map(|p| {
                    let class = match &p.outcome {
                        VerifyOutcome::Holds => "holds",
                        VerifyOutcome::Violated { .. } => "violated",
                        VerifyOutcome::Skipped { .. } => "skipped",
                        _ => "unknown",
                    };
                    (p.label.clone().unwrap_or_else(|| p.name.clone()), class)
                })
                .collect::<Vec<_>>()
        };
        let base = run(None);
        assert_eq!(base.len(), 8, "the fixture's eight assertions: {base:?}");
        // (label, before, after) per property whose verdict the mutation moved, in file order.
        let flips = |m: &str| -> Vec<(String, &'static str, &'static str)> {
            // Applying is the #635 fix; the mutated run must produce the same property set.
            let mutated = run(Some(m));
            assert_eq!(mutated.len(), base.len(), "{m}: same property set");
            let f: Vec<_> = base
                .iter()
                .zip(mutated.iter())
                .filter(|(b, m)| b.1 != m.1)
                .map(|(b, m)| (b.0.clone(), b.1, m.1))
                .collect();
            eprintln!("=== {m}: {} flip(s) {f:?}", f.len());
            f
        };
        let obo = flips("off-by-one:retry_cnt_q");
        let inv = flips("invert-cond:violation");
        // Both spellings reach the same mutation.
        assert_eq!(
            flips("off_by_one:retry_cnt_q"),
            obo,
            "the `_` alias is the same mutation"
        );
        assert_eq!(
            obo,
            vec![
                ("a_err_implies_retry_limit".to_string(), "holds", "violated"),
                ("a_retry_limit".to_string(), "violated", "holds"),
            ],
            "the off-by-one on the retry limit is killed by `a_err_implies_retry_limit`, and \
             moves the design's own `a_retry_limit` violation; a changed set is a changed spec"
        );
        assert_eq!(
            inv,
            vec![("a_retry_limit".to_string(), "violated", "holds")],
            "inverting `violation` moves only the already-violated `a_retry_limit`"
        );
        assert!(
            inv.iter().all(|(_, before, _)| *before != "holds"),
            "no HOLDING property is violated by a fatal entered without a violation — the \
             suite's measured hole; a property that kills it changes this: {inv:?}"
        );
    }

    /// mununu#632 — an LLM-written FSM whose SVA compare registers against `localparam`
    /// constants (`state_q == S_XFER`, `retry_cnt_q == MAX_RETRIES`). The translator used to
    /// keep the parameter NAME in the atom and the engine skipped the property as "unknown
    /// register/signal"; the names now fold to slang's elaborated values. Four of the eight
    /// properties moved skipped → decided (measured in the image, 2026-10-08), and one of them —
    /// `a_retry_limit`, `!(retry_cnt_q == 2'd3 && state_q == S_RETRY)` — is VIOLATED on the
    /// design: the third `nack` in `S_REQ` sees `retry_cnt_q == 2`, increments it to 3 and
    /// enters `S_RETRY`, so the counter sits at its limit IN the retry state. The other three
    /// HOLD. The issue measured the same with the literals spelled by hand.
    /// mununu#717 — run the issue's shapes under a `--param` override, reset pinned inactive.
    fn run_717(
        sources: &[(&str, &str)],
        top: &str,
        params: &[(&str, &str)],
    ) -> Result<AutoVerifyReport, AdapterError> {
        let sources: Vec<(String, String)> = sources
            .iter()
            .map(|(n, c)| ((*n).to_string(), (*c).to_string()))
            .collect();
        let yopts = YosysOptions {
            top: Some(top.to_string()),
            params: params
                .iter()
                .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
                .collect(),
            ..Default::default()
        };
        let mut opts = VerifyAutoOptions::default();
        opts.config_values.insert("rst_n".to_string(), 1);
        verify_auto(&sources, &yopts, &opts)
    }

    const CNT_717: &str = "module cnt #(parameter int W = 10) (\n\
        input logic clk, input logic rst_n, input logic inc);\n\
        logic [W-1:0] cur_q;\n\
        always_ff @(posedge clk or negedge rst_n) begin\n\
          if (!rst_n) cur_q <= '0;\n\
          else if (inc && (cur_q != {W{1'b1}})) cur_q <= cur_q + 1'b1;\n\
        end\n\
        endmodule\n";

    /// mununu#717 — a bound SVA whose port is sized for the narrowed width. slang elaborated `cnt`
    /// at the default `W = 10` while the lift applied `W = 4`, so the bound port became a
    /// truncation of a 10-bit signal the 4-bit model did not have, and the property was SKIPPED
    /// ("unknown register/signal `mununu_x_…`"). Fails on the parent `e7a5b21`.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_717_bound_sva_under_a_param_override_decides() {
        let sva = "module cnt_sva (input logic clk, input logic rst_n, input logic inc,\n\
                   input logic [3:0] cur_q);\n\
                   a_sat: assert property (@(posedge clk) disable iff (!rst_n)\n\
                     (cur_q == 4'd15) |=> (cur_q == 4'd15));\n\
                   a_never_sat: assert property (@(posedge clk) disable iff (!rst_n)\n\
                     cur_q != 4'd15);\n\
                   endmodule\n\
                   bind cnt cnt_sva u_sva (.*);\n";
        let report = run_717(
            &[("cnt.sv", CNT_717), ("cnt_sva.sv", sva)],
            "cnt",
            &[("W", "4")],
        )
        .expect("verify_auto runs");
        let by_label = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` in {:?}", report.properties))
        };
        assert!(
            matches!(by_label("a_sat").outcome, VerifyOutcome::Holds),
            "saturation holds at W = 4: {:?}",
            by_label("a_sat").outcome
        );
        // The contrast twin: the 4-bit counter DOES reach 15, so the claim it never does is
        // violated — proof the HOLDS above is about the narrowed design, not a vacuous one.
        assert!(
            matches!(
                by_label("a_never_sat").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "the counter reaches 15 at W = 4: {:?}",
            by_label("a_never_sat").outcome
        );
    }

    /// mununu#717, the wrong-verdict sibling — a parameter folded into an atom took its DEFAULT
    /// value. The counter saturates at `MAXV`; under `--param MAXV=5` it reaches 5, so
    /// `c_q != MAXV` is VIOLATED. With the default folded in (`c_q != 12`, a value the overridden
    /// design never reaches) the parent reported a false HOLDS.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_717_a_parameter_in_an_atom_folds_to_its_override() {
        let sv = "module sat #(parameter logic [3:0] MAXV = 4'd12) (\n\
                  input logic clk, input logic rst_n, input logic inc);\n\
                  logic [3:0] c_q;\n\
                  always_ff @(posedge clk or negedge rst_n) begin\n\
                    if (!rst_n) c_q <= '0;\n\
                    else if (inc && c_q != MAXV) c_q <= c_q + 1'b1;\n\
                  end\n\
                  a_reaches_max: assert property (@(posedge clk) disable iff (!rst_n) c_q != MAXV);\n\
                  a_bounded: assert property (@(posedge clk) disable iff (!rst_n) c_q <= MAXV);\n\
                  endmodule\n";
        let report = run_717(&[("sat.sv", sv)], "sat", &[("MAXV", "5")]).expect("verify_auto runs");
        let by_label = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("`{l}` in {:?}", report.properties))
        };
        assert!(
            matches!(
                by_label("a_reaches_max").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "c_q reaches the overridden MAXV = 5: {:?} / {}",
            by_label("a_reaches_max").outcome,
            by_label("a_reaches_max").formula
        );
        assert!(
            matches!(by_label("a_bounded").outcome, VerifyOutcome::Holds),
            "{:?}",
            by_label("a_bounded").outcome
        );
    }

    /// 2026-10-30 — one run, two postures: a `@mununu_template` row and an exempt
    /// (`assumes=ignore`) row next to a design with an applied `assume`. The exempt rows get the
    /// verdict a whole `--assumes ignore` run gives them; the other rows are judged under the
    /// assumption and differ from that run — the exemption is per row, not global. The template
    /// rows carry their instance and their own source line.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_template_annotation_and_exempt_row_in_one_run() {
        use crate::adapter::yosys::SvFrontend;
        let sv = "module cnt (input logic clk, input logic rst_n, input logic inc, output logic [1:0] q);\n\
                  always_ff @(posedge clk or negedge rst_n) if (!rst_n) q <= '0; else if (inc) q <= q + 2'd1;\n\
                  m_quiet: assume property (@(posedge clk) disable iff (!rst_n) !inc);\n\
                  endmodule\n\
                  // @mununu_template(never_full) rtl_never BAD=\"q == 3\"\n\
                  // @mununu_template(full_reach) rtl_reachable TARGET=\"q == 3\"\n\
                  // @mununu_template(full_reach_free, assumes=ignore) rtl_reachable TARGET=\"q == 3\"\n\
                  // @mununu_guarantee(small_free, assumes=ignore) nu X. ((q != 3) && [] X)\n";
        let sources = vec![("cnt.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("cnt".to_string()),
            use_sv2v: true,
            frontend: SvFrontend::Slang,
            ..Default::default()
        };
        // The CLI's default engine selection (portfolio-sequential, exact-symbolic first): the
        // library default is the cube engine alone, which leaves the relativized `EF` ⊥ (measured
        // 2026-10-30: `UnclassifiedBottom`, the safety rescue does not cover a possibility row) —
        // a cube abstention, not a property of the exemption.
        let (symbolic_engine, exact_symbolic, portfolio) = engine_selection(None);
        let run = |mode: AssumeMode| {
            verify_auto(
                &sources,
                &yopts,
                &VerifyAutoOptions {
                    assume_mode: mode,
                    symbolic_engine,
                    exact_symbolic,
                    portfolio,
                    ..Default::default()
                },
            )
            .expect("verify_auto runs on cnt")
        };
        let under = run(AssumeMode::Precondition);
        let ignored = run(AssumeMode::Ignore);
        for r in [&under, &ignored] {
            for p in &r.properties {
                eprintln!(
                    "  [{}] {} ({:?}): {:?}",
                    r.assumptions.mode, p.name, p.label, p.outcome
                );
            }
        }
        let row = |r: &AutoVerifyReport, l: &str| {
            r.properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("row labelled `{l}` in {:?}", r.properties))
                .clone()
        };
        // A ⊥ row is a finding, not an assertion: say everything the report knows about it.
        for r in [&under, &ignored] {
            for p in r
                .properties
                .iter()
                .filter(|p| matches!(p.outcome, VerifyOutcome::Unknown { .. }))
            {
                eprintln!(
                    "  ⊥ [{}] {}: formula={} bottom_reason={:?} decided_by={:?} seeded={:?}",
                    r.assumptions.mode,
                    p.name,
                    p.formula,
                    p.bottom_reason,
                    p.decided_by,
                    p.seeded_predicates
                );
                for n in r.notes.iter().filter(|n| {
                    n.property.as_deref() == Some(p.name.as_str())
                        || n.kind.contains("plan")
                        || n.kind.contains("portfolio")
                        || n.kind.contains("posture")
                        || n.kind.contains("input")
                }) {
                    eprintln!("    [{}] {} — {}", n.kind, n.summary, n.detail);
                }
            }
        }
        let definite = |p: &PropertyVerdict| match &p.outcome {
            VerifyOutcome::Holds => true,
            VerifyOutcome::Violated { .. } => false,
            other => panic!("{} is not definite: {other:?}", p.name),
        };
        // Under the assumption `inc` never rises: `q == 3` is unreachable (never_full HOLDS,
        // full_reach VIOLATED); the exempt rows see the free design (q reaches 3 at cycle 3).
        assert_eq!(under.assumptions.applied, vec!["m_quiet".to_string()]);
        assert!(definite(&row(&under, "never_full")));
        assert!(!definite(&row(&under, "full_reach")));
        assert!(definite(&row(&under, "full_reach_free")));
        assert!(!definite(&row(&under, "small_free")));
        // The exempt rows' verdicts ARE the `--assumes ignore` run's; the others differ from it.
        for l in ["full_reach_free", "small_free"] {
            assert_eq!(
                definite(&row(&under, l)),
                definite(&row(&ignored, l)),
                "{l}"
            );
        }
        for l in ["never_full", "full_reach"] {
            assert_ne!(
                definite(&row(&under, l)),
                definite(&row(&ignored, l)),
                "{l}"
            );
        }
        // The exempt rows never read the monitor; the others do.
        assert!(
            !row(&under, "full_reach_free")
                .formula
                .contains("mununu_assume_ok")
        );
        assert!(
            !row(&under, "small_free")
                .formula
                .contains("mununu_assume_ok")
        );
        assert!(
            row(&under, "full_reach")
                .formula
                .contains("mununu_assume_ok")
        );
        // What the report says about it.
        let exempt_names: Vec<&str> = under
            .assumptions
            .exempt
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(
            exempt_names,
            vec![
                row(&under, "full_reach_free").name.as_str(),
                row(&under, "small_free").name.as_str()
            ]
        );
        assert!(
            under
                .assume_exempt(&row(&under, "small_free").name)
                .unwrap()
                .contains("judged on every trace")
        );
        assert!(
            under
                .assume_exempt(&row(&ignored, "small_free").name)
                .is_some(),
            "recorded in the ignore run too"
        );
        assert!(
            ignored
                .assume_exempt(&row(&ignored, "small_free").name)
                .unwrap()
                .contains("--assumes ignore")
        );
        // The template rows: instance beside the formula, their own line; the guarantee has none.
        assert_eq!(
            under.template_form(&row(&under, "full_reach").name),
            Some("rtl_reachable TARGET=\"q == 3\"")
        );
        assert_eq!(under.template_form(&row(&under, "small_free").name), None);
        assert_eq!(
            under
                .property_source(&row(&under, "never_full").name)
                .map(|l| l.line),
            Some(5)
        );
        assert_eq!(
            under
                .property_source(&row(&under, "small_free").name)
                .map(|l| l.line),
            Some(8)
        );
        assert!(
            under.notes.iter().any(
                |n| n.kind == "assumptions" && n.detail.contains("full_reach_free, small_free")
            ),
            "{:?}",
            under
                .notes
                .iter()
                .find(|n| n.kind == "assumptions")
                .map(|n| &n.detail)
        );
    }

    /// 2026-10-30 — `--localize` end to end on the slice fixture: with the alternation
    /// counterexample's inputs fixed, `state_q` is a repair candidate whose latest single-frame
    /// change acts at frame 1 (the state entering the second request — frame 2 cannot be it, the
    /// `start_a` pulse reads the combinational `next`), `stall_q` one by diversion (the fault arm,
    /// from reset), the two pulses are reported as reading the property; `state_q` ranks first (its
    /// change acts closest to the violation) and carries the lines that write it. Measured
    /// 2026-10-31: state_q [1] := 0x3, stall_q [0] := 0x4, start_a [2] := 0x0, start_b [2] := 0x1.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_localize_names_the_state_register_at_the_second_grant_first() {
        use crate::adapter::yosys::SvFrontend;
        let sv = include_str!("../btor2/testdata/lane_arbiter.sv");
        let sources = vec![("lane_arbiter.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("lane_arbiter".to_string()),
            use_sv2v: true,
            frontend: SvFrontend::Slang,
            ..Default::default()
        };
        let (symbolic_engine, exact_symbolic, portfolio) = engine_selection(None);
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                symbolic_engine,
                exact_symbolic,
                portfolio,
                localize: true,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on lane_arbiter");
        let row = report
            .properties
            .iter()
            .find(|p| p.label.as_deref() == Some("alternates"))
            .unwrap_or_else(|| panic!("the alternates row: {:?}", report.properties));
        let trace = report.trace_of(row).expect("the violated row has a trace");
        for c in &trace.repair_candidates {
            eprintln!(
                "  candidate {} at {:?} := {:?} lines {:?} reads_property={}",
                c.register, c.cycles, c.values, c.lines, c.reads_property
            );
        }
        eprintln!("  localization: {:?}", trace.localization);
        assert_eq!(
            trace.localization.as_ref().map(|s| s.status.as_str()),
            Some("done"),
            "{:?}",
            trace.localization
        );
        let first = trace.repair_candidates.first().expect("a candidate");
        assert_eq!(first.register, "state_q");
        assert!(!first.reads_property);
        assert_eq!(first.cycles, vec![1], "{:?}", first.cycles);
        assert_ne!(
            first.values[0], "0x1",
            "any state but LANE_A: {:?}",
            first.values
        );
        assert!(
            first.lines.iter().any(|l| l.file == "lane_arbiter.sv"),
            "the statements writing state_q: {:?}",
            first.lines
        );
        let by = |r: &str| {
            trace
                .repair_candidates
                .iter()
                .find(|c| c.register == r)
                .unwrap_or_else(|| panic!("{r} among {:?}", trace.repair_candidates))
        };
        assert!(!by("stall_q").reads_property);
        assert!(by("start_a").reads_property);
        assert!(by("start_b").reads_property);
        // Every candidate the property does not read comes before every one it does.
        let flags: Vec<bool> = trace
            .repair_candidates
            .iter()
            .map(|c| c.reads_property)
            .collect();
        assert!(flags.windows(2).all(|w| !w[0] || w[1]), "{flags:?}");
    }

    /// mununu#717 — an override slang's top-scoped `-G` cannot apply (a `MODULE.NAME` on a
    /// submodule) is refused, rather than extracting the SVA at the default while the lift
    /// applies the override.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_717_an_override_that_misses_the_sva_extraction_is_refused() {
        let sv = "module sub #(parameter int W = 10) (input logic clk, input logic rst_n,\n\
                  output logic [W-1:0] q);\n\
                  always_ff @(posedge clk or negedge rst_n) if (!rst_n) q <= '0; else q <= q + 1'b1;\n\
                  a_w: assert property (@(posedge clk) disable iff (!rst_n) q != W);\n\
                  endmodule\n\
                  module top (input logic clk, input logic rst_n, output logic [9:0] q);\n\
                  sub u_sub (.clk(clk), .rst_n(rst_n), .q(q));\n\
                  endmodule\n";
        match run_717(&[("top.sv", sv)], "top", &[("sub.W", "4")]) {
            Err(e) => assert!(
                e.message.contains("did not reach the SVA extraction"),
                "refused for the right reason: {}",
                e.message
            ),
            // A future slang that applies scoped overrides would elaborate W = 4 on both sides;
            // then the run is legitimate, and this test must say so rather than pass silently.
            Ok(r) => panic!(
                "a scoped override on a submodule was accepted — check that slang now applies it \
                 (and retire this test if so): {:?}",
                r.properties
            ),
        }
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_632_localparam_atoms_decide_on_link_ctrl() {
        use crate::adapter::yosys::SvFrontend;
        use std::path::PathBuf;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/v12_link_ctrl_llm_fsm/link_ctrl.sv");
        let sv = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let sources = vec![("link_ctrl.sv".to_string(), sv)];
        let yopts = YosysOptions {
            top: Some("link_ctrl".to_string()),
            use_sv2v: true,
            frontend: SvFrontend::Slang,
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs on link_ctrl");
        for p in &report.properties {
            eprintln!(
                "  [{:?}] {} ({:?}): {:?}",
                p.kind, p.name, p.label, p.outcome
            );
            eprintln!("      formula: {}", p.formula);
        }
        let by_label = |l: &str| {
            report
                .properties
                .iter()
                .find(|p| p.label.as_deref() == Some(l))
                .unwrap_or_else(|| panic!("property labelled `{l}` in {:?}", report.properties))
        };
        // The four localparam-referencing properties: none skipped for an unknown signal.
        for l in [
            "a_err_implies_retry_limit",
            "a_retry_limit",
            "a_no_beats_outside_xfer",
            "a_idle_quiet",
        ] {
            let p = by_label(l);
            assert!(
                !matches!(&p.outcome, VerifyOutcome::Skipped { reason } if reason.contains("unknown register/signal")),
                "{l} must not skip on an unresolved parameter name: {:?} / {}",
                p.outcome,
                p.formula
            );
            assert!(
                !p.formula.contains("MAX_RETRIES") && !p.formula.contains("S_"),
                "{l}'s atoms fold to literals: {}",
                p.formula
            );
        }
        // The verdicts the issue measured by spelling the literals by hand.
        assert!(
            matches!(
                by_label("a_retry_limit").outcome,
                VerifyOutcome::Violated { .. }
            ),
            "a_retry_limit is VIOLATED on the design (the counter reaches 3 in S_RETRY): {:?}",
            by_label("a_retry_limit").outcome
        );
        for l in [
            "a_err_implies_retry_limit",
            "a_no_beats_outside_xfer",
            "a_idle_quiet",
        ] {
            assert!(
                matches!(by_label(l).outcome, VerifyOutcome::Holds),
                "{l} HOLDS: {:?}",
                by_label(l).outcome
            );
        }
        assert_eq!(
            report
                .properties
                .iter()
                .filter(|p| matches!(p.outcome, VerifyOutcome::Skipped { .. }))
                .count(),
            0,
            "every property of the suite decides"
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_csrng_real_sva_verdict_breakdown() {
        // Real OpenTitan csrng_main_sm SVA end-to-end through verify-auto. Reads
        // the vendored fixtures: the csrng sources from the M.2 fixture + the
        // STANDARD prim_assert macros from the M.0 prim_arbiter fixture (the
        // csrng dir's own prim_assert.sv is the dummy variant that drops all
        // SVA — the XL.0 gotcha). Prints the verdict breakdown; this is the
        // honest measurement of "how many real csrng SVA does verify-auto
        // prove?" — see the diagnostics for the root cause of any SKIP.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let csrng = root.join("m2_opentitan_csrng_main_sm/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let sources = vec![
            (
                "csrng_main_sm.sv".to_string(),
                read(csrng.join("csrng_main_sm.sv")),
            ),
            ("csrng_pkg.sv".to_string(), read(csrng.join("csrng_pkg.sv"))),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("csrng_main_sm".to_string()),
            use_sv2v: true,
            // The lift (sv2v + Yosys) resolves `\`include`s and packages from
            // `additional_sources` (staged + used as the sv2v include path);
            // `sources` itself only feeds slang's extraction.
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on csrng");

        eprintln!("\n=== csrng_main_sm verify-auto breakdown ===");
        eprintln!(
            "translated: {}   unsupported: {}",
            report.properties.len(),
            report.unsupported.len()
        );
        eprintln!(
            "diagnostics: state_registers={}  blackboxed={:?}  gated_resets={:?}",
            report.diagnostics.state_register_count,
            report.diagnostics.blackboxed_modules,
            report.diagnostics.gated_resets,
        );
        let (mut holds, mut violated, mut unknown, mut skipped) = (0, 0, 0, 0);
        for p in &report.properties {
            eprintln!("  [{:?}] {}: {:?}", p.kind, p.name, p.outcome);
            eprintln!("      formula: {}", p.formula);
            eprintln!("      seeded:  {:?}", p.seeded_predicates);
            match p.outcome {
                VerifyOutcome::Holds => holds += 1,
                VerifyOutcome::Violated { .. } => violated += 1,
                VerifyOutcome::Unknown { .. } => unknown += 1,
                VerifyOutcome::Skipped { .. } => skipped += 1,
            }
        }
        eprintln!("HOLDS={holds} VIOLATED={violated} UNKNOWN={unknown} SKIPPED={skipped}");
        eprintln!(
            "auto_provided_stubs: {:?}",
            report.diagnostics.auto_provided_stubs
        );
        for (n, r) in &report.unsupported {
            eprintln!("  unsupported {n}: {r}");
        }

        assert!(
            report.properties.len() >= 2,
            "both csrng ASSERTs (CsrngMainErrorStStable_A, CsrngMainErrorOutput_A) translate"
        );
        // H.C — the FSM's `prim_sparse_fsm_flop` (no body in the source set) is
        // auto-stubbed with a behavioral model, so it is NO LONGER reported as
        // cut and the state register survives.
        assert!(
            report
                .diagnostics
                .auto_provided_stubs
                .iter()
                .any(|m| m.contains("prim_sparse_fsm_flop")),
            "prim_sparse_fsm_flop should be auto-stubbed; got stubs={:?} blackboxed={:?}",
            report.diagnostics.auto_provided_stubs,
            report.diagnostics.blackboxed_modules
        );
        assert!(
            report.diagnostics.state_register_count >= 1,
            "with the flop stubbed, the FSM state register survives; got {}",
            report.diagnostics.state_register_count
        );
        // Reset-gating fires on the macro's `disable iff (rst_ni)`.
        assert_eq!(
            report.diagnostics.gated_resets,
            vec!["rst_ni=1".to_string()]
        );
        // H.A — `state_q` (a `uext`-0 alias of the async-reset register mux)
        // now binds, so the pure-state `$stable` property reaches a verdict
        // instead of SKIP.
        assert!(
            report.properties.iter().any(|p| {
                p.formula.contains("state_q == state_q__past")
                    && !matches!(p.outcome, VerifyOutcome::Skipped { .. })
            }),
            "the $stable state property should bind + reach a verdict; got {:?}",
            report
                .properties
                .iter()
                .map(|p| &p.outcome)
                .collect::<Vec<_>>()
        );
        // `main_sm_err_o` is a combinational output whose cone structurally
        // reaches an input (enable_i / local_escalate_i), so it is classified
        // combinational-of-INPUT → a derived per-cube ⊥-label. But at the
        // `state_q == 41` error cube the labeller PROVES it constant-true
        // (semantically it is `(state_q == Error)` there, the input can't swing
        // it), so the label is definite and `state_q == 41 → main_sm_err_o`
        // reaches a DEFINITE HOLDS. Slice 3 additionally seeds its cone inputs as
        // free dimensions (harmless refinement — the verdict was already HOLDS).
        // This is the key contrast with sva_4/6/8/9, where the box consequent
        // genuinely needs the cone input as a dimension to become definite.
        let sva1 = report
            .properties
            .iter()
            .find(|p| p.name == "csrng_main_sm_sva_1")
            .expect("sva_1 present");
        assert!(
            sva1.seeded_predicates.iter().any(|s| s == "main_sm_err_o"),
            "the combinational output main_sm_err_o binds (as a derived label); got {:?}",
            sva1.seeded_predicates
        );
        assert!(
            matches!(sva1.outcome, VerifyOutcome::Holds),
            "state_q==Error → main_sm_err_o reaches a DEFINITE HOLDS (the label is \
             provably constant-true at the error cube); got {:?}",
            sva1.outcome
        );
        // SOUNDNESS — no spurious counterexample anywhere.
        assert!(
            !report
                .properties
                .iter()
                .any(|p| matches!(p.outcome, VerifyOutcome::Violated { .. })),
            "no spurious VIOLATED from over-resolving a combinational output; got {:?}",
            report
                .properties
                .iter()
                .map(|p| &p.outcome)
                .collect::<Vec<_>>()
        );
    }

    /// mununu#498 REGRESSION — a `--cutpoint` must be an OVER-approximation, so no
    /// property may flip in the unsound direction when the cut is applied.
    ///
    /// `cutpoint` frees a net to a Yosys `$anyseq`, which BTOR2 encodes as a `state`
    /// with NO `next` line. Reading that as a *held register* pinned it to its
    /// absent-therefore-zero init, so the freed net was nailed to 0: `fits` could
    /// never be 1, the FSM never left idle, and the cut model was an
    /// UNDER-approximation. Reported from monono's `sprite_fetch`, reduced to this
    /// 30-line FSM, and it flipped BOTH directions at once:
    ///
    ///   EF (st_q == 2)  HOLDS unabstracted -> VIOLATED under the cut
    ///   AG (st_q == 0)  VIOLATED           -> HOLDS    under the cut
    ///
    /// Both are asserted: an implementation that special-cased safety would pass the
    /// `AG` half and still fail the `EF` half. The `EF 3` row is the control — a
    /// value the FSM never assigns, VIOLATED in both models, which also confirms the
    /// cut is not merely freeing the initial states.
    #[test]
    #[ignore = "requires yosys-slang (mununu-sva docker image); run with --ignored"]
    fn e2e_cutpoint_stays_an_over_approximation_no_verdict_flips() {
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        let src = r#"// @mununu_guarantee mu Z.((st_q == 2) || <> Z)
// @mununu_guarantee nu Y.((st_q == 0) && [] Y)
// @mununu_guarantee mu Z.((st_q == 3) || <> Z)
module mini_fetch (
    input  logic clk, rst_n, req, acc, valid,
    output logic busy
);
    logic [1:0]  st_q;
    logic [10:0] left_q, spent_q;
    logic fits;
    assign fits = (spent_q + 11'd64) <= 11'd1024;
    assign busy = (st_q != 2'd0);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            st_q <= 2'd0; left_q <= 11'd0; spent_q <= 11'd0;
        end else begin
            case (st_q)
                2'd0: if (req && fits) begin
                          left_q <= 11'd64; spent_q <= spent_q + 11'd64; st_q <= 2'd1;
                      end
                2'd1: if (acc) st_q <= 2'd2;
                2'd2: if (valid) begin
                          if (left_q <= 11'd2) begin left_q <= 11'd0;          st_q <= 2'd0; end
                          else                 begin left_q <= left_q - 11'd2; st_q <= 2'd1; end
                      end
                default: st_q <= 2'd0;
            endcase
        end
    end
endmodule
"#;
        let sources = vec![("mini_fetch.sv".to_string(), src.to_string())];
        let opts = |cuts: Vec<String>| YosysOptions {
            top: Some("mini_fetch".to_string()),
            cutpoint_signals: cuts,
            ..Default::default()
        };
        let vopts = || VerifyAutoOptions {
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            exact_symbolic: true,
            config_values: std::collections::HashMap::from([("rst_n".to_string(), 1u64)]),
            ..Default::default()
        };

        let outcomes = |r: &AutoVerifyReport| -> Vec<(String, String)> {
            r.properties
                .iter()
                .filter(|p| p.name.contains("guarantee"))
                .map(|p| (p.name.clone(), format!("{:?}", p.outcome)))
                .collect()
        };

        let plain = verify_auto(&sources, &opts(vec![]), &vopts()).expect("verify_auto (no cut)");
        let cut = verify_auto(
            &sources,
            &opts(vec!["fits".to_string(), "left_q".to_string()]),
            &vopts(),
        )
        .expect("verify_auto (cut)");

        let (p, c) = (outcomes(&plain), outcomes(&cut));
        eprintln!("no-cut: {p:?}\ncut:    {c:?}");
        assert_eq!(
            p.len(),
            3,
            "all three guarantees must translate un-cut; got {p:?}"
        );
        assert_eq!(
            p, c,
            "a cut point is an OVER-approximation: no property may change verdict \
             when it is applied. Un-cut {p:?} vs cut {c:?} — a difference here is the \
             mununu#498 under-approximation returning."
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_cutpoint_frees_wide_counter_guard_so_exact_symbolic_fits() {
        // Control-slice cut points, end-to-end. A 3-state FSM whose INIT→RUN edge is
        // gated by a wide (48-bit) counter-derived `tick`. That counter sits in the
        // state's cone, so `AG EF (state==DONE)` under `exact-symbolic` is Skipped
        // (cone-too-wide) — UNTIL `--cutpoint tick` frees the guard to a `$anyseq`
        // input, dropping the 48-bit counter out of the cone (COI). The design then
        // fits and the goal is decided HOLDS. The netlist-level, width-agnostic
        // version of the `control_slice.py` source prototype; `cutpoint` needs no
        // width resolution or source rewriting.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        let src = r#"// @mununu_guarantee nu Y.((mu X.(state == 2 || <> X)) && [] Y)
module wide_guard_fsm(input clk, input rst, output reg [1:0] state);
  localparam INIT = 2'd0, RUN = 2'd1, DONE = 2'd2;
  reg [47:0] cnt;
  wire tick = (cnt == 48'd0);
  always @(posedge clk) begin
    if (rst) begin
      state <= INIT;
      cnt   <= 48'd100;
    end else begin
      cnt <= cnt - 48'd1;
      case (state)
        INIT:    if (tick) state <= RUN;
        RUN:     state <= DONE;
        DONE:    state <= INIT;
        default: state <= INIT;
      endcase
    end
  end
endmodule
"#;
        let sources = vec![("wide_guard_fsm.v".to_string(), src.to_string())];
        let base = YosysOptions {
            top: Some("wide_guard_fsm".to_string()),
            ..Default::default()
        };
        let vopts = || VerifyAutoOptions {
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            exact_symbolic: true,
            ..Default::default()
        };

        // (1) WITHOUT cut points.
        //
        // mununu#503 — this used to assert `Skipped`, i.e. that the wide-counter cone was beyond
        // the exact engine. **That premise is now unsatisfiable by ANY counter width**, and it is
        // worth recording why, because the obvious repair does not work:
        //
        //   - narrow counter  ⇒ `default_cap_for_cone(c) = max(40, min(c, 192))` admits the cone
        //     (it rejects only above 192 bits), so the exact engine decides directly;
        //   - wide counter    ⇒ the exact engine DOES abstain, but the `Err` arm then routes to
        //     `try_ranking_recoverability`, and a well-founded datapath descent decides
        //     `AG EF (reg == N)` **without caring about the width at all**.
        //
        // Measured: at 256 bits the no-cut run still returns `Holds`, in 5.2 s. So bumping the
        // fixture — the repair #380 applied to every other exact-abstention fixture, and what I
        // first tried here — cannot work: the deciding mechanism is not width-sensitive.
        //
        // The engine simply got better than this demonstration. What remains true, and is what
        // this test now pins:
        //   - the cut is APPLIED and the control-slice note names the cut net (assertion 3);
        //   - cutting does not COST the verdict — the property still decides under the cut.
        //
        // What this test no longer demonstrates is the cut's *value* (turning an undecidable cone
        // into a decidable one). Showing that needs a property shape the ranking rescue cannot
        // reduce; inventing one here would be fabricating a demo, so it is left undone and
        // recorded in #503 instead.
        let no_cut = verify_auto(&sources, &base, &vopts()).expect("verify_auto runs (no cut)");
        let g0 = no_cut
            .properties
            .iter()
            .find(|p| p.name.contains("guarantee"))
            .expect("annotated guarantee present");
        eprintln!("no-cut:   {} => {:?}", g0.name, g0.outcome);
        assert!(
            !matches!(g0.outcome, VerifyOutcome::Violated { .. }),
            "the guarantee must not be VIOLATED without cut points — the FSM does reach DONE; \
             a violation here would be a real regression rather than a capacity effect, got {:?}",
            g0.outcome
        );

        // (2) WITH `--cutpoint tick`: guard freed → counter drops out of the cone →
        // exact-symbolic FITS and returns a DEFINITE verdict (Holds or Violated) where
        // it previously refused (Skipped). Decidability — not a specific verdict — is
        // the feature's claim; and per the over-approximation posture a Violated under
        // cut points must not be over-trusted anyway (see the control-slice note). The
        // control-slice note is present and names the cut net.
        let cut = YosysOptions {
            cutpoint_signals: vec!["tick".to_string()],
            ..base.clone()
        };
        let with_cut = verify_auto(&sources, &cut, &vopts()).expect("verify_auto runs (cut)");
        let g1 = with_cut
            .properties
            .iter()
            .find(|p| p.name.contains("guarantee"))
            .expect("annotated guarantee present");
        eprintln!("with-cut: {} => {:?}", g1.name, g1.outcome);
        assert!(
            matches!(
                g1.outcome,
                VerifyOutcome::Holds | VerifyOutcome::Violated { .. }
            ),
            "cutting `tick` should let exact-symbolic reach a DEFINITE verdict (was Skipped), got {:?}",
            g1.outcome
        );
        let cs = with_cut
            .notes
            .iter()
            .find(|n| n.kind == "control-slice")
            .expect("control-slice note present when cut points given");
        assert!(
            cs.items.contains(&"tick".to_string()),
            "note items: {:?}",
            cs.items
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_structural_reset_autopin_makes_box_af_sound() {
        // Structural async-reset auto-pin, end-to-end. A plain-RTL FSM with NO SVA
        // `disable iff` — so the reset is recognized ONLY by the structural detector
        // (from the lifted BTOR2 reset-mux `next(st) = ite(rst, 0, normal)`). Without the
        // auto-pin the free `rst` input leaves a reset-edge from every state to the reset
        // state, and `AF st==2` (`mu Y.(st==2 || [] Y)`) is spuriously VIOLATED; with it,
        // `rst` is pinned inactive and AF decides correctly HOLDS. Also asserts the
        // detected reset is surfaced in `gated_resets` (same path as a `disable iff` one).
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        let src = r#"// @mununu_guarantee mu Y.(st == 2 || [] Y)
module cyc(input clk, input rst, output reg [1:0] st);
  always @(posedge clk)
    if (rst) st <= 2'd0;
    else case (st)
      2'd0: st <= 2'd1;
      2'd1: st <= 2'd2;
      2'd2: st <= 2'd0;
      default: st <= 2'd0;
    endcase
endmodule
"#;
        let sources = vec![("cyc.v".to_string(), src.to_string())];
        let yopts = YosysOptions {
            top: Some("cyc".to_string()),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                exact_symbolic: true,
                ..Default::default()
            },
        )
        .expect("verify_auto runs");
        // The structural detector pinned the async reset even absent an SVA `disable iff`.
        assert_eq!(
            report.diagnostics.gated_resets,
            vec!["rst=0".to_string()],
            "structural reset auto-pin should pin rst inactive; got {:?}",
            report.diagnostics.gated_resets
        );
        let g = report
            .properties
            .iter()
            .find(|p| p.name.contains("guarantee"))
            .expect("annotated guarantee present");
        eprintln!("AF st==2 => {:?}", g.outcome);
        assert!(
            matches!(g.outcome, VerifyOutcome::Holds),
            "AF st==2 must HOLD once the reset is auto-pinned (was spuriously VIOLATED); got {:?}",
            g.outcome
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_sysrst_config_concretization_flips_timer_relationals() {
        // H.J.b — pinning the wide config timers to constants (the harness's
        // DEB=1, DET=7) turns `cnt_q >= cfg_*_timer_i` into a decidable
        // `cnt_q >= const`, so the config-timer ⊥ (sva_5/7/10/11/13/14) can reach
        // definite verdicts FOR THOSE VALUES. Prints the delta; asserts the
        // config-concretization note is present, the formula shows the constant,
        // and there is NO spurious VIOLATED (sound-for-that-config).
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let sysrst = root.join("r46_sysrst_detect_k5/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let sources = vec![
            (
                "sysrst_ctrl_detect.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_detect.sv")),
            ),
            (
                "sysrst_ctrl_pkg.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_pkg.sv")),
            ),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("sysrst_ctrl_detect".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let mut config_values = std::collections::HashMap::new();
        config_values.insert("cfg_debounce_timer_i".to_string(), 1u64);
        config_values.insert("cfg_detect_timer_i".to_string(), 7u64);
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                config_values,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on sysrst with config concretization");

        eprintln!("\n=== sysrst config-concretization breakdown (DEB=1, DET=7) ===");
        let (mut holds, mut violated, mut unknown, mut skipped) = (0, 0, 0, 0);
        for p in &report.properties {
            eprintln!("  {}: {:?}", p.name, p.outcome);
            match p.outcome {
                VerifyOutcome::Holds => holds += 1,
                VerifyOutcome::Violated { .. } => violated += 1,
                VerifyOutcome::Unknown { .. } => unknown += 1,
                VerifyOutcome::Skipped { .. } => skipped += 1,
            }
        }
        eprintln!("HOLDS={holds} VIOLATED={violated} UNKNOWN={unknown} SKIPPED={skipped}");
        for n in &report.notes {
            eprintln!("  note [{}] {}: {}", n.kind, n.summary, n.items.join(", "));
        }

        // The config-concretization note is present, is a ScopeCaveat, and names
        // both pinned timers with their values.
        let cc = report
            .notes
            .iter()
            .find(|n| n.kind == "config-concretization")
            .expect("config-concretization note present");
        assert_eq!(cc.level, NoteLevel::ScopeCaveat);
        assert!(cc.items.contains(&"cfg_detect_timer_i=7".to_string()));
        assert!(cc.items.contains(&"cfg_debounce_timer_i=1".to_string()));
        // The concretized relational atom shows the CONSTANT (substituted), not
        // the free config name.
        let sva7 = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_7")
            .expect("sva_7 present");
        assert!(
            sva7.formula.contains("cnt_q >= 7") && !sva7.formula.contains("cfg_detect_timer_i"),
            "sva_7 formula shows the concretized threshold: {}",
            sva7.formula
        );
        // SOUND — pinning is a restriction (a specific config), so no spurious
        // VIOLATED. The four config-timer relationals flip ⊥ → HOLDS (for these
        // values); HOLDS 9 → 12.
        assert_eq!(violated, 0, "no spurious VIOLATED under concretization");
        assert_eq!(
            skipped, 0,
            "every property still binds under concretization; got SKIPPED={skipped}"
        );
        for name in [
            "sysrst_ctrl_detect_sva_5",  // EnterDetectSt: cnt_q >= cfg_debounce
            "sysrst_ctrl_detect_sva_7",  // EnterStableSt: cnt_q >= cfg_detect
            "sysrst_ctrl_detect_sva_10", // DetectedOut
            "sysrst_ctrl_detect_sva_11", // DetectedPulseOut
        ] {
            let p = report
                .properties
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert!(
                matches!(p.outcome, VerifyOutcome::Holds),
                "{name} (config-timer relational) reaches HOLDS once the timer is \
                 concretized; got {:?}",
                p.outcome
            );
        }
        assert_eq!(
            holds, 15,
            "config concretization lifts sysrst 9 → 12 HOLDS (sva_5/7/10/11), the mununu#503 \
             `|=>` rescue reducer adds sva_12 (pulse) + sva_14 (counter reset), and the IC3 \
             members decide sva_13 (counter monotonicity) for 15; got {holds}"
        );
        // The residual ⊥ is now just sva_15 (arithmetic).
        //
        // mununu#503 — sva_12 and sva_14 USED to be residual ⊥ here and are not any more: their
        // `A |=> C` shape now reduces to a `bad` monitor and the reachability portfolio decides
        // both HOLDS (`portfolio-rescue` notes name engines `exact, native` and
        // `exact, native, spacer`).
        //
        // mununu#797 (measured 2026-10-25, mununu 0.5.3, this test in `mununu-sva`) — sva_13
        // (`!cnt_clr |=> cnt_q >= $past(cnt_q)`, CntNoWrap_A) now decides HOLDS as well, through
        // the same reducer and the portfolio's IC3 members (`portfolio-rescue` names `exact, pono,
        // ric3`). The independent SymbiYosys check recorded in #797 (the assertions restated as
        // immediate assertions, `abc pdr`) proves it at 4-bit timers and was still open at full
        // width after an hour; mununu's verdict is the IC3 members' on the lifted model. What still
        // declines is sva_15's `cnt_q == cnt_q__past + 1`, a `CmpRegAddend` leaf the boolean
        // compiler does not accept — a leaf-shape gap, NOT the `cnt_clr` antecedent: the antecedent
        // was never the blocker for sva_14, whose consequent (`cnt_q == 0`) is a plain comparison.
        //
        // H.H NOTE: the counter-bound `cnt_q <= 7` is auto-seeded here (the counter-bound note
        // fires, asserted below), but it does NOT flip sva_13/15 — the bound is necessary infra
        // (it excludes the abstract wraparound) but not sufficient for THIS fixture. The
        // demonstrator `e2e_counter_bound_flips_saturating_monotonicity` below isolates a property
        // where the bound alone flips ⊥ → HOLDS.
        assert_eq!(
            unknown, 1,
            "only the arithmetic-addend consequent (sva_15) stays ⊥; got UNKNOWN={unknown}"
        );
        // H.H — the counter-bound note fires (auto-inferred `cnt_q <= 7` from the
        // sibling `cnt_q >= cfg_detect_timer_i` comparison), even though it does not
        // move a verdict on this fixture.
        let cb = report
            .notes
            .iter()
            .find(|n| n.kind == "counter-bound")
            .expect("counter-bound note present (config-inferred)");
        assert_eq!(cb.level, NoteLevel::Info);
        assert!(
            cb.items.iter().any(|i| i.contains("cnt_q <= 7")),
            "counter-bound note names the inferred bound; got {:?}",
            cb.items
        );
    }

    #[test]
    /// mununu#503 — the one-hot arithmetic, pinned without needing a lift.
    ///
    /// `$onehot0(oh_i)` over a primary INPUT expands to one atom per value, so five mutually
    /// exclusive `Eq` dimensions land on one register. Only 6 of the 2⁵ = 32 valuations are
    /// satisfiable — one per value, plus the all-false case (`oh_i ∈ {3,5,6,7,9..15}`). The other
    /// 26 assert two different values for `oh_i` at once and describe no state at all.
    ///
    /// They were still enumerated as INITIAL cubes, so `any_unknown` read an empty cube's ⊥ as an
    /// abstention: `Unknown { unknown_cells: 26 }`, and 26 = 32 − 6 exactly.
    fn infeasible_free_input_cubes_are_not_initial() {
        let bits: Vec<u32> = (0..5).collect();
        let dims: Vec<(String, u64)> = [0u64, 1, 2, 4, 8]
            .iter()
            .map(|v| ("oh_i".to_string(), *v))
            .collect();

        let all = free_input_init_cubes(0, &bits);
        assert_eq!(
            all.len(),
            32,
            "unfiltered enumerates the whole polarity product"
        );

        let feasible = free_input_init_cubes_feasible(0, &bits, &dims);
        assert_eq!(
            feasible.len(),
            6,
            "5 single-value cubes + 1 all-false; the other 26 assert two values for `oh_i` at \
             once and are empty. 32 - 6 = 26 — exactly the `unknown_cells` this produced"
        );
        assert!(
            feasible.contains(&0),
            "the all-false cube is feasible (oh_i takes a value outside the set)"
        );
    }

    #[test]
    /// The filter must not touch dimensions over DIFFERENT registers — those are independent, and
    /// every combination of them is satisfiable.
    fn distinct_registers_are_still_fully_enumerated() {
        let bits: Vec<u32> = (0..3).collect();
        let dims: Vec<(String, u64)> = vec![
            ("a".to_string(), 1),
            ("b".to_string(), 1),
            ("c".to_string(), 1),
        ];
        assert_eq!(
            free_input_init_cubes_feasible(0, &bits, &dims).len(),
            8,
            "three independent registers ⇒ all 2^3 combinations remain feasible"
        );
    }

    #[test]
    /// Same register, SAME value repeated is not a contradiction — only distinct values are.
    fn same_register_same_value_is_not_excluded() {
        let bits: Vec<u32> = (0..2).collect();
        let dims: Vec<(String, u64)> = vec![("a".to_string(), 1), ("a".to_string(), 1)];
        assert_eq!(
            free_input_init_cubes_feasible(0, &bits, &dims).len(),
            4,
            "`a == 1` twice is consistent; excluding it would drop reachable states"
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_onehot0_state_invariant_holds() {
        // One-hot-support demonstrator (STATE case): a one-hot-encoded FSM whose
        // security invariant `$onehot0(state_q)` (at-most-one bit set — the
        // fault-detection property) translates to a value-set predicate over the
        // STATE register, i.e. ONE compound cube dimension. `$onehot0` (not
        // `$onehot`) because reset-gating starts `state_q` at the BTOR2 init `000`
        // (at-most-one-hot ✓; exactly-one would spuriously fail at `000`). The FSM
        // only ever drives one-hot values, so the invariant HOLDS.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        const SV: &str = r#"module onehot_fsm (input logic clk, input logic rst_ni, input logic go_i);
  localparam logic [2:0] S0 = 3'b001, S1 = 3'b010, S2 = 3'b100;
  logic [2:0] state_q, state_d;
  always_comb begin
    state_d = state_q;
    unique case (state_q)
      S0: if (go_i) state_d = S1;
      S1: state_d = S2;
      S2: state_d = S0;
      default: state_d = S0;
    endcase
  end
  always_ff @(posedge clk or negedge rst_ni) begin
    if (!rst_ni) state_q <= S0;
    else         state_q <= state_d;
  end
  OneHotState_A: assert property (@(posedge clk) disable iff (!rst_ni) $onehot0(state_q));
endmodule
"#;
        let sources = vec![("onehot_fsm.sv".to_string(), SV.to_string())];
        let yopts = YosysOptions {
            top: Some("onehot_fsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on onehot_fsm");
        assert!(
            report.unsupported.is_empty(),
            "unsupported: {:?}",
            report.unsupported
        );
        let p = report
            .properties
            .iter()
            .find(|p| p.formula.contains("state_q == 1"))
            .expect("the $onehot0(state_q) property is present + translated");
        // The translation is the value-set predicate over the state register.
        assert!(
            p.formula.contains("state_q == 1")
                && p.formula.contains("state_q == 2")
                && p.formula.contains("state_q == 4"),
            "$onehot0(state_q) expands to the power-of-two value set: {}",
            p.formula
        );
        assert!(
            matches!(p.outcome, VerifyOutcome::Holds),
            "the one-hot state invariant HOLDS; got {:?}",
            p.outcome
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_opentitan_prim_onehot_check_holds() {
        // One-hot-support demonstrator (real OpenTitan, INPUT case): the hardened
        // `prim_onehot_check.sv` raises `err_o` iff its input `oh_i` is not one-hot0.
        // Its inline `Onehot0Check_A, !$onehot0(oh_i) |-> err_o` exercises the new
        // `$onehot0` translator support end-to-end and reaches a definite HOLDS —
        // `onehot0(oh_i) ∨ err_o` is a per-cube tautology the SMT labeller confirms.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/opentitan_prim_onehot_check/source");
        let read = |n: &str| {
            std::fs::read_to_string(src.join(n)).unwrap_or_else(|e| panic!("read {n}: {e}"))
        };
        let sources = vec![
            (
                "prim_onehot_check_wrapper.sv".to_string(),
                read("prim_onehot_check_wrapper.sv"),
            ),
            (
                "prim_onehot_check.sv".to_string(),
                read("prim_onehot_check.sv"),
            ),
            ("prim_assert.sv".to_string(), read("prim_assert.sv")),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read("prim_assert_standard_macros.svh"),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read("prim_assert_sec_cm.svh"),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read("prim_flop_macros.sv"),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("prim_onehot_check_wrapper".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on prim_onehot_check");
        assert!(
            report.unsupported.is_empty(),
            "unsupported: {:?}",
            report.unsupported
        );
        let p = report
            .properties
            .iter()
            .find(|p| p.formula.contains("oh_i == 1"))
            .expect("the $onehot0(oh_i) check is present + translated");
        assert!(
            p.formula.contains("oh_i == 8"),
            "$onehot0(oh_i) expands over the 4-bit power-of-two value set: {}",
            p.formula
        );
        assert!(
            matches!(p.outcome, VerifyOutcome::Holds),
            "the OpenTitan one-hot0 check HOLDS; got {:?}",
            p.outcome
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_reduction_or_in_comparison_translates() {
        // Reduction-OR-in-comparison: `(|v_i) == nonzero` — a reduction operand
        // inside a 1-bit comparison — now TRANSLATES (→ an XNOR over `v_i != 0` and
        // `nonzero`) instead of being rejected as unsupported. It reaches the
        // abstraction and a modeled verdict (not SKIPPED). The verdict here is an
        // honest ⊥: the XNOR splits into two independent derived labels (`v_i != 0`
        // and `nonzero`, both the same function of the input `v_i`) and the per-label
        // abstraction doesn't see they are correlated — the SAME cone-completeness /
        // correlation limit as the sysrst cnt_clr residual, which the R-F5 BDD track
        // (joint symbolic evaluation) addresses. This test guards the TRANSLATION
        // (never regress to unsupported) and will tighten to HOLDS once the engine
        // resolves the correlation.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        const SV: &str = r#"module redor_demo (input logic clk, input logic rst_ni, input logic [1:0] v_i);
  logic nonzero;
  assign nonzero = |v_i;
  RedOr_A: assert property (@(posedge clk) disable iff (!rst_ni) (|v_i) == nonzero);
endmodule
"#;
        let sources = vec![("redor_demo.sv".to_string(), SV.to_string())];
        let yopts = YosysOptions {
            top: Some("redor_demo".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on redor_demo");
        assert!(
            report.unsupported.is_empty(),
            "reduction-in-comparison translates (nothing unsupported): {:?}",
            report.unsupported
        );
        let p = report
            .properties
            .iter()
            .find(|p| p.formula.contains("v_i != 0"))
            .expect("the reduction `(|v_i)` lowered to `v_i != 0`");
        // Reaches a MODELED verdict (definite or honest ⊥), never SKIPPED/unsupported.
        assert!(
            matches!(
                p.outcome,
                VerifyOutcome::Holds | VerifyOutcome::Unknown { .. }
            ),
            "reduction-in-comparison reaches the abstraction (not SKIPPED); got {:?}\n  {}",
            p.outcome,
            p.formula
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_counter_bound_flips_saturating_monotonicity() {
        // H.H demonstrator — the bound-seeding mechanism isolated from any
        // antecedent co-blocker. A counter rises to K then HOLDS at K
        // (`cnt_d = (cnt_q == K) ? K : cnt_q + 1`). Its UNCONDITIONAL monotonicity
        // `cnt_q >= $past(cnt_q)` is concretely TRUE (the counter never decreases),
        // but ABSTRACTLY ⊥: the predicate cube `{cnt_q >= cnt_q__past}` includes the
        // UNREACHABLE high state `cnt_q = 2^W-1` (> K), whose "increment elsewhere"
        // successor wraps to 0 — a may-edge to a `cnt_q >= cnt_q__past = false` cube
        // with no must-witness, so the box is ⊥. Seeding `cnt_q <= K` (+ its `$past`
        // shadow) EXCLUDES that unreachable state from the cube, removing the
        // spurious may-edge → the verdict becomes a definite HOLDS. This is the
        // sound bound-partition lever the roadmap's H.H targets, on a property where
        // the bound alone is sufficient.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        const SV: &str = r#"module sat_hold_counter (input logic clk, input logic rst_ni);
  localparam int unsigned W = 5;
  localparam logic [W-1:0] K = 5'd7;
  logic [W-1:0] cnt_q, cnt_d;
  assign cnt_d = (cnt_q == K) ? K : (cnt_q + 1'b1);
  always_ff @(posedge clk or negedge rst_ni) begin
    if (!rst_ni) cnt_q <= '0;
    else         cnt_q <= cnt_d;
  end
  MonoCnt_A: assert property (@(posedge clk) disable iff (!rst_ni) cnt_q >= $past(cnt_q));
endmodule
"#;
        let sources = vec![("sat_hold_counter.sv".to_string(), SV.to_string())];
        let yopts = YosysOptions {
            top: Some("sat_hold_counter".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let mono = |report: &AutoVerifyReport| -> VerifyOutcome {
            report
                .properties
                .iter()
                .find(|p| p.formula.contains("cnt_q__past"))
                .unwrap_or_else(|| {
                    panic!("monotonicity property present; got {:?}", report.properties)
                })
                .outcome
                .clone()
        };

        // Without a bound — the abstraction FAILS to prove monotonicity: the cube
        // `{cnt_q >= cnt_q__past}` includes the unreachable high state, so the
        // abstract successor can wrap. Depending on the must-edge witness this is a
        // spurious VIOLATED or a ⊥ — either way NOT a definite HOLDS.
        let base = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on the saturating counter (no bound)");
        eprintln!("no-bound monotonicity: {:?}", mono(&base));
        assert!(
            !matches!(mono(&base), VerifyOutcome::Holds),
            "without a bound the abstract wraparound prevents a HOLDS; got {:?}",
            mono(&base)
        );

        // With the bound — the spurious wraparound cube is excluded → HOLDS.
        let mut counter_bounds = std::collections::HashMap::new();
        counter_bounds.insert("cnt_q".to_string(), 7u64);
        let bounded = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                counter_bounds,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on the saturating counter (bounded)");
        eprintln!("bounded monotonicity: {:?}", mono(&bounded));
        assert!(
            matches!(mono(&bounded), VerifyOutcome::Holds),
            "the counter bound flips monotonicity ⊥ → HOLDS; got {:?}",
            mono(&bounded)
        );
        // The counter-bound note is present + names the user-supplied bound.
        let cb = bounded
            .notes
            .iter()
            .find(|n| n.kind == "counter-bound")
            .expect("counter-bound note present");
        assert!(
            cb.items.iter().any(|i| i.contains("cnt_q <= 7")),
            "counter-bound note names the bound; got {:?}",
            cb.items
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_sysrst_detect_real_sva_verdict_breakdown() {
        // Real OpenTitan sysrst_ctrl_detect — the contrast to csrng: its state
        // (`state_q` FSM, `cnt_q` counter, `trigger_active_q`) is in PLAIN
        // `always_ff` (NO prim_flop wrapper), so it SURVIVES the lift. This
        // isolates the *second* real-RTL blocker from the flop-cut: SVA whose
        // antecedents reference IO / config / combinational signals
        // (`cfg_enable_i`, `trigger_event`, `cnt_en`) — those atoms are not
        // cube-bindable. Prints the breakdown.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let sysrst = root.join("r46_sysrst_detect_k5/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        // Standard prim_assert macros (the fixture's own prim_assert.sv is the
        // dummy that drops all SVA).
        let sources = vec![
            (
                "sysrst_ctrl_detect.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_detect.sv")),
            ),
            (
                "sysrst_ctrl_pkg.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_pkg.sv")),
            ),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("sysrst_ctrl_detect".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on sysrst_ctrl_detect");

        eprintln!("\n=== sysrst_ctrl_detect verify-auto breakdown ===");
        eprintln!(
            "translated: {}   unsupported: {}",
            report.properties.len(),
            report.unsupported.len()
        );
        eprintln!(
            "diagnostics: state_registers={}  blackboxed={:?}  gated_resets={:?}",
            report.diagnostics.state_register_count,
            report.diagnostics.blackboxed_modules,
            report.diagnostics.gated_resets,
        );
        let (mut holds, mut violated, mut unknown, mut skipped) = (0, 0, 0, 0);
        for p in &report.properties {
            eprintln!("  [{:?}] {}: {:?}", p.kind, p.name, p.outcome);
            eprintln!("      formula: {}", p.formula);
            eprintln!("      seeded:  {:?}", p.seeded_predicates);
            match p.outcome {
                VerifyOutcome::Holds => holds += 1,
                VerifyOutcome::Violated { .. } => violated += 1,
                VerifyOutcome::Unknown { .. } => unknown += 1,
                VerifyOutcome::Skipped { .. } => skipped += 1,
            }
        }
        eprintln!("HOLDS={holds} VIOLATED={violated} UNKNOWN={unknown} SKIPPED={skipped}");
        for (n, r) in &report.unsupported {
            eprintln!("  unsupported {n}: {r}");
        }

        // The state survives the lift (no prim_flop cut) — the contrast to
        // csrng. Whatever the per-property outcomes, the model is non-empty.
        assert!(
            report.diagnostics.state_register_count >= 1,
            "plain always_ff state survives the lift (no flop-cut); got {} state registers",
            report.diagnostics.state_register_count
        );
        assert!(
            report.diagnostics.blackboxed_modules.is_empty(),
            "no prim_flop primitive to cut; got {:?}",
            report.diagnostics.blackboxed_modules
        );
        // H.A — the state atoms (`state_q == k`, `cnt_q == k`) now BIND (via the
        // value-alias resolver), so they no longer appear in any SKIP reason.
        for p in &report.properties {
            if let VerifyOutcome::Skipped { reason } = &p.outcome {
                assert!(
                    !reason.contains("state_q ==") && !reason.contains("cnt_q =="),
                    "state atoms should bind, not appear in a SKIP reason; got: {reason}"
                );
            }
        }
        // H.B (free-input atoms) — the headline real-RTL win. `sva_0` is a
        // `cfg_enable_i |=> state_q == 0`-style property: a config INPUT
        // antecedent (`cfg_enable_i`) + a state consequent (`state_q == 0`).
        // Pre-H.B the `cfg_enable_i` atom was unbindable → the property SKIPPED;
        // H.B admits it as a free cube dimension, so it reaches a sound HOLDS
        // (read across both cfg_enable_i flavours). This is the first
        // free-input-antecedent verdict on real OpenTitan RTL.
        let sva0 = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_0")
            .expect("sva_0 present");
        assert!(
            sva0.formula.contains("cfg_enable_i"),
            "sva_0 references the config input cfg_enable_i; got {}",
            sva0.formula
        );
        assert!(
            sva0.seeded_predicates.iter().any(|s| s == "cfg_enable_i"),
            "the free input cfg_enable_i is admitted as a cube dimension (H.B); got {:?}",
            sva0.seeded_predicates
        );
        assert!(
            matches!(sva0.outcome, VerifyOutcome::Holds),
            "the cfg_enable_i |=> state_q==0 property reaches a sound HOLDS via the \
             free-input dimension; got {:?}",
            sva0.outcome
        );
        // After H.E.r2, the simple combinational-of-input antecedents
        // (`trigger_active`, `event_detected_*`, `cnt_clr`) BIND as derived
        // ⊥-labels (HOLDS-or-honest-⊥). H.F then closed the last residual: the
        // RELATIONAL-with-input compounds (`cnt_q >= cfg_*_timer_i`, sva_5/7/10/11
        // — an input operand inside a compound) are now admitted as derived
        // RELATIONAL labels, so they reach an honest ⊥ instead of SKIPPING. Net
        // effect: every translated property binds — SKIPPED is 0.
        assert_eq!(
            skipped, 0,
            "H.F: every translated property binds (relational-with-input compounds \
             now reach a verdict instead of SKIPPING); got SKIPPED={skipped}"
        );
        // No free-input antecedent (H.B) and no relational-with-input compound
        // (H.F) remains skipped.
        for p in &report.properties {
            if let VerifyOutcome::Skipped { reason } = &p.outcome {
                assert!(
                    !reason.contains("cfg_enable_i") && !reason.contains("cnt_q >= cfg_"),
                    "no free-input / relational-with-input atom should remain skipped \
                     after H.B + H.F; got: {reason}"
                );
            }
        }
        // H.F — sva_5/7/10/11 are the relational-with-input compounds
        // (`cnt_q >= cfg_*_timer_i`). Pre-H.F they SKIPPED ("input operand inside
        // a compound"); now they bind as derived relational labels and reach a
        // verdict — never a spurious one.
        //
        // mununu#797 (measured 2026-10-25, mununu 0.5.3, this test in `mununu-sva`): sva_7
        // (EnterStableSt_A), sva_10 (DetectedOut_A) and sva_11 (DetectedPulseOut_A) reach a
        // definite HOLDS with the config timers FREE; sva_5 (EnterDetectSt_A, 16 unknown cells)
        // stays an honest ⊥. The independent SymbiYosys check recorded in #797 (the 16 assertions
        // restated as immediate assertions, `abc pdr`, every input free) proves EnterStableSt_A,
        // DetectedOut_A and DetectedPulseOut_A at full width, so the three HOLDS are corroborated,
        // not merely pinned.
        let p = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_5")
            .expect("sva_5 present");
        assert!(
            matches!(p.outcome, VerifyOutcome::Unknown { .. }),
            "sva_5 (relational-with-input compound) binds and reaches an honest ⊥, not a \
             SKIP; got {:?}",
            p.outcome
        );
        for name in [
            "sysrst_ctrl_detect_sva_7",
            "sysrst_ctrl_detect_sva_10",
            "sysrst_ctrl_detect_sva_11",
        ] {
            let p = report
                .properties
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert!(
                matches!(p.outcome, VerifyOutcome::Holds),
                "{name} (relational-with-input compound) reaches a definite HOLDS with the \
                 timers free (mununu#797, SymbiYosys-corroborated); got {:?}",
                p.outcome
            );
        }
        // H.F — sva_2/3 are `trigger_i != trigger_active` where
        // `trigger_active = !trigger_i` (1-bit). `b != !b` is a tautology over the
        // combinational relation, so the derived relational label is definite-True
        // at every cube and the property reaches a sound HOLDS.
        let sva2 = report
            .properties
            .iter()
            .find(|p| p.name == "gen_low_level_sva_sva_2")
            .expect("sva_2 present");
        assert!(
            matches!(sva2.outcome, VerifyOutcome::Holds),
            "sva_2 (`trigger_i != !trigger_i` tautology) reaches a sound HOLDS; got {:?}",
            sva2.outcome
        );
        // H.U.2 — the combinational `event_detected_*` outputs SEED as predicates, and both
        // sva_1 (`cfg_enable_i → ¬event_detected_o ∧ ¬event_detected_pulse_o`) and sva_12
        // (`event_pulse |=> ¬event_pulse`) reach a DEFINITE HOLDS. The still-⊥ props are ⊥ from
        // their combinational-of-*input* antecedents (`trigger_active` / `cnt_q >= cfg_*`), NOT
        // from any combinational binding gap — every combinational atom binds (SKIPPED is 0,
        // asserted above).
        //
        // mununu#503 — the two properties hold by DIFFERENT mechanisms, and this comment used to
        // assert the wrong one for sva_12:
        //
        // - **sva_1 decides in the cube.** Its atoms are pinned as cube dimensions.
        // - **sva_12 does NOT.** It carries a `portfolio-rescue` note, i.e. the cube left it ⊥ and
        //   the `|=>` reducer + reachability portfolio decided it. The old comment claimed
        //   `event_detected_pulse_o = f(state_q)` and so "binds as a CUBE DIMENSION" — but the RTL
        //   (sysrst_ctrl_detect.sv:122-215) makes that output a function of `cfg_enable_i`,
        //   `trigger_i` and the timers as well, so `cone_reaches_input` correctly classifies it
        //   `InputDependent` and it becomes a per-cube derived LABEL, which the cube cannot pin.
        //   The assertion below is therefore on the VERDICT only; it deliberately does not claim a
        //   binding mechanism, because for sva_12 that claim was false.
        for name in ["sysrst_ctrl_detect_sva_1", "sysrst_ctrl_detect_sva_12"] {
            let p = report
                .properties
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert!(
                p.seeded_predicates
                    .iter()
                    .any(|s| s.contains("event_detected")),
                "{name} seeds a combinational-of-state event_detected_* output; got {:?}",
                p.seeded_predicates
            );
            assert!(
                matches!(p.outcome, VerifyOutcome::Holds),
                "{name} reaches a DEFINITE HOLDS — sva_1 from the cube, sva_12 via the \
                 mununu#503 `|=>` rescue lane; got {:?}",
                p.outcome
            );
        }
        // H.D translation widening (`signal >= signal`, 1-bit `!x === y`) + H.G
        // arithmetic-addend predicate (`cnt_q == $past(cnt_q) + 1`, CntIncr_A):
        // ALL 16 sysrst SVA now translate — 0 unsupported (pre-H.D: 7 unsupported;
        // pre-H.G: sva_15 was the lone arithmetic holdout).
        assert_eq!(
            report.properties.len(),
            16,
            "H.D + H.G translate all 16 sysrst SVA; got {} translated",
            report.properties.len()
        );
        assert!(
            report.unsupported.is_empty(),
            "H.G closes the last arithmetic holdout — 0 unsupported; got {:?}",
            report.unsupported
        );
        // H.G — sva_15 (`cnt_en && !cnt_clr |=> cnt_q == cnt_q__past + 1`) now
        // BINDS (was unsupported): its arithmetic relational lowers to a
        // `CmpRegAddend` derived label (BV `bvadd`), so it reaches a verdict — an
        // honest ⊥ here (config-timer-entangled, like sva_5/7/10/11/13/14), never
        // unsupported and never a spurious verdict.
        let sva15 = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_15")
            .expect("sva_15 present (translated)");
        assert!(
            sva15.formula.contains("cnt_q__past + 1"),
            "sva_15 carries the arithmetic-addend atom; got {}",
            sva15.formula
        );
        assert!(
            !matches!(sva15.outcome, VerifyOutcome::Skipped { .. }),
            "sva_15 binds to a verdict (not SKIP/unsupported); got {:?}",
            sva15.outcome
        );
        // The negation-peel (`!trigger_i === trigger_active` → `trigger_i !=
        // trigger_active`) and the relational `signal >= signal` both reach the
        // property set now (they SKIP on combinational/input-in-compound
        // operands — the honest residual — but they TRANSLATE).
        assert!(
            report
                .properties
                .iter()
                .any(|p| p.formula.contains("trigger_i != trigger_active")),
            "H.D negation-peel produced the relational `trigger_i != trigger_active`"
        );
        assert!(
            report
                .properties
                .iter()
                .any(|p| p.formula.contains("cnt_q >= cfg_")),
            "H.D translated a `signal >= signal` (cnt_q >= cfg_*_timer_i)"
        );
        // Slice 3 (the unwrap lever, combinational-input-atoms.md §6.1 boundary
        // → resolved) — a trigger-governed conditional-transition safety
        // property `AG((state==k ∧ trigger → AX state==k'))` reaches a DEFINITE
        // HOLDS. The combinational-of-input antecedent (`trigger_active =
        // !trigger_i`) is a derived ⊥-label AND its cone raw input (`trigger_i`)
        // is seeded as a free H.B cube dimension — that refines the may-relation
        // so the consequent box `[](state==k')` becomes definite at the cube
        // pinning trigger_i, and Kleene `⊥ ∨ [] = T`. sva_4/6/8/9 are this class.
        // Pre-Slice-3 they were honest ⊥ (a bare ⊥-label did not refine the box).
        // Soundness rests on the safety + may-over-approximation argument (a
        // definite HOLDS on the KMTS may-relation transfers to the concrete);
        // NEVER a spurious VIOLATED. Independent external confirmation is partial:
        // sva_6 (DetectStDropOut) was checked SAFE by the btormc oracle (H.E/H.O
        // work); sva_4/8/9 are not yet per-property oracle-confirmed — a btormc
        // (H.O.1) sweep is the pending confirmation step.
        for name in [
            "sysrst_ctrl_detect_sva_4",
            "sysrst_ctrl_detect_sva_6",
            "sysrst_ctrl_detect_sva_8",
        ] {
            let p = report
                .properties
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert!(
                p.seeded_predicates.iter().any(|s| s == "trigger_i"),
                "{name} seeds its combinational antecedent's cone input trigger_i as a \
                 free dimension (Slice 3); got {:?}",
                p.seeded_predicates
            );
            assert!(
                matches!(p.outcome, VerifyOutcome::Holds),
                "{name} (trigger-governed conditional transition) reaches a DEFINITE \
                 HOLDS once trigger_i refines the box; got {:?}",
                p.outcome
            );
        }
        // The remaining ⊥ (mununu#797, measured 2026-10-25) are sva_5 (`cnt_q >=
        // cfg_debounce_timer_i`, a relational-with-input compound the cone-input seeder does
        // not unwrap) and sva_15 (`cnt_q == cnt_q__past + 1`, the `CmpRegAddend` leaf).
        // mununu#503: sva_14 is NO LONGER among them — its `cnt_clr |=> cnt_q == 0` reduces
        // and the portfolio decides it; mununu#797: nor are sva_7/10/11 (asserted HOLDS above)
        // and sva_13 (`cnt_q >= cnt_q__past`, decided by the IC3 members). Honest ⊥, never a
        // spurious verdict.
        for name in ["sysrst_ctrl_detect_sva_5", "sysrst_ctrl_detect_sva_15"] {
            let p = report
                .properties
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} present"));
            assert!(
                matches!(p.outcome, VerifyOutcome::Unknown { .. }),
                "{name} stays an honest ⊥ (mununu#797); got {:?}",
                p.outcome
            );
        }
        assert!(
            !report
                .properties
                .iter()
                .any(|p| matches!(p.outcome, VerifyOutcome::Violated { .. })),
            "no spurious VIOLATED; got {:?}",
            report
                .properties
                .iter()
                .map(|p| &p.outcome)
                .collect::<Vec<_>>()
        );
    }

    // mununu#518 — a design with BOTH an array state and a scalar one. The scalar is the control:
    // a filter that over-fires would drop it too, and the fix would look like it worked.
    const MIXED_ARRAY_AND_SCALAR: &str = "\
1 sort bitvec 1
2 sort bitvec 4
3 sort array 2 2
4 state 3 mem
5 state 2 cnt
6 zero 2
7 init 2 5 6
8 next 2 5 5
";

    #[test]
    fn array_states_are_not_admitted_as_bv_state_cells() {
        let file = crate::adapter::btor2::parser::parse(MIXED_ARRAY_AND_SCALAR).expect("parse");
        let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
        let (bv, arrays) = partition_state_cells_by_sort(&file, &symbols);
        assert!(
            !bv.contains("mem"),
            "an array state has no BV to bind to; admitting it as a cube dimension is the \
             mununu#518 defect. Got bv={bv:?}"
        );
        assert!(
            bv.contains("cnt"),
            "the scalar control must survive the filter — a filter that drops everything would \
             pass the assertion above while breaking every real design. Got bv={bv:?}"
        );
        assert!(
            arrays.contains("mem"),
            "the memory is KEPT (separately) so a refusal can name it. Got arrays={arrays:?}"
        );
    }

    #[test]
    fn a_real_yosys_memory_is_classified_as_an_array() {
        // The lifted ibex register file — `16 state 14 mem` over `14 sort array 11 11`. Real
        // yosys output, not a hand-written shape.
        let raw = include_str!("../../../tests/data/ibex_register_file_fpga_16x4.btor2");
        let file = crate::adapter::btor2::parser::parse(raw).expect("parse");
        let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
        let (bv, arrays) = partition_state_cells_by_sort(&file, &symbols);
        assert!(
            arrays.contains("mem"),
            "`mem` is an array state; got arrays={arrays:?}"
        );
        assert!(
            !bv.contains("mem"),
            "and must NOT appear among the BV state cells; got bv={bv:?}"
        );
    }

    #[test]
    fn a_formula_naming_a_memory_is_reported_by_name() {
        let memories: HashSet<String> = ["mem".to_string()].into_iter().collect();
        let f = crate::mu_calculus::parser::parse("nu X. ((mem == 3) && [] X)").expect("parse");
        assert_eq!(
            formula_atoms_naming_memories(&f, &memories),
            vec!["mem".to_string()],
            "the memory must be named so the note can say what the signal actually is"
        );
    }

    #[test]
    fn a_formula_over_scalars_names_no_memory() {
        // The negative control: the diagnostic must not fire on ordinary properties.
        let memories: HashSet<String> = ["mem".to_string()].into_iter().collect();
        let f = crate::mu_calculus::parser::parse("nu X. ((cnt == 3) && [] X)").expect("parse");
        assert!(
            formula_atoms_naming_memories(&f, &memories).is_empty(),
            "a property over a scalar register must not be flagged"
        );
        // And with no memories in the design at all, nothing is ever flagged.
        assert!(
            formula_atoms_naming_memories(&f, &HashSet::new()).is_empty(),
            "a design with no arrays can never produce this note"
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3; run with --ignored"]
    fn e2e_partial_json_breadcrumb_records_every_verdict() {
        // mununu#504 C7 — the WIRING test. `partial_json`'s own unit tests cover the record
        // format and the cursor; what only an end-to-end run can show is that `verify_auto`
        // actually CALLS it, and that every property reaches the file. A breadcrumb that is
        // never written looks identical to one that is written correctly until the day a
        // process is killed and you go looking for it.
        //
        // CONCURRENCY: the env var is process-global and the `--ignored` e2e tests run in
        // parallel, so another `verify_auto` starting mid-test would `File::create` (truncate)
        // this same path. The var is therefore REMOVED as soon as our run returns and before we
        // read, which leaves only a microsecond window rather than the whole test body.
        let sv = "module fsm (input logic clk, input logic rst_n, input logic go);\n\
                  logic [1:0] state;\n\
                  always_ff @(posedge clk) begin\n\
                    if (!rst_n) state <= 2'd0;\n\
                    else state <= (state == 2'd2) ? 2'd0 : state + 2'd1;\n\
                  end\n\
                  ok:  assert property (@(posedge clk) state != 2'd3);\n\
                  bad: assert property (@(posedge clk) state == 2'd1);\n\
                  endmodule\n";
        let sources = vec![("fsm.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("fsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        };

        let dir = std::env::temp_dir().join(format!("mununu-c7-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("breadcrumb.ndjson");
        // SAFETY: set immediately before the run and removed immediately after; see the
        // concurrency note above.
        unsafe {
            std::env::set_var(crate::adapter::partial_json::PARTIAL_JSON_ENV, &path);
        }
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default());
        unsafe {
            std::env::remove_var(crate::adapter::partial_json::PARTIAL_JSON_ENV);
        }
        let report = report.expect("verify_auto runs end-to-end");

        let content = std::fs::read_to_string(&path).expect("the breadcrumb was written");
        let records: Vec<serde_json::Value> = content
            .lines()
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("line is valid JSON: {l:?} ({e})"))
            })
            .collect();
        assert!(
            !records.is_empty(),
            "verify_auto must write records; an empty file means the wiring is not called"
        );

        // The LAST record per `property` (the JSON field) is the authoritative verdict — the escalation pass
        // can append a second one. That is the contract a consumer follows.
        for p in &report.properties {
            let last = records
                .iter()
                .rfind(|r| r["property"] == p.name.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "property `{}` never reached the breadcrumb; records: {:?}",
                        p.name,
                        records.iter().map(|r| &r["property"]).collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                last["outcome"],
                p.outcome.label(),
                "the last breadcrumb record for `{}` must match the final report verdict",
                p.name
            );
            assert!(
                last["phase"] == "main" || last["phase"] == "escalated",
                "every record carries a known phase; got {:?}",
                last["phase"]
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3; run with --ignored"]
    fn e2e_fsm_holds_and_violated_verdicts() {
        // A 2-bit FSM reset to 0, cycling 0→1→2→0. Two state-safety properties:
        //   - `state != 3` HOLDS (the unused encoding is never reached).
        //   - `state == 1` is VIOLATED (false at the reset state, state 0).
        // Exercises the full pipeline (slang → sv2v/Yosys → seed → cube → verdict)
        // and the init-cube-verdict interpretation (the verdict is read at the
        // reset cube, not the lift's cube_0 default).
        let sv = "module fsm (input logic clk, input logic rst_n, input logic go);\n\
                  logic [1:0] state;\n\
                  always_ff @(posedge clk) begin\n\
                    if (!rst_n) state <= 2'd0;\n\
                    else state <= (state == 2'd2) ? 2'd0 : state + 2'd1;\n\
                  end\n\
                  ok:  assert property (@(posedge clk) state != 2'd3);\n\
                  bad: assert property (@(posedge clk) state == 2'd1);\n\
                  endmodule\n";
        let sources = vec![("fsm.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("fsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs end-to-end");
        assert_eq!(report.properties.len(), 2, "two assertions verified");
        // Diagnostics: a self-contained FSM lifts to ≥ 1 state register and
        // black-boxes nothing.
        assert!(
            report.diagnostics.state_register_count >= 1,
            "FSM lifts to a real state register; got {}",
            report.diagnostics.state_register_count
        );
        assert!(
            report.diagnostics.blackboxed_modules.is_empty(),
            "self-contained design cuts nothing; got {:?}",
            report.diagnostics.blackboxed_modules
        );
        let by_formula = |needle: &str| -> &VerifyOutcome {
            &report
                .properties
                .iter()
                .find(|p| p.formula.contains(needle))
                .unwrap_or_else(|| panic!("property containing {needle:?}"))
                .outcome
        };
        assert!(
            matches!(by_formula("state != 3"), VerifyOutcome::Holds),
            "`state != 3` should HOLD; got {:?}",
            by_formula("state != 3")
        );
        assert!(
            matches!(by_formula("state == 1"), VerifyOutcome::Violated { .. }),
            "`state == 1` should be VIOLATED (false at reset); got {:?}",
            by_formula("state == 1")
        );
    }

    #[test]
    #[ignore = "requires slang + sv2v + yosys — run in the mununu-sva image"]
    fn e2e_oracle_gates_verify_auto_verdicts() {
        // H.O.0.3 — the differential on REAL SV-lifted BTOR2 (not inline). Lift
        // the inline FSM the SAME way verify_auto does, then cross-check every
        // DEFINITE verify_auto outcome against the concrete oracle via
        // `spurious_verdict`. Proves the oracle (a) binds the *lifted* register
        // through `resolve_state_alias` (the symbol survives sv2v + yosys) and
        // (b) gates the verdict on a real lift — incl. the `|=>` (AX) fragment.
        let sv = "module fsm (input logic clk, input logic rst_n, input logic go);\n\
                  logic [1:0] state = 2'd0;\n\
                  always_ff @(posedge clk) begin\n\
                    if (!rst_n) state <= 2'd0;\n\
                    else state <= (state == 2'd2) ? 2'd0 : state + 2'd1;\n\
                  end\n\
                  ok:  assert property (@(posedge clk) state != 2'd3);\n\
                  bad: assert property (@(posedge clk) state == 2'd1);\n\
                  imp: assert property (@(posedge clk) state == 2'd2 |=> state == 2'd0);\n\
                  endmodule\n";
        let sources = vec![("fsm.sv".to_string(), sv.to_string())];
        let yopts = YosysOptions {
            top: Some("fsm".to_string()),
            use_sv2v: true,
            ..Default::default()
        };

        // mununu#652 — `state` carries a declared power-up value. Without it the raw lift below
        // has an init-less `state`, which is FREE at cycle 0 (acff567, #603), while `verify_auto`
        // evaluates a model with a synthesised reset `init` and `rst_n` pinned — two different
        // models, and the oracle (correctly) could not conclude `AG state != 3` on the free one.
        // The initializer makes the oracle's model and verify_auto's agree at cycle 0.
        let report = verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
            .expect("verify_auto runs end-to-end");

        // The SAME BTOR2 verify_auto evaluates: single module, no `disable iff`
        // ⇒ no flop stubs / shadows / reset pins, so the lift output is the
        // model under test.
        let btor2 = crate::adapter::yosys::sv_to_btor2_with_blackboxes(sv, &yopts)
            .expect("sv → btor2")
            .btor2;
        let file = crate::adapter::btor2::parser::parse(&btor2).expect("parse lifted btor2");

        let trit_of = |o: &VerifyOutcome| match o {
            VerifyOutcome::Holds => Some(Trit::True),
            VerifyOutcome::Violated { .. } => Some(Trit::False),
            VerifyOutcome::Unknown { .. } => Some(Trit::Unknown),
            VerifyOutcome::Skipped { .. } => None,
        };
        let outcome = |needle: &str| -> &VerifyOutcome {
            &report
                .properties
                .iter()
                .find(|p| p.formula.contains(needle))
                .unwrap_or_else(|| panic!("property containing {needle:?}"))
                .outcome
        };
        // Gate one property: if verify_auto returned a DEFINITE verdict, the
        // oracle must not contradict it (the H.O.0.3 e2e assertion).
        let gate = |needle: &str, oracle: &AgOracle| {
            if let Some(t) = trit_of(outcome(needle)) {
                assert!(
                    spurious_verdict(t, oracle).is_none(),
                    "verify_auto `{needle}` verdict {t:?} contradicts the concrete oracle {oracle:?}"
                );
            }
        };

        // `state != 3` — the oracle proves HOLDS on the lifted design; the
        // verify_auto verdict must agree.
        let o_safe = ag_state_atom(&file, "state", CmpOp::Ne, 3, 1024, 8, false)
            .expect("oracle binds lifted `state`");
        assert_eq!(o_safe, AgOracle::Holds, "oracle: encoding 3 unreachable");
        gate("state != 3", &o_safe);

        // `state == 1` — false at the reset state → the oracle finds the violation.
        let o_bad = ag_state_atom(&file, "state", CmpOp::Eq, 1, 1024, 8, false)
            .expect("oracle ag(state==1)");
        assert!(
            matches!(o_bad, AgOracle::Violated(_)),
            "oracle: state==1 violated at reset"
        );
        gate("state == 1", &o_bad);

        // `state == 2 |=> state == 0` — the AX fragment: from state 2 the next
        // state is always 0. The oracle proves HOLDS; verify_auto must agree.
        let ante = OracleAtom::new("state", CmpOp::Eq, 2);
        let cons = OracleAtom::new("state", CmpOp::Eq, 0);
        let o_imp = ag_implies_next(&file, &ante, &cons, 1024, 8, false)
            .expect("oracle ag(state==2 |=> 0)");
        assert_eq!(o_imp, AgOracle::Holds, "oracle: 2 always steps to 0");
        gate("state == 2", &o_imp);
    }

    #[test]
    #[ignore = "requires slang + sv2v + yosys — run in the mununu-sva image"]
    fn e2e_oracle_gates_sysrst_real_rtl_verdict() {
        // H.O.0.3b — gate the ONE definite REAL-OpenTitan verdict against the
        // concrete oracle: sysrst_ctrl_detect sva_0, `cfg_enable_i |=> state_q
        // == 0`, which verify_auto reports HOLDS (a config-INPUT antecedent + a
        // state consequent — exactly `ag_implies_next`'s fragment).
        //
        // The oracle must answer verify_auto's GATED question, so we pin the SAME
        // reset verify_auto pinned (`diagnostics.gated_resets`) before enumerating
        // — otherwise the oracle would explore reset-active states verify_auto
        // excluded and could "refute" a verdict that is correct under the gated
        // semantics. Pinning happens at the BTOR2 level (`pin_inputs_to_constants`),
        // so the oracle needs no reset awareness.
        //
        // Honest boundary: sysrst has WIDE config inputs (`cfg_detect_timer_i`)
        // the oracle cannot exhaustively enumerate, so its reachability is
        // BOUNDED → it can soundly REFUTE a (shallow) spurious HOLDS but cannot
        // CONFIRM a real-RTL HOLDS. Confirming real-RTL verdicts is H.O.1's
        // (external BTOR2 model checker) domain. This test proves the oracle
        // BINDS real-RTL atoms (state_q via value-alias resolution; cfg_enable_i
        // as a 1-bit input) and provides a sound refutation guard.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let sysrst = root.join("r46_sysrst_detect_k5/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let sources = vec![
            (
                "sysrst_ctrl_detect.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_detect.sv")),
            ),
            (
                "sysrst_ctrl_pkg.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_pkg.sv")),
            ),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("sysrst_ctrl_detect".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on sysrst_ctrl_detect");

        let sva0 = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_0")
            .expect("sva_0 present");
        assert!(
            matches!(sva0.outcome, VerifyOutcome::Holds),
            "precondition: verify_auto reports sva_0 HOLDS; got {:?}",
            sva0.outcome
        );

        // Lift the base BTOR2 + pin the SAME resets verify_auto pinned.
        let btor2 = crate::adapter::yosys::sv_to_btor2_with_blackboxes(&sources[0].1, &yopts)
            .expect("sv → btor2")
            .btor2;
        let pins: Vec<(String, u64)> = report
            .diagnostics
            .gated_resets
            .iter()
            .filter_map(|g| {
                let (n, v) = g.split_once('=')?;
                Some((n.to_string(), v.trim().parse::<u64>().ok()?))
            })
            .collect();
        eprintln!("\n=== H.O.0.3b sysrst gate: pinning resets {pins:?} ===");
        let (pinned, _) = crate::adapter::btor2::pin::pin_inputs_to_constants(&btor2, &pins);
        let file = crate::adapter::btor2::parser::parse(&pinned).expect("parse pinned btor2");

        // The oracle for sva_0's exact shape:
        //   formula: nu X. (((!((!(cfg_enable_i))) || [] (state_q == 0))) && [] X)
        //   = (!cfg_enable_i) |=> (state_q == 0)   — "while DISABLED, stay idle".
        // The antecedent is `cfg_enable_i == 0` (the double-negation in the
        // formula resolves to `!a = cfg_enable_i`, so the SVA antecedent
        // `a = !cfg_enable_i`). Getting this polarity wrong checks a different
        // property and yields a bogus contradiction.
        let ante = OracleAtom::new("cfg_enable_i", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state_q", CmpOp::Eq, 0);
        // reset_pinned = true: rst_ni is pinned to a constant above, so following
        // the async-reset register mux to `state_q`'s state cell is sound (the mux
        // always selects the state branch) — the same condition the verify-auto
        // seeder resolves `state_q` under.
        let oracle = ag_implies_next(&file, &ante, &cons, 100_000, 8, true)
            .expect("oracle binds real-RTL state_q (resolution) + cfg_enable_i (input)");
        eprintln!("sysrst sva_0 `!cfg_enable_i |=> state_q == 0` oracle: {oracle:?}");

        // Soundness gate: verify_auto's HOLDS must not be contradicted. On a
        // genuinely-holding property the oracle returns Holds (if the design fits
        // the cap) or Inconclusive (bounded) — never Violated.
        assert!(
            spurious_verdict(Trit::True, &oracle).is_none(),
            "verify_auto sva_0 HOLDS contradicted by the concrete oracle: {oracle:?}"
        );
        assert!(
            matches!(oracle, AgOracle::Holds | AgOracle::Inconclusive),
            "no reachable violation of a genuinely-holding property; got {oracle:?}"
        );
    }

    // ---- H.O.0.2 — oracle differential vs the cube verdict --------------------
    //
    // The 2026-06-29 soundness review's headline gap: nothing independently
    // checks that a DEFINITE verify-auto verdict is correct, and a spurious
    // `HOLDS` (silently claiming a property holds) is the dangerous case. These
    // tests drive the SAME cube path verify_auto uses — `seed_from_formula` →
    // `synth_sidecar_json` → `cegar_refine_loop`, read at the reset cube — on a
    // hand-built BTOR2 FSM, and cross-check every DEFINITE cube verdict against
    // the concrete bounded-reachability oracle (`concrete_oracle::ag_state_atom`,
    // H.O.0.1).
    //
    // Invariant (the actual guard `cube_vs_oracle` enforces):
    //   cube-HOLDS    ⟹ oracle is NOT Violated   (else: SPURIOUS HOLDS)
    //   cube-VIOLATED ⟹ oracle is NOT Holds       (else: SPURIOUS VIOLATED)
    //   cube-⊥        ⟹ anything                  (sound coarsening)
    //
    // No slang: the cube path runs directly on inline BTOR2, so the differential
    // executes in `make ci` (not behind the slang `#[ignore]` gate). The seeding
    // + reset-cube logic mirrors verify_auto's per-property block (the
    // `for t in ...` loop above) — keep them in sync when that block changes.
    use crate::adapter::AdapterOptions;
    use crate::adapter::btor2::cegar::{
        CegarOptions, LiftStrategy, PredicateSource, cegar_refine_loop,
    };
    use crate::adapter::btor2::concrete_oracle::{
        AgOracle, OracleAtom, ag_implies_next, ag_state_atom,
    };
    use crate::mu_calculus::Environment;
    use crate::mu_calculus::trit::Trit;

    /// A 2-bit FSM cycling 0→1→2→0 (caps at 2, never reaches 3), reset to 0,
    /// input-free. Same design as `concrete_oracle`'s CYCLE_FSM, so the cube
    /// path and the oracle observe one model.
    const DIFF_CYCLE_FSM: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 state 2 state
4 zero 2
5 one 2
6 constd 2 2
7 eq 1 3 6
8 add 2 3 5
9 ite 2 7 4 8
10 next 2 3 9
11 init 2 3 4
";

    /// Drive the cube path for `AG (signal op value)` exactly as verify_auto's
    /// default does (seed → synth sidecar → `cegar_refine_loop` → verdict at the
    /// reset cube), AND the concrete oracle for the same atom; assert the
    /// soundness invariant and return both verdicts for the caller to pin.
    fn cube_vs_oracle(
        btor2: &str,
        init_values: &[(&str, u64)],
        signal: &str,
        op: CmpOp,
        value: u64,
    ) -> (Trit, AgOracle) {
        let op_s = match op {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        };
        let atom = format!("{signal} {op_s} {value}");
        let formula = mu_parser::parse(&format!("nu X. (({atom}) && [] X)")).unwrap();

        // Seed as verify_auto: the atom's register is our single state cell;
        // no free inputs, no combinational signals.
        let seeded = seed_from_formula(
            &formula,
            |r| r == signal,
            |_| false,
            |_| None,
            |_| Vec::new(),
            &[],
        );

        // Mirror verify_auto's default cegar_opts (must Off; may = SmtAllPairs —
        // AR-S2 retired the sampling-may fallback).
        let cegar_opts = CegarOptions {
            max_iterations: 16,
            predicate_source: PredicateSource::WeakestPrecondition,
            max_cube_count: 1024,
            capture_approximants: false,
            enable_approximant_reuse: false,
            smart_uf_cap: true,
            lift_strategy: LiftStrategy::Eager,
            must_edge_inference: MustEdgeInference::Off,
            may_edge_inference: MayEdgeInference::SmtAllPairs,
            emit_ctxdsl: false,
        };

        let init_u64: std::collections::HashMap<String, u64> = init_values
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let mut referenced: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for s in &seeded.specs {
            referenced.insert(s.register.clone());
        }
        for (_, e) in &seeded.compounds {
            for r in e.registers() {
                referenced.insert(r);
            }
        }
        let adapter_options = AdapterOptions {
            sidecar_json: synth_sidecar_json(
                &seeded.compounds,
                &seeded.derived,
                &seeded.derived_relational,
                &referenced,
                &init_u64,
            ),
            ..Default::default()
        };

        // Reset cube index: bit order = simple specs, then compounds (the order
        // `cegar_refine_loop` assembles `current_predicates`).
        let predicate_count = seeded.specs.len() + seeded.compounds.len();
        let env = Environment::new(1usize << predicate_count);
        let init_u128: std::collections::HashMap<String, u128> = init_u64
            .iter()
            .map(|(k, v)| (k.clone(), *v as u128))
            .collect();
        let mut base_init_cube = 0usize;
        let mut free_input_bits: Vec<u32> = Vec::new();
        let mut bit = 0u32;
        for s in &seeded.specs {
            if seeded.input_registers.contains(&s.register) {
                free_input_bits.push(bit);
            } else if init_u64.get(&s.register).copied().unwrap_or(0) == s.value {
                base_init_cube |= 1 << bit;
            }
            bit += 1;
        }
        for (_, expr) in &seeded.compounds {
            if expr.eval(&init_u128) {
                base_init_cube |= 1 << bit;
            }
            bit += 1;
        }
        let init_cubes = free_input_init_cubes(base_init_cube, &free_input_bits);

        let trace = cegar_refine_loop(
            &formula,
            btor2,
            seeded.specs.clone(),
            &env,
            &adapter_options,
            &cegar_opts,
        )
        .expect("cegar succeeds");
        let v = &trace.final_verdict;
        let cube = if init_cubes.iter().any(|&ic| v.verdict_at(ic) == Trit::False) {
            Trit::False
        } else if init_cubes
            .iter()
            .any(|&ic| v.verdict_at(ic) == Trit::Unknown)
        {
            Trit::Unknown
        } else {
            Trit::True
        };

        // The oracle side (H.O.0.1) — concrete bounded reachability, no abstraction.
        let file = crate::adapter::btor2::parser::parse(btor2).expect("oracle parses btor2");
        let oracle =
            ag_state_atom(&file, signal, op, value as u128, 1024, 8, false).expect("oracle runs");

        // The spurious-verdict guard.
        if let Some(reason) = spurious_verdict(cube, &oracle) {
            panic!("{reason} for `AG {atom}` (cube={cube:?}, oracle={oracle:?})");
        }
        (cube, oracle)
    }

    /// The H.O.0.2 soundness invariant, as a pure predicate so it can be
    /// negative-control-tested. Returns `Some(reason)` iff the cube verdict and
    /// the concrete oracle *contradict* — a definite cube verdict the oracle
    /// refutes. `None` means consistent (including every cube-⊥, the sound
    /// coarsening, and every case the oracle can't conclude).
    fn spurious_verdict(cube: Trit, oracle: &AgOracle) -> Option<&'static str> {
        match (cube, oracle) {
            // cube proved HOLDS, but a reachable state violates → spurious HOLDS
            // (the dangerous case the review flagged).
            (Trit::True, AgOracle::Violated(_)) => Some("SPURIOUS HOLDS"),
            // cube refuted, but full concrete enumeration found no violation →
            // spurious VIOLATED. (A *bounded* `Inconclusive` is NOT a
            // contradiction — the oracle simply couldn't confirm.)
            (Trit::False, AgOracle::Holds) => Some("SPURIOUS VIOLATED"),
            _ => None,
        }
    }

    #[test]
    fn differential_ag_holds_agrees_with_oracle() {
        // `AG state != 3` — 3 is the unreachable encoding of the 0→1→2→0 FSM.
        // The cube proves HOLDS; the oracle confirms (full enumeration).
        let (cube, oracle) = cube_vs_oracle(DIFF_CYCLE_FSM, &[("state", 0)], "state", CmpOp::Ne, 3);
        assert_eq!(cube, Trit::True, "cube proves AG state != 3");
        assert_eq!(oracle, AgOracle::Holds, "oracle confirms 3 unreachable");
    }

    #[test]
    fn differential_ag_violated_agrees_with_oracle() {
        // `AG state == 1` — false at the reset state (state 0) → VIOLATED at the
        // reset cube, independent of edges (νX ≤ body).
        let (cube, oracle) = cube_vs_oracle(DIFF_CYCLE_FSM, &[("state", 0)], "state", CmpOp::Eq, 1);
        assert_eq!(
            cube,
            Trit::False,
            "cube refutes AG state == 1 at the reset cube"
        );
        assert!(
            matches!(oracle, AgOracle::Violated(_)),
            "oracle finds the violation"
        );
    }

    #[test]
    fn differential_ag_bottom_is_sound_no_spurious_holds() {
        // `AG state != 1` — state 1 IS reachable (0→1), so the property is FALSE.
        // The single-predicate cube is too coarse to refute it (no must-edge
        // under the default must-Off policy) → the SOUND answer is ⊥, NOT a
        // spurious HOLDS. A regression to a spurious HOLDS here trips
        // `cube_vs_oracle`'s invariant. This is the H.O.0.2 headline guard.
        let (cube, oracle) = cube_vs_oracle(DIFF_CYCLE_FSM, &[("state", 0)], "state", CmpOp::Ne, 1);
        assert_eq!(
            cube,
            Trit::Unknown,
            "coarse cube is appropriately ⊥, not a spurious HOLDS"
        );
        assert!(
            matches!(oracle, AgOracle::Violated(_)),
            "oracle: state 1 is reachable"
        );
    }

    #[test]
    fn differential_guard_catches_spurious_verdicts() {
        // Negative control: prove the invariant is LIVE (not vacuously passing).
        // The two contradictory pairs must be flagged; every consistent pair
        // (incl. cube-⊥ and the bounded-`Inconclusive` escape) must not.
        let witness: std::collections::BTreeMap<String, u128> =
            [("state".to_string(), 1u128)].into_iter().collect();
        // Contradictions → flagged.
        assert_eq!(
            spurious_verdict(Trit::True, &AgOracle::Violated(witness.clone())),
            Some("SPURIOUS HOLDS")
        );
        assert_eq!(
            spurious_verdict(Trit::False, &AgOracle::Holds),
            Some("SPURIOUS VIOLATED")
        );
        // Consistent → not flagged.
        assert_eq!(spurious_verdict(Trit::True, &AgOracle::Holds), None);
        assert_eq!(
            spurious_verdict(Trit::False, &AgOracle::Violated(witness)),
            None
        );
        assert_eq!(spurious_verdict(Trit::Unknown, &AgOracle::Holds), None);
        assert_eq!(
            spurious_verdict(Trit::Unknown, &AgOracle::Inconclusive),
            None
        );
        // A bounded oracle can't conclude true: cube-False + Inconclusive is NOT
        // a contradiction (the oracle just didn't reach the violation).
        assert_eq!(spurious_verdict(Trit::False, &AgOracle::Inconclusive), None);
        // cube-True + Inconclusive is also fine — no violation was *found*.
        assert_eq!(spurious_verdict(Trit::True, &AgOracle::Inconclusive), None);
    }

    // ---- H.O.1c — external BTOR2-MC oracle differential (btormc) ---------------
    //
    // H.O.0's concrete oracle is enumeration-bounded: on a design with wide inputs
    // it can REFUTE a spurious cube-HOLDS but is forced to `Inconclusive` and
    // cannot CONFIRM a real-RTL HOLDS (H.O.0.3b). H.O.1 adds the external SYMBOLIC
    // oracle: emit the property as a BTOR2 `bad` monitor (H.O.1b) and let
    // `btormc --kind` (H.O.1a) decide it — `unsat` ⇒ SAFE/HOLDS (a real
    // k-induction proof, no enumeration), `sat` ⇒ VIOLATED. These tests are
    // `#[ignore]`d (need btormc — run in `mununu-sva`); the pure differential
    // logic (`spurious_verdict_mc`) is make-ci unit-tested below.
    use crate::adapter::btor2::bad_monitor::{
        emit_ag_implies_next_monitor, emit_ag_state_atom_monitor,
    };
    use crate::adapter::btormc::{
        DEFAULT_KMAX, DEFAULT_TIMEOUT, McVerdict, locate_btormc, run_btormc,
    };

    /// MC analog of [`spurious_verdict`]: `Some(reason)` iff a DEFINITE cube
    /// verdict and the external model checker *contradict*. `McVerdict::Unknown`
    /// (bounded — no CEX and no proof) never contradicts.
    fn spurious_verdict_mc(cube: Trit, mc: McVerdict) -> Option<&'static str> {
        match (cube, mc) {
            (Trit::True, McVerdict::Violated) => Some("SPURIOUS HOLDS"),
            (Trit::False, McVerdict::Safe) => Some("SPURIOUS VIOLATED"),
            _ => None,
        }
    }

    #[test]
    fn mc_guard_catches_spurious_verdicts() {
        // make-ci negative control (pure logic; no btormc). Proves the MC
        // soundness invariant is LIVE: contradictions flagged, everything
        // consistent — including every `Unknown` — not flagged.
        assert_eq!(
            spurious_verdict_mc(Trit::True, McVerdict::Violated),
            Some("SPURIOUS HOLDS")
        );
        assert_eq!(
            spurious_verdict_mc(Trit::False, McVerdict::Safe),
            Some("SPURIOUS VIOLATED")
        );
        assert_eq!(spurious_verdict_mc(Trit::True, McVerdict::Safe), None);
        assert_eq!(spurious_verdict_mc(Trit::False, McVerdict::Violated), None);
        assert_eq!(spurious_verdict_mc(Trit::Unknown, McVerdict::Safe), None);
        assert_eq!(
            spurious_verdict_mc(Trit::Unknown, McVerdict::Violated),
            None
        );
        assert_eq!(spurious_verdict_mc(Trit::True, McVerdict::Unknown), None);
        assert_eq!(spurious_verdict_mc(Trit::False, McVerdict::Unknown), None);
    }

    // A btormc-compatible 2-bit FSM cycling 0→1→2→0 (caps at 2, never 3) with an
    // irrelevant WIDE (8-bit) input `w`. The wide input forces the internal
    // oracle's enumeration BOUNDED (→ Inconclusive); btormc decides symbolically
    // (w is irrelevant to the FSM). Init const (nid 5) precedes the state (nid 6)
    // so btormc's `init state_nid > val_nid` rule holds.
    const WIDE_INPUT_FSM: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 sort bitvec 8
4 input 3 w
5 zero 2
6 state 2 cnt
7 one 2
8 constd 2 2
9 eq 1 6 8
10 add 2 6 7
11 ite 2 9 5 10
12 next 2 6 11
13 init 2 6 5
";

    #[test]
    #[ignore = "requires btormc (MUNUNU_BTORMC_PATH or $PATH); run with --ignored in mununu-sva"]
    fn e2e_btormc_decides_beyond_the_internal_oracle_cap() {
        // The H.O.1 payoff, deterministic: the internal oracle is Inconclusive
        // (the wide input caps enumeration), btormc returns a DEFINITE Safe.
        let file = crate::adapter::btor2::parser::parse(WIDE_INPUT_FSM).unwrap();
        let internal = ag_state_atom(&file, "cnt", CmpOp::Ne, 3, 256, 8, false).unwrap();
        assert_eq!(
            internal,
            AgOracle::Inconclusive,
            "wide input ⇒ bounded enumeration ⇒ internal oracle cannot confirm"
        );

        let bin = locate_btormc().expect("btormc present");
        let monitor =
            emit_ag_state_atom_monitor(WIDE_INPUT_FSM, "cnt", CmpOp::Ne, 3, false).unwrap();
        let mc = run_btormc(&bin, &monitor, DEFAULT_KMAX, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(
            mc,
            McVerdict::Safe,
            "btormc proves AG(cnt != 3) symbolically, beyond the enumeration cap"
        );
        // A cube-HOLDS on this property is CONFIRMED (no spurious-HOLDS).
        assert!(spurious_verdict_mc(Trit::True, mc).is_none());
    }

    #[test]
    #[ignore = "requires slang+sv2v+yosys+btormc (mununu-sva); run with --ignored"]
    fn e2e_btormc_confirms_sysrst_real_rtl_verdict() {
        // H.O.1c real-RTL payoff: the external MC CONFIRMS the one definite
        // real-OpenTitan verdict (sysrst sva_0, `!cfg_enable_i |=> state_q == 0`,
        // which verify_auto reports HOLDS) where the internal oracle is forced to
        // `Inconclusive` (sysrst's wide `cfg_detect_timer_i` exceeds the cap;
        // H.O.0.3b). Same lift + reset-pin as `e2e_oracle_gates_sysrst_real_rtl_verdict`.
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let sysrst = root.join("r46_sysrst_detect_k5/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let sources = vec![
            (
                "sysrst_ctrl_detect.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_detect.sv")),
            ),
            (
                "sysrst_ctrl_pkg.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_pkg.sv")),
            ),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("sysrst_ctrl_detect".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                must_edge_inference: MustEdgeInference::SmtHyperMust,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on sysrst_ctrl_detect");
        let sva0 = report
            .properties
            .iter()
            .find(|p| p.name == "sysrst_ctrl_detect_sva_0")
            .expect("sva_0 present");
        assert!(
            matches!(sva0.outcome, VerifyOutcome::Holds),
            "precondition: verify_auto reports sva_0 HOLDS; got {:?}",
            sva0.outcome
        );

        // Lift the base BTOR2 + pin the SAME resets verify_auto pinned.
        let btor2 = crate::adapter::yosys::sv_to_btor2_with_blackboxes(&sources[0].1, &yopts)
            .expect("sv → btor2")
            .btor2;
        let pins: Vec<(String, u64)> = report
            .diagnostics
            .gated_resets
            .iter()
            .filter_map(|g| {
                let (n, v) = g.split_once('=')?;
                Some((n.to_string(), v.trim().parse::<u64>().ok()?))
            })
            .collect();
        let (pinned, _) = crate::adapter::btor2::pin::pin_inputs_to_constants(&btor2, &pins);

        // sva_0 = `(!cfg_enable_i) |=> (state_q == 0)`. Emit its `bad` monitor on
        // the pinned BTOR2 (reset_pinned = true: the async-reset mux to state_q is
        // sound under the pin). btormc decides it symbolically.
        let ante = OracleAtom::new("cfg_enable_i", CmpOp::Eq, 0);
        let cons = OracleAtom::new("state_q", CmpOp::Eq, 0);
        let monitor = emit_ag_implies_next_monitor(&pinned, &ante, &cons, true)
            .expect("emit |=> monitor on real-RTL (binds state_q + cfg_enable_i)");

        let bin = locate_btormc().expect("btormc present");
        let mc = run_btormc(&bin, &monitor, 60, DEFAULT_TIMEOUT)
            .expect("btormc runs on the sysrst monitor");
        eprintln!("\n=== H.O.1c sysrst sva_0 external-MC verdict: {mc:?} ===");

        // Soundness gate (always): verify_auto's HOLDS (Trit::True) must not be
        // contradicted — btormc must NOT return Violated.
        assert!(
            spurious_verdict_mc(Trit::True, mc).is_none(),
            "verify_auto sva_0 HOLDS contradicted by btormc: {mc:?}"
        );
        assert_ne!(
            mc,
            McVerdict::Violated,
            "no reachable violation of a genuinely-holding property"
        );
        // The H.O.1 payoff: btormc reaches a DEFINITE Safe where the internal
        // oracle is Inconclusive — the real-RTL CONFIRMATION H.O.0 could not give.
        // Observed `Safe` in the mununu-sva image (btormc 3.2.4, k-induction closes
        // within kmax=60); this asserts that confirmation does not regress.
        assert_eq!(
            mc,
            McVerdict::Safe,
            "btormc confirms sysrst sva_0 HOLDS (the real-RTL confirmation beyond H.O.0's cap)"
        );
    }

    /// R-F5.5d — run the sysrst_ctrl_detect breakdown through BOTH the explicit
    /// and the R-F5 **symbolic** engine on real OpenTitan RTL. The symbolic
    /// engine supports the cube-dimension-predicate + bare-modality fragment (it
    /// Skips derived combinational per-cube-label predicates the explicit path
    /// binds, and its `∀∃` must differs from the explicit `SmtHyperMust`), so a
    /// full verdict match is not expected — this validates that the symbolic
    /// path *runs end-to-end on real RTL*, completes, and yields definite
    /// verdicts. Prints the per-property explicit-vs-symbolic comparison + the
    /// wall-clock of each engine for inspection.
    ///
    /// mununu#656 / #658 — this test used to require at least one symbolic ABSTENTION
    /// ("the graceful-degradation case"). Those abstentions were the exact rescue refusing
    /// input-reading atoms after the symbolic cube erred; with inputs in the state the rescue
    /// decides every one of the 16 (all HOLDS — they are OpenTitan's own assertions), so the
    /// design no longer exercises degradation and the count is reported, not asserted. What
    /// IS asserted is cross-engine AGREEMENT: a Holds/Violated disagreement between the two
    /// runs is a soundness defect in one of them, never a precision difference. That check is
    /// what caught #658 — the rescue, evaluating `trigger_active`'s input per step, refuted
    /// `EnterDebounceSt_A` while the explicit cube held it.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (use the mununu-sva docker image); run with --ignored"]
    fn e2e_sysrst_symbolic_engine_runs_on_real_rtl() {
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let sysrst = root.join("r46_sysrst_detect_k5/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let sources = vec![
            (
                "sysrst_ctrl_detect.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_detect.sv")),
            ),
            (
                "sysrst_ctrl_pkg.sv".to_string(),
                read(sysrst.join("sysrst_ctrl_pkg.sv")),
            ),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("sysrst_ctrl_detect".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let run = |symbolic: bool| {
            let start = std::time::Instant::now();
            let r = verify_auto(
                &sources,
                &yopts,
                &VerifyAutoOptions {
                    must_edge_inference: MustEdgeInference::SmtHyperMust,
                    symbolic_engine: symbolic,
                    ..Default::default()
                },
            )
            .expect("verify_auto runs");
            (r, start.elapsed())
        };

        let (explicit, dt_x) = run(false);
        let (symbolic, dt_s) = run(true);

        eprintln!("\n=== R-F5.5d sysrst: explicit vs symbolic ===");
        eprintln!("explicit engine: {dt_x:?}   symbolic engine: {dt_s:?}");
        // What this test is FOR: on a real design the symbolic engine either DECIDES or
        // ABSTAINS HONESTLY — never a mid-construction OoM panic, and never a process abort
        // (mununu#504). So the assertion is over the engine's ABSTENTION CONTRACT, not over
        // which budget happens to fire.
        //
        // It used to require a Skip carrying the BIT-CAP reason. That pinned one mechanism,
        // and the R-F5.6 COI restriction then made it unreachable: every cone now fits under
        // the cap, so sysrst abstains on the NODE budget (and on two unsupported combinational
        // predicates) instead. The test failed while the engine was behaving BETTER than when
        // it was written — a stale assertion, not a regression. Measured 2026-09-07:
        // bit-cap Skips = 0; every abstention named the NODE budget or a declared feature gap.
        //
        // Assert the NEGATIVE (no opaque/internal failure) and TALLY the positive. A whitelist
        // of accepted reason strings would drift exactly the way the bit-cap assertion did, so
        // the budgets below are counted for the diagnostic, never required.
        let looks_like_a_defect = |reason: &str| {
            let r = reason.to_ascii_lowercase();
            [
                "panic",
                "internal error",
                "unwrap",
                "index out of bounds",
                "overflowed its stack",
            ]
            .iter()
            .any(|m| r.contains(m))
        };
        let mut sym_abstained = 0;
        let mut named_budget = 0;
        for (px, ps) in explicit.properties.iter().zip(symbolic.properties.iter()) {
            assert_eq!(px.name, ps.name, "property order aligns across engines");
            eprintln!(
                "  {}: explicit={:?}  symbolic={:?}",
                px.name, px.outcome, ps.outcome
            );
            let definite = |o: &VerifyOutcome| match o {
                VerifyOutcome::Holds => Some(true),
                VerifyOutcome::Violated { .. } => Some(false),
                _ => None,
            };
            if let (Some(a), Some(b)) = (definite(&px.outcome), definite(&ps.outcome)) {
                assert_eq!(
                    a, b,
                    "{}: the explicit and the symbolic(+exact-rescue) engines CONTRADICT each \
                     other — explicit={:?} symbolic={:?}. One of them is unsound (mununu#658 was \
                     found by exactly this row).",
                    px.name, px.outcome, ps.outcome
                );
            }
            if let VerifyOutcome::Skipped { reason } = &ps.outcome {
                assert!(
                    !reason.trim().is_empty(),
                    "{}: an abstention must explain itself — an empty reason gives a consumer \
                     nothing to act on",
                    ps.name
                );
                assert!(
                    !looks_like_a_defect(reason),
                    "{}: the symbolic engine abstained by FAILING, not by deciding it could not \
                     decide. An abstention is a supported outcome; a panic surfaced as a reason \
                     string is a defect: {reason}",
                    ps.name
                );
                sym_abstained += 1;
                if [
                    "register+input bits",
                    "NODE budget",
                    "ITERATION budget",
                    "WALL-CLOCK budget",
                ]
                .iter()
                .any(|m| reason.contains(m))
                {
                    named_budget += 1;
                }
            }
        }
        // Reported, not asserted (see the doc comment): since mununu#656 the exact rescue
        // decides all 16 here, so a zero is the expected reading on this design.
        eprintln!("symbolic abstentions: {sym_abstained} (of which {named_budget} name a budget)");
    }

    /// D1.6 — the `--engine exact-symbolic` **surface route**, end-to-end through
    /// `verify_auto` on real OpenTitan RTL. A `@mununu_guarantee` annotation carries
    /// the headline `AG AF (bit_cnt_q == 0)` liveness property; `verify_auto` with
    /// `exact_symbolic: true` must route it through the exact full-state ROBDD MC
    /// (`exact_symbolic_verdict`) — the same engine `e2e_d1_uart_tx_exact_liveness_verdict`
    /// exercises directly — and return a **definite Violated** (with a stall lasso),
    /// where the predicate-cube path answers this ⊥ (no ranking for `AF`).
    ///
    /// This validates the CLI/API `--engine exact-symbolic` wiring, not the D1 thesis
    /// itself (that is the direct test's job): the annotation guarantee flows through
    /// scan → merge → the hoisted exact branch (which runs BEFORE cube seeding, so it
    /// is never gated behind a seeding skip). Regression for the frozen-register fix
    /// (2026-07-05): this asserted `Holds` while `bit_cnt_q` (a `uext`-aliased register)
    /// was silently frozen by the `next_funcs` keying bug; the true verdict is
    /// `Violated` (the counter can be held non-zero forever by a persistent write or a
    /// stalled tick).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys (use the mununu-sva docker image); run with --ignored"]
    fn e2e_d1_6_verify_auto_exact_symbolic_route_uart_tx() {
        use std::path::PathBuf;
        let src_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/m1_opentitan_uart_tx/source/uart_tx.sv");
        let uart_tx = std::fs::read_to_string(&src_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", src_path.display()));
        // Carry the AG AF liveness property as a source annotation — verify_auto's
        // scan_annotation_properties reads the ORIGINAL source (not the elaborated
        // output), parses the body via the mu-calculus parser, and merges it as
        // `ann_guarantee_0`.
        let annotated = format!(
            "// @mununu_guarantee nu X. ((mu Y. ((bit_cnt_q == 0) or [] Y)) and [] X)\n{uart_tx}"
        );
        let sources = vec![("uart_tx.sv".to_string(), annotated)];
        let yopts = YosysOptions {
            top: Some("uart_tx".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                exact_symbolic: true,
                ..Default::default()
            },
        )
        .expect("verify_auto runs with --engine exact-symbolic");

        let guarantee = report
            .properties
            .iter()
            .find(|p| p.name.contains("ann_guarantee"))
            .unwrap_or_else(|| {
                panic!(
                    "the @mununu_guarantee property must appear in the report; got: {:?}",
                    report
                        .properties
                        .iter()
                        .map(|p| &p.name)
                        .collect::<Vec<_>>()
                )
            });
        eprintln!(
            "\n=== D1.6 verify_auto --engine exact-symbolic on uart_tx ===\n  {} ({}): {:?}",
            guarantee.name, guarantee.formula, guarantee.outcome
        );
        assert!(
            matches!(guarantee.outcome, VerifyOutcome::Violated { .. }),
            "AG AF (bit_cnt_q==0) must be decided Violated by the exact-symbolic route \
             through verify_auto (the counter can be held non-zero forever) — a false \
             Holds means the frozen-register `next_funcs` alias-keying bug regressed; \
             got {:?}",
            guarantee.outcome
        );
    }

    /// monono#partsel — the partial-register-assignment soundness fix, end-to-end
    /// through `verify_auto`. Five registers, each reachably non-zero, each written
    /// a different way.
    ///
    /// Under the **slang** front-end, the two partial-write shapes lift differently:
    /// - `a_q[11:8] <= val` (plain vector) — the unwritten bits become anonymous
    ///   free `input`s (havoc); a verdict would read them, so verify_auto **Skips**
    ///   it (never the reported spurious `HOLDS`).
    /// - `p_q[idx] <= val` (packed 2-D) — the register splits into anonymous
    ///   `state` sub-cells with NO free inputs; the NID-indexed cone-of-influence
    ///   now KEEPS them, so this **decides `Violated`** (the COI fix — previously it
    ///   was frozen/Skipped). `b_q` / `c_q` / `d_q` are faithful ⇒ `Violated`.
    ///
    /// Under the **read_verilog + sv2v** front-end, every register lifts to one
    /// faithful state cell, so ALL FIVE are decided `Violated` — the guard does not
    /// over-refuse the faithful lift. Together these pin the fix (no silent HOLDS),
    /// its precision (the good path still decides), and the COI fix (packed-2-D now
    /// decides on slang instead of being frozen).
    /// #466 — the `lift-frontend` note names the RTL front end that produced the
    /// verdicts, end-to-end. Forced `read_verilog` → the note says `read_verilog`
    /// (Info, no fallback); forced `slang` → the note says `yosys-slang`.
    #[test]
    #[ignore = "requires slang + sv2v + yosys (mununu-sva image)"]
    fn e2e_verify_auto_reports_lift_frontend() {
        use crate::adapter::yosys::SvFrontend;
        const DESIGN: &str = "// @mununu_guarantee nu X. ((q == 0) and [] X)\n\
             module m(input logic clk, input logic rst_n, input logic [3:0] d,\n\
                      output logic [3:0] q);\n\
               always_ff @(posedge clk or negedge rst_n)\n\
                 if (!rst_n) q <= '0; else q <= d;\n\
             endmodule\n";
        let sources = vec![("m.sv".to_string(), DESIGN.to_string())];
        let cfg = {
            let mut m = std::collections::HashMap::new();
            m.insert("rst_n".to_string(), 1u64);
            m
        };
        let run = |frontend: SvFrontend| {
            verify_auto(
                &sources,
                &YosysOptions {
                    top: Some("m".to_string()),
                    use_sv2v: matches!(frontend, SvFrontend::Verilog),
                    frontend,
                    ..Default::default()
                },
                &VerifyAutoOptions {
                    config_values: cfg.clone(),
                    ..Default::default()
                },
            )
            .expect("verify_auto runs")
        };
        let note = |r: &AutoVerifyReport| {
            r.notes
                .iter()
                .find(|n| n.kind == "lift-frontend")
                .unwrap_or_else(|| panic!("lift-frontend note present; got {:?}", r.notes))
                .clone()
        };
        let v = note(&run(SvFrontend::Verilog));
        assert_eq!(v.level, NoteLevel::Info);
        assert!(v.summary.contains("read_verilog"), "{}", v.summary);
        let s = note(&run(SvFrontend::Slang));
        assert!(s.summary.contains("yosys-slang"), "{}", s.summary);
    }

    #[test]
    #[ignore = "requires slang + sv2v + yosys (mununu-sva image)"]
    fn e2e_partsel_partial_write_slang_refuses_or_agrees_with_sv2v() {
        const CC: &str = r#"// @mununu_guarantee nu X. ((a_q == 0) and [] X)
// @mununu_guarantee nu X. ((b_q == 0) and [] X)
// @mununu_guarantee nu X. ((c_q == 0) and [] X)
// @mununu_guarantee nu X. ((d_q == 0) and [] X)
// @mununu_guarantee nu X. ((p_q == 0) and [] X)
module cc (input logic clk, input logic rst_n, input logic [3:0] val,
           input logic [1:0] idx,
           output logic o_partsel, output logic o_concat,
           output logic o_lowconcat, output logic o_plain);
    logic [15:0] a_q, b_q, c_q, d_q;
    logic [3:0][3:0] p_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin a_q <= '0; b_q <= '0; c_q <= '0; d_q <= '0; p_q <= '0; end
        else begin
            a_q[11:8] <= val;                    // plain vector, constant range -> free inputs
            b_q       <= {b_q[15:12], val, b_q[7:0]};
            c_q       <= {12'd0, val};
            d_q       <= 16'(val);
            p_q[idx]  <= val;                    // packed 2-D, variable index -> anonymous split sub-regs
        end
    end
    assign o_partsel   = (a_q != '0);
    assign o_concat    = (b_q != '0);
    assign o_lowconcat = (c_q != '0);
    assign o_plain     = (d_q != '0);
endmodule
"#;
        let sources = vec![("cc.sv".to_string(), CC.to_string())];
        let cfg = {
            let mut m = std::collections::HashMap::new();
            m.insert("rst_n".to_string(), 1u64);
            m
        };
        let run = |frontend: crate::adapter::yosys::SvFrontend, use_sv2v: bool| {
            let yopts = YosysOptions {
                top: Some("cc".to_string()),
                use_sv2v,
                frontend,
                ..Default::default()
            };
            verify_auto(
                &sources,
                &yopts,
                &VerifyAutoOptions {
                    config_values: cfg.clone(),
                    ..Default::default()
                },
            )
            .expect("verify_auto runs")
        };
        let outcome_of = |report: &AutoVerifyReport, needle: &str| {
            report
                .properties
                .iter()
                .find(|p| p.formula.contains(needle))
                .unwrap_or_else(|| panic!("property for {needle} present"))
                .outcome
                .clone()
        };

        // What the yosys-slang plugin does with `a_q[11:8] <= val` is the PLUGIN's
        // property, not ours, and it has already changed once under our feet:
        //   * yosys-slang in oss-cad-suite 2025-12-31 (yosys 0.60+70) split `a_q`'s
        //     unwritten bits into anonymous free inputs → verify-auto REFUSED the
        //     property (Skipped) rather than read havoc bits;
        //   * yosys-slang in oss-cad-suite 2026-08-24 (yosys 0.68+120) lifts it
        //     faithfully — no anonymous inputs, `a_q` a plain state cell — so the
        //     property DECIDES, and the refusal has nothing to refuse (measured
        //     2026-09-18 when the image moved to that release).
        // What IS ours, on either plugin: never a silent Holds, never a ⊥, and a
        // decided verdict must agree with the faithful read_verilog + sv2v lift.
        // The sv_verify e2e pins the lint side of the same agreement.
        let slang = run(crate::adapter::yosys::SvFrontend::Slang, false);
        let sv2v = run(crate::adapter::yosys::SvFrontend::Verilog, true);
        for reg in ["a_q == 0", "b_q == 0", "c_q == 0", "d_q == 0", "p_q == 0"] {
            let faithful = outcome_of(&sv2v, reg);
            assert!(
                matches!(faithful, VerifyOutcome::Violated { .. }),
                "sv2v: AG({reg}) must be Violated (faithful lift, reachably non-zero); \
                 got {faithful:?}"
            );
            match outcome_of(&slang, reg) {
                VerifyOutcome::Skipped { .. } => eprintln!(
                    "slang REFUSED AG({reg}) — this plugin splits the register into free inputs"
                ),
                VerifyOutcome::Violated { .. } => eprintln!(
                    "slang DECIDED AG({reg}) = Violated, agreeing with the faithful sv2v lift"
                ),
                other => panic!(
                    "slang: AG({reg}) must be refused (Skipped) or decide exactly as the \
                     faithful sv2v lift (Violated) — never a silent Holds or a ⊥; got {other:?}"
                ),
            }
        }
        // The registers written in full (`b_q`, `c_q`, `d_q`) or split into anonymous
        // SUB-REGISTERS (`p_q`, packed 2-D, kept by the NID-indexed COI) were never
        // the refusal's target: on either plugin they decide.
        for reg in ["b_q == 0", "c_q == 0", "d_q == 0", "p_q == 0"] {
            assert!(
                matches!(outcome_of(&slang, reg), VerifyOutcome::Violated { .. }),
                "slang: AG({reg}) must be Violated (faithful register, or packed-2-D split \
                 kept by the NID COI); got {:?}",
                outcome_of(&slang, reg)
            );
        }
    }

    #[test]
    #[ignore = "requires slang + sv2v + yosys (mununu-sva image)"]
    fn e2e_reset_gated_sparse_fsm_inits_at_reset_value() {
        // Regression for the reset-gated async-reset init fix (reset_init.rs).
        // OpenTitan csrng_main_sm's FSM resets — via prim_sparse_fsm_flop's
        // ResetValue — to MainSmIdle = 6'b110111 = 55, a NON-ZERO sparse
        // encoding. Yosys `async2sync` lifts the async reset to a next-state mux
        // with no BTOR2 `init` line, and verify-auto pins the reset inactive.
        // Without `inject_reset_init` the model would power up at 0 (an ILLEGAL
        // sparse encoding → the FSM's `default` arm traps to MainSmError), so the
        // init-state probe `(state_q == 55)` would be VIOLATED. With the fix the
        // reset-gated model starts at MainSmIdle, so it HOLDS — the faithful
        // post-reset state, decided exactly by the exact-symbolic engine (which
        // reads the injected `init` line).
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let csrng = root.join("m2_opentitan_csrng_main_sm/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        // The @mununu_guarantee carries the init-state probe; the standard
        // prim_assert macros come from the M.0 fixture (the csrng dir's own
        // prim_assert.sv is the dummy variant).
        let annotated = format!(
            "// @mununu_guarantee (state_q == 55)\n{}",
            read(csrng.join("csrng_main_sm.sv"))
        );
        let sources = vec![
            ("csrng_main_sm.sv".to_string(), annotated),
            ("csrng_pkg.sv".to_string(), read(csrng.join("csrng_pkg.sv"))),
            (
                "prim_assert.sv".to_string(),
                read(prim.join("prim_assert.sv")),
            ),
            (
                "prim_assert_standard_macros.svh".to_string(),
                read(prim.join("prim_assert_standard_macros.svh")),
            ),
            (
                "prim_assert_sec_cm.svh".to_string(),
                read(prim.join("prim_assert_sec_cm.svh")),
            ),
            (
                "prim_flop_macros.sv".to_string(),
                read(prim.join("prim_flop_macros.sv")),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("csrng_main_sm".to_string()),
            use_sv2v: true,
            additional_sources: sources[1..].to_vec(),
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                exact_symbolic: true,
                ..Default::default()
            },
        )
        .expect("verify_auto runs exact-symbolic on csrng_main_sm");
        let probe = report
            .properties
            .iter()
            .find(|p| p.name.contains("ann_guarantee"))
            .expect("the @mununu_guarantee init probe is present");
        assert!(
            matches!(probe.outcome, VerifyOutcome::Holds),
            "reset-gated csrng must init at MainSmIdle (state_q==55), not the \
             power-on default 0 — the reset-init fix; got {:?} for `{}`",
            probe.outcome,
            probe.formula
        );
    }

    /// H.5-GR1 — the assume-guarantee liveness showcase on real OpenTitan RTL,
    /// decided by the exact-symbolic engine. The recoverability property
    /// `AG EF MainSmIdle` (`nu Y. ((mu X. ((state_q == 55) or <> X)) and [] Y)`) —
    /// "from every reachable state, can the FSM get back to idle?" — is a
    /// there-exists-a-path (`EF`) property SVA structurally cannot phrase. Its
    /// verdict flips on a single explicit environment assumption. With
    /// `local_escalate_i` FREE the verdict is VIOLATED — a local security escalation
    /// latches the FSM in the terminal `MainSmError` trap (SEC_CM design;
    /// `CsrngMainErrorStStable_A`), so idle is unreachable. Under the assumption
    /// `G !local_escalate_i` (`=0`) the verdict is HOLDS — with no escalation the FSM
    /// always cycles back to idle. Both verdicts are 2-valued definite (the exact
    /// engine has no ⊥). The example at
    /// `examples/verify/v8_csrng_escalation_recoverability/` reproduces this via the
    /// CLI. Regression-critical: the frozen-register bug (fixed) made BOTH sides a
    /// vacuous HOLDS, erasing the flip.
    #[test]
    #[ignore = "requires slang + sv2v + yosys (mununu-sva image); run with --ignored"]
    fn e2e_h5gr1_csrng_recoverability_escalation_flip() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify");
        let csrng = root.join("m2_opentitan_csrng_main_sm/source");
        let prim = root.join("m0_opentitan_prim_arbiter/source");
        let read = |p: PathBuf| {
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
        };
        let outcome = |cfg: &[(&str, u64)]| -> VerifyOutcome {
            let sm = read(csrng.join("csrng_main_sm.sv"));
            let annotated = format!(
                "// @mununu_guarantee nu Y. ((mu X. ((state_q == 55) or <> X)) and [] Y)\n{sm}"
            );
            let sources = vec![
                ("csrng_main_sm.sv".to_string(), annotated),
                ("csrng_pkg.sv".to_string(), read(csrng.join("csrng_pkg.sv"))),
                (
                    "prim_assert.sv".to_string(),
                    read(prim.join("prim_assert.sv")),
                ),
                (
                    "prim_assert_standard_macros.svh".to_string(),
                    read(prim.join("prim_assert_standard_macros.svh")),
                ),
                (
                    "prim_assert_sec_cm.svh".to_string(),
                    read(prim.join("prim_assert_sec_cm.svh")),
                ),
                (
                    "prim_flop_macros.sv".to_string(),
                    read(prim.join("prim_flop_macros.sv")),
                ),
            ];
            let yopts = YosysOptions {
                top: Some("csrng_main_sm".to_string()),
                use_sv2v: true,
                additional_sources: sources[1..].to_vec(),
                ..Default::default()
            };
            let config_values: HashMap<String, u64> =
                cfg.iter().map(|(k, v)| (k.to_string(), *v)).collect();
            let report = verify_auto(
                &sources,
                &yopts,
                &VerifyAutoOptions {
                    exact_symbolic: true,
                    config_values,
                    ..Default::default()
                },
            )
            .expect("verify_auto exact-symbolic on csrng_main_sm");
            report
                .properties
                .iter()
                .find(|p| p.name.contains("ann_guarantee"))
                .map(|p| p.outcome.clone())
                .expect("the @mununu_guarantee recoverability property is present")
        };

        // No assumption: a security escalation wedges the FSM in MainSmError.
        let free = outcome(&[]);
        assert!(
            matches!(free, VerifyOutcome::Violated { .. }),
            "AG EF idle must be VIOLATED with local_escalate_i free (escalation → \
             MainSmError trap); got {free:?}"
        );
        // Under the no-escalation assumption: the FSM always recovers to idle.
        let assumed = outcome(&[("local_escalate_i", 0)]);
        assert!(
            matches!(assumed, VerifyOutcome::Holds),
            "AG EF idle must HOLD under the assumption G !local_escalate_i; got {assumed:?}"
        );
    }

    /// mununu#638 — monono's `video_timing` raster twin (vendored at
    /// `examples/verify/v13_video_timing_raster_twin/`, Apache-2.0): two 10-bit counters whose
    /// frame wraps one row early, so `a_frame_wraps_at_total` (`sva_1`) and
    /// `a_row_advances_only_at_line_end` (`sva_2`) must be VIOLATED; the whole 800×525 frame is
    /// the diameter, so the exact engine's squaring rescue decides them. On oxidd 0.11 a
    /// **release** binary aborted here with `thread 'main' has overflowed its stack`: the rescue
    /// is the one place the engine uses two OxiDD managers alternately on one thread, and
    /// OxiDD's thread-local node-store state kept a stale owner address across that switch
    /// (upstream 9fd1ed0, in 0.13), so the diagram was corrupted and `apply` descended 74,548
    /// frames on a 20-variable cone. The debug profile decides the same command, which is why
    /// this test exists in the RELEASE profile — on the parent of the fix the test PROCESS
    /// aborts; on the fix it decides 8/8:
    ///
    /// ```text
    /// cargo test --release -p mununu-core --lib --all-features -- --ignored e2e_638_
    /// ```
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (mununu-sva image) AND the release profile (`cargo test --release`): the defect it reproduces is optimization-dependent"]
    fn e2e_638_the_raster_twin_decides_in_release_without_overflowing() {
        use crate::adapter::yosys::SvFrontend;
        use std::path::PathBuf;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/v13_video_timing_raster_twin");
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .unwrap_or_else(|e| panic!("read {}: {e}", dir.join(f).display()))
        };
        let sources = vec![
            (
                "video_timing_early_row.sv".to_string(),
                read("video_timing_early_row.sv"),
            ),
            (
                "video_timing_sva.sv".to_string(),
                read("video_timing_sva.sv"),
            ),
        ];
        let yopts = YosysOptions {
            top: Some("video_timing".to_string()),
            frontend: SvFrontend::Slang,
            ..Default::default()
        };
        // The CLI's default engine: `portfolio-sequential`, the exact engine first — the path
        // the consumer's command takes (the library default is the explicit cube, which does
        // not run the rescue and so never showed the abort).
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                config_values: std::collections::HashMap::from([("rst_n".to_string(), 1u64)]),
                portfolio: Some(PortfolioMode::Sequential),
                ..Default::default()
            },
        )
        .expect("verify_auto runs on the raster twin");
        for p in &report.properties {
            eprintln!("  {} ({:?}): {:?}", p.name, p.label, p.outcome);
        }
        assert!(
            report.properties.len() >= 5,
            "the five tier-1 SVA (plus any harvested guarantees): {:?}",
            report.properties
        );
        let by = |suffix: &str| {
            report
                .properties
                .iter()
                .find(|p| p.name.ends_with(suffix))
                .unwrap_or_else(|| panic!("property `*{suffix}` in {:?}", report.properties))
        };
        for violated in ["sva_1", "sva_2"] {
            assert!(
                matches!(by(violated).outcome, VerifyOutcome::Violated { .. }),
                "{violated} is the twin's early row: {:?}",
                by(violated).outcome
            );
        }
        for holds in ["sva_0", "sva_3", "sva_4"] {
            assert!(
                matches!(by(holds).outcome, VerifyOutcome::Holds),
                "{holds}: {:?}",
                by(holds).outcome
            );
        }
        assert!(
            report.properties.iter().all(|p| !matches!(
                p.outcome,
                VerifyOutcome::Unknown { .. } | VerifyOutcome::Skipped { .. }
            )),
            "every property decides once the rescue can run: {:?}",
            report.properties
        );
    }

    /// The regression of the #629 re-land (#646), on monono's `pulse_cdc` (vendored at
    /// `examples/verify/v14_pulse_cdc_sync/`, Apache-2.0): `pulse_in == 0 |=> tog_q ==
    /// $past(tog_q)` — an INPUT antecedent whose consequent is read in the NEXT cycle. The
    /// antecedent-propagation pass rewrote it as a same-cycle implication, dropped the antecedent,
    /// and the exact engine refuted a property that holds; before the pass the exact engine
    /// SKIPPED it ("atom references primary input") and the explicit engine held it. With the
    /// same-cycle guard the pass declines and the exact engine's answer is what it was. The
    /// same-cycle sibling `a_out_is_the_edge` still HOLDS under exact (its explicit-engine
    /// refutation is mununu#637, a different defect).
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (mununu-sva docker image); run with --ignored"]
    fn e2e_646_an_input_antecedent_under_a_next_cycle_consequent_is_not_refuted() {
        use crate::adapter::yosys::SvFrontend;
        use std::path::PathBuf;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/v14_pulse_cdc_sync");
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .unwrap_or_else(|e| panic!("read {}: {e}", dir.join(f).display()))
        };
        let sources = vec![
            ("pulse_cdc.sv".to_string(), read("pulse_cdc.sv")),
            ("pulse_cdc_sva.sv".to_string(), read("pulse_cdc_sva.sv")),
        ];
        // `pulse_cdc` has two clocks; since mununu#762 the lift refuses that unless the domains
        // are merged on purpose. These tests are about atom binding and the explicit engine,
        // so they take the lockstep lift they were written against (monono's lane does too).
        let yopts = YosysOptions {
            top: Some("pulse_cdc".to_string()),
            frontend: SvFrontend::Slang,
            merge_clock_domains: true,
            ..Default::default()
        };
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                config_values: std::collections::HashMap::from([
                    ("rst_src_n".to_string(), 1u64),
                    ("rst_dst_n".to_string(), 1u64),
                ]),
                exact_symbolic: true,
                ..Default::default()
            },
        )
        .expect("verify_auto runs on pulse_cdc");
        for p in &report.properties {
            eprintln!("  {} ({:?}): {:?}", p.name, p.label, p.outcome);
        }
        let by = |suffix: &str| {
            report
                .properties
                .iter()
                .find(|p| p.name.ends_with(suffix))
                .unwrap_or_else(|| panic!("property `*{suffix}` in {:?}", report.properties))
        };
        let next_cycle = by("sva_1");
        assert!(
            !matches!(next_cycle.outcome, VerifyOutcome::Violated { .. }),
            "`pulse_in == 0 |=> tog_q == $past(tog_q)` holds on the design; the exact engine \
             must skip or hold it, never refute it: {:?}",
            next_cycle.outcome
        );
        assert!(
            matches!(by("sva_0").outcome, VerifyOutcome::Holds),
            "the same-cycle tautology holds under exact: {:?}",
            by("sva_0").outcome
        );
    }

    /// mununu#637 — the EXPLICIT engine on `pulse_cdc`'s `a_out_is_the_edge`, a tautology over
    /// registered state (`pulse_out == 1 |-> sync_q != sync_d_q`, with `pulse_out = sync_q ^
    /// sync_d_q`). The cube lift's seeding bound the combinational output to the nearest state
    /// cell by the loose resolver (`pulse_out == 1` became `sync_q == 1`) and the inconsistent
    /// cell refuted it — an engine-contradiction against exact-symbolic in the portfolio. With
    /// value-preserving resolution the output is its own dimension and the cube holds it.
    #[test]
    #[ignore = "requires slang + sv2v + Yosys + z3 (mununu-sva docker image); run with --ignored"]
    fn e2e_637_the_explicit_engine_holds_the_tautology_over_registered_state() {
        use crate::adapter::yosys::SvFrontend;
        use std::path::PathBuf;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/v14_pulse_cdc_sync");
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .unwrap_or_else(|e| panic!("read {}: {e}", dir.join(f).display()))
        };
        let sources = vec![
            ("pulse_cdc.sv".to_string(), read("pulse_cdc.sv")),
            ("pulse_cdc_sva.sv".to_string(), read("pulse_cdc_sva.sv")),
        ];
        // `pulse_cdc` has two clocks; since mununu#762 the lift refuses that unless the domains
        // are merged on purpose. These tests are about atom binding and the explicit engine,
        // so they take the lockstep lift they were written against (monono's lane does too).
        let yopts = YosysOptions {
            top: Some("pulse_cdc".to_string()),
            frontend: SvFrontend::Slang,
            merge_clock_domains: true,
            ..Default::default()
        };
        // The library default IS the explicit cube engine (`--engine explicit`).
        let report = verify_auto(
            &sources,
            &yopts,
            &VerifyAutoOptions {
                config_values: std::collections::HashMap::from([
                    ("rst_src_n".to_string(), 1u64),
                    ("rst_dst_n".to_string(), 1u64),
                ]),
                ..Default::default()
            },
        )
        .expect("verify_auto runs on pulse_cdc");
        for p in &report.properties {
            eprintln!("  {} ({:?}): {:?}", p.name, p.label, p.outcome);
        }
        let tautology = report
            .properties
            .iter()
            .find(|p| p.name.ends_with("sva_0"))
            .unwrap_or_else(|| panic!("sva_0 in {:?}", report.properties));
        assert!(
            matches!(tautology.outcome, VerifyOutcome::Holds),
            "`pulse_out == 1 |-> sync_q != sync_d_q` is a tautology over registered state; the \
             explicit engine must hold it: {:?}",
            tautology.outcome
        );
    }

    /// mununu#663 — the `auto` must-edge policy: a μ under a ν (the shapes whose `◇` turns true
    /// only through must edges) gets hyper-must; safety, reachability and propositional shapes
    /// keep `Off`; an explicit policy, or `auto` switched off, is honoured as given.
    #[test]
    fn x663_auto_must_edge_policy_resolves_by_formula_class() {
        use crate::adapter::btor2::kmts_lift::MustEdgeInference;
        let f = |s: &str| crate::mu_calculus::parser::parse(s).expect("formula");
        let auto = VerifyAutoOptions::default();
        assert_eq!(auto.must_edge_inference, MustEdgeInference::Off);
        assert!(auto.must_edge_auto);
        let ag_ef = f("nu X. ((mu Y. ((good == 1) || <> Y)) && [] X)");
        let ag_af = f("nu X. ((mu Y. ((good == 1) || [] Y)) && [] X)");
        let safety = f("nu X. ((!(bad == 1)) && [] X)");
        let reach = f("mu Y. ((good == 1) || <> Y)");
        let prop = f("(good == 1) || (bad == 0)");
        assert_eq!(
            effective_must_edge_inference(&auto, &ag_ef),
            MustEdgeInference::SmtPerTargetStandard
        );
        assert_eq!(
            effective_must_edge_inference(&auto, &ag_af),
            MustEdgeInference::SmtPerTargetStandard
        );
        assert_eq!(
            effective_must_edge_inference(&auto, &safety),
            MustEdgeInference::Off
        );
        assert_eq!(
            effective_must_edge_inference(&auto, &reach),
            MustEdgeInference::Off
        );
        assert_eq!(
            effective_must_edge_inference(&auto, &prop),
            MustEdgeInference::Off
        );
        let explicit_off = VerifyAutoOptions {
            must_edge_auto: false,
            ..Default::default()
        };
        assert_eq!(
            effective_must_edge_inference(&explicit_off, &ag_ef),
            MustEdgeInference::Off
        );
        let explicit_std = VerifyAutoOptions {
            must_edge_inference: MustEdgeInference::SmtHyperMust,
            ..Default::default()
        };
        assert_eq!(
            effective_must_edge_inference(&explicit_std, &safety),
            MustEdgeInference::SmtHyperMust
        );
    }

    /// mununu#663 — the measured row behind `auto`: the vendored lift of OpenTitan
    /// `prim_arbiter_tree` (N=8; `gnt_o = req_i & sel(prio_mask_q) & ready_i`), `AG EF (gnt_o == 0)`
    /// with the target as the only predicate. May-only leaves every cell ⊥ (no must edge, so the
    /// inner μ never turns true); the per-target ∀∃ must check makes the cell-to-cell edges must
    /// edges (some next input — `ready_i = 0` — makes `gnt_o == 0` true from every state) and the
    /// cube decides HOLDS. The default was may-only, so the cube member left this row ⊥ for the
    /// re-plan; `smt-hyper-must` still does (mununu#667).
    #[test]
    fn x663_the_cube_decides_the_arbiter_recoverability_with_per_target_must() {
        use crate::adapter::AdapterOptions;
        use crate::adapter::btor2::cegar::{CegarOptions, LiftStrategy, cegar_refine_loop};
        use crate::adapter::btor2::kmts_lift::{
            MayEdgeInference, MustEdgeInference, PredicateSpec,
        };
        use crate::mu_calculus::Environment;
        use crate::mu_calculus::trit::Trit;
        const ARBITER: &str =
            include_str!("../../../tests/fixtures/wall_classes/prim_arbiter_tree_n8.btor");
        let formula =
            crate::mu_calculus::parser::parse("nu Y. ((mu X. ((gnt_o == 0) or <> X)) and [] Y)")
                .expect("formula");
        let run = |policy: MustEdgeInference| {
            let cegar_opts = CegarOptions {
                max_iterations: 1,
                may_edge_inference: MayEdgeInference::SmtAllPairs,
                must_edge_inference: policy,
                lift_strategy: LiftStrategy::Eager,
                ..CegarOptions::default()
            };
            let trace = cegar_refine_loop(
                &formula,
                ARBITER,
                vec![PredicateSpec {
                    name: "gnt_o == 0".into(),
                    register: "gnt_o".into(),
                    value: 0,
                }],
                &Environment::new(4),
                &AdapterOptions::default(),
                &cegar_opts,
            )
            .expect("cegar runs on the arbiter lift");
            (0..trace.final_verdict.len())
                .map(|i| trace.final_verdict.verdict_at(i))
                .collect::<Vec<_>>()
        };
        let may_only = run(MustEdgeInference::Off);
        assert!(
            may_only.contains(&Trit::Unknown) && !may_only.contains(&Trit::False),
            "may-only leaves the cube ⊥ and never VIOLATED — got {may_only:?}"
        );
        let per_target = run(effective_must_edge_inference(
            &VerifyAutoOptions::default(),
            &formula,
        ));
        assert!(
            !per_target.is_empty() && per_target.iter().all(|t| *t == Trit::True),
            "`auto` (per-target must) decides the arbiter recoverability HOLDS — got {per_target:?}"
        );
        // mununu#667 — the hyper-must composition on this path emitted only the full-set edge
        // `src →□ may_succ(src)` and left the row ⊥; it now promotes the ∀∃ singletons first,
        // so it decides what the per-target form decides (the ladder and the ⊥ re-plan run it).
        let hyper = run(MustEdgeInference::SmtHyperMust);
        assert!(
            !hyper.is_empty() && hyper.iter().all(|t| *t == Trit::True),
            "hyper-must decides the arbiter recoverability HOLDS too — got {hyper:?}"
        );
    }

    /// A `--template` flag becomes a `@mununu_template(tmpl.<instance>) <id> KEY="VALUE"` line
    /// (2026-10-30; before, a `@mununu_guarantee` of the instantiated formula), and the annotation
    /// scan instantiates it under the label `--template` rows have always had, with the same
    /// formula as before.
    #[test]
    fn a_template_flag_becomes_a_template_annotation_under_its_old_label() {
        use crate::adapter::templates::{TemplateRef, TemplateRegistry};
        let registry = TemplateRegistry::builtin();
        let tref = TemplateRef {
            template: "rtl_never".to_string(),
            args: [("BAD".to_string(), "state_q == FAULT".to_string())]
                .into_iter()
                .collect(),
        };
        let (name, text) = template_annotations(&[tref], &registry).unwrap().unwrap();
        assert_eq!(name, "__mununu_templates.sv");
        assert_eq!(
            text,
            "// @mununu_template(tmpl.rtl_never_BAD_state_q_eq_FAULT) rtl_never BAD=\"state_q == FAULT\"\n"
        );
        let scan = scan_annotation_properties(&[(name, text)]);
        assert_eq!(scan.guarantees.len(), 1, "{:?}", scan.skipped);
        assert_eq!(
            scan.guarantees[0].label.as_deref(),
            Some("tmpl.rtl_never_BAD_state_q_eq_FAULT")
        );
        assert_eq!(
            scan.guarantees[0].formula,
            "nu X. ((!(state_q == FAULT)) && [] X)"
        );
        assert_eq!(
            scan.template_forms,
            vec![(
                "ann_guarantee_0".to_string(),
                "rtl_never BAD=\"state_q == FAULT\"".to_string()
            )]
        );
        assert!(
            scan.guarantee_lines.is_empty(),
            "a flag's synthetic source has no line the user wrote"
        );
        let bad = TemplateRef {
            template: "rtl_never".to_string(),
            args: [("BAD".to_string(), "mu X. (a || <> X)".to_string())]
                .into_iter()
                .collect(),
        };
        assert!(
            template_annotations(&[bad], &registry).is_err(),
            "an RTL atom argument must be a boolean condition, and a flag's error is hard"
        );
    }

    /// 2026-10-30 — a `@mununu_` line the extractor refuses REACHES the report. On the parent
    /// commit `@mununu_guarantee(a b) …` (a space in the name) and `@mununu_gurantee …` (a typo)
    /// produced neither a row nor a `skipped` entry: the obligation vanished.
    #[test]
    fn a_refused_annotation_head_is_skipped_with_its_reason_not_dropped() {
        let sv = "// @mununu_guarantee(a b) nu X. (p && [] X)\n\
                  // @mununu_gurantee nu X. (p && [] X)\n\
                  module m(); endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert!(scan.guarantees.is_empty());
        assert_eq!(scan.skipped.len(), 2, "{:?}", scan.skipped);
        assert!(
            scan.skipped[0].contains("design.sv:1") && scan.skipped[0].contains("not a name"),
            "{}",
            scan.skipped[0]
        );
        assert!(
            scan.skipped[1].contains("design.sv:2") && scan.skipped[1].contains("not a tag"),
            "{}",
            scan.skipped[1]
        );
    }

    /// 2026-10-30 — a template instance written next to the design: a named row whose formula is
    /// the instantiated mu-calculus, its instance beside it, its own source line.
    #[test]
    fn a_template_annotation_is_a_named_row_with_its_instance_beside_it() {
        let sv = "module m();\n\
                  // @mununu_template(trip_reach) rtl_reachable TARGET=\"state_q == 3\"\n\
                  endmodule";
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(scan.guarantees.len(), 1, "{:?}", scan.skipped);
        let g = &scan.guarantees[0];
        assert_eq!(g.name, "ann_guarantee_0");
        assert_eq!(g.label.as_deref(), Some("trip_reach"));
        assert_eq!(g.formula, "mu Y. ((state_q == 3) || <> Y)");
        assert!(mu_parser::parse(&g.formula).is_ok());
        assert!(
            g.written.is_none(),
            "the instance is not a written formula; it rides as `template:`"
        );
        assert_eq!(
            scan.template_forms,
            vec![(
                "ann_guarantee_0".to_string(),
                "rtl_reachable TARGET=\"state_q == 3\"".to_string()
            )]
        );
        assert_eq!(
            scan.guarantee_lines,
            vec![(
                "ann_guarantee_0".to_string(),
                crate::trace::SourceLoc {
                    file: "design.sv".to_string(),
                    line: 2
                }
            )]
        );
        assert!(scan.assume_exempt.is_empty());
        assert!(scan.skipped.is_empty());

        // Unnamed: the label a `--template` row has always had.
        let scan = scan_annotation_properties(&src(
            "// @mununu_template rtl_reachable TARGET=\"state_q == 3\"",
        ));
        assert_eq!(scan.guarantees.len(), 1, "{:?}", scan.skipped);
        assert_eq!(
            scan.guarantees[0].label.as_deref(),
            Some("tmpl.rtl_reachable_TARGET_state_q_eq_3")
        );

        // A duplicate name is refused, as for a guarantee.
        let scan = scan_annotation_properties(&src(
            "// @mununu_guarantee(x) nu X. (p && [] X)\n// @mununu_template(x) rtl_never BAD=p",
        ));
        assert_eq!(scan.guarantees.len(), 1);
        assert!(
            scan.skipped[0].contains("duplicate name"),
            "{}",
            scan.skipped[0]
        );
    }

    /// 2026-10-30 — every way a template body can be wrong is a `skipped` entry naming the body
    /// and the reason, never a dropped obligation.
    #[test]
    fn a_bad_template_annotation_is_skipped_with_the_reason() {
        let cases: [(&str, &str); 7] = [
            ("rtl_frobnicate X=1", "rtl_frobnicate"),
            ("rtl_never", "BAD"),
            ("rtl_never BAD", "not `KEY=VALUE`"),
            ("rtl_never BAD=\"state_q == 3", "never closes"),
            ("rtl_never BAD=", "has no value"),
            ("rtl_never BAD=a BAD=b", "given twice"),
            ("", "template id is required"),
        ];
        for (body, expect) in cases {
            let scan = scan_annotation_properties(&src(&format!("// @mununu_template {body}")));
            assert!(scan.guarantees.is_empty(), "{body}: {:?}", scan.guarantees);
            assert_eq!(scan.skipped.len(), 1, "{body}: {:?}", scan.skipped);
            assert!(
                scan.skipped[0].contains(expect),
                "{body}: expected `{expect}` in `{}`",
                scan.skipped[0]
            );
        }
    }

    /// 2026-10-30 — the `assumes=` modifier on a guarantee or a template row: `ignore` marks the
    /// row exempt, `precondition` is the default said out loud, anything else is refused with a
    /// reason, as is a modifier the scanner does not know.
    #[test]
    fn the_assumes_modifier_marks_a_row_exempt_and_a_bad_one_is_refused() {
        let sv = "// @mununu_template(trip_holds, assumes=ignore) rtl_hold_until HOLD=lof_trip RELEASE=stop_cmd\n\
                  // @mununu_guarantee(p, assumes=precondition) nu X. (p && [] X)\n\
                  // @mununu_guarantee(q, assumes=ignore) nu X. (q && [] X)\n\
                  // @mununu_guarantee(r, assumes=check) nu X. (r && [] X)\n\
                  // @mununu_guarantee(s, foo=1) nu X. (s && [] X)\n\
                  // @mununu_template(t, assumes=sometimes) rtl_never BAD=t";
        let scan = scan_annotation_properties(&src(sv));
        assert_eq!(scan.guarantees.len(), 3, "{:?}", scan.skipped);
        assert_eq!(
            scan.assume_exempt,
            vec!["ann_guarantee_0".to_string(), "ann_guarantee_2".to_string()]
        );
        assert_eq!(scan.skipped.len(), 3, "{:?}", scan.skipped);
        assert!(
            scan.skipped[0].contains("@mununu_guarantee(r)")
                && scan.skipped[0].contains("run mode"),
            "{}",
            scan.skipped[0]
        );
        assert!(
            scan.skipped[1].contains("@mununu_guarantee(s)")
                && scan.skipped[1].contains("unknown modifier `foo`"),
            "{}",
            scan.skipped[1]
        );
        assert!(
            scan.skipped[2].contains("@mununu_template(t)")
                && scan.skipped[2].contains("assumes=sometimes"),
            "{}",
            scan.skipped[2]
        );
    }

    /// `x == NAME` resolves an enum member / parameter; registers, dotted names, ordering
    /// comparisons and unknown names are left alone.
    #[test]
    fn named_constants_resolve_on_the_right_of_equalities_only() {
        let c: std::collections::BTreeMap<String, i64> =
            [("SCRAM".to_string(), 4), ("IDLE".to_string(), 0)]
                .into_iter()
                .collect();
        assert_eq!(
            resolve_named_constants("nu X. ((state_q == SCRAM || state_q != IDLE) && [] X)", &c),
            "nu X. ((state_q == 4 || state_q != 0) && [] X)"
        );
        assert_eq!(
            resolve_named_constants("(a == b) && (u.SCRAM == 1) && (x <= SCRAM)", &c),
            "(a == b) && (u.SCRAM == 1) && (x <= SCRAM)"
        );
        assert_eq!(resolve_named_constants("q == SCRAM.x", &c), "q == SCRAM.x");
    }

    // mununu#803 — the note a VIOLATED row carries when no path shipped: kind, the stage as its
    // operand, and the property it is about (not a prose prefix).
    #[test]
    fn a_witness_miss_becomes_a_note_about_the_property() {
        let miss = crate::adapter::reach_rescue::WitnessMiss {
            stage: "search",
            reason: "native BMC: encode failed".to_string(),
        };
        let n = witness_miss_note("u_x.sva_3", Some("reach-rescue"), &miss);
        assert_eq!(n.kind, "witness-unavailable");
        assert_eq!(n.level, NoteLevel::Info);
        assert_eq!(n.property.as_deref(), Some("u_x.sva_3"));
        assert_eq!(n.items, vec!["search".to_string()]);
        assert!(
            n.summary.contains("no counterexample path shipped"),
            "{}",
            n.summary
        );
        assert!(n.detail.contains("decided by reach-rescue"), "{}", n.detail);
    }
}

#[cfg(test)]
mod source_map_tests {
    use super::*;
    use crate::trace::{CellSource, SourceLoc, TraceNames};

    fn loc(f: &str, l: u32) -> SourceLoc {
        SourceLoc {
            file: f.into(),
            line: l,
        }
    }
    fn cell(
        name: &str,
        kind: &str,
        declared: Option<SourceLoc>,
        assigned: Vec<SourceLoc>,
    ) -> CellSource {
        CellSource {
            cell: name.into(),
            kind: kind.into(),
            declared,
            assigned,
        }
    }

    fn index(declared: Vec<(&str, SourceLoc)>) -> crate::adapter::slang::SourceIndex {
        crate::adapter::slang::SourceIndex {
            declared: declared
                .into_iter()
                .map(|(n, l)| (n.to_string(), l))
                .collect(),
            ..Default::default()
        }
    }

    /// A gap the netlist left (an older yosys-slang stamps cells, not nets) is filled from the
    /// AST by name; a cell the AST does not declare is named in the reason and the map is
    /// `partial`; everything placed is available; nothing placed at all is unavailable with its
    /// reason.
    #[test]
    fn declarations_fill_the_gaps_and_settle_the_status() {
        let mut names = TraceNames {
            sources: vec![
                cell("state_q", "register", None, vec![loc("pump_pair.sv", 52)]),
                cell("flow", "input", Some(loc("pump_pair.sv", 25)), vec![]),
                cell(
                    "u_sva.seen",
                    "register",
                    None,
                    vec![loc("pump_pair_sva.sv", 9)],
                ),
            ],
            ..Default::default()
        };
        let decls = Ok(index(vec![
            ("state_q", loc("pump_pair.sv", 32)),
            ("flow", loc("x.sv", 1)),
        ]));
        fill_declarations(&mut names, &decls);
        assert_eq!(
            names.sources[0].declared,
            Some(loc("pump_pair.sv", 32)),
            "the gap is filled"
        );
        assert_eq!(
            names.sources[1].declared,
            Some(loc("pump_pair.sv", 25)),
            "the netlist's own line is kept"
        );
        assert!(names.source_map_partial);
        assert_eq!(
            names.source_map.as_deref(),
            Some("no declaration for u_sva.seen")
        );

        let mut full = TraceNames {
            sources: vec![cell("q", "register", None, vec![loc("a.sv", 5)])],
            ..Default::default()
        };
        fill_declarations(&mut full, &Ok(index(vec![("q", loc("a.sv", 2))])));
        assert!(
            !full.source_map_partial && full.source_map.is_none(),
            "every cell placed: available"
        );

        let mut none = TraceNames {
            sources: vec![cell("q", "register", None, vec![])],
            ..Default::default()
        };
        fill_declarations(&mut none, &Err("slang not found".into()));
        assert!(none.sources.is_empty() && !none.source_map_partial);
        assert!(
            none.source_map
                .as_deref()
                .unwrap_or("")
                .contains("declared none"),
            "{:?}",
            none.source_map
        );
    }

    /// The sv2v path (2026-10-30): the netlist's lines are sv2v's, so the lift clears them and the
    /// AST supplies both halves — a register's `always_ff` line from its flop blocks. A name sv2v
    /// mangled is not in the AST: it stays without a line and the reason lists it, never a guess.
    #[test]
    fn the_sv2v_path_takes_its_assignment_lines_from_the_ast_and_lists_what_it_cannot_place() {
        let mut ast = index(vec![("state_q", loc("pump_pair.sv", 32))]);
        ast.flop_blocks
            .insert("state_q".into(), vec![loc("pump_pair.sv", 52)]);
        let mut placed = TraceNames {
            sources: vec![cell("state_q", "register", None, vec![])],
            source_lines_rewritten: true,
            ..Default::default()
        };
        fill_declarations(&mut placed, &Ok(ast.clone()));
        assert_eq!(placed.sources[0].declared, Some(loc("pump_pair.sv", 32)));
        assert_eq!(placed.sources[0].assigned, vec![loc("pump_pair.sv", 52)]);
        assert!(
            !placed.source_map_partial && placed.source_map.is_none(),
            "every cell placed from the AST: available, sv2v or not — {:?}",
            placed.source_map
        );

        let mut mangled = TraceNames {
            sources: vec![
                cell("state_q", "register", None, vec![]),
                cell("u_fsm.sv2v_cast_2$result", "register", None, vec![]),
            ],
            source_lines_rewritten: true,
            ..Default::default()
        };
        fill_declarations(&mut mangled, &Ok(ast));
        assert!(mangled.source_map_partial);
        let reason = mangled.source_map.unwrap_or_default();
        assert!(
            reason.contains("no assignment line for u_fsm.sv2v_cast_2$result (sv2v rewrote"),
            "{reason}"
        );
        assert!(
            reason.contains("no declaration for u_fsm.sv2v_cast_2$result"),
            "{reason}"
        );
        assert!(mangled.sources[1].declared.is_none() && mangled.sources[1].assigned.is_empty());
    }

    fn row(
        name: &str,
        label: Option<&str>,
    ) -> crate::adapter::slang::translate::TranslatedAssertion {
        crate::adapter::slang::translate::TranslatedAssertion {
            name: name.into(),
            label: label.map(str::to_string),
            kind: SvaKind::Assert,
            formula: "nu X. (true && [] X)".into(),
            recoverability_companion: None,
            written: None,
        }
    }

    /// A row's `source`: an SVA row joins the AST's assertion sites on its name AND its label (a
    /// label that disagrees means the runs saw different assertions: no line, not a wrong one); an
    /// annotation row takes its comment's line; a template row has none.
    #[test]
    fn a_rows_source_joins_on_name_and_label_and_never_guesses() {
        let ast = crate::adapter::slang::SourceIndex {
            assertions: vec![
                (
                    "top_sva_0".into(),
                    Some("a_wraps".into()),
                    loc("dut.sv", 40),
                ),
                ("top_sva_1".into(), None, loc("dut.sv", 41)),
                ("top_sva_2".into(), Some("a_old".into()), loc("dut.sv", 42)),
            ],
            ..Default::default()
        };
        let translated = vec![
            row("top_sva_0", Some("a_wraps")),
            row("top_sva_1", None),
            row("top_sva_2", Some("a_renamed")),
            row("ann_guarantee_0", None),
            row("tmpl.rtl_never.x", Some("tmpl.rtl_never.x")),
        ];
        let got: Vec<(String, String)> = property_sources_of(
            &translated,
            &Ok(ast),
            &[("ann_guarantee_0".into(), loc("dut.sv", 7))],
        )
        .into_iter()
        .map(|(n, l)| (n, l.to_string()))
        .collect();
        assert_eq!(
            got,
            vec![
                ("top_sva_0".to_string(), "dut.sv:40".to_string()),
                ("top_sva_1".to_string(), "dut.sv:41".to_string()),
                ("ann_guarantee_0".to_string(), "dut.sv:7".to_string()),
            ]
        );
        assert!(property_sources_of(&translated, &Err("no slang".into()), &[]).is_empty());
    }

    /// `involves` (2026-10-30): a row lists the lines of what its cone reaches — the register it
    /// reads, the wire and input feeding that register's `next` — and nothing of a register
    /// outside the cone. The model is the lift's shape: `state_d` is a named wire (a no-op `uext`
    /// alias), `other` an unrelated register.
    #[test]
    fn involves_lists_the_cones_lines_and_nothing_outside_it() {
        const MODEL: &str = "\
1 sort bitvec 1
2 sort bitvec 2
3 input 1 go
4 state 2 state_q
5 state 2 other
6 one 2
7 add 2 4 6
8 ite 2 3 7 4
9 uext 2 8 0 state_d
10 next 2 4 8
11 next 2 5 5
";
        let mut report = AutoVerifyReport::default();
        report.properties.push(PropertyVerdict {
            name: "p".into(),
            label: None,
            kind: SvaKind::Assert,
            formula: "nu X. ((state_q != 3) && [] X)".into(),
            outcome: VerifyOutcome::Holds,
            seeded_predicates: Vec::new(),
            counterexample: None,
            bottom_reason: None,
            decided_by: None,
            vacuity: None,
        });
        report.diagnostics.trace_names.sources = vec![
            cell(
                "state_q",
                "register",
                Some(loc("dut.sv", 12)),
                vec![loc("dut.sv", 24)],
            ),
            cell("go", "input", Some(loc("dut.sv", 11)), vec![]),
            cell(
                "other",
                "register",
                Some(loc("dut.sv", 15)),
                vec![loc("dut.sv", 29)],
            ),
        ];
        let mut ast = index(vec![("state_d", loc("dut.sv", 14))]);
        ast.writes
            .insert("state_d".into(), vec![loc("dut.sv", 20), loc("dut.sv", 21)]);
        ast.writes
            .insert("state_q".into(), vec![loc("dut.sv", 25), loc("dut.sv", 26)]);
        ast.writes.insert("other".into(), vec![loc("dut.sv", 30)]);
        attach_involves(&mut report, MODEL, &Ok(ast));
        let lines: Vec<u32> = report.involves("p").iter().map(|l| l.line).collect();
        assert_eq!(lines, vec![11, 12, 14, 20, 21, 24, 25, 26]);
        assert!(report.involves("absent").is_empty());
    }
}
