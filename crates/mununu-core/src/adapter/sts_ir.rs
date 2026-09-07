//! Symbolic Transition System IR — the frontend-agnostic abstraction seam.
//!
//! > Status (as-built; 2026-07-05 AR audit, updated 2026-07-06): the canonical
//! > seam, **partially adopted**. Design: `docs/design/sts-ir.md`. This module defines
//! > the *interface* the abstraction engines need from a symbolic transition system,
//! > plus one implementation ([`BtorSts`]) wrapping a parsed BTOR2 file. **Live call
//! > sites:** the eager predicate-cube lift routes its opt-in `SmtAllPairs`
//! > may/must/hyper-must edges + `combinational_labels` + register-name resolution
//! > through this seam (`kmts_lift.rs`); the exact engine uses it for
//! > `resolve_register` **and (AR-S1) for its leaf-cell enumeration + naming**
//! > (`BddBitBlaster::build_with_keep` reads [`BtorSts::leaf_cells`], all three
//! > cell-naming sites share the one [`parser::resolve_leaf_symbol`]); **and (AR-S2)
//! > the default cube path's no-UF concrete step ([`StepEval::step`]) + its canonical
//! > ∀∃ KMTS must post-pass ([`SmtEncode::must_edges_over`]).** **Still bypassing the
//! > seam:** the default cube path's *UF-representative* step
//! > (`simulate_one_step_with_uf_rep`) and its ∀∀ / hyper-set must-checks
//! > (`smt_must_edge::*`, the `SmtPerTarget` / `SmtHyperMust` post-passes), and the
//! > symbolic BDD engine's `abstract_relation` (a different algorithm — reads
//! > `Btor2File` directly). So the seam is canonical + faithful; the "single
//! > de-duplicated predicate image" goal is closer — the ∀∃ must-image is now single
//! > ([`SmtEncode::must_edges_over`], shared by the eager all-pairs and the lazy
//! > sampling-candidate consumers); the ∀∀ / hyper-must / sampling-may images remain
//! > (#242 was a symptom of two symbol-resolution paths drifting; **AR-S1 collapsed
//! > the exact engine's copy of that class, AR-S2 the ∀∃ must copy**).
//! >
//! > **Adoption is now scheduled, not blocked.** The per-impl differential harness the
//! > AR review gated full seam adoption on has shipped
//! > (`crates/mununu-core/tests/differential_oracle_e2e.rs` —
//! > `diff_corpus_cegar_vs_symbolic_engine_parity` hard-gates every cube definite
//! > against the exact oracle), so the roll-in is a graduated, differential-guarded
//! > sequence (roadmap Track AR §AR-S): **S1 ✅ (2026-07-06)** converged the exact
//! > engine's infra (leaf var-enumeration + cell naming) through [`BtorSts::leaf_cells`]
//! > + [`parser::resolve_leaf_symbol`] — the #242 drift class is dead by construction;
//! > **S2 ◑ (2026-07-06)** routed the default cube path's no-UF step through
//! > [`StepEval`] and its canonical ∀∃ must through [`SmtEncode::must_edges_over`]
//! > (the ∀∀ / hyper-set must-checks + the UF-representative step remain — follow-up);
//! > **S3** (optional) the literal single image. Note the BDD engines'
//! > `abstract_relation` is a *different* algorithm (symbolic BDD, not SMT
//! > per-cube-pair), so only their infra converges — they never fold into
//! > `may_edges`.
//!
//! The seam is two traits over a shared metadata trait:
//!
//! - [`StepEval`] — concrete one-step semantics. The *explicit /
//!   Enumerate* edge-strategy (today's `bit_blast`) needs only this.
//! - [`SmtEncode`] — SMT predicate-image. The *predicate-cube / SmtImage*
//!   edge-strategy (today's `predicate_cube_lift`) needs this.
//!
//! Neither trait surface names a BTOR2 or Z3 type: state/input structure
//! is [`StsVar`] (name + width); the concrete step is name-keyed
//! `HashMap<String, u128>`; the SMT predicate-image is expressed over
//! [`PredicateSpec`](crate::adapter::btor2::kmts_lift::PredicateSpec) (a
//! frontend-agnostic `{name, register, value}`) and
//! returns plain cube-index pairs. A future non-RTL frontend that can
//! emit these two semantics inherits both abstraction policies + the
//! KMTS evaluator + CEGAR with no new abstraction code (the P4/P5 goal).

use std::collections::HashMap;

use crate::adapter::AdapterError;
use crate::adapter::btor2::ast::{Btor2File, Nid, Node};
use crate::adapter::btor2::smt_must_edge::PredicateLike;
use crate::adapter::btor2::{bit_blast, parser};

/// A typed state or input variable of a symbolic transition system.
/// Frontend-agnostic: just a name and a bit width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StsVar {
    /// The variable's symbol (e.g. a register name `u_chan0.prediv_q`).
    pub name: String,
    /// Bit width.
    pub width: u32,
}

/// A single BTOR2 leaf cell (state register or free input) with its
/// NID-level identity — the *concrete*, frontend-specific enumeration the
/// bit-blaster needs, one level below the agnostic [`StsVar`] view.
///
/// AR-S1: this is the **single** BTOR2 leaf enumeration + naming. [`StsVar`]
/// ([`SymbolicTransitionSystem::state_vars`] / [`input_vars`]) is the
/// name-and-width-only agnostic projection of it; the exact bit-blaster
/// ([`BddBitBlaster::build_with_keep`]) consumes the full cell (NID + width +
/// `is_state`) directly. Both share the one naming function
/// ([`parser::resolve_leaf_symbol`]), so the #242 drift class cannot recur.
///
/// [`input_vars`]: SymbolicTransitionSystem::input_vars
/// [`BddBitBlaster::build_with_keep`]: crate::adapter::btor2::symbolic_bitblast::BddBitBlaster::build_with_keep
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafCell {
    /// The defining line's NID (identity for `env` binding, keep-set
    /// membership, the flat BDD-variable slice, and `next`/`init` keying).
    pub nid: Nid,
    /// Canonical user-visible symbol, resolved once via
    /// [`parser::resolve_leaf_symbol`] (alias-aware, `{tag}_{nid}` for a truly
    /// anonymous cell).
    pub name: String,
    /// Whether the cell carried a real resolved symbol (`true`) or fell back
    /// to the synthetic `{tag}_{nid}` name (`false`). The agnostic
    /// [`StsVar`] projection surfaces only `named` cells.
    pub named: bool,
    /// `true` for a `state` register, `false` for a free `input`.
    pub is_state: bool,
    /// Bit width.
    pub width: u32,
}

