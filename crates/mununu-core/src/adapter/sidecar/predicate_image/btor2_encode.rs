//! BTOR2 → Z3 transition-relation encoder (Phase A.4 step 4.2).
//!
//! Walks the BTOR2 NID DAG and produces a single Z3 boolean
//! `T(s, s')` representing the transition relation. State / input
//! NIDs become Z3 `BV` variables over `s` (current step) or
//! `s_next` (next step); `next` lines become equalities tying the
//! two together; the `Btor2SmtView` returned by [`encode_design`]
//! exposes these to the all-SMT enumerator in [`super::all_smt`].
//!
//! Z3 must be called inside a [`z3::with_z3_config`] scope — the
//! crate ships its context as a thread-local. Callers (the all-SMT
//! enumerator, the recall harness) own that scope and drop the
//! `Btor2SmtView` before exiting it.
//!
//! # SOUNDNESS
//!
//! The encoder is **exact** for every operator it supports. Operators
//! outside [`crate::adapter::btor2::ast::Op::is_blastable`] cause the
//! encoder to refuse the design (hard error, surfaced as
//! [`EncodeError::UnsupportedOperator`]) rather than emit an
//! over-approximation. Array sorts (`read`/`write`) are rejected in
//! `Theory::BvOnly`; step 4.5 will handle them under `Theory::BvUfArray`.

use std::collections::{HashMap, HashSet};

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Op, Operand, Sort};
use crate::adapter::btor2::parser::bv_width;
use crate::adapter::btor2::term_backend::{WalkError, walk_design};

/// Error variants raised by [`encode_design`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("BTOR2 NID {nid}: operator '{op_name}' not supported by the predicate-image encoder.")]
    UnsupportedOperator { nid: i64, op_name: &'static str },
    #[error("BTOR2 NID {nid}: array sort under Theory::BvOnly. Use Theory::BvUfArray (step 4.5).")]
    ArraySortUnsupportedInBvOnly { nid: i64 },
    /// A `next` line references an operand whose width does not match
    /// its state cell. The BTOR2 parser usually catches this; surfaced
    /// here only as a defence-in-depth.
    #[error(
        "BTOR2 NID {nid}: width mismatch — state cell is {state_width} bits, operand is {operand_width} bits."
    )]
    WidthMismatch {
        nid: i64,
        state_width: u32,
        operand_width: u32,
    },
    /// Bit-vector width too small for the integer constant. Should
    /// not happen on well-formed BTOR2; surfaced as a hard error
    /// rather than silently truncating.
    #[error("BTOR2 NID {nid}: constant value does not fit in declared sort width.")]
    ConstantOutOfRange { nid: i64 },
}

/// A signal exposed by the encoded design — state cell, input, or
/// (H.E.1) a named **combinational** node. Carries the human-readable
/// symbol (when the BTOR2 file declared one) and the bit-width.
#[derive(Debug, Clone)]
pub struct EncodedSignal {
    pub nid: Nid,
    pub width: u32,
    pub symbol: Option<String>,
    pub kind: SignalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    State,
    Input,
    /// H.E.1 — a named combinational node (an `Op`/output that is neither
    /// a `state` cell nor an `input`). Its current-cycle Z3 BV is in
    /// [`Btor2SmtView::signal_bvs`]; its value is a *determined function* of
    /// (state, inputs), so the predicate-image labels it per cube (Approach B,
    /// free-input-atoms.md §4) rather than treating it as a free cube dimension.
    Combinational,
}

/// Output of [`encode_design`]. Holds the Z3 bit-vector handles for
/// every state cell + input under both current-step (`s`) and
/// next-step (`s_next`) projections, plus the conjoined transition
/// relation.
///
/// Must be dropped before exiting the surrounding
/// [`z3::with_z3_config`] scope.
pub struct Btor2SmtView {
    /// Z3 BV handle for the current-step value of each state cell.
    /// Indexed by BTOR2 NID.
    pub state_curr: HashMap<Nid, z3::ast::BV>,
    /// Z3 BV handle for the next-step value of each state cell.
    pub state_next: HashMap<Nid, z3::ast::BV>,
    /// Z3 BV handle for each input (constant across the step).
    pub inputs: HashMap<Nid, z3::ast::BV>,
    /// §Phase 10 stage 3.c.1 — Z3 Array handle for the current-step
    /// value of each array-sorted (memory) state cell. Empty under
    /// `Theory::BvOnly`; populated under `Theory::BvUfArray`. The
    /// array's domain sort is the BTOR2 index width, the range sort
    /// is the element width. Memory cells only flow into the BV
    /// world through `Op::Read` (→ `select`); their own next-state
    /// is an extensional array equality in the transition relation.
    /// mununu#512 — the next-step **expression** for each BV state cell, i.e. the
    /// value `next` assigns, built over current-state + inputs only. The transition
    /// builder already computes this to constrain [`Self::state_next`]; exposing it
    /// lets a query SUBSTITUTE the next state instead of quantifying over it, which
    /// is what keeps an array-bearing must-edge query inside the decidable fragment.
    /// Empty until [`Btor2Backend::transition`] has run.
    pub state_next_expr: HashMap<Nid, z3::ast::BV>,
    /// mununu#512 — the array counterpart of [`Self::state_next_expr`]
    /// (`ite(we, store(mem, waddr, wdata), mem)`). This is the one that matters:
    /// universally binding `state_next_arr` is what pushes the query from QF_AUFBV
    /// into AUFBV + ∀-array.
    pub state_next_arr_expr: HashMap<Nid, z3::ast::Array>,
    pub state_curr_arr: HashMap<Nid, z3::ast::Array>,
    /// §Phase 10 stage 3.c.1 — Z3 Array handle for the next-step
    /// value of each array-sorted state cell. Mirror of
    /// `state_curr_arr` over `s_next`.
    pub state_next_arr: HashMap<Nid, z3::ast::Array>,
    /// SEL — array-cell NAME → NID, for array-content (`Select`) predicates. Memory cells
    /// are intentionally absent from `signals` (they are not BV cube dimensions), so the
    /// BV `nid_map` cannot resolve an array name; this map lets a `select(arr, idx)` atom
    /// bind the array by its symbol. Populated under `Theory::BvUfArray`, empty under BvOnly.
    pub array_name_nid: HashMap<String, Nid>,
    /// Per-signal metadata for callers that need to round-trip
    /// names / widths back to the sidecar. Includes State, Input, and
    /// (H.E.1) named Combinational nodes.
    pub signals: Vec<EncodedSignal>,
    /// H.E.1 — current-cycle Z3 BV of **every** node the encode walk
    /// computed (the retained backend cache), keyed by NID. A combinational
    /// signal's value is `signal_bvs[nid]` over `state_curr` + `inputs`. The
    /// per-cube combinational-label pass (H.E.2) reads these to decide each
    /// derived predicate's KleeneT/F/Bot label per cube. State/input nodes are
    /// also present (their curr BV); `state_curr`/`inputs` remain the canonical
    /// maps for those.
    pub signal_bvs: HashMap<Nid, z3::ast::BV>,
    /// The conjoined transition relation `T(s, s') = ⋀ s_next == next(...)`.
    pub transition: z3::ast::Bool,
}

impl Btor2SmtView {
    /// Look up the next-step BV for a state-cell NID.
    pub fn next_state(&self, nid: Nid) -> Option<&z3::ast::BV> {
        self.state_next.get(&nid)
    }

    /// Look up the current-step BV for a state-cell NID.
    pub fn curr_state(&self, nid: Nid) -> Option<&z3::ast::BV> {
        self.state_curr.get(&nid)
    }

    /// H.E.1 — look up the current-step BV for ANY node by NID (state cell,
    /// input, or combinational signal), from the retained encode cache. The
    /// per-cube combinational-label pass uses this to read a derived predicate's
    /// signal value.
    pub fn curr_signal(&self, nid: Nid) -> Option<&z3::ast::BV> {
        self.signal_bvs.get(&nid)
    }

    /// mununu#512 — the next-step EXPRESSION for a BV state cell, if the transition
    /// has been built. Prefer this over [`Self::next_state`] in a query that would
    /// otherwise have to quantify over the next state.
    pub fn next_state_expr(&self, nid: Nid) -> Option<&z3::ast::BV> {
        self.state_next_expr.get(&nid)
    }

    /// mununu#512 — the next-step EXPRESSION for an array state cell.
    pub fn next_state_arr_expr(&self, nid: Nid) -> Option<&z3::ast::Array> {
        self.state_next_arr_expr.get(&nid)
    }

    /// Bit-width of a signal by NID.
    pub fn width_of(&self, nid: Nid) -> Option<u32> {
        self.signals.iter().find(|s| s.nid == nid).map(|s| s.width)
    }
}

/// Encode a BTOR2 file into a Z3 transition relation under
/// `Theory::BvOnly` (pure bit-vectors; array sorts rejected).
///
/// Must be called inside a [`z3::with_z3_config`] scope. Returns
/// [`EncodeError::UnsupportedOperator`] on the first non-blastable
/// op encountered; the caller has no way to recover other than
/// rejecting the design or switching to an external symbolic engine.
///
/// §Phase 10 stage 3.c.1 — this is now a thin delegator to
/// [`encode_design_with_theory`] with [`Theory::BvOnly`]. Every
/// existing caller (must-edge queries, all-SMT enumeration) keeps
/// the identical behaviour: array-sorted designs still error with
/// [`EncodeError::ArraySortUnsupportedInBvOnly`]. Memory-bearing
/// designs opt into the array encoding via
/// `encode_design_with_theory(file, Theory::BvUfArray)`.
pub fn encode_design(file: &Btor2File) -> Result<Btor2SmtView, EncodeError> {
    encode_design_with_theory(file, super::theory::Theory::BvOnly)
}

/// §Phase 10 stage 3.c.1 (2026-06-12) — encode a BTOR2 file into a
/// Z3 transition relation under the chosen [`Theory`].
///
/// - [`Theory::BvOnly`]: pure QF_BV. Array-sorted state cells +
///   `Op::Read`/`Op::Write` error out (the pre-stage-3.c.1
///   behaviour, preserved bit-for-bit).
/// - [`Theory::BvUfArray`]: QF_AUFBV. Array-sorted state cells are
///   declared as Z3 `Array` consts (domain = index width, range =
///   element width); `Op::Read` encodes as `select` (→ BV);
///   array-sorted `next` lines encode as extensional array
///   equalities in the transition relation. The functional-
///   consistency axioms (`read(write(a,i,v),j) = i==j ? v :
///   read(a,j)`) are emitted automatically by Z3's Array theory —
///   no manual axiom encoding.
///
/// **Soundness.** The array encoding is *exact* for the memory
/// semantics BTOR2 expresses — it does not over-approximate. (The
/// §Phase 10 havoc-may / array-must composition handles the
/// may-side over-approximation upstream in the predicate-cube lift;
/// this encoder is the precise must-side oracle.)
pub fn encode_design_with_theory(
    file: &Btor2File,
    theory: super::theory::Theory,
) -> Result<Btor2SmtView, EncodeError> {
    let arrays_ok = theory.supports_indirect_references();

    // Z3 0.20's handles carry a lifetime tied to the global context
    // the `with_z3_config` scope installed; the crate's API returns
    // `'static`-flavoured handles.
    let mut state_curr: HashMap<Nid, z3::ast::BV> = HashMap::new();
    let mut state_next: HashMap<Nid, z3::ast::BV> = HashMap::new();
    let mut state_curr_arr: HashMap<Nid, z3::ast::Array> = HashMap::new();
    let mut state_next_arr: HashMap<Nid, z3::ast::Array> = HashMap::new();
    let mut array_name_nid: HashMap<String, Nid> = HashMap::new();
    let mut inputs: HashMap<Nid, z3::ast::BV> = HashMap::new();
    let mut signals = Vec::new();

    let symbols = crate::adapter::btor2::parser::collect_symbols(file);

    // Pass 1 — declare handles for every state cell and input.
    for line in &file.lines {
        match &line.node {
            Node::State { sort, .. } => {
                // Distinguish bit-vector state cells from array
                // (memory) state cells. `bv_width` returns None for
                // array sorts.
                if let Some(width) = bv_width(file, *sort) {
                    let sym = symbols.get(&line.nid).cloned();
                    let label = sym.clone().unwrap_or_else(|| format!("st{}", line.nid));
                    let curr = z3::ast::BV::new_const(format!("s_{label}").as_str(), width);
                    let next = z3::ast::BV::new_const(format!("s_next_{label}").as_str(), width);
                    state_curr.insert(line.nid, curr);
                    state_next.insert(line.nid, next);
                    signals.push(EncodedSignal {
                        nid: line.nid,
                        width,
                        symbol: sym,
                        kind: SignalKind::State,
                    });
                } else if arrays_ok {
                    // Array-sorted memory cell — declare Z3 Array
                    // consts for the current + next step.
                    let (idx_w, elem_w) = array_widths(file, *sort)
                        .ok_or(EncodeError::ArraySortUnsupportedInBvOnly { nid: line.nid })?;
                    let sym = symbols.get(&line.nid).cloned();
                    let label = sym.clone().unwrap_or_else(|| format!("mem{}", line.nid));
                    let dom = z3::Sort::bitvector(idx_w);
                    let rng = z3::Sort::bitvector(elem_w);
                    let curr = z3::ast::Array::new_const(format!("a_{label}").as_str(), &dom, &rng);
                    let next =
                        z3::ast::Array::new_const(format!("a_next_{label}").as_str(), &dom, &rng);
                    state_curr_arr.insert(line.nid, curr);
                    state_next_arr.insert(line.nid, next);
                    if let Some(s) = &sym {
                        array_name_nid.insert(s.clone(), line.nid);
                    }
                    // Memory cells are tracked in the array maps, not
                    // the BV `signals` vector (which feeds bit-vector
                    // cube enumeration). The predicate-cube lift's
                    // cube predicates reference BV registers only; the
                    // `array_name_nid` map lets a `Select` atom bind them.
                } else {
                    return Err(EncodeError::ArraySortUnsupportedInBvOnly { nid: line.nid });
                }
            }
            Node::Input { sort, .. } => {
                let width = bv_width(file, *sort)
                    .ok_or(EncodeError::ArraySortUnsupportedInBvOnly { nid: line.nid })?;
                let sym = symbols.get(&line.nid).cloned();
                let label = sym.clone().unwrap_or_else(|| format!("in{}", line.nid));
                let v = z3::ast::BV::new_const(format!("in_{label}").as_str(), width);
                inputs.insert(line.nid, v);
                signals.push(EncodedSignal {
                    nid: line.nid,
                    width,
                    symbol: sym,
                    kind: SignalKind::Input,
                });
            }
            _ => {}
        }
    }

    // Pass 2 (§Phase 10 Option-4 step 1c.3) — build the transition
    // relation through the unified `walk_design` driver instead of an
    // inline recursive loop. `Z3Backend` is the SMT instantiation of
    // `BvTermBackend`: the forward walk binds every Const/Op node into
    // the backend's cache, then `transition()` conjoins the per-`next`
    // equalities (BV `s_next == eval(value)` + array
    // `a_next == eval_array(value)`), reading the already-bound
    // operands from the cache. Proven equivalent to the prior inline
    // pass-2 by the `z3backend_transition_matches_*` tests (XOR-UNSAT,
    // bit-for-bit) and guarded by the full must-edge + lift suites.
    //
    // This puts the must-edge SMT path on the SAME driver as the
    // concrete bit-blast path (step 1b), so the `uf_substitute` hook
    // (step 1d real UF) applies uniformly without separate plumbing.
    //
    // The view is assembled with a placeholder transition first so the
    // backend can read the `state_*` / `input` declarations through
    // `from_view` (which clones them — it does not borrow the view);
    // the real transition then overwrites the placeholder.
    let mut view = Btor2SmtView {
        state_curr,
        state_next,
        state_curr_arr,
        state_next_arr,
        array_name_nid,
        inputs,
        signals,
        signal_bvs: HashMap::new(),
        transition: z3::ast::Bool::from_bool(true),
        state_next_expr: HashMap::new(),
        state_next_arr_expr: HashMap::new(),
    };
    let mut backend = Z3Backend::from_view(file, &view);
    walk_design(file, &mut backend).map_err(|e| match e {
        WalkError::Backend(inner) => inner,
        // A genuinely non-bitvec, non-array sort on a value node — not
        // expected on well-formed BTOR2 the parser accepts.
        WalkError::NonBitvecSort(nid) => EncodeError::ArraySortUnsupportedInBvOnly { nid },
        // `honor_init()` is false for the SMT backend, so the walk
        // never honours `Init` lines; this arm is unreachable in
        // practice but mapped defensively.
        WalkError::Unevaluated(nid) => EncodeError::UnsupportedOperator {
            nid,
            op_name: "uninitialized-init-value",
        },
    })?;
    let (t, next_expr, next_arr_expr) = backend.transition(&view)?;
    view.transition = t;
    view.state_next_expr = next_expr;
    view.state_next_arr_expr = next_arr_expr;

    // H.E.1 — retain the backend's per-node BV cache (current-cycle value of
    // every walked node) so the per-cube combinational-label pass (H.E.2) can
    // read a derived predicate's signal value, and register each *named*
    // combinational node (an Op/output that is neither a state cell nor an
    // input) as a `SignalKind::Combinational` signal so the nid-map builder +
    // seeder classifier can discover it. Behaviour-preserving: existing callers
    // read only `state_curr`/`state_next`/`inputs` + State/Input `signals`.
    let cache = std::mem::take(&mut backend.cache);
    // Register each named combinational node — an `Op` carrying its OWN symbol
    // that is neither a state cell nor an input. NB: read the Op line's own
    // `symbol` field, NOT `collect_symbols` (whose pass-2 traces Op symbols back
    // to the *state* nid they alias — the wrong target for a genuine
    // combinational node like `main_sm_err_o`).
    for line in &file.lines {
        let Node::Op {
            symbol: Some(name), ..
        } = &line.node
        else {
            continue;
        };
        let nid = line.nid;
        if view.state_curr.contains_key(&nid) || view.inputs.contains_key(&nid) {
            continue;
        }
        if let Some(bv) = cache.get(&nid) {
            view.signals.push(EncodedSignal {
                nid,
                width: bv.get_size(),
                symbol: Some(name.clone()),
                kind: SignalKind::Combinational,
            });
        }
    }
    // H.U.1d — also register a combinational node named only on an `output` line
    // (a top-level combinational output port whose driving Op carries no symbol
    // of its own). The output's symbol aliases the driven signal node; if that
    // node is combinational (in the cache, neither a state cell nor an input),
    // expose it as a `Combinational` signal under the output-port name so a
    // predicate can reference it. An Op that already carries its own symbol is
    // registered above; this adds the output alias (a nid may have multiple
    // names — every entry resolves through the nid-map).
    for line in &file.lines {
        let Node::Output {
            symbol: Some(name),
            signal,
        } = &line.node
        else {
            continue;
        };
        let nid = signal.nid();
        if view.state_curr.contains_key(&nid) || view.inputs.contains_key(&nid) {
            continue;
        }
        if let Some(bv) = cache.get(&nid) {
            view.signals.push(EncodedSignal {
                nid,
                width: bv.get_size(),
                symbol: Some(name.clone()),
                kind: SignalKind::Combinational,
            });
        }
    }
    view.signal_bvs = cache;
    Ok(view)
}