/// Structure of a symbolic transition system — the metadata both
/// abstraction engines need before they can lift.
pub trait SymbolicTransitionSystem {
    /// State variables (registers / latches), sorted by name.
    fn state_vars(&self) -> Vec<StsVar>;
    /// Free input variables, sorted by name.
    fn input_vars(&self) -> Vec<StsVar>;
    /// P0 (IR-unification track) — resolve a user-facing register name
    /// (which may be an *alias*, e.g. the `_q` flavour Yosys leaves on a
    /// `uext` node after flatten strips the symbol from the state line)
    /// to the **canonical state-cell name** that the SMT predicate-image
    /// binds against (the `_d` symbol the SMT view keys on). Returns
    /// `None` when the name resolves to no state cell, or the resolved
    /// cell carries no symbol.
    ///
    /// This is the architectural home of the DR1 #1 blocker fix: the
    /// predicate-cube path can use this so a sidecar predicate over
    /// `bit_cnt_q` binds to the real counter register even when the
    /// state line is labelled `bit_cnt_d`. The `bit_blast`/sidecar path
    /// already does the equivalent BFS via `resolve_state_by_symbol` +
    /// the `drives` override; surfacing it here lets the cube path share
    /// the same resolution (P1 wires it into `predicate_cube_lift`).
    fn resolve_register(&self, name: &str) -> Option<String>;
}

/// P1 #4 Phase 1 (IR-unification track, Q2 = B) — the structured result of
/// an observability-rich step. Everything the *explicit / Enumerate*
/// edge-strategy (`enumerate_and_blast`) needs from one concrete step,
/// WITHOUT the trait knowing anything about the abstraction model
/// (`FieldDomain` / OOB sink stay caller-side policy).
#[derive(Debug, Clone, Default)]
pub struct StepOutcome {
    /// Next state assignment, keyed by [`StsVar::name`].
    pub next_state: HashMap<String, u128>,
    /// Current-cycle value of each requested observable signal name
    /// (combinational signals, ports, registers). Names that resolve to
    /// no signal are omitted — callers treat absence as "not observable
    /// in this design".
    pub observed: HashMap<String, u128>,
    /// Whether every constraint of the system holds under this
    /// `(state, inputs)` assignment. Lets the Enumerate strategy drop
    /// inadmissible input combinations without re-opening the frontend.
    pub admissible: bool,
}

/// Concrete one-step semantics: given a full assignment of state vars +
/// inputs, compute the next state assignment. This is everything the
/// *explicit / Enumerate* edge-strategy (today's `bit_blast`) needs to
/// build a Sharp CLTS.
pub trait StepEval: SymbolicTransitionSystem {
    /// P1 #4 Phase 1 (Q2 = B) — the **observability-rich** one-step
    /// primitive. Steps the design one clock and additionally reports the
    /// current-cycle value of each requested `observe` signal plus whether
    /// the step's constraints held. Both `state` / `inputs` maps are keyed
    /// by [`StsVar::name`]; absent keys default to 0 (`setundef -zero`).
    ///
    /// This is the contract the Enumerate strategy consumes: it supplies
    /// the concrete next-state (for the caller's domain-encoding / OOB
    /// policy), the property/combinational signal values (for state
    /// splitting), and the admissibility verdict (for constraint
    /// filtering) — all without the trait referencing the abstraction
    /// model. A non-RTL frontend that can answer these inherits the
    /// Enumerate strategy with mununu's abstraction policy layered on top.
    fn step_observe(
        &self,
        state: &HashMap<String, u128>,
        inputs: &HashMap<String, u128>,
        observe: &[String],
    ) -> Result<StepOutcome, AdapterError>;

    /// The narrow `(state, inputs) ↦ next_state` step. Both maps are keyed
    /// by [`StsVar::name`]; absent keys default to 0 (the `setundef -zero`
    /// convention). Provided in terms of [`StepEval::step_observe`] with no
    /// observed signals; the admissibility verdict is discarded (the
    /// next-state is returned regardless, matching the pre-Phase-1 contract
    /// the predicate-cube sampling path relies on).
    fn step(
        &self,
        state: &HashMap<String, u128>,
        inputs: &HashMap<String, u128>,
    ) -> Result<HashMap<String, u128>, AdapterError> {
        Ok(self.step_observe(state, inputs, &[])?.next_state)
    }
}

/// SMT predicate-image: the *predicate-cube / SmtImage* edge-strategy
/// (today's `predicate_cube_lift`). Batched per the Z3-scope-reuse
/// consideration in `docs/design/sts-ir.md` §"Z3 scope".
pub trait SmtEncode: SymbolicTransitionSystem {
    /// Sound over-approximating may-relation over predicate cubes.
    ///
    /// Cubes are indexed `0..2^|predicates|`; bit `i` of a cube index is
    /// the polarity of `predicates[i]` (identical encoding to
    /// `predicate_cube_lift`). Returns every `(src, tgt)` pair for which
    /// a concrete witness `∃ s ⊨ src, s' ⊨ tgt. (s, s') ∈ R` exists — a
    /// pair is **excluded only when the SMT backend proves it
    /// impossible**, so the relation is a sound over-approximation
    /// (timeouts / unresolved predicates conservatively keep the edge).
    ///
    /// The must-relation (`∀∀` / `∀∃` / hyper-must) follows the same
    /// shape over the same encoding (`smt_must_edge::smt_per_target_must_*`);
    /// DR0 ships only the may-relation to prove the seam.
    fn may_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)>;

    /// P1 #3 (IR-unification track) — sound under-approximating
    /// **must-relation** over predicate cubes, the canonical ∀∃ KMTS
    /// must per Bruns–Godefroid CONCUR 2000:
    ///
    /// ```text
    /// (src, tgt) ∈ R_must  ⟺  ∀ s ⊨ src. ∃ inputs, s'. (s, s') ∈ R ∧ s' ⊨ tgt
    /// ```
    ///
    /// Same cube encoding as [`SmtEncode::may_edges`] (indices
    /// `0..2^|predicates|`). A pair is **included only when the SMT
    /// backend proves the ∀∃ obligation** (UNSAT of its negation), so the
    /// relation is a sound under-approximation: timeouts / unresolved
    /// predicates / encoder failure conservatively *drop* the edge (never
    /// fabricate a must-witness). By construction `R_must ⊆ R_may` (∀∃ ⟹
    /// ∃), so callers can promote each returned pair from `MayOnly` to
    /// `Sharp`.
    ///
    /// The ∀∃ *standard* form is the default companion to `may_edges`;
    /// the stricter ∀∀ form and the generalised hyper-must form remain
    /// available directly via `smt_must_edge::smt_per_target_must_check`
    /// / `smt_hyper_must_check` on the sampling-candidate path.
    fn must_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)>;

    /// AR-S2 — the ∀∃ must-relation restricted to an explicit `candidates`
    /// list of `(src, tgt)` cube-index pairs, sharing ONE build (`view` +
    /// primed node cache + nid-map) across all candidates. Same soundness as
    /// [`must_edges`](SmtEncode::must_edges) — a pair is returned only on a
    /// proved ∀∃ obligation (UNSAT of its negation); timeouts / encoder
    /// failure drop it.
    ///
    /// This is the single predicate-image both the eager all-pairs consumer
    /// and the *lazy* sampling-candidate consumer share:
    /// [`must_edges`](SmtEncode::must_edges) is exactly `must_edges_over`
    /// applied to the full `0..2^p × 0..2^p` grid, while the default cube
    /// path's `SmtPerTargetStandard` post-pass applies it to only the
    /// **sampled** candidate targets — so laziness is preserved without a
    /// second must-image implementation. Returned pairs are a subset of
    /// `candidates`, in `candidates` order.
    fn must_edges_over<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        candidates: &[(usize, usize)],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)>;

    /// Generalised **GKMTS hyper-must** relation. For each source cube,
    /// the set of targets `T` (its **may-successor set**, from `may_edges`)
    /// such that
    ///
    /// ```text
    /// (src, T) ∈ R_hyper-must  ⟺  ∀ s ⊨ src. ∃ inputs, s'. (s, s') ∈ R ∧ ∃ t ∈ T. s' ⊨ t
    /// ```
    ///
    /// A pair is included only when the SMT backend proves the obligation
    /// (UNSAT of its negation) — sound under-approximation: NotMust /
    /// Unknown / encoder failure drop the edge (never fabricate a witness).
    ///
    /// Unlike [`SmtEncode::must_edges`] (the ∀∃ standard per-target must,
    /// which is **non-monotone** under refinement on alternating
    /// fixpoints), the hyper-must form is **monotone** (Shoham–Grumberg
    /// LMCS 2007 §4), so a definite νμ verdict over a KMTS carrying these
    /// `MustHyperOnly` edges is sound *under refinement*. The SmtAllPairs /
    /// compound lift uses this when `MustEdgeInference::SmtHyperMust` is
    /// requested, giving compound-predicate recoverability (νμ) verdicts
    /// that are clean-sound rather than soundness-tagged.
    ///
    /// Default candidate `T` = the full may-successor set (the coarsest
    /// sound hyper-must; tightening `T` is a future refinement).
    fn hyper_must_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        may_edges: &[(usize, usize)],
        timeout_ms: u32,
    ) -> Vec<(usize, Vec<usize>)>;

    /// H.E.2 — per-cube labels for **derived combinational predicates** (Approach
    /// B). `cube_predicates` are the cube *dimensions* (state + free inputs,
    /// indices `0..2^|cube_predicates|`); `derived` are the combinational
    /// predicates to label (their `register()` names a combinational node, NOT a
    /// dimension). Returns `(cube_index, derived_index, label)` for every (cube,
    /// derived) pair: KleeneT/F where the cube pins the signal, KleeneBot where it
    /// doesn't (or on encoder failure / timeout — the conservative, honest
    /// label). The lift writes these into `state_3valued_predicates` so the
    /// evaluator binds the formula atom by name.
    fn combinational_labels<P: PredicateLike + Sync>(
        &self,
        cube_predicates: &[P],
        derived: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize, crate::clts::Tristate)>;
}