/// H.U.0 — the "next-cycle node cache": every Op/Const node's value evaluated
/// over `(s', i')`, where `s'` is the view's `state_next` BVs and `i'` are FRESH
/// next-cycle input consts. This is the primed projection of the netlist the
/// uniform predicate-image (H.U) reads for the TARGET side of a may/must edge —
/// the same role `signal_bvs` (current cache, over `(s, i)`) plays for the
/// SOURCE side. Built by a second [`walk_design`] over a [`Z3Backend`] whose
/// `state_curr` is bound to `state_next` and whose `inputs` are the fresh `i'`.
///
/// **H.U.1a (2026-06-29).** Promoted to production: the uniform may-edge path
/// (`smt_must_edge::smt_per_target_may_check_uniform`, via the STS-IR seam)
/// reads it for an edge's target side.
pub(crate) struct PrimedEnv {
    /// Primed value of every walked Op/Const node, keyed by nid (over `(s', i')`).
    pub cache: HashMap<Nid, z3::ast::BV>,
    /// Fresh next-cycle input consts, keyed by the input's nid.
    pub inputs: HashMap<Nid, z3::ast::BV>,
}

/// H.U.0 — build the [`PrimedEnv`] for `view`. Reuses the view's `state_next`
/// BVs as the register valuations and declares one fresh `in_prime_<sym>` const
/// per input, then re-walks the design so every node is re-evaluated at the next
/// cycle. A register leaf's primed value is exactly `state_next[reg]` (so a
/// state-only predicate's target constraint matches the per-kind path's
/// `next_state` by construction); a combinational node gets a real next-cycle
/// term; an input maps to its fresh `i'`.
///
/// **Caller must hold a [`z3::with_z3_config`] scope** (it allocates Z3 consts).
pub(crate) fn encode_primed(
    file: &Btor2File,
    view: &Btor2SmtView,
) -> Result<PrimedEnv, EncodeError> {
    let symbols = crate::adapter::btor2::parser::collect_symbols(file);
    let mut primed_inputs: HashMap<Nid, z3::ast::BV> = HashMap::new();
    for (nid, bv) in &view.inputs {
        let label = symbols
            .get(nid)
            .cloned()
            .unwrap_or_else(|| format!("in{nid}"));
        primed_inputs.insert(
            *nid,
            z3::ast::BV::new_const(format!("in_prime_{label}").as_str(), bv.get_size()),
        );
    }
    // Synthetic view whose CURRENT step is the original's NEXT step + fresh
    // inputs. `Z3Backend::from_view` clones `state_curr` + `inputs` from it, so
    // the walk evaluates every node over `(s', i')`. `state_next` / arrays are
    // carried unchanged (unused by the walk); the transition is a placeholder.
    let primed_view = Btor2SmtView {
        state_curr: view.state_next.clone(),
        state_next: view.state_next.clone(),
        state_curr_arr: view.state_next_arr.clone(),
        state_next_arr: view.state_next_arr.clone(),
        array_name_nid: view.array_name_nid.clone(),
        inputs: primed_inputs.clone(),
        signals: view.signals.clone(),
        signal_bvs: HashMap::new(),
        transition: z3::ast::Bool::from_bool(true),
        state_next_expr: HashMap::new(),
        state_next_arr_expr: HashMap::new(),
    };
    let mut backend = Z3Backend::from_view(file, &primed_view);
    walk_design(file, &mut backend).map_err(|e| match e {
        WalkError::Backend(inner) => inner,
        WalkError::NonBitvecSort(nid) => EncodeError::ArraySortUnsupportedInBvOnly { nid },
        WalkError::Unevaluated(nid) => EncodeError::UnsupportedOperator {
            nid,
            op_name: "uninitialized-init-value",
        },
    })?;
    Ok(PrimedEnv {
        cache: std::mem::take(&mut backend.cache),
        inputs: primed_inputs,
    })
}

/// §Phase 10 stage 3.c.1 — resolve the index + element bit-widths of
/// an array sort. Thin alias over [`crate::adapter::btor2::parser::array_widths`]
/// (the canonical implementation, shared with the `walk_design` driver);
/// kept as a module-local name so existing call sites + the test read
/// unchanged.
fn array_widths(file: &Btor2File, sort_nid: Nid) -> Option<(u32, u32)> {
    crate::adapter::btor2::parser::array_widths(file, sort_nid)
}

/// §Phase 10 stage 3.c.1 — resolve an array-sorted operand to a Z3
/// `Array` term. Array values occupy a sub-DAG that exits to the BV
/// world only through `Op::Read` (→ `select`). The producers of
/// array values are:
/// - `Node::State` (array sort) → the declared current-step Array.
/// - `Op::Write(arr, idx, val)` → `arr.store(idx, val)`.
/// - `Op::Ite(cond, then_arr, else_arr)` → conditional array select.
///
/// Caches by NID so DAG sharing in the BTOR2 file maps to Z3
/// expression sharing. Index/value sub-terms are evaluated through
/// the BV path (`eval_operand`).
#[allow(clippy::too_many_arguments)]
fn eval_array_operand(
    operand: Operand,
    file: &Btor2File,
    state_curr: &HashMap<Nid, z3::ast::BV>,
    inputs: &HashMap<Nid, z3::ast::BV>,
    state_curr_arr: &HashMap<Nid, z3::ast::Array>,
    cache: &mut HashMap<Nid, z3::ast::BV>,
    array_cache: &mut HashMap<Nid, z3::ast::Array>,
) -> Result<z3::ast::Array, EncodeError> {
    // Array operands are never negated (BTOR2 only negates bit-vec
    // signals); `nid()` strips any sign defensively.
    let nid = operand.nid();

    if let Some(cached) = array_cache.get(&nid) {
        return Ok(cached.clone());
    }

    let line = file.lookup(nid).ok_or(EncodeError::UnsupportedOperator {
        nid,
        op_name: "<missing-array-nid>",
    })?;

    let arr = match &line.node {
        Node::State { .. } => {
            state_curr_arr
                .get(&nid)
                .cloned()
                .ok_or(EncodeError::UnsupportedOperator {
                    nid,
                    op_name: "array-state",
                })?
        }
        Node::Op {
            op: Op::Write,
            args,
            ..
        } => {
            let base = eval_array_operand(
                args[0],
                file,
                state_curr,
                inputs,
                state_curr_arr,
                cache,
                array_cache,
            )?;
            let idx = eval_operand(
                args[1],
                file,
                state_curr,
                inputs,
                Some(state_curr_arr),
                cache,
            )?;
            let val = eval_operand(
                args[2],
                file,
                state_curr,
                inputs,
                Some(state_curr_arr),
                cache,
            )?;
            base.store(&idx, &val)
        }
        Node::Op {
            op: Op::Ite, args, ..
        } => {
            let cond = eval_operand(
                args[0],
                file,
                state_curr,
                inputs,
                Some(state_curr_arr),
                cache,
            )?;
            let then_arr = eval_array_operand(
                args[1],
                file,
                state_curr,
                inputs,
                state_curr_arr,
                cache,
                array_cache,
            )?;
            let else_arr = eval_array_operand(
                args[2],
                file,
                state_curr,
                inputs,
                state_curr_arr,
                cache,
                array_cache,
            )?;
            let zero = z3::ast::BV::from_u64(0, cond.get_size());
            let cond_bool = cond.eq(&zero).not();
            cond_bool.ite(&then_arr, &else_arr)
        }
        _ => {
            return Err(EncodeError::UnsupportedOperator {
                nid,
                op_name: "non-array-producing node in array position",
            });
        }
    };

    array_cache.insert(nid, arr.clone());
    Ok(arr)
}

/// Recursively evaluate an operand to a Z3 BV. Caches sub-expressions
/// keyed by NID so the DAG sharing in the BTOR2 file translates
/// directly into Z3 expression sharing.
fn eval_operand(
    operand: Operand,
    file: &Btor2File,
    state_curr: &HashMap<Nid, z3::ast::BV>,
    inputs: &HashMap<Nid, z3::ast::BV>,
    array_curr: Option<&HashMap<Nid, z3::ast::Array>>,
    cache: &mut HashMap<Nid, z3::ast::BV>,
) -> Result<z3::ast::BV, EncodeError> {
    let nid = operand.nid();
    let negated = operand.is_negated();

    if let Some(cached) = cache.get(&nid) {
        let v = cached.clone();
        return Ok(if negated { v.bvnot() } else { v });
    }

    let line = file.lookup(nid).ok_or(EncodeError::UnsupportedOperator {
        nid,
        op_name: "<missing-nid>",
    })?;

    let v = match &line.node {
        Node::State { .. } => {
            state_curr
                .get(&nid)
                .cloned()
                .ok_or(EncodeError::UnsupportedOperator {
                    nid,
                    op_name: "state",
                })?
        }
        Node::Input { .. } => {
            inputs
                .get(&nid)
                .cloned()
                .ok_or(EncodeError::UnsupportedOperator {
                    nid,
                    op_name: "input",
                })?
        }
        Node::Const { sort, value } => encode_const(line.nid, *sort, value, file)?,
        Node::Op { sort, op, args, .. } => {
            let width = bv_width(file, *sort)
                .ok_or(EncodeError::ArraySortUnsupportedInBvOnly { nid: line.nid })?;
            encode_op(
                line.nid,
                *op,
                args,
                &line.immediates,
                width,
                file,
                state_curr,
                inputs,
                array_curr,
                cache,
            )?
        }
        Node::Sort { .. }
        | Node::Init { .. }
        | Node::Next { .. }
        | Node::Bad { .. }
        | Node::Constraint { .. }
        | Node::Fair { .. }
        | Node::Output { .. }
        | Node::Justice { .. } => {
            return Err(EncodeError::UnsupportedOperator {
                nid,
                op_name: "non-value-node",
            });
        }
    };

    cache.insert(nid, v.clone());
    Ok(if negated { v.bvnot() } else { v })
}

fn encode_const(
    nid: Nid,
    sort_nid: Nid,
    value: &ConstValue,
    file: &Btor2File,
) -> Result<z3::ast::BV, EncodeError> {
    let width =
        bv_width(file, sort_nid).ok_or(EncodeError::ArraySortUnsupportedInBvOnly { nid })?;
    encode_const_bits(nid, value, width)
}

/// §Phase 10 Option-4 step 1c.1 (2026-06-12) — width-keyed constant
/// encoding, shared by [`encode_const`] (recursive path) and
/// [`Z3Backend::eval_const`] (the `BvTermBackend` forward-walk
/// path) so both produce identical Z3 constants from one source.
fn encode_const_bits(nid: Nid, value: &ConstValue, width: u32) -> Result<z3::ast::BV, EncodeError> {
    let bits: u64 = match value {
        ConstValue::Zero => 0,
        ConstValue::One => 1,
        ConstValue::Ones => {
            if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            }
        }
        ConstValue::Bin(s) => {
            u64::from_str_radix(s, 2).map_err(|_| EncodeError::ConstantOutOfRange { nid })?
        }
        ConstValue::Dec(d) => {
            if *d < 0 || *d > (i128::from(u64::MAX)) {
                return Err(EncodeError::ConstantOutOfRange { nid });
            }
            *d as u64
        }
        ConstValue::Hex(s) => {
            u64::from_str_radix(s, 16).map_err(|_| EncodeError::ConstantOutOfRange { nid })?
        }
    };
    Ok(z3::ast::BV::from_u64(bits, width))
}

#[allow(clippy::too_many_arguments)]
fn encode_op(
    nid: Nid,
    op: Op,
    args: &[Operand],
    immediates: &[u32],
    width: u32,
    file: &Btor2File,
    state_curr: &HashMap<Nid, z3::ast::BV>,
    inputs: &HashMap<Nid, z3::ast::BV>,
    array_curr: Option<&HashMap<Nid, z3::ast::Array>>,
    cache: &mut HashMap<Nid, z3::ast::BV>,
) -> Result<z3::ast::BV, EncodeError> {
    // §Phase 10 stage 3.c.1 — `Op::Read` is the one operator that
    // joins the array sub-DAG to the BV world (`select` yields the
    // element BV). It must be handled BEFORE the `get` closure below
    // borrows `cache`, because it needs both the BV cache (for the
    // index sub-term) and the array evaluator (for the array
    // sub-term). Under `Theory::BvOnly` (`array_curr == None`) it
    // falls through to the array-rejection error, preserving the
    // pre-stage-3.c.1 behaviour.
    if op == Op::Read {
        let array_curr = array_curr.ok_or(EncodeError::ArraySortUnsupportedInBvOnly { nid })?;
        let mut array_cache: HashMap<Nid, z3::ast::Array> = HashMap::new();
        let arr = eval_array_operand(
            args[0],
            file,
            state_curr,
            inputs,
            array_curr,
            cache,
            &mut array_cache,
        )?;
        let idx = eval_operand(args[1], file, state_curr, inputs, Some(array_curr), cache)?;
        let elem = arr
            .select(&idx)
            .as_bv()
            .ok_or(EncodeError::UnsupportedOperator {
                nid,
                op_name: "read (select did not yield a bit-vector element)",
            })?;
        return Ok(elem);
    }

    // Helpers to materialise operands inline.
    let mut get = |i: usize| -> Result<z3::ast::BV, EncodeError> {
        eval_operand(args[i], file, state_curr, inputs, array_curr, cache)
    };
    let bool_to_bv1 = |b: z3::ast::Bool| {
        let one = z3::ast::BV::from_u64(1, 1);
        let zero = z3::ast::BV::from_u64(0, 1);
        b.ite(&one, &zero)
    };

    match op {
        // Bitwise / boolean
        Op::Not => Ok(get(0)?.bvnot()),
        Op::And => Ok(get(0)?.bvand(&get(1)?)),
        Op::Or => Ok(get(0)?.bvor(&get(1)?)),
        Op::Xor => Ok(get(0)?.bvxor(&get(1)?)),
        Op::Nand => Ok(get(0)?.bvand(&get(1)?).bvnot()),
        Op::Nor => Ok(get(0)?.bvor(&get(1)?).bvnot()),
        Op::Xnor => Ok(get(0)?.bvxor(&get(1)?).bvnot()),
        Op::Iff => Ok(bool_to_bv1(get(0)?.eq(&get(1)?))),
        Op::Implies => {
            // a → b  ≡  ¬a ∨ b ; over BV(1).
            let a = get(0)?;
            let b = get(1)?;
            Ok(a.bvnot().bvor(&b))
        }
        // Comparison (always emits BV(1))
        Op::Eq => Ok(bool_to_bv1(get(0)?.eq(&get(1)?))),
        Op::Neq => Ok(bool_to_bv1(get(0)?.eq(&get(1)?).not())),
        Op::Ult => Ok(bool_to_bv1(get(0)?.bvult(&get(1)?))),
        Op::Ulte => Ok(bool_to_bv1(get(0)?.bvule(&get(1)?))),
        Op::Ugt => Ok(bool_to_bv1(get(0)?.bvugt(&get(1)?))),
        Op::Ugte => Ok(bool_to_bv1(get(0)?.bvuge(&get(1)?))),
        Op::Slt => Ok(bool_to_bv1(get(0)?.bvslt(&get(1)?))),
        Op::Slte => Ok(bool_to_bv1(get(0)?.bvsle(&get(1)?))),
        Op::Sgt => Ok(bool_to_bv1(get(0)?.bvsgt(&get(1)?))),
        Op::Sgte => Ok(bool_to_bv1(get(0)?.bvsge(&get(1)?))),
        // Arithmetic
        Op::Add => Ok(get(0)?.bvadd(&get(1)?)),
        Op::Sub => Ok(get(0)?.bvsub(&get(1)?)),
        Op::Mul => Ok(get(0)?.bvmul(&get(1)?)),
        Op::Neg => Ok(get(0)?.bvneg()),
        Op::Inc => {
            let a = get(0)?;
            let one = z3::ast::BV::from_u64(1, a.get_size());
            Ok(a.bvadd(&one))
        }
        Op::Dec => {
            let a = get(0)?;
            let one = z3::ast::BV::from_u64(1, a.get_size());
            Ok(a.bvsub(&one))
        }
        // Shifts
        Op::Sll => Ok(get(0)?.bvshl(&get(1)?)),
        Op::Srl => Ok(get(0)?.bvlshr(&get(1)?)),
        Op::Sra => Ok(get(0)?.bvashr(&get(1)?)),
        Op::Rol => Ok(get(0)?.bvrotl(&get(1)?)),
        Op::Ror => Ok(get(0)?.bvrotr(&get(1)?)),
        // Concat / slice / extension
        Op::Concat => Ok(get(0)?.concat(&get(1)?)),
        Op::Slice => {
            let inner = get(0)?;
            let upper = *immediates.first().ok_or(EncodeError::UnsupportedOperator {
                nid,
                op_name: "slice (missing immediate)",
            })?;
            let lower = *immediates.get(1).ok_or(EncodeError::UnsupportedOperator {
                nid,
                op_name: "slice (missing immediate)",
            })?;
            Ok(inner.extract(upper, lower))
        }
        Op::Uext => {
            let inner = get(0)?;
            let pad = width.saturating_sub(inner.get_size());
            Ok(inner.zero_ext(pad))
        }
        Op::Sext => {
            let inner = get(0)?;
            let pad = width.saturating_sub(inner.get_size());
            Ok(inner.sign_ext(pad))
        }
        // Reductions
        Op::Redand => {
            let inner = get(0)?;
            let ones = z3::ast::BV::from_i64(-1, inner.get_size());
            Ok(bool_to_bv1(inner.eq(&ones)))
        }
        Op::Redor => {
            let inner = get(0)?;
            let zero = z3::ast::BV::from_u64(0, inner.get_size());
            Ok(bool_to_bv1(inner.eq(&zero).not()))
        }
        Op::Redxor => {
            // Parity bit. For an n-bit value v, redxor = v[0] xor v[1] xor ... xor v[n-1].
            let inner = get(0)?;
            let n = inner.get_size();
            let mut acc = inner.extract(0, 0);
            for i in 1..n {
                let bit = inner.extract(i, i);
                acc = acc.bvxor(&bit);
            }
            Ok(acc)
        }
        // ite (3-operand)
        Op::Ite => {
            let cond = get(0)?;
            let then_bv = get(1)?;
            let else_bv = get(2)?;
            let zero = z3::ast::BV::from_u64(0, cond.get_size());
            let cond_bool = cond.eq(&zero).not();
            Ok(cond_bool.ite(&then_bv, &else_bv))
        }
        // Unsupported in Phase A.4 BvOnly — array ops + divisions +
        // overflow detectors. Match the bit-blaster's behaviour:
        // refuse with a clear error.
        Op::Sdiv | Op::Udiv | Op::Smod | Op::Srem | Op::Urem => {
            Err(EncodeError::UnsupportedOperator {
                nid,
                op_name: "division/modulo (sdiv/udiv/smod/srem/urem)",
            })
        }
        Op::Saddo | Op::Ssubo | Op::Smulo | Op::Uaddo | Op::Usubo | Op::Umulo | Op::Sdivo => {
            Err(EncodeError::UnsupportedOperator {
                nid,
                op_name: "overflow detector",
            })
        }
        Op::Read | Op::Write => Err(EncodeError::ArraySortUnsupportedInBvOnly { nid }),
    }
}