/// The BTOR2 implementation of the STS-IR seam — a thin borrow over a
/// parsed [`Btor2File`]. Every method delegates to an already-shipped
/// function, so DR0 changes no behaviour and rewires no call site.
/// A GKMTS hyper-must relation: per source cube, the may-successor target set it must
/// reach. Aliased to keep the session-cache signatures readable (clippy type-complexity).
pub(crate) type HyperMustEdges = Vec<(usize, Vec<usize>)>;

pub struct BtorSts<'a> {
    file: &'a Btor2File,
    /// Lazy, memoized structural derivations of `file` (Phase 0.a). The may / must /
    /// hyper-must edge methods share ONE instance, so the theory selection
    /// (`detect_btor2_memories`) runs once per `BtorSts`, not once per `encode_*` call.
    facts: crate::adapter::btor2::model_facts::ModelFacts<'a>,
}

impl<'a> BtorSts<'a> {
    /// Wrap a parsed BTOR2 file as a symbolic transition system.
    pub fn new(file: &'a Btor2File) -> Self {
        Self {
            file,
            facts: crate::adapter::btor2::model_facts::ModelFacts::new(file),
        }
    }

    /// Phase 0.c (session cache) — compute the may-relation AND the GKMTS hyper-must
    /// relation in ONE z3 scope, sharing a SINGLE encoded view + primed node cache +
    /// nid-map across both. Today [`SmtEncode::may_edges`] and
    /// [`SmtEncode::hyper_must_edges`] each open their own `with_z3_config` scope and
    /// re-encode the transition relation; this builds it once.
    ///
    /// SOUNDNESS (scope-lived projection, ownership/lifetime): z3 0.20 uses a
    /// thread-local `Context` that `with_z3_config` installs for the closure and drops
    /// on return, so a `Btor2SmtView`'s z3 handles are valid exactly while that
    /// `Context` is alive. The view is a **non-escaping local** here — it never leaves
    /// the closure, so its handles cannot outlive the `Context`. That nesting is what
    /// makes the shared projection sound (z3-0.20 handles carry no lifetime parameter,
    /// so a long-lived cache would compile yet dangle; a scope-local one cannot).
    ///
    /// Behaviour-identical to `may_edges` followed by `hyper_must_edges(&may)`: the same
    /// view, primed cache, nid-map, and per-pair checks — only built once. Verified by
    /// the differential edge suites (`sts_ir` / `kmts_lift`).
    pub(crate) fn may_and_hyper_must_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        timeout_ms: u32,
    ) -> (Vec<(usize, usize)>, HyperMustEdges) {
        use crate::adapter::btor2::smt_must_edge::{
            SmtMayVerdict, SmtMustVerdict, build_register_nid_map_with_inputs,
            smt_hyper_must_check_uniform, smt_per_target_may_check_uniform,
        };
        use crate::adapter::sidecar::predicate_image::btor2_encode::encode_primed;

        // mununu#503 — no empty-predicate early return. At |P| = 0 there is ONE cube (the
        // universal set) and the loops below reduce to a single 0 -> 0 check. Returning no
        // edges here left that cube edgeless, which `downgrade_unsatisfiable_cells` masks to ⊥
        // — silently making every derived-label-only property Unknown. See the note in
        // `predicate_cube_lift`'s SmtAllPairs block.
        let n_cubes = 1usize << predicates.len();
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            // ONE encode + ONE primed cache + ONE nid-map, shared by both relations.
            let view = match self.facts.encode() {
                Ok(v) => v,
                Err(_) => return (Vec::new(), Vec::new()),
            };
            let primed = match encode_primed(self.file, &view) {
                Ok(p) => p,
                Err(_) => return (Vec::new(), Vec::new()),
            };
            let nid_map = build_register_nid_map_with_inputs(&view);

            // may-relation (all-pairs) — identical to `may_edges`.
            let mut may: Vec<(usize, usize)> = Vec::new();
            for i in 0..n_cubes {
                for j in 0..n_cubes {
                    if matches!(
                        smt_per_target_may_check_uniform(
                            &view, &primed, i as u64, j as u64, predicates, &nid_map, timeout_ms,
                        ),
                        SmtMayVerdict::May
                    ) {
                        may.push((i, j));
                    }
                }
            }

            // hyper-must over each source's may-successor set — identical to
            // `hyper_must_edges(&may)`.
            let mut may_succ: Vec<Vec<usize>> = vec![Vec::new(); n_cubes];
            for &(i, j) in &may {
                if i < n_cubes && j < n_cubes {
                    may_succ[i].push(j);
                }
            }
            for v in &mut may_succ {
                v.sort_unstable();
                v.dedup();
            }
            let mut hyper: Vec<(usize, Vec<usize>)> = Vec::new();
            for (src, targets) in may_succ.iter().enumerate() {
                if targets.is_empty() {
                    continue;
                }
                let target_bits_set: Vec<u64> = targets.iter().map(|&t| t as u64).collect();
                if matches!(
                    smt_hyper_must_check_uniform(
                        &view,
                        &primed,
                        src as u64,
                        &target_bits_set,
                        predicates,
                        &nid_map,
                        timeout_ms,
                    ),
                    SmtMustVerdict::Must
                ) {
                    hyper.push((src, targets.clone()));
                }
            }
            (may, hyper)
        })
    }

    /// NIDs of `state` lines that carry a `next` function.
    ///
    /// SOUNDNESS (mununu#498): a BTOR2 `state` with **no** `next` line is
    /// unconstrained at every step — the format's free variable. Yosys emits
    /// exactly that for `$anyseq`, which is what a `cutpoint` leaves behind.
    /// Modelling it as a *held register* instead pins it to its (absent, hence
    /// zero) init and freezes it, turning the abstraction into an
    /// UNDER-approximation precisely where a cut point documents an
    /// over-approximation — a freed net that can never be 1 removes behaviours
    /// rather than adding them. So such a state is classified as an INPUT here,
    /// which is free-each-step and quantifiable, and every consumer of this seam
    /// inherits the correct semantics.
    ///
    /// `$anyconst` is a different cell and is already right: Yosys emits it as
    /// `next(s) = s`, so it has a `next` line and stays a state (held, as
    /// intended).
    fn states_with_next(&self) -> std::collections::HashSet<Nid> {
        self.file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                Node::Next { state, .. } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn vars_of(&self, want_state: bool) -> Vec<StsVar> {
        let symbols = parser::collect_symbols(self.file);
        let with_next = self.states_with_next();
        let mut out: Vec<StsVar> = self
            .file
            .lines
            .iter()
            .filter_map(|line| {
                let (sort, is_state) = match &line.node {
                    // A `state` with no `next` is a free variable, not a
                    // register — see `states_with_next`.
                    Node::State { sort, .. } => (*sort, with_next.contains(&line.nid)),
                    Node::Input { sort, .. } => (*sort, false),
                    _ => return None,
                };
                if is_state != want_state {
                    return None;
                }
                let name = symbols.get(&line.nid)?.clone();
                let width = parser::bv_width(self.file, sort)?;
                Some(StsVar { name, width })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out.dedup();
        out
    }

    /// AR-S1 — the ONE canonical BTOR2 leaf enumeration, in file order. Every
    /// `state`/`input` cell (including anonymous ones, named `{tag}_{nid}`),
    /// each carrying its NID, resolved symbol, `named` flag, and width. The
    /// exact bit-blaster ([`BddBitBlaster::build_with_keep`]) consumes this
    /// directly — it no longer re-walks `file.lines` or re-implements cell
    /// naming, so its enumeration and the seam's cannot drift.
    ///
    /// Naming goes through the single [`parser::resolve_leaf_symbol`]; width
    /// through [`parser::bv_width`]. Errors (matching the bit-blaster's
    /// existing contract) when a `state`/`input` cell has a non-bit-vector
    /// (e.g. array/memory) sort — the caller degrades that design to
    /// `Skipped` rather than bit-blasting it. This is deliberately stricter
    /// than the agnostic [`Self::vars_of`] projection, which tolerates a
    /// non-bitvec cell by dropping just that cell (the cube path can still
    /// reason over the scalar registers of a design with memory arrays).
    ///
    /// [`BddBitBlaster::build_with_keep`]: crate::adapter::btor2::symbolic_bitblast::BddBitBlaster::build_with_keep
    pub fn leaf_cells(&self) -> Result<Vec<LeafCell>, String> {
        let symbols = parser::collect_symbols(self.file);
        let with_next = self.states_with_next();
        let mut out = Vec::new();
        for line in &self.file.lines {
            let (sort, raw, is_state, tag) = match &line.node {
                // A `state` with no `next` is a free variable, not a register —
                // see `states_with_next`. It keeps the "state" naming tag so its
                // symbol resolution is unchanged; only its CLASSIFICATION moves.
                Node::State { sort, symbol } => {
                    (*sort, symbol, with_next.contains(&line.nid), "state")
                }
                Node::Input { sort, symbol } => (*sort, symbol, false, "input"),
                _ => continue,
            };
            let width = parser::bv_width(self.file, sort)
                .ok_or_else(|| format!("NID {}: {tag} has non-bitvec sort", line.nid))?;
            out.push(LeafCell {
                nid: line.nid,
                named: symbols.contains_key(&line.nid),
                name: parser::resolve_leaf_symbol(&symbols, line.nid, raw, tag),
                is_state,
                width,
            });
        }
        Ok(out)
    }
}

impl SymbolicTransitionSystem for BtorSts<'_> {
    fn state_vars(&self) -> Vec<StsVar> {
        self.vars_of(true)
    }

    fn input_vars(&self) -> Vec<StsVar> {
        self.vars_of(false)
    }

    fn resolve_register(&self, name: &str) -> Option<String> {
        // AR-GO-1 — the LOOSE resolution ("nearest state in the signal's cone"), made
        // explicit. BFS-backward from any node carrying `name` (direct state match, or an
        // Op / Output alias) to the nearest state cell, then return that cell's own symbol
        // — the name the SMT view + predicate-image bind against. Mirrors the sidecar
        // resolver's `drives` path.
        parser::resolve_to_canonical_name(self.file, name, parser::ResolveStrictness::Loose)
    }
}

impl StepEval for BtorSts<'_> {
    fn step_observe(
        &self,
        state: &HashMap<String, u128>,
        inputs: &HashMap<String, u128>,
        observe: &[String],
    ) -> Result<StepOutcome, AdapterError> {
        // Delegate to the shipped observability-rich evaluator (it returns
        // `StepOutcome` directly). `step` (the narrow next-state-only form)
        // is the provided default over this, so it keeps delegating to the
        // same concrete-step logic.
        bit_blast::simulate_one_step_observe(self.file, state, inputs, observe)
    }
}

impl SmtEncode for BtorSts<'_> {
    fn may_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)> {
        use crate::adapter::btor2::smt_must_edge::{
            SmtMayVerdict, build_register_nid_map_with_inputs, smt_per_target_may_check_uniform,
        };
        use crate::adapter::sidecar::predicate_image::btor2_encode::encode_primed;

        // mununu#503 — no empty-predicate early return: at |P| = 0 the single universal cube
        // still has a self-loop to compute, and returning no edges left it edgeless (⇒ masked
        // to ⊥ by `downgrade_unsatisfiable_cells`).
        // P0 — faithful, memory-aware encode (array theory for `$mem`
        // cells), matching the production `predicate_cube_lift`. (DR0 used
        // the BvOnly `encode_design`.)
        let n_cubes = 1usize << predicates.len();
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = match self.facts.encode() {
                Ok(v) => v,
                // Encoder can't build the view (e.g. an unsupported op):
                // no may-edges rather than an unsound guess. Mirrors the
                // predicate_cube_lift fallback.
                Err(_) => return Vec::new(),
            };
            // H.U.1a — the uniform predicate-image. Build the next-cycle node
            // cache (`encode_primed`: every node re-evaluated over `(s', i')`)
            // ONCE, then run the uniform may-check (`term over (s,i)` source /
            // `(s', i')` target) per pair. May-EQUIVALENT to the per-kind
            // `smt_per_target_may_check` on state / input / compound predicates
            // (a fresh `i'` target ≡ the per-kind target-free under the `∃`
            // may-query); only combinational-of-state targets gain precision,
            // and none are routed as cube dimensions yet. Primed-encode failure
            // → no edges (sound, mirrors the encode fallback).
            let primed = match encode_primed(self.file, &view) {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            // H.B — map inputs too, so a free-input predicate resolves. State
            // symbols take precedence on a name collision, so state-only
            // predicate sets get the identical map + identical edges.
            let nid_map = build_register_nid_map_with_inputs(&view);
            let mut edges = Vec::new();
            for i in 0..n_cubes {
                // mununu#504 — SOUNDNESS, and the direction here is the OPPOSITE of the must
                // side. `may` OVER-approximates: correctness needs may ⊇ concrete. So when the
                // budget expires we SATURATE — every remaining pair becomes a may-edge. A denser
                // may relation means more ⊥, never a wrong verdict. DROPPING the remaining pairs
                // (the intuitive "stop early") would UNDER-approximate and could manufacture a
                // spurious definite `True` for a box property, which is a wrong answer rather
                // than a slow one.
                if crate::adapter::run_budget::expired() {
                    for si in i..n_cubes {
                        for sj in 0..n_cubes {
                            edges.push((si, sj));
                        }
                    }
                    break;
                }
                for j in 0..n_cubes {
                    if matches!(
                        smt_per_target_may_check_uniform(
                            &view,
                            &primed,
                            i as u64,
                            j as u64,
                            predicates,
                            &nid_map,
                            crate::adapter::run_budget::clamp_query_ms(timeout_ms),
                        ),
                        SmtMayVerdict::May
                    ) {
                        edges.push((i, j));
                    }
                }
            }
            edges.sort_unstable();
            edges.dedup();
            edges
        })
    }

    fn must_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)> {
        // mununu#503 — no empty-predicate early return: at |P| = 0 the single universal cube
        // still has a self-loop to compute, and returning no edges left it edgeless (⇒ masked
        // to ⊥ by `downgrade_unsatisfiable_cells`).
        // The all-pairs ∀∃ must is exactly `must_edges_over` applied to the
        // full `0..2^p × 0..2^p` grid — one image, two candidate shapes.
        let n_cubes = 1usize << predicates.len();
        let all_pairs: Vec<(usize, usize)> = (0..n_cubes)
            .flat_map(|i| (0..n_cubes).map(move |j| (i, j)))
            .collect();
        self.must_edges_over(predicates, &all_pairs, timeout_ms)
    }

    fn must_edges_over<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        candidates: &[(usize, usize)],
        timeout_ms: u32,
    ) -> Vec<(usize, usize)> {
        use crate::adapter::btor2::smt_must_edge::{
            SmtMustVerdict, build_register_nid_map_with_inputs, smt_per_target_must_check_uniform,
        };
        use crate::adapter::sidecar::predicate_image::btor2_encode::encode_primed;

        // mununu#503 — the `predicates.is_empty()` half of this guard is gone (see the sibling
        // note above); `candidates.is_empty()` stays, since no candidate targets means no work.
        if candidates.is_empty() {
            return Vec::new();
        }
        // H.U.1c — same encode + the next-cycle node cache (`encode_primed`)
        // built ONCE, then the uniform ∀∃ must-check per candidate pair
        // (`smt_per_target_must_check_uniform`). BEHAVIOUR-IDENTICAL to the
        // per-kind `smt_per_target_must_check_standard` on every existing cube
        // dimension (state target = `state_next`; free-input target = free; state
        // compound leaves = `state_next`) — the uniform builder produces the same
        // Z3 terms. Keeps a pair only on a definite `Must` (UNSAT); NotMust /
        // Unknown / encoder failure drop it (sound under-approximation).
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = match self.facts.encode() {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            let primed = match encode_primed(self.file, &view) {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            let nid_map = build_register_nid_map_with_inputs(&view);
            let mut edges = Vec::new();
            for &(i, j) in candidates {
                // mununu#504 — SOUNDNESS, and note this is the MIRROR IMAGE of `may_edges`.
                // `must` UNDER-approximates: an edge is included only on a PROVED ∀∃ obligation.
                // So on expiry we DROP the remaining candidates and stop. Fewer must-edges means
                // a weaker must relation, hence more ⊥ — never a wrong verdict. SATURATING here
                // (the may-side move) would assert obligations nobody proved, which is unsound.
                // This also matches the contract already documented on this method: a timeout
                // "conservatively drops the edge".
                if crate::adapter::run_budget::expired() {
                    break;
                }
                if matches!(
                    smt_per_target_must_check_uniform(
                        &view,
                        &primed,
                        i as u64,
                        j as u64,
                        predicates,
                        &nid_map,
                        crate::adapter::run_budget::clamp_query_ms(timeout_ms),
                    ),
                    SmtMustVerdict::Must
                ) {
                    edges.push((i, j));
                }
            }
            edges
        })
    }

    fn hyper_must_edges<P: PredicateLike + Sync>(
        &self,
        predicates: &[P],
        may_edges: &[(usize, usize)],
        timeout_ms: u32,
    ) -> Vec<(usize, Vec<usize>)> {
        use crate::adapter::btor2::smt_must_edge::{
            SmtMustVerdict, build_register_nid_map_with_inputs, smt_hyper_must_check_uniform,
        };
        use crate::adapter::sidecar::predicate_image::btor2_encode::encode_primed;

        // mununu#503 — no empty-predicate early return: at |P| = 0 the single universal cube
        // still has a self-loop to compute, and returning no edges left it edgeless (⇒ masked
        // to ⊥ by `downgrade_unsatisfiable_cells`).
        // Candidate hyper-must target set per source = its may-successor
        // set (sorted, deduped). A source with no may-successors gets no
        // hyper-must edge (an empty T is trivially NotMust).
        let n_cubes = 1usize << predicates.len();
        let mut may_succ: Vec<Vec<usize>> = vec![Vec::new(); n_cubes];
        for &(i, j) in may_edges {
            if i < n_cubes && j < n_cubes {
                may_succ[i].push(j);
            }
        }
        for v in &mut may_succ {
            v.sort_unstable();
            v.dedup();
        }

        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = match self.facts.encode() {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            // H.U.1c — uniform hyper-must over the next-cycle node cache (built
            // once); behaviour-identical to the per-kind `smt_hyper_must_check`
            // on existing cube dimensions.
            let primed = match encode_primed(self.file, &view) {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            // H.B — inputs mapped too (state precedence on collision).
            let nid_map = build_register_nid_map_with_inputs(&view);
            let mut edges = Vec::new();
            for (src, targets) in may_succ.iter().enumerate() {
                if targets.is_empty() {
                    continue;
                }
                let target_bits_set: Vec<u64> = targets.iter().map(|&t| t as u64).collect();
                if matches!(
                    smt_hyper_must_check_uniform(
                        &view,
                        &primed,
                        src as u64,
                        &target_bits_set,
                        predicates,
                        &nid_map,
                        timeout_ms,
                    ),
                    SmtMustVerdict::Must
                ) {
                    edges.push((src, targets.clone()));
                }
            }
            edges
        })
    }

    fn combinational_labels<P: PredicateLike + Sync>(
        &self,
        cube_predicates: &[P],
        derived: &[P],
        timeout_ms: u32,
    ) -> Vec<(usize, usize, crate::clts::Tristate)> {
        use crate::adapter::btor2::smt_must_edge::{
            build_register_nid_map_with_inputs, smt_combinational_label,
        };
        use crate::clts::Tristate;

        if derived.is_empty() {
            return Vec::new();
        }
        let n_cubes = 1usize << cube_predicates.len();
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = match self.facts.encode() {
                Ok(v) => v,
                // Encoder failure → every derived predicate is conservatively
                // KleeneBot in every cube (honest "couldn't determine").
                Err(_) => {
                    let mut out = Vec::with_capacity(n_cubes * derived.len());
                    for i in 0..n_cubes {
                        for d in 0..derived.len() {
                            out.push((i, d, Tristate::KleeneBot));
                        }
                    }
                    return out;
                }
            };
            let nid_map = build_register_nid_map_with_inputs(&view);
            // Each derived predicate (a combinational-of-input atom OR an H.F
            // relational over input/combinational operands) is labelled per cube
            // by `smt_combinational_label`, which resolves its operands through
            // `nid_map` (state ∪ inputs ∪ combinational) + the uniform source
            // lookup. No separate combinational-NID resolution is needed.
            let mut out = Vec::with_capacity(n_cubes * derived.len());
            for i in 0..n_cubes {
                for (d_idx, d) in derived.iter().enumerate() {
                    let label = smt_combinational_label(
                        &view,
                        i as u64,
                        cube_predicates,
                        &nid_map,
                        d,
                        timeout_ms,
                    );
                    out.push((i, d_idx, label));
                }
            }
            out
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::kmts_lift::PredicateSpec;

    // A 1-bit register `q` with `q' = en`, plus an input `en`. Exercises
    // both the state/input metadata and the concrete-step delegation.
    const STEP_BTOR2: &str =
        "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 input 1 en\n6 next 1 3 5\n";

    // P0 — a 4-bit state cell labelled `cnt_d` on the state line, with the
    // `cnt_q` flavour surviving only on a `uext` alias. This mirrors the
    // uart_tx pattern where Yosys' flatten strips the `_q` symbol from the
    // state line (the DR1 #1 blocker).
    const ALIASED_BTOR2: &str = "1 sort bitvec 4\n2 zero 4\n3 state 4 cnt_d\n4 init 4 3 2\n5 uext 4 3 0 cnt_q\n6 next 4 3 2\n";

    /// mununu#504 — the two budget degradations point in OPPOSITE directions, and getting
    /// either backwards is an unsoundness rather than a slowdown. This test asserts both in one
    /// place so a future change cannot quietly flip one to match the other.
    ///
    /// - `may` OVER-approximates (may ⊇ concrete) ⇒ on expiry **saturate**. Dropping edges would
    ///   under-approximate and could manufacture a spurious definite `True` for a box property.
    /// - `must` UNDER-approximates (only proved ∀∃ obligations) ⇒ on expiry **drop**. Saturating
    ///   would assert obligations nobody proved.
    ///
    /// Forced with an already-expired budget, so it is deterministic and runs in milliseconds.
    #[test]
    fn budget_expiry_saturates_may_and_drops_must() {
        use crate::adapter::btor2::kmts_lift::PredicateSpec;
        use crate::adapter::run_budget::{Budget, enter};
        // `q' = q` — a held register, so the TRUE may-relation is strictly sparser than the full
        // grid (cube 0 reaches only cube 0). Without that, saturation would be indistinguishable
        // from the correct answer and this test would prove nothing.
        const B: &str = "1 sort bitvec 1\n2 state 1 q\n3 zero 1\n4 init 1 2 3\n5 next 1 2 2\n";
        let file = parser::parse(B).expect("parse");
        let sts = BtorSts::new(&file);
        let preds = vec![PredicateSpec {
            name: "q == 1".into(),
            register: "q".into(),
            value: 1,
        }];

        // Control first: unbounded, the real relations.
        let may_real = sts.may_edges(&preds, 5_000);
        let must_real = sts.must_edges_over(&preds, &[(0, 0), (1, 1)], 5_000);
        assert!(
            may_real.len() < 4,
            "the TRUE may relation must be sparser than the full grid, else saturation is \
             untestable on this fixture; got {may_real:?}"
        );

        let t0 = std::time::Instant::now();
        let (may_starved, must_starved) = {
            let _g = enter(&Budget::with_ms(0));
            (
                sts.may_edges(&preds, 5_000),
                sts.must_edges_over(&preds, &[(0, 0), (1, 1)], 5_000),
            )
        };
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "an expired budget must bail fast, took {:?}",
            t0.elapsed()
        );

        assert_eq!(
            may_starved,
            vec![(0, 0), (0, 1), (1, 0), (1, 1)],
            "MAY must SATURATE to the full grid on expiry (over-approximation); a sparser set \
             here is an under-approximated may relation, which is UNSOUND"
        );
        assert!(
            must_starved.is_empty(),
            "MUST must DROP on expiry (under-approximation); a non-empty set here asserts \
             obligations nobody proved, which is UNSOUND. got {must_starved:?}"
        );
        assert!(
            !must_real.is_empty(),
            "the unstarved must relation is non-empty, so the emptiness above is the BUDGET and \
             not a degenerate fixture"
        );
    }

    #[test]
    fn btor_sts_resolve_register_follows_uext_alias_to_state_cell() {
        // The DR1 #1 blocker fix, homed in the seam: a predicate over the
        // alias `cnt_q` resolves to the canonical state-cell name `cnt_d`.
        let file = parser::parse(ALIASED_BTOR2).expect("parse");
        let sts = BtorSts::new(&file);
        assert_eq!(sts.resolve_register("cnt_q").as_deref(), Some("cnt_d"));
        // The canonical name resolves to itself.
        assert_eq!(sts.resolve_register("cnt_d").as_deref(), Some("cnt_d"));
        // A name matching no state cell does not resolve.
        assert_eq!(sts.resolve_register("nonexistent"), None);
    }

    // P1 #4 Phase 1 (Q2 = B) — `q' = en`, combinational `g = q & en`,
    // and `en` constrained high. Exercises the seam's observability-rich
    // step + the provided narrow `step` default.
    const OBSERVE_BTOR2: &str =
        "1 sort bitvec 1\n2 state 1 q\n3 input 1 en\n4 and 1 2 3 g\n5 next 1 2 3\n6 constraint 3\n";

    #[test]
    fn btor_sts_step_observe_reports_observed_and_admissible() {
        let file = parser::parse(OBSERVE_BTOR2).expect("parse");
        let sts = BtorSts::new(&file);
        let st = HashMap::from([("q".to_string(), 1u128)]);

        // en=1 → g=1, q'=1, admissible.
        let out = sts
            .step_observe(
                &st,
                &HashMap::from([("en".to_string(), 1u128)]),
                &["g".to_string()],
            )
            .expect("step_observe");
        assert_eq!(out.next_state.get("q"), Some(&1));
        assert_eq!(out.observed.get("g"), Some(&1));
        assert!(out.admissible);

        // en=0 → g=0, q'=0, NOT admissible (constraint violated).
        let out0 = sts
            .step_observe(
                &st,
                &HashMap::from([("en".to_string(), 0u128)]),
                &["g".to_string()],
            )
            .expect("step_observe");
        assert_eq!(out0.observed.get("g"), Some(&0));
        assert!(!out0.admissible);

        // The provided narrow `step` default returns just the next-state,
        // identical to step_observe's, discarding observability/admissibility.
        let next = sts
            .step(&st, &HashMap::from([("en".to_string(), 1u128)]))
            .expect("step");
        assert_eq!(
            next, out.next_state,
            "provided `step` matches step_observe's next-state"
        );
    }

    #[test]
    fn btor_sts_reports_state_and_input_vars() {
        let file = parser::parse(STEP_BTOR2).expect("parse");
        let sts = BtorSts::new(&file);
        assert_eq!(
            sts.state_vars(),
            vec![StsVar {
                name: "q".into(),
                width: 1
            }]
        );
        assert_eq!(
            sts.input_vars(),
            vec![StsVar {
                name: "en".into(),
                width: 1
            }]
        );
    }

    // An anonymous 1-bit state cell (no symbol on the state line, no alias) —
    // the truly-unnamed case that must fall back to `state_<nid>` / `named=false`.
    const ANON_STATE_BTOR2: &str =
        "1 sort bitvec 1\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 next 1 3 2\n";

    #[test]
    fn btor_sts_leaf_cells_is_the_canonical_enumeration() {
        // AR-S1 — `leaf_cells` is the ONE enumeration the exact bit-blaster reads.
        // It carries every state/input in file order with its NID + resolved name.
        let file = parser::parse(STEP_BTOR2).expect("parse");
        let cells = BtorSts::new(&file).leaf_cells().expect("leaf_cells");
        assert_eq!(
            cells,
            vec![
                LeafCell {
                    nid: 3,
                    name: "q".into(),
                    named: true,
                    is_state: true,
                    width: 1,
                },
                LeafCell {
                    nid: 5,
                    name: "en".into(),
                    named: true,
                    is_state: false,
                    width: 1,
                },
            ],
            "state cell (nid 3) precedes the input (nid 5) in file order"
        );

        // The agnostic `state_vars`/`input_vars` projection must agree cell-for-cell
        // with the named leaf cells — the two views cannot drift (the #242 class).
        let sts = BtorSts::new(&file);
        let named_states: Vec<StsVar> = cells
            .iter()
            .filter(|c| c.is_state && c.named)
            .map(|c| StsVar {
                name: c.name.clone(),
                width: c.width,
            })
            .collect();
        assert_eq!(sts.state_vars(), named_states);
    }

    // The real flattened-yosys shape: a 4-bit state cell with NO symbol on the
    // state line, its user-visible name (`cnt_q`) surviving only on a
    // width-matched `uext` alias. `collect_symbols` walks the alias back to the
    // state cell; `leaf_cells` must surface that resolved name — the exact name
    // the bit-blaster's `next_funcs`/`init` key against (the DR1 #1 / #242 path).
    const ALIASED_ANON_BTOR2: &str =
        "1 sort bitvec 4\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 uext 1 3 0 cnt_q\n6 next 1 3 2\n";

    #[test]
    fn btor_sts_leaf_cells_resolves_uext_alias_name() {
        let file = parser::parse(ALIASED_ANON_BTOR2).expect("parse");
        let cells = BtorSts::new(&file).leaf_cells().expect("leaf_cells");
        let state: Vec<_> = cells.iter().filter(|c| c.is_state).collect();
        assert_eq!(state.len(), 1);
        assert_eq!(
            state[0].name, "cnt_q",
            "the state cell is named by its width-matched uext alias, not `state_3`"
        );
        assert!(state[0].named);
        assert_eq!(state[0].width, 4);
    }

    #[test]
    fn btor_sts_leaf_cells_names_anonymous_cell_by_nid() {
        // A cell with no symbol and no alias falls back to `state_<nid>` and is
        // flagged `named = false` — so the agnostic `state_vars` view (named-only)
        // omits it, while the bit-blaster still allocates a frame slot for it.
        let file = parser::parse(ANON_STATE_BTOR2).expect("parse");
        let sts = BtorSts::new(&file);
        let cells = sts.leaf_cells().expect("leaf_cells");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].name, "state_3");
        assert!(!cells[0].named);
        assert!(cells[0].is_state);
        // The agnostic projection drops the anonymous cell.
        assert!(sts.state_vars().is_empty());
    }

    #[test]
    fn btor_sts_step_delegates_to_simulate_one_step() {
        let file = parser::parse(STEP_BTOR2).expect("parse");
        let sts = BtorSts::new(&file);
        // q' = en: with en=1, q goes 0 → 1.
        let next = sts
            .step(
                &HashMap::from([("q".to_string(), 0u128)]),
                &HashMap::from([("en".to_string(), 1u128)]),
            )
            .expect("step");
        assert_eq!(next.get("q"), Some(&1u128));
        // en=0 keeps q at 0.
        let next0 = sts
            .step(
                &HashMap::from([("q".to_string(), 1u128)]),
                &HashMap::from([("en".to_string(), 0u128)]),
            )
            .expect("step");
        assert_eq!(next0.get("q"), Some(&0u128));
    }

    #[test]
    fn btor_sts_may_edges_match_predicate_image() {
        // Toggle: `q' = !q`. Predicate `q == 1` → cube_0 = {q=0},
        // cube_1 = {q=1}. The transition relation forces 0→1 and 1→0, so
        // the sound may-relation is exactly {(0,1),(1,0)} — never the
        // self-loops (0,0)/(1,1).
        let toggle =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let file = parser::parse(toggle).expect("parse");
        let sts = BtorSts::new(&file);
        let preds = vec![PredicateSpec {
            name: "q_is_1".into(),
            register: "q".into(),
            value: 1,
        }];
        let edges = sts.may_edges(&preds, 5_000);
        assert!(edges.contains(&(0, 1)), "0→1 must be a may-edge: {edges:?}");
        assert!(edges.contains(&(1, 0)), "1→0 must be a may-edge: {edges:?}");
        assert!(
            !edges.contains(&(0, 0)) && !edges.contains(&(1, 1)),
            "self-loops are provably impossible: {edges:?}"
        );
    }

    #[test]
    fn btor_sts_must_edges_match_predicate_image() {
        // P1 #3 — same deterministic toggle `q' = !q`. The transition is
        // a function (no inputs), so the ∀∃ must-relation coincides with
        // the may-relation: 0→1 and 1→0 are forced, the self-loops are
        // provably never must. Confirms the seam's must companion is sound
        // and the cube encoding matches `may_edges`.
        let toggle =
            "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 not 1 3\n6 next 1 3 5\n";
        let file = parser::parse(toggle).expect("parse");
        let sts = BtorSts::new(&file);
        let preds = vec![PredicateSpec {
            name: "q_is_1".into(),
            register: "q".into(),
            value: 1,
        }];
        let must = sts.must_edges(&preds, 5_000);
        assert!(must.contains(&(0, 1)), "0→1 must be a must-edge: {must:?}");
        assert!(must.contains(&(1, 0)), "1→0 must be a must-edge: {must:?}");
        assert!(
            !must.contains(&(0, 0)) && !must.contains(&(1, 1)),
            "self-loops are never must (the toggle never stays): {must:?}"
        );
        // R_must ⊆ R_may by construction.
        let may: std::collections::HashSet<_> = sts.may_edges(&preds, 5_000).into_iter().collect();
        for e in &must {
            assert!(may.contains(e), "must-edge {e:?} must also be a may-edge");
        }
    }

    #[test]
    fn may_and_hyper_must_edges_session_equals_the_split_relations() {
        // Phase 0.c — the combined session-cache pass (ONE z3 scope, ONE encoded view
        // shared by may + hyper-must) must be byte-identical to `may_edges` followed by
        // `hyper_must_edges(&may)` (each its own scope + re-encode). Exercised on a
        // free-input design (`reg_a' = in_a`) so both relations are non-trivial.
        const FREE_INPUT: &str = "1 sort bitvec 1\n2 input 1 in_a\n3 state 1 reg_a\n4 zero 1\n5 init 1 3 4\n6 next 1 3 2\n";
        let file = parser::parse(FREE_INPUT).expect("parse");
        let sts = BtorSts::new(&file);
        let preds = vec![
            PredicateSpec {
                name: "reg_a == 0".into(),
                register: "reg_a".into(),
                value: 0,
            },
            PredicateSpec {
                name: "in_a == 0".into(),
                register: "in_a".into(),
                value: 0,
            },
        ];
        let may_split = sts.may_edges(&preds, 5_000);
        let hyper_split = sts.hyper_must_edges(&preds, &may_split, 5_000);
        let (may_combined, hyper_combined) = sts.may_and_hyper_must_edges(&preds, 5_000);
        assert_eq!(
            may_combined, may_split,
            "session-cache may-relation must equal the standalone `may_edges`"
        );
        assert_eq!(
            hyper_combined, hyper_split,
            "session-cache hyper-must must equal the standalone `hyper_must_edges(&may)`"
        );
    }

    // H.B (free-input atoms) — `reg_a' = in_a`, with a free input `in_a`.
    const FREE_INPUT_BTOR2: &str =
        "1 sort bitvec 1\n2 input 1 in_a\n3 state 1 reg_a\n4 zero 1\n5 init 1 3 4\n6 next 1 3 2\n";

    #[test]
    fn btor_sts_may_edges_admit_free_input_predicate_source_pin_target_free() {
        // H.B increment 2 — admit a predicate over a primary INPUT as a cube
        // dimension. Cube bits: bit0 = `reg_a == 0` (state), bit1 = `in_a == 0`
        // (free input). Cube index = (in_a_bit << 1) | reg_a_bit, polarity 1 =
        // predicate holds.
        //
        // Transition `reg_a' = in_a` means the next reg_a equals the *source*
        // input. The two soundness properties this proves:
        //   (1) SOURCE PIN is effective — a source with `in_a == 0` reaches
        //       `reg_a' == 0` (p0' true); a source with `in_a == 1` reaches
        //       `reg_a' == 1` (p0' false). Different source-input polarities
        //       give different successors ⇒ the source input genuinely drives
        //       the transition (shared existential with the relation).
        //   (2) TARGET is FREE — from any source, BOTH next-input flavours of
        //       the successor get a may-edge (the next-cycle input is not a
        //       variable of the one-step relation). The "for all input
        //       sequences" over-approximation.
        let file = parser::parse(FREE_INPUT_BTOR2).expect("parse free-input fixture");
        let sts = BtorSts::new(&file);
        let preds = vec![
            PredicateSpec {
                name: "reg_a == 0".into(),
                register: "reg_a".into(),
                value: 0,
            },
            PredicateSpec {
                name: "in_a == 0".into(),
                register: "in_a".into(),
                value: 0,
            },
        ];
        let edges: std::collections::HashSet<(usize, usize)> =
            sts.may_edges(&preds, 5_000).into_iter().collect();

        // cube 0: reg_a=1,in_a=1  → reg_a'=1 (p0'=F) → tgts {0 (in_a'=1), 2 (in_a'=0)}
        // cube 1: reg_a=0,in_a=1  → reg_a'=1 (p0'=F) → tgts {0, 2}
        // cube 2: reg_a=1,in_a=0  → reg_a'=0 (p0'=T) → tgts {1 (in_a'=1), 3 (in_a'=0)}
        // cube 3: reg_a=0,in_a=0  → reg_a'=0 (p0'=T) → tgts {1, 3}
        let expected: std::collections::HashSet<(usize, usize)> = [
            (0, 0),
            (0, 2),
            (1, 0),
            (1, 2),
            (2, 1),
            (2, 3),
            (3, 1),
            (3, 3),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            edges, expected,
            "free-input may-relation must be source-pinned + target-free: {edges:?}"
        );

        // (2) explicit: source cube 2 (in_a=0) reaches BOTH in_a' flavours of
        // the reg_a'=0 successor — cube 1 (in_a'=1) AND cube 3 (in_a'=0).
        assert!(
            edges.contains(&(2, 1)) && edges.contains(&(2, 3)),
            "target input dimension must be free (both next-input flavours reached)"
        );
        // (1) explicit: a source with in_a=1 (cube 0) never reaches reg_a'=0
        // (no edge into a p0'=true cube), proving the source pin is effective.
        assert!(
            !edges.contains(&(0, 1)) && !edges.contains(&(0, 3)),
            "source in_a=1 must NOT reach reg_a'==0 — the source input pin is load-bearing"
        );
    }

    // H.E.2 — a state-only combinational signal `g = !q` (NID 3). With cube
    // dimension `q == 1`, the derived label `g == 1` is DETERMINED per cube:
    // q==1 ⇒ g==0 (KleeneF for g==1); q==0 ⇒ g==1 (KleeneT).
    const COMB_LABEL_BTOR2: &str =
        "1 sort bitvec 1\n2 state 1 q\n3 not 1 2 g\n4 zero 1\n5 init 1 2 4\n6 next 1 2 2\n";

    #[test]
    fn btor_sts_combinational_labels_determined_by_state_cube() {
        use crate::clts::Tristate;
        let file = parser::parse(COMB_LABEL_BTOR2).expect("parse combinational-label fixture");
        let sts = BtorSts::new(&file);
        let cube_preds = vec![PredicateSpec {
            name: "q == 1".into(),
            register: "q".into(),
            value: 1,
        }];
        let derived = vec![PredicateSpec {
            name: "g == 1".into(),
            register: "g".into(), // the combinational node's own symbol
            value: 1,
        }];
        let labels: std::collections::HashMap<(usize, usize), Tristate> = sts
            .combinational_labels(&cube_preds, &derived, 5_000)
            .into_iter()
            .map(|(c, d, t)| ((c, d), t))
            .collect();
        // cube 0 = (q==1 false ⇒ q==0) ⇒ g = !0 = 1 ⇒ g==1 is KleeneT.
        assert_eq!(
            labels.get(&(0, 0)),
            Some(&Tristate::KleeneT),
            "q==0 ⇒ g==1 true"
        );
        // cube 1 = (q==1 true) ⇒ g = !1 = 0 ⇒ g==1 is KleeneF.
        assert_eq!(
            labels.get(&(1, 0)),
            Some(&Tristate::KleeneF),
            "q==1 ⇒ g==1 false"
        );
    }
}