// Suppress the unused-arg warning emitted by `Sort` checks in the
// non-array paths; the encoder doesn't need the sort node itself
// since the parser already validated widths.
#[allow(dead_code)]
fn _sort_check(_s: &Sort) {}

// ─────────────────────────────────────────────────────────────────────
// §Phase 10 Option-4 step 1c.1 (2026-06-12) — Z3Backend: the SMT
// instantiation of `BvTermBackend`.
// ─────────────────────────────────────────────────────────────────────

/// The Z3 (SMT) instantiation of
/// [`crate::adapter::btor2::term_backend::BvTermBackend`], over
/// `Value = z3::ast::BV`.
///
/// Delegates operator evaluation to the existing [`encode_op`] —
/// which reads operands through [`eval_operand`]'s cache. Because
/// the generic `walk_design` driver visits nodes in declaration
/// (topological) order and binds each into the backend's cache, by
/// the time `encode_op` runs for a node every operand is ALREADY
/// cached, so `eval_operand` returns the cached term WITHOUT
/// recursing. This is how the forward-bind walk reconciles with
/// `encode_design`'s recursive-on-demand resolution: same
/// `encode_op`, same cached operands, same terms — proven by the
/// `z3backend_transition_matches_encode_design` test.
///
/// **The walk's `Value` is `BV`** — and stays so through array
/// support (step 1c.2). Array values are never bound as walk values:
/// the `walk_design` driver skips array-sorted op nodes, and the
/// array sub-DAG is resolved on-demand inside [`Z3Backend::transition`]
/// via [`eval_array_operand`] (which self-caches into `array_cache`).
/// `Op::Read` results ARE BVs (`select` yields an element), so they
/// flow through the BV cache normally. The plan's anticipated
/// `Z3Term { Bv, Arr }` enum proved unnecessary: forcing arrays into
/// the walk's value type would route array nodes through the trait
/// for no functional gain — the on-demand path produces identical Z3
/// terms (proven by XOR-UNSAT vs `encode_design_with_theory(BvUfArray)`
/// in `z3backend_transition_matches_uf_array`).
///
/// Constructed from an already-encoded [`Btor2SmtView`]
/// ([`Z3Backend::from_view`]) so it reuses the view's variable
/// declarations (pass-1). [`encode_design_with_theory`] drives it as
/// its pass-2 (step 1c.3 cutover): it assembles the view with a
/// placeholder transition, builds the backend over those decls, runs
/// [`walk_design`], and overwrites the placeholder with
/// [`Z3Backend::transition`]'s result. The equivalence tests isolate
/// the *forward-walk term construction* against the prior inline
/// recursive pass-2 (XOR-UNSAT).
pub(crate) struct Z3Backend<'a> {
    file: &'a Btor2File,
    state_curr: HashMap<Nid, z3::ast::BV>,
    inputs: HashMap<Nid, z3::ast::BV>,
    state_curr_arr: HashMap<Nid, z3::ast::Array>,
    cache: HashMap<Nid, z3::ast::BV>,
    /// §Phase 10 step 1c.2 — array sub-DAG cache for the on-demand
    /// `eval_array_operand` resolution in `transition()`. Empty for
    /// BV-only designs.
    array_cache: HashMap<Nid, z3::ast::Array>,
    /// §Phase 10 step 1d.1 — Op NIDs to abstract as uninterpreted
    /// functions (real `z3::FuncDecl`). Empty in the production
    /// must-edge path (`from_view`): the SMT backend then evaluates
    /// every operator precisely, which is the only sound choice for
    /// must-edges. A non-empty set is the **may-only** UF
    /// over-approximation — populate it via [`Z3Backend::with_uf_wrapped`].
    uf_wrapped: HashSet<Nid>,
    /// §Phase 10 step 1d.1 — per-signature uninterpreted-function
    /// registry, keyed by `uf_<op>_<arg-widths>_<result-width>`. One
    /// `FuncDecl` per signature so that equal operator applications
    /// across the design share the SAME function symbol — Z3's
    /// functional-consistency axiom (`f(x) == f(x)`) is then the only
    /// thing the abstracted operator is known to satisfy. Wrapped in
    /// `Rc` because `z3::FuncDecl` is not `Clone` (and to guarantee
    /// object identity for consistency rather than relying on Z3's
    /// name interning).
    uf_decls: HashMap<String, std::rc::Rc<z3::FuncDecl>>,
}

impl<'a> Z3Backend<'a> {
    /// Build a Z3 backend reusing an encoded view's current-step
    /// variable declarations. The cache starts empty; `walk_design`
    /// fills it with the per-node terms.
    pub(crate) fn from_view(file: &'a Btor2File, view: &Btor2SmtView) -> Self {
        Self {
            file,
            state_curr: view.state_curr.clone(),
            inputs: view.inputs.clone(),
            state_curr_arr: view.state_curr_arr.clone(),
            cache: HashMap::new(),
            array_cache: HashMap::new(),
            uf_wrapped: HashSet::new(),
            uf_decls: HashMap::new(),
        }
    }

    /// §Phase 10 step 1d.1 — enable real-UF abstraction for the given
    /// Op NIDs (the **may-side** over-approximation). When set, the
    /// walk replaces each listed operator node with an uninterpreted
    /// `FuncDecl` application instead of its precise BV semantics.
    ///
    /// **Soundness (may-only).** A `FuncDecl` knows only functional
    /// consistency (`f(x) == f(x)`); it admits *more* behaviours than
    /// the concrete operator, so it is a sound over-approximation for
    /// may-edges but an **unsound** must-witness. The production
    /// must-edge path ([`from_view`] alone) leaves this empty, so
    /// must-edges stay precise. This builder is the may-side capability
    /// (the consumer wiring is a follow-up); it replaces the concrete
    /// Zero/Ones representative hack with a proper EUF over-approximation.
    // Staged-API: test-pinned in step 1d.1 (the over-approximation /
    // consistency test is the only consumer). The production may-path
    // wiring — `predicate_cube_lift` running the Z3 backend in may-mode
    // with UF — is step 1d.2, at which point the allow is removed.
    #[allow(dead_code)]
    pub(crate) fn with_uf_wrapped(mut self, uf_wrapped: HashSet<Nid>) -> Self {
        self.uf_wrapped = uf_wrapped;
        self
    }

    /// §Phase 10 step 1d.1 — fetch (or lazily build + cache) the
    /// uninterpreted function symbol for an operator of the given
    /// signature. Keyed by `uf_<op>_<arg-widths>_<result-width>` so
    /// that two operator applications with the same signature share a
    /// symbol — the cross-design functional-consistency guarantee.
    fn uf_decl_for(
        &mut self,
        op: Op,
        args: &[z3::ast::BV],
        width: u32,
    ) -> std::rc::Rc<z3::FuncDecl> {
        let arg_widths: Vec<String> = args.iter().map(|b| b.get_size().to_string()).collect();
        let name = format!("uf_{op:?}_{}_{width}", arg_widths.join("x"));
        if let Some(fd) = self.uf_decls.get(&name) {
            return fd.clone();
        }
        let domain_sorts: Vec<z3::Sort> = args
            .iter()
            .map(|b| z3::Sort::bitvector(b.get_size()))
            .collect();
        let domain_refs: Vec<&z3::Sort> = domain_sorts.iter().collect();
        let range = z3::Sort::bitvector(width);
        let fd = std::rc::Rc::new(z3::FuncDecl::new(name.as_str(), &domain_refs, &range));
        self.uf_decls.insert(name, fd.clone());
        fd
    }

    /// Resolve a BV operand from the backend's env (cache, then
    /// state, then input), applying BTOR2 negative-NID negation.
    fn resolve_bv(&self, op: Operand) -> Option<z3::ast::BV> {
        let nid = op.nid();
        let v = self
            .cache
            .get(&nid)
            .or_else(|| self.state_curr.get(&nid))
            .or_else(|| self.inputs.get(&nid))
            .cloned()?;
        Some(if op.is_negated() { v.bvnot() } else { v })
    }

    /// Build the transition relation from the post-`walk_design` cache,
    /// mirroring pass-2 of [`encode_design_with_theory`]. Each
    /// `Node::Next` over a BV state cell contributes
    /// `s_next == eval(value)` (the `value` operand is resolved through
    /// [`eval_operand`], which hits the cache the forward walk
    /// pre-populated, so no recursion happens for already-bound nodes);
    /// each `Node::Next` over an array (memory) state cell contributes
    /// the extensional array equality `a_next == eval_array(value)`
    /// (resolved on-demand through [`eval_array_operand`], which builds
    /// the `store`/`ite` array sub-DAG and self-caches into
    /// `array_cache`). Reads the `state_next` / `state_next_arr` handles
    /// from `view` (the backend does not own the next-step decls).
    ///
    /// §Phase 10 step 1c.2 — array-`next` support. The walk itself
    /// stays BV-valued (it skips array-sorted op nodes); arrays appear
    /// only here, as the RHS of the array-equality conjuncts.
    #[allow(clippy::type_complexity)]
    pub(crate) fn transition(
        &mut self,
        view: &Btor2SmtView,
    ) -> Result<
        (
            z3::ast::Bool,
            HashMap<Nid, z3::ast::BV>,
            HashMap<Nid, z3::ast::Array>,
        ),
        EncodeError,
    > {
        // mununu#512 — record the next-state EXPRESSIONS alongside the equalities.
        // They are already computed here to constrain the fresh `state_next*`
        // variables; exposing them lets a query substitute the next state instead of
        // quantifying over it.
        let mut next_expr: HashMap<Nid, z3::ast::BV> = HashMap::new();
        let mut next_arr_expr: HashMap<Nid, z3::ast::Array> = HashMap::new();
        let array_curr = if self.state_curr_arr.is_empty() {
            None
        } else {
            Some(&self.state_curr_arr)
        };
        let mut conjuncts: Vec<z3::ast::Bool> = Vec::new();
        for line in &self.file.lines {
            if let Node::Next { state, value, .. } = &line.node {
                // Array-sorted next: extensional array equality. Checked
                // first (mirrors `encode_design_with_theory`).
                if let Some(arr_next) = view.state_next_arr.get(state) {
                    let value_arr = eval_array_operand(
                        *value,
                        self.file,
                        &self.state_curr,
                        &self.inputs,
                        &self.state_curr_arr,
                        &mut self.cache,
                        &mut self.array_cache,
                    )?;
                    next_arr_expr.insert(*state, value_arr.clone());
                    conjuncts.push(arr_next.eq(&value_arr));
                    continue;
                }
                // BV-sorted next.
                let value_bv = eval_operand(
                    *value,
                    self.file,
                    &self.state_curr,
                    &self.inputs,
                    array_curr,
                    &mut self.cache,
                )?;
                let next_bv = view
                    .state_next
                    .get(state)
                    .ok_or(EncodeError::WidthMismatch {
                        nid: line.nid,
                        state_width: 0,
                        operand_width: value_bv.get_size(),
                    })?
                    .clone();
                if next_bv.get_size() != value_bv.get_size() {
                    return Err(EncodeError::WidthMismatch {
                        nid: line.nid,
                        state_width: next_bv.get_size(),
                        operand_width: value_bv.get_size(),
                    });
                }
                next_expr.insert(*state, value_bv.clone());
                conjuncts.push(next_bv.eq(&value_bv));
            }
        }
        let t = if conjuncts.is_empty() {
            z3::ast::Bool::from_bool(true)
        } else {
            let refs: Vec<&z3::ast::Bool> = conjuncts.iter().collect();
            z3::ast::Bool::and(&refs)
        };
        Ok((t, next_expr, next_arr_expr))
    }
}

impl crate::adapter::btor2::term_backend::BvTermBackend for Z3Backend<'_> {
    type Value = z3::ast::BV;
    type Error = EncodeError;

    fn eval_const(&mut self, value: &ConstValue, width: u32) -> Result<z3::ast::BV, EncodeError> {
        // `nid` is used only for ConstantOutOfRange diagnostics; the
        // walk does not thread the const node's nid into eval_const,
        // so use a 0 sentinel (well-formed constants never error).
        encode_const_bits(0, value, width)
    }

    fn eval_op(
        &mut self,
        nid: Nid,
        op: Op,
        immediates: &[u32],
        args: &[Operand],
        width: u32,
    ) -> Result<z3::ast::BV, EncodeError> {
        let array_curr = if self.state_curr_arr.is_empty() {
            None
        } else {
            Some(&self.state_curr_arr)
        };
        encode_op(
            nid,
            op,
            args,
            immediates,
            width,
            self.file,
            &self.state_curr,
            &self.inputs,
            array_curr,
            &mut self.cache,
        )
    }

    fn bind(&mut self, nid: Nid, value: z3::ast::BV) {
        self.cache.insert(nid, value);
    }

    fn honor_init(&self) -> bool {
        // The transition relation is built from `next` lines, not
        // `init` (init is a separate constraint in the SMT model).
        // So the walk never copies init values — matching
        // `encode_design`, which ignores `Init` nodes.
        false
    }

    fn read_operand(&self, op: Operand) -> Option<z3::ast::BV> {
        // Never invoked by `walk_design` (honor_init is false), but
        // implemented for completeness + future init-constraint use.
        self.resolve_bv(op)
    }

    fn uf_substitute(&mut self, nid: Nid, width: u32) -> Option<z3::ast::BV> {
        // §Phase 10 step 1d.1 — real UF via z3::FuncDecl. Only fires
        // for NIDs the caller declared UF-wrapped (the may-only
        // over-approximation); the production must-edge path leaves
        // `uf_wrapped` empty, so this returns None and the operator is
        // evaluated precisely (the sound choice for must-edges).
        if !self.uf_wrapped.contains(&nid) {
            return None;
        }
        // Resolve the operator + its operands (cloned so the immutable
        // `file` borrow drops before the `&mut self` FuncDecl build).
        let (op, args): (Op, Vec<Operand>) = match &self.file.lookup(nid)?.node {
            Node::Op { op, args, .. } => (*op, args.clone()),
            _ => return None,
        };
        // Operands are already bound by the forward walk (topological
        // order), so they resolve from the env without recursion.
        let arg_bvs: Vec<z3::ast::BV> = args
            .iter()
            .map(|a| self.resolve_bv(*a))
            .collect::<Option<_>>()?;
        let fd = self.uf_decl_for(op, &arg_bvs, width);
        let arg_refs: Vec<&dyn z3::ast::Ast> =
            arg_bvs.iter().map(|b| b as &dyn z3::ast::Ast).collect();
        fd.apply(&arg_refs).as_bv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser::parse;

    #[test]
    fn encode_safety_demo_btor() {
        let src = include_str!("../../../../../../examples/btor2/safety_demo.btor");
        let file = parse(src).expect("parse safety_demo");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode safety_demo");
            // safety_demo has 3 state cells (cnt + 2 anonymous);
            // 2 inputs (rst, clk).
            assert!(
                view.signals
                    .iter()
                    .any(|s| s.symbol.as_deref() == Some("cnt"))
            );
            assert!(
                view.signals
                    .iter()
                    .any(|s| s.symbol.as_deref() == Some("rst"))
            );
            assert!(
                view.signals
                    .iter()
                    .any(|s| s.symbol.as_deref() == Some("clk"))
            );
        });
    }

    #[test]
    fn encode_cap_overflow_btor() {
        let src = include_str!(
            "../../../../../../examples/verify/bench_predicate_image_a4/adversarial/cap_overflow.btor"
        );
        let file = parse(src).expect("parse cap_overflow");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode cap_overflow");
            assert_eq!(
                view.signals
                    .iter()
                    .filter(|s| s.kind == SignalKind::State)
                    .count(),
                1
            );
            // `cnt` should be 8 bits.
            let cnt = view
                .signals
                .iter()
                .find(|s| s.symbol.as_deref() == Some("cnt"))
                .expect("cnt signal present");
            assert_eq!(cnt.width, 8);
        });
    }

    #[test]
    fn encode_sparse_predicates_btor() {
        let src = include_str!(
            "../../../../../../examples/verify/bench_predicate_image_a4/adversarial/sparse_predicates.btor"
        );
        let file = parse(src).expect("parse sparse_predicates");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode sparse_predicates");
            let s = view
                .signals
                .iter()
                .find(|sig| sig.symbol.as_deref() == Some("s"))
                .expect("s signal present");
            assert_eq!(s.width, 3);
            assert_eq!(s.kind, SignalKind::State);
        });
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 stage 3.c.1 — Z3 Array theory (QF_AUFBV) encoding
    // ─────────────────────────────────────────────────────────────

    use super::super::theory::Theory;

    /// A tiny memory design: a single array-sorted state cell `mem`
    /// (5-bit address, 8-bit data), one ite-on-array next (no-op
    /// write), one read. Mirrors `bit_blast.rs`'s
    /// PHASE10_FIXTURE_WITH_MEMORY.
    const MEM_BTOR_READ_ONLY: &str = r#"
1 sort bitvec 1
2 sort bitvec 5
3 sort bitvec 8
4 sort array 2 3
5 state 4 mem
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

    /// A memory design that performs an actual `write` (store) then
    /// reads back the same address into an observed BV register. The
    /// array functional-consistency axiom forces
    /// `read(store(mem, a, v), a) == v`.
    const MEM_BTOR_WRITE_THEN_READ: &str = r#"
1 sort bitvec 1
2 sort bitvec 5
3 sort bitvec 8
4 sort array 2 3
5 state 4 mem
6 state 3 observed
7 input 2 a
8 input 3 v
9 write 4 5 7 8
10 next 4 5 9
11 read 3 9 7
12 next 3 6 11
"#;

    #[test]
    fn bvonly_rejects_array_design() {
        // Regression guard: the BvOnly path (and the `encode_design`
        // delegator) must STILL reject array-sorted designs, exactly
        // as before stage 3.c.1.
        let file = parse(MEM_BTOR_READ_ONLY).expect("parse mem design");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let result = encode_design(&file);
            assert!(
                matches!(
                    result.err(),
                    Some(EncodeError::ArraySortUnsupportedInBvOnly { .. })
                ),
                "BvOnly must reject array-sorted state cells with ArraySortUnsupportedInBvOnly"
            );
        });
    }

    #[test]
    fn bvufarray_encodes_array_design() {
        // Under BvUfArray the same design encodes successfully.
        let file = parse(MEM_BTOR_READ_ONLY).expect("parse mem design");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_with_theory(&file, Theory::BvUfArray)
                .expect("BvUfArray must encode the array design");
            // The memory cell is tracked in the array maps, not the
            // BV `signals` vector.
            assert_eq!(view.state_curr_arr.len(), 1, "one array state cell");
            assert_eq!(view.state_next_arr.len(), 1);
            // The BV input registers are still present.
            assert!(view.inputs.values().count() >= 3, "we + addr + wdata");
        });
    }

    // H.E.1 — a named combinational node `g = q & en` (NID 4). The encode must
    // retain its current-cycle BV in `signal_bvs` and register it as a
    // `SignalKind::Combinational` signal.
    const COMB_BTOR2: &str =
        "1 sort bitvec 1\n2 state 1 q\n3 input 1 en\n4 and 1 2 3 g\n5 next 1 2 3\n";

    #[test]
    fn h_e_1_retains_combinational_signal_bv_and_metadata() {
        let file = parse(COMB_BTOR2).expect("parse combinational fixture");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_with_theory(&file, Theory::BvOnly).expect("encode");
            // The combinational node `g` (NID 4) has a retained current-cycle BV.
            assert!(
                view.curr_signal(4).is_some(),
                "combinational node g (nid 4) must be in signal_bvs"
            );
            // It is registered as a Combinational signal with its symbol + width.
            let g = view
                .signals
                .iter()
                .find(|s| s.symbol.as_deref() == Some("g"))
                .expect("g registered in signals");
            assert_eq!(g.kind, SignalKind::Combinational);
            assert_eq!(g.nid, 4);
            assert_eq!(g.width, 1);
            // State / input nodes keep their canonical kinds (not reclassified).
            assert!(
                view.signals
                    .iter()
                    .any(|s| s.symbol.as_deref() == Some("q") && s.kind == SignalKind::State),
                "q stays State"
            );
            assert!(
                view.signals
                    .iter()
                    .any(|s| s.symbol.as_deref() == Some("en") && s.kind == SignalKind::Input),
                "en stays Input"
            );
        });
    }

    #[test]
    fn bvufarray_read_after_write_is_forced_by_array_axiom() {
        // The transition relation must FORCE
        // `observed_next == v` because
        // `read(store(mem, a, v), a) == v` by Z3's extensional
        // array axiom. We prove this by asserting the transition
        // AND `observed_next != v` and checking UNSAT.
        let file = parse(MEM_BTOR_WRITE_THEN_READ).expect("parse write-then-read");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_with_theory(&file, Theory::BvUfArray)
                .expect("encode write-then-read");

            // Locate the `observed` state cell's next BV (NID 6) and
            // the `v` input BV (NID 8).
            let observed_next = view
                .state_next
                .get(&6)
                .expect("observed next BV present")
                .clone();
            let v_input = view.inputs.get(&8).expect("v input BV present").clone();

            let solver = z3::Solver::new();
            solver.assert(&view.transition);
            // Negation of the array-consistency consequence.
            solver.assert(observed_next.eq(&v_input).not());
            assert_eq!(
                solver.check(),
                z3::SatResult::Unsat,
                "read(store(mem,a,v),a) != v must be UNSAT under array theory"
            );
        });
    }

    #[test]
    fn bvufarray_read_after_write_equality_is_sat() {
        // Dual of the above: the transition AND
        // `observed_next == v` must be SAT (the axiom is satisfiable,
        // not vacuous).
        let file = parse(MEM_BTOR_WRITE_THEN_READ).expect("parse write-then-read");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_with_theory(&file, Theory::BvUfArray)
                .expect("encode write-then-read");
            let observed_next = view.state_next.get(&6).expect("observed next").clone();
            let v_input = view.inputs.get(&8).expect("v input").clone();

            let solver = z3::Solver::new();
            solver.assert(&view.transition);
            solver.assert(observed_next.eq(&v_input));
            assert_eq!(
                solver.check(),
                z3::SatResult::Sat,
                "read(store(mem,a,v),a) == v must be SAT (axiom not vacuous)"
            );
        });
    }

    #[test]
    fn array_widths_resolves_index_and_element() {
        // The array_widths helper must resolve both sort widths from
        // a `sort array <idx> <elem>` declaration.
        let file = parse(MEM_BTOR_READ_ONLY).expect("parse");
        // Sort NID 4 is `sort array 2 3` → index sort 2 (5-bit),
        // element sort 3 (8-bit).
        let (idx_w, elem_w) = array_widths(&file, 4).expect("array_widths resolves NID 4");
        assert_eq!(idx_w, 5);
        assert_eq!(elem_w, 8);
        // A bitvec sort NID returns None.
        assert!(array_widths(&file, 3).is_none());
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 Option-4 step 1c.1 — Z3Backend transition-equivalence
    // ─────────────────────────────────────────────────────────────
    // (`walk_design` is in scope via `use super::*` — the production
    // pass-2 imports it at module level since the step 1c.3 cutover.)

    /// Drive `walk_design::<Z3Backend>` over `src` and assert the
    /// transition relation it builds is SEMANTICALLY EQUIVALENT to the
    /// one `encode_design`'s recursive path builds. Both run over the
    /// SAME Z3 variable declarations (the backend reuses
    /// `encode_design`'s view), so the only thing under test is whether
    /// the forward-bind walk reconstructs the same per-node terms as
    /// the recursive resolution. `z3::ast::BV` has no structural `==`,
    /// so equivalence is checked by UNSAT of `ref XOR walk`.
    fn assert_z3backend_matches_encode_design(src: &str, name: &str) {
        let file = parse(src).unwrap_or_else(|e| panic!("parse {name}: {e:?}"));
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            // Reference: the recursive-on-demand encoder.
            let ref_view = encode_design(&file).unwrap_or_else(|e| panic!("encode {name}: {e:?}"));
            // Backend: forward-bind walk over the same variable decls.
            let mut backend = Z3Backend::from_view(&file, &ref_view);
            walk_design(&file, &mut backend)
                .unwrap_or_else(|e| panic!("walk_design {name}: {e:?}"));
            let walk_transition = backend
                .transition(&ref_view)
                .unwrap_or_else(|e| panic!("backend.transition {name}: {e:?}"))
                .0;

            // The two Bools reference identical Z3 consts (same names);
            // they are equivalent iff `ref XOR walk` is UNSAT.
            let solver = z3::Solver::new();
            solver.assert(ref_view.transition.xor(&walk_transition));
            assert_eq!(
                solver.check(),
                z3::SatResult::Unsat,
                "Z3Backend transition for {name} must match encode_design's bit-for-bit (XOR UNSAT)"
            );
        });
    }

    #[test]
    fn z3backend_transition_matches_encode_design_safety_demo() {
        assert_z3backend_matches_encode_design(
            include_str!("../../../../../../examples/btor2/safety_demo.btor"),
            "safety_demo",
        );
    }

    #[test]
    fn z3backend_transition_matches_encode_design_cap_overflow() {
        assert_z3backend_matches_encode_design(
            include_str!(
                "../../../../../../examples/verify/bench_predicate_image_a4/adversarial/cap_overflow.btor"
            ),
            "cap_overflow",
        );
    }

    #[test]
    fn z3backend_transition_matches_encode_design_sparse_predicates() {
        assert_z3backend_matches_encode_design(
            include_str!(
                "../../../../../../examples/verify/bench_predicate_image_a4/adversarial/sparse_predicates.btor"
            ),
            "sparse_predicates",
        );
    }

    /// §Phase 10 step 1c.2 — array equivalence. `MEM_BTOR_WRITE_THEN_READ`
    /// exercises BOTH next kinds (array-next `next mem = write(...)` and
    /// BV-next `next observed = read(...)`) plus `Op::Write` + `Op::Read`.
    /// The walk skips the array-sorted `write` op node; `transition()`
    /// rebuilds it on-demand via `eval_array_operand`. The result must
    /// match `encode_design_with_theory(BvUfArray)`'s recursive pass-2
    /// bit-for-bit (XOR UNSAT). Reference must be the BvUfArray view
    /// (BvOnly rejects the array sort).
    #[test]
    fn z3backend_transition_matches_uf_array() {
        let file = parse(MEM_BTOR_WRITE_THEN_READ).expect("parse mem write-then-read");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let ref_view =
                encode_design_with_theory(&file, Theory::BvUfArray).expect("encode BvUfArray");
            let mut backend = Z3Backend::from_view(&file, &ref_view);
            walk_design(&file, &mut backend).expect("walk_design mem");
            let walk_transition = backend
                .transition(&ref_view)
                .expect("backend.transition mem")
                .0;
            let solver = z3::Solver::new();
            solver.assert(ref_view.transition.xor(&walk_transition));
            assert_eq!(
                solver.check(),
                z3::SatResult::Unsat,
                "Z3Backend transition for the memory design must match encode_design_with_theory(BvUfArray) (XOR UNSAT)"
            );
        });
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 step 1d.1 — real UF (z3::FuncDecl) in Z3Backend
    // ─────────────────────────────────────────────────────────────

    /// `x * 0`: the precise BV semantics force the product to 0, but
    /// real UF (a `FuncDecl`) does NOT know that — it admits a nonzero
    /// product (sound MAY over-approximation), while staying
    /// functionally consistent (two `x*0` nodes share the same
    /// uninterpreted symbol, so they are forced equal). Together these
    /// two assertions fully characterise real UF: uninterpreted
    /// (over-approximating) yet consistent.
    #[test]
    fn z3backend_real_uf_over_approximates_and_is_consistent() {
        // Two duplicate `x * 0` mul nodes (NIDs 4, 5) feeding two
        // separate state cells s1 (6) / s2 (7).
        let src = r#"
1 sort bitvec 3
2 input 1 x
3 zero 1
4 mul 1 2 3
5 mul 1 2 3
6 state 1 s1
7 state 1 s2
8 next 1 6 4
9 next 1 7 5
"#;
        let file = parse(src).expect("parse uf-mul fixture");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let zero3 = z3::ast::BV::from_u64(0, 3);

            // Baseline: precise (no UF) forces s1_next == 0.
            let precise = encode_design(&file).expect("encode precise");
            {
                let solver = z3::Solver::new();
                solver.assert(&precise.transition);
                let s1_next = precise.state_next.get(&6).expect("s1_next");
                solver.assert(s1_next.eq(&zero3).not());
                assert_eq!(
                    solver.check(),
                    z3::SatResult::Unsat,
                    "precise x*0 must force s1_next == 0 (UNSAT of != 0)"
                );
            }

            // Real UF: wrap both mul nodes.
            let uf_view = encode_design(&file).expect("encode for uf");
            let mut backend =
                Z3Backend::from_view(&file, &uf_view).with_uf_wrapped(HashSet::from([4, 5]));
            walk_design(&file, &mut backend).expect("walk uf");
            let uf_transition = backend.transition(&uf_view).expect("uf transition").0;
            let s1_next = uf_view.state_next.get(&6).expect("s1_next");
            let s2_next = uf_view.state_next.get(&7).expect("s2_next");

            // (a) Over-approximation — f_mul(x,0) may be nonzero.
            {
                let solver = z3::Solver::new();
                solver.assert(&uf_transition);
                solver.assert(s1_next.eq(&zero3).not());
                assert_eq!(
                    solver.check(),
                    z3::SatResult::Sat,
                    "real UF must admit f_mul(x,0) != 0 (over-approximation)"
                );
            }
            // (b) Functional consistency — both `x*0` share f_mul, so
            // s1_next == s2_next is forced.
            {
                let solver = z3::Solver::new();
                solver.assert(&uf_transition);
                solver.assert(s1_next.eq(s2_next).not());
                assert_eq!(
                    solver.check(),
                    z3::SatResult::Unsat,
                    "functional consistency: f_mul(x,0) == f_mul(x,0) (UNSAT of !=)"
                );
            }
        });
    }

    // ─────────────────────────────────────────────────────────────
    // §Phase 10 step 1d.3 — ibex register-file milestone (real RTL)
    // ─────────────────────────────────────────────────────────────

    /// **Milestone.** Read-after-write on the REAL ibex register file
    /// (`ibex_register_file_fpga`, lowRISC/ibex, Apache-2.0), at the
    /// RV32E=1 / DataWidth=4 instance → a 16×4 `$mem` array. The BTOR2
    /// fixture is the verbatim Yosys output (sv2v-free; SV → yosys
    /// `memory_collect` → `write_btor`), checked in under `tests/data`
    /// as a generated artifact (the RTL itself is NOT vendored, per the
    /// plan's §10.3).
    ///
    /// The defining property: when `we_a_i` writes `wdata_a_i` to a
    /// nonzero address, the post-write memory reads that value back at
    /// that address. This is a transition-relation property — the array
    /// functional-consistency axiom `read(store(m,A,v),A) == v` forces
    /// it — and mununu's `BvUfArray` encoder (now driven through
    /// `walk_design::<Z3Backend>` after the 1c.3 cutover) proves it:
    /// the *violation* is UNSAT.
    ///
    /// Cross-checked independently by `yosys-smtbmc -s z3` on the same
    /// RTL (see the example README); a divergence would be a soundness
    /// bug in the encoder.
    #[test]
    fn ibex_regfile_fpga_read_after_write_holds() {
        let src = include_str!("../../../../tests/data/ibex_register_file_fpga_16x4.btor2");
        let file = parse(src).expect("parse ibex regfile btor2");

        // The design lifts to exactly one inferred `$mem` array.
        let mems = crate::adapter::btor2::bit_blast::detect_btor2_memories(&file);
        assert_eq!(mems.len(), 1, "ibex_register_file_fpga has one $mem array");

        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design_with_theory(&file, Theory::BvUfArray)
                .expect("encode ibex regfile under BvUfArray");

            // Resolve the write-port inputs by symbol (robust to NID
            // renumbering if the fixture is regenerated).
            let nid = |sym: &str| {
                view.signals
                    .iter()
                    .find(|s| s.symbol.as_deref() == Some(sym))
                    .map(|s| s.nid)
                    .unwrap_or_else(|| panic!("signal {sym} not found"))
            };
            let we = view.inputs.get(&nid("we_a_i")).expect("we_a_i");
            let waddr = view.inputs.get(&nid("waddr_a_i")).expect("waddr_a_i"); // 5-bit
            let wdata = view.inputs.get(&nid("wdata_a_i")).expect("wdata_a_i"); // 4-bit

            // The single array state cell's next-step handle.
            let next_mem = view
                .state_next_arr
                .values()
                .next()
                .expect("one next-step array");

            // read(next_mem, waddr[3:0]) — the array index sort is 4-bit.
            let waddr4 = waddr.extract(3, 0);
            let read_back = next_mem
                .select(&waddr4)
                .as_bv()
                .expect("array element is a bit-vector");

            // Violation of read-after-write must be UNSAT: there is NO
            // state/input where a write of `wdata` to a nonzero address
            // fails to read `wdata` back next step. (Address 0 is
            // hard-wired to zero in the regfile, so the property is
            // conditioned on `waddr_a_i != 0`, matching the RTL's
            // write-enable carve-out.)
            let solver = z3::Solver::new();
            solver.assert(&view.transition);
            solver.assert(we.eq(z3::ast::BV::from_u64(1, 1)));
            solver.assert(waddr.eq(z3::ast::BV::from_u64(0, 5)).not());
            solver.assert(read_back.eq(wdata).not());
            assert_eq!(
                solver.check(),
                z3::SatResult::Unsat,
                "read-after-write must hold on the ibex regfile \
                 (array axiom forces read(store(mem,A,v),A) == v)"
            );
        });
    }
}
