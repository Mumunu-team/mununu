//! R-F5.3a (2026-07-02) — a **BDD bit-blaster** for the BTOR2 transition
//! function, the first concrete slice of R-F5.3 (symbolic edge construction).
//!
//! # Why this exists
//!
//! The predicate-cube CEGAR path builds its may/must transition relation with
//! `O(2^2|P|)` Z3 queries (`kmts_lift` → `smt_must_edge`), which is the H.H.c
//! "29 min, no output" wall at `|P| ≈ 12`. R-F5's thesis (see
//! [`.claude/plans/r-f5-symbolic-bdd-track.md`]) is that the relation should be
//! built **symbolically** — as a BDD over the register/input bits — by
//! bit-blasting the BTOR2 transition function once, rather than sampled pair by
//! pair with SMT.
//!
//! This module is the foundation for that: it lifts each BTOR2 node to a
//! **bit-vector of BDDs** (`Vec<BDDFunction>`, LSB-first) over one BDD variable
//! per register bit + per input bit, using the same generic
//! [`BvTermBackend`] / [`walk_design`] seam the concrete evaluator and the Z3
//! encoder already share. The per-op BDD gadgets (ripple-carry adder, unsigned
//! comparator, mux, sign/zero extension, …) mirror the concrete
//! [`eval_op`](super::bit_blast) semantics bit-for-bit.
//!
//! # Correctness gate
//!
//! The bit-blaster is validated **cell-for-cell against the concrete
//! simulator** [`simulate_one_step`](super::bit_blast::simulate_one_step): for
//! every reachable (register, input) assignment, restricting the symbolic
//! next-state BDDs to that assignment must equal the concrete next state. The
//! differential is exhaustive on the small hand-built fixtures below, so a
//! semantic drift in any gadget is caught immediately — the oracle is the
//! same one the explicit path trusts.
//!
//! # Scope (R-F5.3a)
//!
//! Covers the FSM + counter core op set: `not/and/or/xor/nand/nor/xnor`,
//! `inc/dec/neg`, `add/sub`, `eq/neq/iff/implies`, unsigned compares
//! (`ult/ulte/ugt/ugte`), reductions (`redor/redand/redxor`),
//! `concat/slice/uext/sext`, and `ite`. Multiplication, division, variable
//! shifts, signed compares, and array ops are rejected with a clear error —
//! they are later R-F5.3 slices (the FSM+counter residual the H.H.c ceiling
//! needs does not use them).
//!
//! # R-F5.3b — predicate BDDs
//!
//! [`BddBitBlaster::predicate_bdd`] compiles a
//! [`PredicateExpr`](crate::adapter::btor2::predicate_expr::PredicateExpr) into
//! a single BDD over the register-bit variables — the characteristic function
//! of the register valuations that satisfy the predicate. Comparisons reuse the
//! R-F5.3a `eq_bits` / `uge` gadgets at a common (zero-extended) width, so the
//! BDD matches `PredicateExpr::eval` bit-for-bit — the differential oracle.
//! This is the bridge to predicate cubes: each `P_i` is one BDD, so a cube is
//! the conjunction of the `⟦P_i⟧` / `¬⟦P_i⟧`.
//!
//! # R-F5.3c — the abstract may relation (the H.H.c unblock)
//!
//! [`BddBitBlaster::abstract_may_relation`] builds the abstract **may**
//! transition relation over predicate cubes as one BDD over `2k` predicate
//! variables — `p_i` (present) and `p'_i` (next):
//!
//! ```text
//! A(x, p)      = ⋀_i (p_i  ⟺ ⟦P_i⟧(x))
//! A'(x, i, p') = ⋀_i (p'_i ⟺ ⟦P_i⟧(next(x, i)))
//! R_may(p, p') = ∃ (x ∪ i). A(x, p) ∧ A'(x, i, p')
//! ```
//!
//! `⟦P_i⟧(next(x, i))` is the R-F5.3b predicate BDD with every register bit
//! `substitute`d by that register's bit-blasted next-state function (held
//! registers keep their present var; inputs stay free and are quantified out
//! by the `apply_exists`). This is one `substitute` + one `apply_exists` per
//! predicate — **construction cost, not the `O(2^2|P|)` per-cube-pair SMT** the
//! explicit path pays. Validated cell-for-cell against a brute-force concrete
//! may-relation (`simulate_one_step` over every register + input assignment).
//!
//! # R-F5.3d — the must relation
//!
//! [`BddBitBlaster::abstract_relation`] additionally computes `R_must` under a
//! [`MustSemantics`]: `∀x. (A(x,p) → ∃i. A'(x,i,p'))` (the canonical KMTS
//! `∀∃` must-edge — every concrete state in the source cube has *some* input
//! landing in the target) or the stricter `∀(x∪i). (A → A')` (`∀∀`,
//! deterministic-into-target). Built with OxiDD `exists` / `forall` over the
//! input / register cubes — again no per-cube-pair SMT. Must-edges are
//! restricted to **feasible** source cubes (`∧ ∃x. A(x,p)`) so `R_must ⊆ R_may`
//! holds globally over the whole `2^k` cube space (an infeasible cube is
//! vacuously a ∀-must-edge to everything but has no may-edge; the lift never
//! materialises it). Validated cell-for-cell against the brute-force concrete
//! must-relation + the `R_must ⊆ R_may` invariant.
//!
//! # R-F5.4.1 — the symbolic mu-calculus evaluator
//!
//! [`AbstractRelation::evaluate`] runs the mu-calculus directly over
//! `R_may` / `R_must` by BDD image/preimage (box/diamond via
//! `apply_exists(And,…)` on the predicate-var frame) + μ/ν Kleene fixpoint over
//! the `(must, may)` [`TritBdd`], never materialising cube states. It is the
//! `SymbolicKmts` counterpart sourced from the BTOR2-derived relation rather
//! than a `Clts`; an atomic proposition `P_i`'s characteristic set is simply the
//! present predicate var `p_i`. Supports the audited-sound fragment
//! (`True`/`False`, predicates, `!`/`&&`/`||`, `mu`/`nu`, bare `[]`/`<>`, and —
//! R-F5.5c — `req_cur`/`forb_cur`/`req_next`/`forb_next` state-predicate-guarded
//! modalities); label / controllability / step-bounded guards remain an honest
//! error (out-of-fragment over a predicate cube per
//! [`crate::mu_calculus::cube_modality_soundness_warnings`]). Validated
//! cell-for-cell against `evaluate_tri` on the equivalent explicit cube-KMTS
//! (nested νμ + guarded modalities included). R-F5.4.2 then wires this behind
//! `--engine symbolic`.

use std::collections::{BTreeMap, HashMap};

use oxidd::bdd::{self, BDDFunction, BDDManagerRef};
use oxidd::{BooleanFunction, FunctionSubst, Manager, ManagerRef, Subst, VarNo};

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Op, Operand};
use crate::adapter::btor2::bit_blast::eval_const_value;
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr, parse_predicate_atom_bool};
use crate::adapter::btor2::term_backend::{BvTermBackend, WalkError, walk_design};
// R-F5.4.1 — the symbolic mu-calculus evaluator reuses the (must, may) `TritBdd`
// pair + the crate `Trit` verdict; the mu-calculus `Node` is aliased to avoid a
// clash with the BTOR2 `Node` above.
use crate::mu_calculus::symbolic::TritBdd;
use crate::mu_calculus::trit::Trit;
use crate::mu_calculus::{Control, Formula, FormulaVarId, Guard, ModalKind, Node as MuNode};

/// DEFAULT bit-count cap for the shared [`BddBitBlaster`]: a cheap pre-filter on cone width.
/// Bit count is only a crude upper bound on BDD cost — a ROBDD shares structure, so a
/// wide-but-structured *control* cone (correlated bits) can have a small BDD while a narrow
/// multiplier is exponential. The real robustness gate is now the mid-build **node-budget
/// guard** ([`BddBitBlaster::node_budget`], checked in `eval_op` via `approx_num_inner_nodes`)
/// plus `catch_unwind` in [`exact_symbolic_verdict_with_witness`] — together they turn a BDD
/// blowup into a clean `Skipped` instead of an OoM panic (the 2026-07-06 `prim_esc_receiver`
/// failure). Those make it SAFE to raise the cap.
///
/// The conservative default bit cap when `MUNUNU_BDD_MAX_BITS` is unset — the *floor* the
/// auto-cap never goes below. The common control cone fits well under it, and the tiered arena
/// (below) keeps the ≤ 40-bit path allocation-identical. A concrete cone that modestly exceeds
/// it is admitted automatically up to [`AUTO_CAP_CEILING`]; see [`effective_bitblast_cap`].
const MAX_BITBLAST_BITS: u32 = 40;

/// Auto-cap-management ceiling (verification-execution-planner Phase 4.6 / P2.5-A). With NO explicit
/// `MUNUNU_BDD_MAX_BITS`, a concrete cone up to this width is admitted even though it exceeds the
/// conservative [`MAX_BITBLAST_BITS`] default, so exact decides the CONCRETE model directly (no
/// abstraction) instead of Skipping to the cube.
///
/// **Raised 64 → 192 (2026-07-27, P2.5-A).** The bit COUNT is a poor proxy for BDD tractability —
/// measured, i2c's 173-bit recovery cone bit-blasts to an 11 135-node BDD and exact decides
/// `AG EF (scl_padoen_o==1)` in 242 ms, but the old 64-bit ceiling Skipped it, leaving a ledger `⊥`
/// (a cap ARTIFACT, not intractability). The size gates catch a large *BDD*: the mid-build
/// node-budget guard ([`BddBitBlaster::node_budget`]) bounds node count and — since #813f505 — an
/// arena exhaustion abstains GRACEFULLY (caught, not a crash); the fixpoint-iteration budget
/// ([`fixpoint_iter_budget`], #369) bounds the `2^W`-diameter counter fixpoint.
///
/// **Why 192, and why the cap alone is NOT the safety guard.** Those two size/count gates do NOT
/// bound wall-clock TIME. A deep FREE COUNTER (`twocount32` — two 32-bit counters, 2^32 fixpoint
/// diameter) has a TINY BDD (never trips the node budget) but its EF fixpoint grinds ~1M cheap
/// preimages ≈ 132 min before the iteration budget bails it. And the bit COUNT cannot exclude it:
/// `twocount32`'s cone is 65 bits < i2c's 173, so ANY cap that admits the control cap-artifacts
/// admits the counter too. (Admission is on the cone-bit SUM, not a sort width: the raw-228-bit
/// `ponylink-slaveTXlen-sat` sums to 2870 cone bits and is excluded at EVERY cap — it is never the
/// boundary; the actual hanger is the 65-bit counter.) So the cap can NOT be the tractability gate
/// — the [`ExactModel::deadline`] wall-clock backstop is. 192 is chosen only to admit the measured
/// control cap-artifacts (i2c's 173-bit cone) with modest headroom while keeping the auto-allocated
/// arena bounded; a cone > 192 still needs an explicit `MUNUNU_BDD_MAX_BITS` opt-in (clamped to
/// [`HARD_BITBLAST_CEILING`] = 1024). The residual — a single pathological in-op `apply_exists`
/// (uninterruptible in-process; the BDD-blowup form is caught by the node budget) — needs a thread
/// + `recv_timeout` to bound (P2.5-A-follow).
const AUTO_CAP_CEILING: u32 = 192;

/// Absolute ceiling the `MUNUNU_BDD_MAX_BITS` override is clamped to — bounds the largest
/// arena we will allocate regardless of the env value. Raised 256 → 1024 (2026-07-27): measured
/// that wide-but-STRUCTURED control cones (e.g. i2c's 173-bit cone → an 11k-node BDD) are exact-
/// decidable, and the bit COUNT is a poor proxy for BDD size; the real gates are the node-budget
/// guard and the fixpoint-iteration budget, which now abstain GRACEFULLY on a genuine blow-up
/// (the arena-exhaustion panic is caught in [`BddBitBlaster::build_with_keep`]).
const HARD_BITBLAST_CEILING: u32 = 1024;

/// The default cap for a cone of `cone_bits` KEPT bits when no env override is present: the
/// [`MAX_BITBLAST_BITS`] floor, auto-raised to fit the concrete cone up to [`AUTO_CAP_CEILING`]
/// (never lowered below the floor). Pure — no env read — so the auto-raise thresholds are
/// unit-testable without touching process-global state.
fn default_cap_for_cone(cone_bits: u32) -> u32 {
    MAX_BITBLAST_BITS.max(cone_bits.min(AUTO_CAP_CEILING))
}

/// The effective bit cap for a cone of `cone_bits` KEPT register+input bits. An explicit
/// `MUNUNU_BDD_MAX_BITS` is respected exactly (clamped to [`HARD_BITBLAST_CEILING`]) — the user
/// asked for a specific limit, so we neither raise nor lower it. With NO override, the cap is
/// [`default_cap_for_cone`]: the conservative default auto-raised to admit a modestly-wide
/// concrete cone.
pub(crate) fn effective_bitblast_cap(cone_bits: u32) -> u32 {
    match std::env::var("MUNUNU_BDD_MAX_BITS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    {
        Some(v) => v.min(HARD_BITBLAST_CEILING),
        None => default_cap_for_cone(cone_bits),
    }
}

/// A bit-vector of BDDs, LSB-first: index `b` is the BDD for bit `b`. Every
/// node's value carries exactly `width` bits.
type BitVec = Vec<BDDFunction>;

/// A register or input cell: its symbol, whether it is a `state` (vs `input`),
/// the fresh BDD variables standing for its bits (LSB-first), and their
/// variable numbers. The bit-blaster restricts these to concrete values for
/// the differential; R-F5.3c substitutes register vars with next-state
/// functions via the [`VarNo`]s.
struct Cell {
    symbol: String,
    is_state: bool,
    vars: BitVec,
    varnos: Vec<VarNo>,
}

/// A BDD bit-blaster over one BTOR2 file. Owns one OxiDD manager; every node's
/// value is a `Vec<BDDFunction>` over the register-bit + input-bit variables.
pub struct BddBitBlaster {
    _manager: BDDManagerRef,
    tt: BDDFunction,
    ff: BDDFunction,
    /// `Nid → value` binding store, filled by [`walk_design`].
    env: HashMap<Nid, BitVec>,
    /// Register + input cells, in file order (drives the restriction minterm).
    cells: Vec<Cell>,
    /// State symbol → its next-state BDD vector, from the `Node::Next` lines.
    next_funcs: HashMap<String, BitVec>,
    /// Named COMBINATIONAL signal (module output / named wire) → its BDD vector over the
    /// leaf vars, from `walk_design`. Lets a predicate bind to a combinational output
    /// (`depth_o`, `gnt_o`, …) that is not a state register — the atom's cone still seeds
    /// correctly (`resolve_atom_to_terminals` walks the output to its state/input terminals).
    named_signals: HashMap<String, BitVec>,
    /// The conjunction of every `constraint` line's 1-bit signal (a BDD over the
    /// state + input vars); `tt` when the design has no `constraint`. The exact
    /// model injects it into the modal pre-image so `◇`/`□` range only over
    /// constraint-respecting transitions — modelling BTOR2 `constraint` (assume)
    /// soundly instead of over-approximating.
    constraint_bdd: BDDFunction,
    /// Node-budget guard: if the manager's live inner-node count exceeds this while
    /// building the transition BDDs (`walk_design`, `eval_op`), the build bails with a
    /// clean error (→ `Skipped`) instead of overflowing the fixed OxiDD arena and
    /// panicking. Sized below the arena capacity so the per-op check fires with margin;
    /// `catch_unwind` in [`exact_symbolic_verdict_with_witness`] is the backstop for a
    /// single op that jumps past the budget in one apply.
    node_budget: usize,
}

impl BddBitBlaster {
    /// Bit-blast `file`: allocate one BDD variable per register/input bit,
    /// bind the leaves, walk every `Const`/`Op` node into a BDD vector, and
    /// collect the per-register next-state functions.
    pub fn build(file: &Btor2File) -> Result<Self, String> {
        Self::build_with_keep(file, None)
    }

    /// R-F5.6 — build the exact bit-blaster, optionally restricted to a cone-of-influence
    /// KEEP-SET of leaf NIDs. A leaf (register/input) whose NID is NOT in `keep` is PINNED to
    /// a constant 0: it uses ZERO BDD variables, never appears in the state/input frame, and
    /// (for a register) never transitions — so `total_bits` (the bit cap) counts only the
    /// property's cone. `keep = None` bit-blasts the full design (the pre-R-F5.6 behaviour).
    ///
    /// SOUNDNESS: an out-of-cone leaf cannot influence any atom (the cone is closed under the
    /// data-flow dependency relation AND `constraint`/`fair`/`justice` coupling — see
    /// [`crate::adapter::btor2::dep_graph::cone_leaf_nids`]), so pinning it to any fixed value
    /// is verdict-preserving for the FULL mu-calculus over the atoms — an EXACT reduction, not
    /// an over/under-approximation. The dep graph over-approximates influence (spurious edges
    /// keep MORE), so the cone is a superset of the true cone: it can never drop a relevant
    /// leaf. `keep = None` is unchanged from before.
    pub fn build_with_keep(
        file: &Btor2File,
        keep: Option<&std::collections::HashSet<Nid>>,
    ) -> Result<Self, String> {
        // AR-S1 / D1.4 — the ONE canonical leaf enumeration + naming lives on the
        // STS-IR seam (`BtorSts::leaf_cells`). It resolves each state/input to its
        // user-visible name via `collect_symbols` (walking Yosys's `uext _ NID 0 NAME`
        // alias ops back to the underlying, often unnamed, state cell) through the
        // single `parser::resolve_leaf_symbol`. Without this the flattened yosys BTOR2
        // leaves registers unnamed (`state_<nid>`), so a predicate over a real register
        // name (`bit_cnt_q`) never binds; for a directly-named cell the resolved name
        // equals the raw symbol, so it is a no-op there. The bit-blaster no longer
        // re-walks `file.lines` or re-implements naming — its enumeration and the
        // seam's cannot drift (the #242 class), and Pass 3's `next_funcs` keying below
        // reuses these same names.
        let leaf_cells = crate::adapter::sts_ir::BtorSts::new(file).leaf_cells()?;
        // Pass 1 — register + input cells with their widths, in file order.
        let leaf_specs: Vec<(Nid, String, bool, u32)> = leaf_cells
            .iter()
            .map(|c| (c.nid, c.name.clone(), c.is_state, c.width))
            .collect();

        // R-F5.6 — a leaf is KEPT (a BDD variable) iff no keep-set is given, or its NID is in
        // the property cone. Out-of-cone leaves are pinned to constant 0 (zero variables).
        let is_kept = |nid: Nid| keep.is_none_or(|k| k.contains(&nid));

        // The bit cap now counts only the KEPT (cone) bits.
        let total_bits: u32 = leaf_specs
            .iter()
            .filter(|(nid, _, _, _)| is_kept(*nid))
            .map(|(_, _, _, w)| *w)
            .sum();

        // R-F5.6 guard — the bit-blaster builds BDDs over every KEPT register+input bit. The
        // bit COUNT is only a crude upper bound on cost (a ROBDD shares structure), so the real
        // robustness gate is the node-budget guard below; this cap just avoids allocating a
        // large arena for an absurdly wide cone. The cap is `MUNUNU_BDD_MAX_BITS` (clamped) or,
        // with no override, the default auto-raised to fit a modestly-wide concrete cone up to
        // `AUTO_CAP_CEILING` — see [`effective_bitblast_cap`].
        let cap = effective_bitblast_cap(total_bits);
        if total_bits > cap {
            return Err(format!(
                "symbolic bit-blaster: design has {total_bits} register+input bits \
                 (> {cap}) after cone-of-influence restriction (R-F5.6) — abstained on the BIT CAP; \
                 the property's cone is too wide to bit-blast; raise MUNUNU_BDD_MAX_BITS, or use \
                 `--engine explicit`"
            ));
        }

        // Arena sizing, TIERED. The HARD arena size (the memory limit) and the node BUDGET (the
        // decidability threshold: a build whose BDD passes the budget ABSTAINS → `--engine
        // explicit`) are DECOUPLED so the arena sits WELL ABOVE the budget. That headroom is
        // load-bearing: a single wide op (e.g. an `Ite` over a wide word) can grow the BDD past
        // the budget in ONE apply, before the between-op guard runs — if that one apply also
        // overran the ARENA, OxiDD returns `OutOfMemory`, the `.unwrap()` panics, and unwinding
        // then drops the EXHAUSTED manager, which ABORTS the process (SIGABRT — `catch_unwind`
        // cannot save it). With the arena above the budget, that one over-budget apply still fits,
        // so the guard sees `live > budget` and returns a clean `Err` (graceful abstain) instead.
        // Small tier: 2M budget in an 8M arena (6M headroom, ~128 MB). Wide tier: env-tunable
        // arena, 80 % budget (already ~20 % = several-M headroom). Combined with the start-of-op
        // guard in `eval_op`, no single op can reach the arena limit. Manager is per-blaster.
        let arena_nodes: usize = if total_bits <= 40 {
            1 << 23 // 8M nodes (~128 MB) — big headroom below the budget (was 2M)
        } else {
            std::env::var("MUNUNU_BDD_ARENA_NODES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1 << 25) // 32M nodes (~1 GB); bump via env for wider control cones
        };
        let node_budget = if total_bits <= 40 {
            1 << 21 // 2M — decidability threshold; the 8M arena leaves 6M single-op headroom
        } else {
            arena_nodes * 8 / 10
        };
        let manager = bdd::new_manager(arena_nodes, arena_nodes / 4, 1);
        let (all_vars, var_base, tt, ff) = manager.with_manager_exclusive(|m| {
            let range = m.add_vars(total_bits as VarNo);
            let vars: Vec<BDDFunction> = (0..total_bits)
                .map(|i| BDDFunction::var(m, range.start + i as VarNo).unwrap())
                .collect();
            (vars, range.start, BDDFunction::t(m), BDDFunction::f(m))
        });

        // Slice the flat variable pool out per KEPT cell (LSB-first); PIN each out-of-cone
        // leaf to a constant-0 BitVec (env only — never a `Cell`, so it is invisible to the
        // init BDD, the input cube, and the next-state substitution).
        let mut env: HashMap<Nid, BitVec> = HashMap::new();
        let mut cells: Vec<Cell> = Vec::new();
        let mut cursor = 0usize;
        for (nid, symbol, is_state, width) in leaf_specs {
            if is_kept(nid) {
                let vars: BitVec = all_vars[cursor..cursor + width as usize].to_vec();
                let varnos: Vec<VarNo> = (0..width as usize)
                    .map(|b| var_base + (cursor + b) as VarNo)
                    .collect();
                cursor += width as usize;
                env.insert(nid, vars.clone());
                cells.push(Cell {
                    symbol,
                    is_state,
                    vars,
                    varnos,
                });
            } else {
                // Pinned: constant 0, zero BDD variables. `walk_design` resolves the leaf to
                // this constant; downstream ops over it fold to constants (kept BDDs stay in
                // the cone frame). Not pushed to `cells`.
                env.insert(nid, vec![ff.clone(); width as usize]);
            }
        }

        let mut blaster = BddBitBlaster {
            _manager: manager,
            constraint_bdd: tt.clone(), // real value computed after walk_design (Pass 5)
            tt,
            ff,
            env,
            cells,
            next_funcs: HashMap::new(),
            named_signals: HashMap::new(),
            node_budget,
        };

        // Passes 2–5 do all the BDD allocation. A cone that does not compress can EXHAUST the fixed
        // OxiDD arena inside a SINGLE wide op — before the between-op node-budget guard runs — which
        // OxiDD surfaces as an allocation `.unwrap()` panic. Catch it and abstain GRACEFULLY (→ cube
        // / `--engine explicit`) instead of crashing the process: this half-built blaster owns its
        // manager and is dropped on the unwind (single-threaded apply, so the RAII lock guard is
        // released during unwinding), so there is no shared/global state to corrupt.
        let build_passes =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                // Pass 2 — walk the node DAG, evaluating every Const/Op into a BitVec.
                walk_design(file, &mut blaster).map_err(|e| match e {
                    WalkError::NonBitvecSort(nid) => {
                        format!("NID {nid}: non-bitvec sort in the transition cone")
                    }
                    WalkError::Unevaluated(nid) => format!("NID {nid}: operand unevaluated"),
                    WalkError::Backend(msg) => msg,
                })?;

                // Pass 3 — collect the per-register next-state functions, keyed by the state's
                // symbol. AR-S1 / #242 drift guard: these names come from the SAME `leaf_cells`
                // enumeration as Pass 1, so `next_funcs.get(&cell.symbol)` in `exact_model()`
                // cannot miss and silently freeze a register at its init value (the #242
                // soundness bug: a false `EF`-VIOLATED / vacuous `AG AF`-HOLDS). Pinned
                // (out-of-cone) registers are SKIPPED — they are constants and must not transition.
                let nid_symbol: HashMap<Nid, String> = leaf_cells
                    .iter()
                    .filter(|c| c.is_state)
                    .map(|c| (c.nid, c.name.clone()))
                    .collect();
                for line in &file.lines {
                    if let Node::Next { state, value, .. } = &line.node {
                        if !is_kept(*state) {
                            continue; // pinned register — stays constant, no next-state function
                        }
                        let Some(sym) = nid_symbol.get(state) else {
                            continue;
                        };
                        let next = blaster.resolve(*value)?;
                        blaster.next_funcs.insert(sym.clone(), next);
                    }
                }

                // Pass 4 — named combinational signals (module outputs / named wires), so a predicate
                // can bind to a non-register signal (`depth_o`, `gnt_o`). Its BDD is already in `env`
                // (walk_design); the atom's cone still seeds via the output's terminal fan-in.
                for line in &file.lines {
                    match &line.node {
                        Node::Output {
                            symbol: Some(s),
                            signal,
                        } => {
                            if let Some(bits) = blaster.env.get(&signal.nid()) {
                                blaster
                                    .named_signals
                                    .entry(s.clone())
                                    .or_insert_with(|| bits.clone());
                            }
                        }
                        Node::Op {
                            symbol: Some(s), ..
                        } => {
                            if let Some(bits) = blaster.env.get(&line.nid) {
                                blaster
                                    .named_signals
                                    .entry(s.clone())
                                    .or_insert_with(|| bits.clone());
                            }
                        }
                        _ => {}
                    }
                }

                // Pass 5 — the `constraint` (assume) conjunction: AND every `constraint`
                // line's 1-bit signal into `constraint_bdd` (a function of the state +
                // input vars). The exact model injects it into the modal pre-image so a
                // `constraint` design is checked over constraint-respecting runs, not
                // over-approximated. An out-of-cone (pinned) constraint signal folds to a
                // constant, which is correct (the cone keeps every constraint-coupled leaf).
                for line in &file.lines {
                    if let Node::Constraint { signal } = &line.node {
                        let bits = blaster.resolve(*signal)?;
                        let c = bits.into_iter().next().ok_or_else(|| {
                            "exact bit-blaster: a `constraint` signal has zero width".to_string()
                        })?;
                        blaster.constraint_bdd = blaster.constraint_bdd.and(&c).unwrap();
                    }
                }
                Ok(())
            }));
        match build_passes {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(
                    "exact bit-blaster: BDD arena exhausted during build (a wide op overran \
                            the node budget in one apply) — abstained on the NODE budget; the cone \
                            does not compress to a tractable BDD; raise MUNUNU_BDD_ARENA_NODES, or \
                            use `--engine explicit`"
                        .into(),
                );
            }
        }

        Ok(blaster)
    }

    /// The BDD bit-vector for a predicate operand: a state/input register ([`register_bits`])
    /// or, failing that, a named combinational signal (module output / wire). `None` if neither
    /// names it.
    fn signal_bits(&self, name: &str) -> Option<&BitVec> {
        self.register_bits(name)
            .or_else(|| self.named_signals.get(name))
    }

    /// Symbolically compute one clock step and restrict it to a concrete
    /// assignment: the BDD counterpart of
    /// [`simulate_one_step`](super::bit_blast::simulate_one_step). Returns each
    /// register symbol (that has a `Next` line) mapped to its next value.
    ///
    /// `assignment` maps every register + input symbol to its current value.
    /// Symbols absent from the map default to `0` (the `setundef -zero`
    /// discipline the concrete oracle also follows).
    pub fn eval_step(&self, assignment: &HashMap<String, u128>) -> HashMap<String, u128> {
        let minterm = self.minterm_for(assignment);
        self.next_funcs
            .iter()
            .map(|(sym, bits)| (sym.clone(), self.value_of(bits, &minterm)))
            .collect()
    }

    /// The full-assignment minterm over every register + input bit.
    fn minterm_for(&self, assignment: &HashMap<String, u128>) -> BDDFunction {
        let mut mt = self.tt.clone();
        for cell in &self.cells {
            let v = assignment.get(&cell.symbol).copied().unwrap_or(0);
            for (b, var) in cell.vars.iter().enumerate() {
                let lit = if (v >> b) & 1 == 1 {
                    var.clone()
                } else {
                    var.not().unwrap()
                };
                mt = mt.and(&lit).unwrap();
            }
        }
        mt
    }

    /// Read a bit-vector of BDDs at a full assignment: bit `b` is set iff the
    /// minterm implies `bits[b]` (`mt ⊑ f ⟺ mt ∧ f == mt`).
    fn value_of(&self, bits: &[BDDFunction], minterm: &BDDFunction) -> u128 {
        let mut val = 0u128;
        for (b, f) in bits.iter().enumerate() {
            if minterm.and(f).unwrap() == *minterm {
                val |= 1u128 << b;
            }
        }
        val
    }

    /// Resolve an operand to its BDD vector, applying the BTOR2 negative-NID
    /// bitwise-NOT shorthand (mirrors the concrete `read_operand`).
    fn resolve(&self, op: Operand) -> Result<BitVec, String> {
        let v = self
            .env
            .get(&op.nid())
            .ok_or_else(|| format!("operand {} unevaluated", op.nid()))?;
        if op.is_negated() {
            Ok(v.iter().map(|b| b.not().unwrap()).collect())
        } else {
            Ok(v.clone())
        }
    }

    // ---- Boolean gadgets (each mirrors a concrete `eval_op` arm) ----

    fn xor(&self, a: &BDDFunction, b: &BDDFunction) -> Result<BDDFunction, String> {
        // (a ∧ ¬b) ∨ (¬a ∧ b) — kept crate-portable (oxidd exposes `.and/.or/.not`).
        // Every apply is `oom(...)?` so an arena-exhausting op ABSTAINS (Err) rather
        // than `.unwrap()`-panicking / SIGABRT-ing on the exhausted-manager drop.
        let lhs = oom(a.and(&oom(b.not())?))?;
        let rhs = oom(oom(a.not())?.and(b))?;
        oom(lhs.or(&rhs))
    }

    /// Build a `width`-bit result one bit at a time, checking the node budget after
    /// each bit so a wide per-bit op (bitwise AND/OR/…) ABSTAINS at the budget with
    /// arena headroom intact (mununu#462) instead of growing the shared BDD arena to
    /// exhaustion mid-collect — where the exhausted-manager drop would SIGABRT.
    fn map_bits_checked(
        &self,
        width: u32,
        mut f: impl FnMut(usize) -> Result<BDDFunction, String>,
    ) -> Result<BitVec, String> {
        let mut out = BitVec::with_capacity(width as usize);
        for i in 0..width as usize {
            out.push(f(i)?);
            self.check_node_budget()?;
        }
        Ok(out)
    }

    /// Ripple-carry adder over `width` bits: returns `(sum, carry_out)`.
    /// Missing high bits of either operand read as `0`. Each bit checks the node
    /// budget (mirrors `Op::Ite`): the whole width builds inside one `eval_op`, so
    /// the between-op guard cannot fire mid-loop; the per-bit check bounds growth to
    /// a single adder stage, so a wide add ABSTAINS at the budget with arena
    /// headroom intact instead of exhausting the manager (mununu#462).
    fn add_bits(
        &self,
        a: &[BDDFunction],
        b: &[BDDFunction],
        cin: BDDFunction,
        width: u32,
    ) -> Result<(BitVec, BDDFunction), String> {
        let mut carry = cin;
        let mut sum = Vec::with_capacity(width as usize);
        for i in 0..width as usize {
            let ai = a.get(i).cloned().unwrap_or_else(|| self.ff.clone());
            let bi = b.get(i).cloned().unwrap_or_else(|| self.ff.clone());
            let axb = self.xor(&ai, &bi)?;
            let s = self.xor(&axb, &carry)?;
            // carry' = (ai ∧ bi) ∨ (carry ∧ (ai ⊕ bi))
            let c = oom(oom(ai.and(&bi))?.or(&oom(carry.and(&axb))?))?;
            sum.push(s);
            carry = c;
            self.check_node_budget()?;
        }
        Ok((sum, carry))
    }

    /// Barrel shifter: shift `a` by the VARIABLE amount `s` (a `width`-bit BitVec).
    /// `left` ⇒ shift left (fill 0); else shift right, filling with 0 (logical) or the sign bit
    /// `a[width-1]` (`arith`). log-depth: stage `k` conditionally shifts by `2^k` on `s[k]`.
    /// A shift ≥ width falls out to the fill (the `sh < w`/`j+sh` bounds handle it).
    fn barrel_shift(
        &self,
        a: &[BDDFunction],
        s: &[BDDFunction],
        width: u32,
        left: bool,
        arith: bool,
    ) -> Result<BitVec, String> {
        let w = width as usize;
        let sign = if arith {
            a.get(w.wrapping_sub(1))
                .cloned()
                .unwrap_or_else(|| self.ff.clone())
        } else {
            self.ff.clone()
        };
        let mut cur: BitVec = (0..w)
            .map(|i| a.get(i).cloned().unwrap_or_else(|| self.ff.clone()))
            .collect();
        for (k, sk) in s.iter().enumerate() {
            let sh = 1usize.checked_shl(k as u32).unwrap_or(usize::MAX); // 2^k, saturating
            let nsk = oom(sk.not())?;
            cur = (0..w)
                .map(|j| -> Result<BDDFunction, String> {
                    // The bit selected when s[k] is set (shift by 2^k this stage).
                    let shifted = if left {
                        // left: result[j] = (j ≥ 2^k) ? cur[j − 2^k] : 0
                        if sh <= j {
                            cur[j - sh].clone()
                        } else {
                            self.ff.clone()
                        }
                    } else {
                        // right: result[j] = (j + 2^k < w) ? cur[j + 2^k] : fill
                        if sh < w && j + sh < w {
                            cur[j + sh].clone()
                        } else {
                            sign.clone()
                        }
                    };
                    // result[j] = s[k] ? shifted : cur[j]
                    oom(oom(sk.and(&shifted))?.or(&oom(nsk.and(&cur[j]))?))
                })
                .collect::<Result<BitVec, String>>()?;
            self.check_node_budget()?;
        }
        Ok(cur)
    }

    /// OR-reduce a bit-vector to a single BDD (`|x`). Budget-checked per bit so a
    /// wide reduction ABSTAINS at the budget rather than exhausting the arena.
    fn or_reduce(&self, bits: &[BDDFunction]) -> Result<BDDFunction, String> {
        let mut acc = self.ff.clone();
        for b in bits {
            acc = oom(acc.or(b))?;
            self.check_node_budget()?;
        }
        Ok(acc)
    }

    /// AND-reduce a bit-vector to a single BDD (`&x`). Budget-checked per bit.
    fn and_reduce(&self, bits: &[BDDFunction]) -> Result<BDDFunction, String> {
        let mut acc = self.tt.clone();
        for b in bits {
            acc = oom(acc.and(b))?;
            self.check_node_budget()?;
        }
        Ok(acc)
    }

    /// XOR-reduce a bit-vector to a single BDD (`^x`). Budget-checked per bit.
    fn xor_reduce(&self, bits: &[BDDFunction]) -> Result<BDDFunction, String> {
        let mut acc = self.ff.clone();
        for b in bits {
            acc = self.xor(&acc, b)?;
            self.check_node_budget()?;
        }
        Ok(acc)
    }

    /// Structural bit-equality: AND of per-bit XNOR. Returns a 1-bit vector.
    /// Budget-checked per bit (mununu#462): a wide equality over a large state cone
    /// is the AND-reduction that blows the arena, so it ABSTAINS at the budget with
    /// arena headroom intact instead of `.unwrap()`-panicking on OxiDD OutOfMemory.
    fn eq_bits(&self, a: &[BDDFunction], b: &[BDDFunction]) -> Result<BitVec, String> {
        let n = a.len().max(b.len());
        let mut acc = self.tt.clone();
        for i in 0..n {
            let ai = a.get(i).cloned().unwrap_or_else(|| self.ff.clone());
            let bi = b.get(i).cloned().unwrap_or_else(|| self.ff.clone());
            let xnor = oom(self.xor(&ai, &bi)?.not())?;
            acc = oom(acc.and(&xnor))?;
            self.check_node_budget()?;
        }
        Ok(vec![acc])
    }

    /// Unsigned `a >= b`: the carry-out of `a + ¬b + 1` (borrow-free subtract).
    fn uge(&self, a: &[BDDFunction], b: &[BDDFunction], width: u32) -> Result<BDDFunction, String> {
        let not_b: BitVec = (0..width as usize)
            .map(|i| oom(b.get(i).cloned().unwrap_or_else(|| self.ff.clone()).not()))
            .collect::<Result<BitVec, String>>()?;
        let (_, cout) = self.add_bits(a, &not_b, self.tt.clone(), width)?;
        Ok(cout)
    }

    /// Signed (two's-complement) `a < b`: if the signs differ, the negative operand (MSB set)
    /// is the smaller; if the signs match, unsigned `<` decides. `slt = (a_msb ⊕ b_msb) ? a_msb
    /// : (a <u b)`, with `a <u b = ¬(a ≥u b)`.
    fn slt(&self, a: &[BDDFunction], b: &[BDDFunction], width: u32) -> Result<BDDFunction, String> {
        let w = width as usize;
        let amsb = a.get(w - 1).cloned().unwrap_or_else(|| self.ff.clone());
        let bmsb = b.get(w - 1).cloned().unwrap_or_else(|| self.ff.clone());
        let signs_differ = self.xor(&amsb, &bmsb)?;
        let ult = oom(self.uge(a, b, width)?.not())?; // a <u b
        // signs_differ ? amsb : ult
        oom(oom(signs_differ.and(&amsb))?.or(&oom(oom(signs_differ.not())?.and(&ult))?))
    }

    // ---- R-F5.3b: predicate BDDs over the register-bit variables ----

    /// The BDD variable vector for a register/input symbol, LSB-first, or
    /// `None` if the symbol names no cell.
    pub fn register_bits(&self, name: &str) -> Option<&BitVec> {
        self.cells
            .iter()
            .find(|c| c.symbol == name)
            .map(|c| &c.vars)
    }

    /// Zero-extend a bit-vector to `width` bits (high bits become `⊥`).
    fn zero_extend(&self, bits: &[BDDFunction], width: usize) -> BitVec {
        (0..width)
            .map(|i| bits.get(i).cloned().unwrap_or_else(|| self.ff.clone()))
            .collect()
    }

    /// A constant bit-vector of `value` at `width` bits, LSB-first.
    fn const_bits(&self, value: u128, width: usize) -> BitVec {
        (0..width)
            .map(|b| {
                if b < 128 && (value >> b) & 1 == 1 {
                    self.tt.clone()
                } else {
                    self.ff.clone()
                }
            })
            .collect()
    }

    /// An unsigned comparison of two equal-width bit-vectors, as a 1-bit BDD.
    /// Mirrors [`crate::adapter::btor2::predicate_expr`]'s `cmp_apply` (unsigned).
    fn cmp_bits(
        &self,
        a: &[BDDFunction],
        op: CmpOp,
        b: &[BDDFunction],
        width: u32,
    ) -> Result<BDDFunction, String> {
        Ok(match op {
            CmpOp::Eq => self.eq_bits(a, b)?.swap_remove(0),
            CmpOp::Ne => oom(self.eq_bits(a, b)?.swap_remove(0).not())?,
            CmpOp::Ge => self.uge(a, b, width)?,
            CmpOp::Lt => oom(self.uge(a, b, width)?.not())?,
            CmpOp::Le => self.uge(b, a, width)?, // a ≤ b ⟺ b ≥ a
            CmpOp::Gt => oom(self.uge(b, a, width)?.not())?, // a > b ⟺ ¬(b ≥ a)
        })
    }

    /// R-F5.3b — compile a [`PredicateExpr`] into a single BDD over the
    /// present-state register-bit variables: the characteristic function of the
    /// set of concrete register valuations that satisfy the predicate.
    ///
    /// This is the bridge from the register-bit BDDs (R-F5.3a) to predicate
    /// cubes: each predicate `P_i` becomes one BDD `⟦P_i⟧(x)` over the leaf
    /// vars, so a cube (an assignment to the `|P|` predicates) is the
    /// conjunction of the `⟦P_i⟧` / `¬⟦P_i⟧`. R-F5.3c then builds the abstract
    /// may/must relation from these + the bit-blasted next-state functions.
    ///
    /// Comparisons are **unsigned** and evaluated at a common width (operands
    /// zero-extended), so the BDD matches [`PredicateExpr::eval`]'s `u128`
    /// semantics bit-for-bit — including when a literal exceeds the register
    /// width. `And`/`Or`/`Not` map to the BDD boolean ops.
    pub fn predicate_bdd(&self, expr: &PredicateExpr) -> Result<BDDFunction, String> {
        let unknown = |r: &str| format!("predicate references unknown register/signal `{r}`");
        match expr {
            PredicateExpr::Cmp {
                register,
                op,
                value,
            } => {
                let reg = self
                    .signal_bits(register)
                    .ok_or_else(|| unknown(register))?;
                // Compare at max(reg width, 64) so a u64 literal wider than the
                // register keeps its high bits (reg's zero-extend to those bits
                // is `0`, matching `eval`'s unmasked u128 comparison).
                let w = reg.len().max(64);
                let a = self.zero_extend(reg, w);
                let b = self.const_bits(*value as u128, w);
                self.cmp_bits(&a, *op, &b, w as u32)
            }
            PredicateExpr::CmpReg { lhs, op, rhs } => {
                let l = self.signal_bits(lhs).ok_or_else(|| unknown(lhs))?;
                let r = self.signal_bits(rhs).ok_or_else(|| unknown(rhs))?;
                let w = l.len().max(r.len());
                let a = self.zero_extend(l, w);
                let b = self.zero_extend(r, w);
                self.cmp_bits(&a, *op, &b, w as u32)
            }
            PredicateExpr::CmpRegAddend {
                lhs,
                op,
                rhs,
                addend,
                width: _,
            } => {
                let l = self.signal_bits(lhs).ok_or_else(|| unknown(lhs))?;
                let r = self.signal_bits(rhs).ok_or_else(|| unknown(rhs))?;
                // sum = (rhs + addend) mod 2^(rhs width) — wraps exactly as the
                // RTL `+` (the `bvadd` in `build_constraint`; the `width` field
                // is `eval`'s modulus and equals the rhs register width here).
                let rw = r.len();
                let addend_bits = self.const_bits(*addend as u128, rw);
                let (sum, _) = self.add_bits(r, &addend_bits, self.ff.clone(), rw as u32)?;
                let w = l.len().max(rw);
                let a = self.zero_extend(l, w);
                let b = self.zero_extend(&sum, w);
                self.cmp_bits(&a, *op, &b, w as u32)
            }
            PredicateExpr::And(a, b) => {
                oom(self.predicate_bdd(a)?.and(&self.predicate_bdd(b)?))
            }
            PredicateExpr::Or(a, b) => oom(self.predicate_bdd(a)?.or(&self.predicate_bdd(b)?)),
            PredicateExpr::Not(a) => oom(self.predicate_bdd(a)?.not()),
            PredicateExpr::Select { .. } => Err(
                "exact-symbolic ROBDD cannot bit-blast an array-content (Select) predicate — it is \
                 SMT-only (predicate-cube path); the exact engine abstains on it"
                    .to_string(),
            ),
        }
    }

    /// Evaluate a predicate BDD (over the register vars) at a concrete register
    /// assignment: `true` iff the assignment's minterm implies the BDD. The
    /// differential counterpart of [`PredicateExpr::eval`].
    pub fn eval_predicate_at(&self, bdd: &BDDFunction, assignment: &HashMap<String, u128>) -> bool {
        let mt = self.minterm_for(assignment);
        mt.and(bdd).unwrap() == mt
    }

    // ---- R-F5.3c: the abstract may relation over predicate cubes ----

    /// R-F5.3c — build the abstract **may** transition relation over predicate
    /// cubes, symbolically, without the `O(2^2|P|)` SMT the explicit path uses.
    ///
    /// Given predicates `P_0..P_{k-1}`, this returns an [`AbstractRelation`]
    /// whose `R_may` is a BDD over `2k` fresh predicate variables — `p_i`
    /// (present cube) and `p'_i` (next cube) — such that cube `c → c'` is a
    /// may-edge iff *some* concrete state in `c` can, under *some* input, step
    /// to a concrete state in `c'`:
    ///
    /// ```text
    /// A(x, p)      = ⋀_i (p_i  ⟺ ⟦P_i⟧(x))
    /// A'(x, i, p') = ⋀_i (p'_i ⟺ ⟦P_i⟧(next(x, i)))
    /// R_may(p, p') = ∃ (x ∪ i). A(x, p) ∧ A'(x, i, p')
    /// ```
    ///
    /// `⟦P_i⟧(next(x, i))` is the R-F5.3b predicate BDD with every register bit
    /// substituted by that register's bit-blasted next-state function (held
    /// registers keep their present var; inputs stay free and are quantified
    /// out). The whole relation is one `substitute` + one `apply_exists` per
    /// predicate — construction cost, not per-cube-pair SMT.
    pub fn abstract_may_relation(
        &self,
        predicates: &[PredicateExpr],
    ) -> Result<AbstractRelation, String> {
        self.abstract_relation(predicates, None)
    }

    /// R-F5.3c/d — build the abstract may (and optionally must) transition
    /// relation over predicate cubes, symbolically. `R_may` is always computed;
    /// `R_must` is computed iff `must` is `Some`.
    ///
    /// With `A(x,p) = ⋀_i (p_i ⟺ ⟦P_i⟧(x))` and
    /// `A'(x,i,p') = ⋀_i (p'_i ⟺ ⟦P_i⟧(next(x,i)))`:
    ///
    /// ```text
    /// R_may(p, p')  = ∃ (x ∪ i). A ∧ A'                         (some x, some i)
    /// R_must_∀∃     = ∀ x. ( A(x,p) → ∃ i. A'(x,i,p') )         (every x, some i)
    /// R_must_∀∀     = ∀ (x ∪ i). ( A(x,p) → A'(x,i,p') )        (every x, every i)
    /// ```
    ///
    /// The `∀∃` form is the canonical KMTS must-edge (Bruns–Godefroid; the
    /// explicit path's [`MustEdgeInference::SmtPerTargetStandard`]); `∀∀` is
    /// the stricter deterministic-into-target form ([`MustEdgeInference::SmtPerTarget`]).
    /// `⟦P_i⟧(next(x,i))` is the R-F5.3b predicate BDD with every register bit
    /// substituted by that register's bit-blasted next-state function.
    ///
    /// [`MustEdgeInference::SmtPerTargetStandard`]: super::kmts_lift::MustEdgeInference::SmtPerTargetStandard
    /// [`MustEdgeInference::SmtPerTarget`]: super::kmts_lift::MustEdgeInference::SmtPerTarget
    pub fn abstract_relation(
        &self,
        predicates: &[PredicateExpr],
        must: Option<MustSemantics>,
    ) -> Result<AbstractRelation, String> {
        let pred_bdds = predicates
            .iter()
            .map(|e| self.predicate_bdd(e))
            .collect::<Result<Vec<_>, _>>()?;
        self.abstract_relation_impl(&pred_bdds, must, None)
    }

    /// P2.5-F — build the abstract relation as a TWO-PLAYER game: `controllable` names the controller's
    /// input signals (the rest are the environment's). The result additionally carries the concrete
    /// game pieces ([`GamePieces`]) so [`AbstractRelation::cpre_controllable`] can solve the 3-valued
    /// game. `Control::All` modalities still use `r_may`/`r_must`; `Control::{Controllable, Environment}`
    /// use the controllable predecessor. Requires a [`MustSemantics`] (the 3-valued steps need `r_must`).
    pub fn abstract_game(
        &self,
        predicates: &[PredicateExpr],
        must: MustSemantics,
        controllable: &std::collections::HashSet<String>,
    ) -> Result<AbstractRelation, String> {
        let pred_bdds = predicates
            .iter()
            .map(|e| self.predicate_bdd(e))
            .collect::<Result<Vec<_>, _>>()?;
        self.abstract_relation_impl(&pred_bdds, Some(must), Some(controllable))
    }

    /// P2.5-F (game-CEGAR) — build the two-player game from PRE-BUILT predicate BDDs (over the present
    /// register vars) rather than [`PredicateExpr`]s. CEGAR's refinement predicates are the pre-image
    /// regions of existing predicates — arbitrary BDDs, not `REG op VALUE` atoms — so the loop supplies
    /// them directly here.
    pub fn abstract_game_bdd(
        &self,
        pred_bdds: &[BDDFunction],
        must: MustSemantics,
        controllable: &std::collections::HashSet<String>,
    ) -> Result<AbstractRelation, String> {
        self.abstract_relation_impl(pred_bdds, Some(must), Some(controllable))
    }

    fn abstract_relation_impl(
        &self,
        pred_bdds: &[BDDFunction],
        must: Option<MustSemantics>,
        controllable: Option<&std::collections::HashSet<String>>,
    ) -> Result<AbstractRelation, String> {
        use oxidd::{BooleanFunctionQuant, BooleanOperator};

        let k = pred_bdds.len();

        // Allocate 2k fresh predicate vars at the bottom of the order: p_i then p'_i.
        //
        // mununu#542 — the two `BDDFunction::var(..).unwrap()` below are the only `unwrap`s left
        // in this function. Each allocates ONE node for a fresh variable: O(1), independent of the
        // cone, and done at entry before any wide operation has grown the arena. They also sit
        // inside `with_manager_exclusive`, whose closure returns a tuple rather than a `Result`, so
        // propagating would mean restructuring the allocation for a case that cannot realistically
        // arise. Every cone-sized operation below propagates via `oom`.
        let (present, next, present_varnos) = self._manager.with_manager_exclusive(|m| {
            let range = m.add_vars(2 * k as VarNo);
            let present: Vec<BDDFunction> = (0..k)
                .map(|i| BDDFunction::var(m, range.start + i as VarNo).unwrap())
                .collect();
            let next: Vec<BDDFunction> = (0..k)
                .map(|i| BDDFunction::var(m, range.start + (k + i) as VarNo).unwrap())
                .collect();
            let present_varnos: Vec<VarNo> = (0..k as VarNo).map(|i| range.start + i).collect();
            (present, next, present_varnos)
        });
        // The cube of next predicate vars (to quantify out in a preimage).
        let mut next_cube = self.tt.clone();
        for v in &next {
            next_cube = oom(next_cube.and(v))?;
        }

        // The present→next substitution: each register bit var ↦ its
        // bit-blasted next-state function. Registers without a `Next` line
        // (held) and inputs are absent ⇒ left free (identity), which is the
        // correct next value for a held register and keeps inputs quantifiable.
        let mut sub_vars: Vec<VarNo> = Vec::new();
        let mut sub_repl: Vec<BDDFunction> = Vec::new();
        for cell in &self.cells {
            if !cell.is_state {
                continue;
            }
            if let Some(next_fn) = self.next_funcs.get(&cell.symbol) {
                for (vn, f) in cell.varnos.iter().zip(next_fn.iter()) {
                    sub_vars.push(*vn);
                    sub_repl.push(f.clone());
                }
            }
        }

        // Cubes: register vars, input vars (split controllable / environment for the two-player game),
        // and their union.
        let mut reg_cube = self.tt.clone();
        let mut input_cube = self.tt.clone();
        let mut ctrl_cube = self.tt.clone();
        let mut env_cube = self.tt.clone();
        for cell in &self.cells {
            let is_ctrl = !cell.is_state && controllable.is_some_and(|c| c.contains(&cell.symbol));
            for v in &cell.vars {
                if cell.is_state {
                    reg_cube = oom(reg_cube.and(v))?;
                } else {
                    input_cube = oom(input_cube.and(v))?;
                    if is_ctrl {
                        ctrl_cube = oom(ctrl_cube.and(v))?;
                    } else {
                        env_cube = oom(env_cube.and(v))?;
                    }
                }
            }
        }
        let xi_cube = oom(reg_cube.and(&input_cube))?;

        // A(x, p) and A'(x, i, p').
        let mut a = self.tt.clone();
        let mut a_prime = self.tt.clone();
        for (i, pred) in pred_bdds.iter().enumerate() {
            let pred = pred.clone(); // ⟦P_i⟧(x) — a pre-built predicate BDD
            let pred_next = if sub_vars.is_empty() {
                pred.clone()
            } else {
                oom(pred.substitute(&Subst::new(sub_vars.clone(), sub_repl.clone())))?
            };
            // p_i ⟺ pred  and  p'_i ⟺ pred_next  (via XNOR).
            let iff_present = oom(self.xor(&present[i], &pred)?.not())?;
            let iff_next = oom(self.xor(&next[i], &pred_next)?.not())?;
            a = oom(a.and(&iff_present))?;
            a_prime = oom(a_prime.and(&iff_next))?;
        }

        // R_may = ∃(x ∪ i). A ∧ A'   (fused relational product).
        // The widest operation in this function — the fused relational product over the whole
        // (register ∪ input) cube. mununu#542: exhausting the arena here must ABSTAIN, not panic.
        let r_may = oom(a.apply_exists(BooleanOperator::And, &a_prime, &xi_cube))?;

        // The FEASIBLE present cubes `∃x. A(x,p)` — the cubes some concrete
        // register state inhabits. Used to restrict `R_must` (below) and to scope
        // the verdict tally (R-F5.4.2) to materialisable abstract states. Kept as
        // `reg_cube` (unchanged) — the two-player GAME relation depends on this
        // exact value; the input-predicate hazard is handled by disabling `R_must`
        // on the standard path (below), not by re-scoping `feasible_present`.
        let feasible_present = oom(a.exists(&reg_cube))?;

        // SOUNDNESS (must ⊆ may fix, symbolic engine) — must-edges are DISABLED
        // (may-only, `r_must = None`) when a predicate is over an INPUT.
        //
        // An input is both a present *observable* (the predicate reads it) and the
        // transition *nondeterminism* (the ∃-input in the ∀∃ must-edge) — the same
        // conflation that makes the exact ROBDD engine refuse an input atom. Here
        // it leaves the constructed `raw` must-relation with input vars in its
        // support, so `R_must ⊄ R_may` and `box.must ⊄ box.may` trips
        // `TritBdd::from_parts` (a debug panic; a wrong verdict in release). Rather
        // than fabricate a must-edge whose semantics are ill-defined, fall back to
        // the may-only KMTS: definite HOLDS still transfers (may over-approximation
        // + safety), and a refutation lands on ⊥ — the honest "abstraction can't
        // decide", never a spurious VIOLATED. `A` depends on an input iff
        // quantifying inputs changes feasibility.
        //
        // This applies ONLY to the standard (`Control::All`) relation, where inputs
        // are demonic transition nondeterminism and box_pre trips on the dirty
        // support. The TWO-PLAYER GAME relation (`controllable.is_some()`) models
        // inputs explicitly as env/ctrl moves and *requires* `r_must` for the
        // controllable predecessor (`AbstractRelation::evaluate`); it does not go
        // through the box_pre panic, so it keeps its must-relation unchanged.
        let inputs_in_predicates = feasible_present != oom(a.exists(&xi_cube))?;
        let r_must = if inputs_in_predicates && controllable.is_none() {
            None
        } else {
            must.map(|sem| -> Result<BDDFunction, String> {
                let raw = match sem {
                    MustSemantics::ForallExists => {
                        // ∀x. ( A(x,p) → ∃i. A'(x,i,p') )
                        //
                        // mununu#542 was reported here: the `∃i` exhausted the arena and
                        // `.unwrap()` panicked, which aborts on the unwind of the exhausted
                        // manager (see `oom`) — exit 101/134 and NO report, where the engine
                        // owes an abstention.
                        let exists_i = oom(a_prime.exists(&input_cube))?;
                        let not_a = oom(a.not())?;
                        let implies = oom(not_a.or(&exists_i))?;
                        oom(implies.forall(&reg_cube))?
                    }
                    MustSemantics::ForallForall => {
                        // ∀(x ∪ i). ( A(x,p) → A'(x,i,p') )
                        let not_a = oom(a.not())?;
                        let implies = oom(not_a.or(&a_prime))?;
                        oom(implies.forall(&xi_cube))?
                    }
                };
                // A vacuously-empty (unsatisfiable) source cube yields `¬A ≡ ⊤`
                // there, so the ∀ would make it a must-edge to *every* cube —
                // breaking `R_must ⊆ R_may` (an empty cube has no may-edge).
                // Intersecting with `feasible_present` keeps must-edges leaving
                // only inhabited cubes, so `R_must ⊆ R_may` holds globally over
                // the `2^k` cube space (the R-F5.4.1 evaluator relies on it).
                oom(raw.and(&feasible_present))
            })
            .transpose()?
        };

        // P2.5-F — retain the concrete game pieces when a controllable partition was requested.
        let game = controllable.map(|_| GamePieces {
            a: a.clone(),
            a_prime: a_prime.clone(),
            reg_cube: reg_cube.clone(),
            ctrl_cube,
            env_cube,
        });

        Ok(AbstractRelation {
            num_predicates: k,
            feasible_present,
            present,
            next,
            present_varnos,
            next_cube,
            r_may,
            r_must,
            game,
            tt: self.tt.clone(),
            ff: self.ff.clone(),
        })
    }
}

/// R-F5.3d — which under-approximating must-edge semantics to compute for the
/// abstract relation. Mirrors the explicit path's must-edge options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MustSemantics {
    /// `∀x∈c. ∃i. next(x,i)∈c'` — the canonical KMTS must-edge (every concrete
    /// state in the source cube has *some* input that lands in the target).
    /// The more permissive of the two; the sound `[]`/`<>` under-approximation.
    ForallExists,
    /// `∀x∈c. ∀i. next(x,i)∈c'` — stricter: *every* input lands in the target
    /// (deterministic into the target cube). Every `∀∀` must-edge is also a
    /// `∀∃` must-edge.
    ForallForall,
}

/// R-F5.3c/d — the abstract may + must transition relation over predicate
/// cubes, as BDDs over `2k` predicate variables. A cube is an index in
/// `0..2^k`; bit `i` of the index is the truth of predicate `P_i`.
pub struct AbstractRelation {
    num_predicates: usize,
    /// The feasible present cubes `∃x. A(x,p)` — a BDD over the present
    /// predicate vars marking the cubes inhabited by some concrete state.
    feasible_present: BDDFunction,
    /// Present-cube predicate variables `p_0..p_{k-1}`.
    present: Vec<BDDFunction>,
    /// Next-cube predicate variables `p'_0..p'_{k-1}`.
    next: Vec<BDDFunction>,
    /// Variable numbers of the present predicate vars (for the present→next
    /// rename in a preimage).
    present_varnos: Vec<VarNo>,
    /// The cube of next-cube predicate vars, to quantify out in a preimage.
    next_cube: BDDFunction,
    /// The may relation as a BDD over `(p, p')`.
    r_may: BDDFunction,
    /// The must relation over `(p, p')`, when a [`MustSemantics`] was requested.
    r_must: Option<BDDFunction>,
    /// P2.5-F — the TWO-PLAYER game pieces, present only when the relation was built for a game
    /// ([`BddBitBlaster::abstract_game`]). They carry the CONCRETE `A(x,p)` / `A'(x,i,p')` predicates
    /// and the controllable/environment input split, which the single-agent `r_may`/`r_must` (having
    /// quantified all inputs away) cannot express. The 3-valued controllable predecessor
    /// ([`Self::cpre_controllable`]) reads them.
    game: Option<GamePieces>,
    tt: BDDFunction,
    ff: BDDFunction,
}

/// P2.5-F — the concrete predicates + input partition a two-player [`AbstractRelation`] retains so the
/// controllable predecessor can quantify `∀env ∃ctrl` (which the input-quantified `r_may`/`r_must`
/// cannot). All BDDs are over the bit-blaster's concrete register + input vars and the cube vars.
struct GamePieces {
    /// `A(x, p)` — concrete state `x` is in cube `p`.
    a: BDDFunction,
    /// `A'(x, i, p')` — the successor of `x` under input `i` is in cube `p'`.
    a_prime: BDDFunction,
    /// Cube of the concrete register-bit vars (quantified for the ∀x/∃x cube abstraction).
    reg_cube: BDDFunction,
    /// Cube of the CONTROLLABLE input bits.
    ctrl_cube: BDDFunction,
    /// Cube of the ENVIRONMENT (uncontrolled) input bits.
    env_cube: BDDFunction,
}

impl AbstractRelation {
    /// Number of predicates `k` (so `2^k` cubes).
    pub fn num_predicates(&self) -> usize {
        self.num_predicates
    }

    /// Is cube `cube` feasible — inhabited by at least one concrete state?
    /// Infeasible cubes are never materialised as abstract states.
    pub fn is_feasible(&self, cube: usize) -> bool {
        let mt = self.cube_minterm(cube, &self.present);
        mt.and(&self.feasible_present).unwrap() == mt
    }

    /// The feasible cube indices, ascending.
    pub fn feasible_cubes(&self) -> Vec<usize> {
        (0..(1usize << self.num_predicates))
            .filter(|&c| self.is_feasible(c))
            .collect()
    }

    /// The may relation BDD over the present + next predicate variables.
    pub fn may(&self) -> &BDDFunction {
        &self.r_may
    }

    /// The must relation BDD, or `None` if no [`MustSemantics`] was requested.
    pub fn must(&self) -> Option<&BDDFunction> {
        self.r_must.as_ref()
    }

    /// The complete-assignment minterm for a cube index over the given
    /// predicate variables (`vars[i]` true iff bit `i` of `cube` is set).
    fn cube_minterm(&self, cube: usize, vars: &[BDDFunction]) -> BDDFunction {
        let mut m = self.tt.clone();
        for (i, v) in vars.iter().enumerate() {
            let lit = if (cube >> i) & 1 == 1 {
                v.clone()
            } else {
                v.not().unwrap()
            };
            m = m.and(&lit).unwrap();
        }
        m
    }

    /// Is `present_cube → next_cube` a may-edge? (its full `(p, p')` minterm
    /// implies `R_may`).
    pub fn may_holds(&self, present_cube: usize, next_cube: usize) -> bool {
        self.holds(&self.r_may, present_cube, next_cube)
    }

    /// Is `present_cube → next_cube` a must-edge? Panics if no [`MustSemantics`]
    /// was requested when the relation was built.
    pub fn must_holds(&self, present_cube: usize, next_cube: usize) -> bool {
        let r_must = self
            .r_must
            .as_ref()
            .expect("must_holds called on a relation built without a MustSemantics");
        self.holds(r_must, present_cube, next_cube)
    }

    /// Does the `(present_cube, next_cube)` pair belong to relation `r`?
    fn holds(&self, r: &BDDFunction, present_cube: usize, next_cube: usize) -> bool {
        let mt = self
            .cube_minterm(present_cube, &self.present)
            .and(&self.cube_minterm(next_cube, &self.next))
            .unwrap();
        mt.and(r).unwrap() == mt
    }

    // ---- R-F5.4.1: the symbolic mu-calculus evaluator over the relation ----

    /// `⊤` / `⊥` as `TritBdd`s over the present predicate vars (built directly
    /// from the manager's constants, since we have no `SymbolicContext`).
    fn tb_top(&self) -> TritBdd {
        TritBdd::from_parts(self.tt.clone(), self.tt.clone())
    }
    fn tb_bot(&self) -> TritBdd {
        TritBdd::from_parts(self.ff.clone(), self.ff.clone())
    }

    /// Rename a present-var function to the next-var frame (for a preimage).
    fn to_next(&self, f: &BDDFunction) -> BDDFunction {
        f.substitute(&Subst::new(self.present_varnos.clone(), self.next.clone()))
            .unwrap()
    }

    /// 3-valued box preimage of `phi` (Bruns–Godefroid): `box.must` uses
    /// `R_may`, `box.may` uses `R_must`. Mirrors [`crate::mu_calculus::symbolic`]'s
    /// `SymbolicKmts::box_pre` on this relation's own predicate-var frame.
    ///
    /// `r_may` / `r_must` are passed explicitly (rather than reading
    /// `self.r_may`) so a guarded modality can supply the guard-restricted
    /// relations from [`Self::guarded_relations`] (R-F5.5c). A bare modality
    /// passes `&self.r_may` and the relation's own `r_must`.
    fn box_pre(&self, phi: &TritBdd, r_may: &BDDFunction, r_must: &BDDFunction) -> TritBdd {
        use oxidd::{BooleanFunctionQuant, BooleanOperator};
        let must_next = self.to_next(phi.must());
        let box_must = r_may
            .apply_exists(
                BooleanOperator::And,
                &must_next.not().unwrap(),
                &self.next_cube,
            )
            .unwrap()
            .not()
            .unwrap();
        let may_next = self.to_next(phi.may());
        let box_may = r_must
            .apply_exists(
                BooleanOperator::And,
                &may_next.not().unwrap(),
                &self.next_cube,
            )
            .unwrap()
            .not()
            .unwrap();
        TritBdd::from_parts(box_must, box_may)
    }

    /// 3-valued diamond preimage of `phi`: `dia.must` uses `R_must`, `dia.may`
    /// uses `R_may`. `r_may` / `r_must` are passed explicitly for the same
    /// guard-restriction reason as [`Self::box_pre`] (R-F5.5c).
    fn diamond_pre(&self, phi: &TritBdd, r_may: &BDDFunction, r_must: &BDDFunction) -> TritBdd {
        use oxidd::{BooleanFunctionQuant, BooleanOperator};
        let must_next = self.to_next(phi.must());
        let dia_must = r_must
            .apply_exists(BooleanOperator::And, &must_next, &self.next_cube)
            .unwrap();
        let may_next = self.to_next(phi.may());
        let dia_may = r_may
            .apply_exists(BooleanOperator::And, &may_next, &self.next_cube)
            .unwrap();
        TritBdd::from_parts(dia_must, dia_may)
    }

    /// P2.5-F — the 3-valued CONTROLLABLE PREDECESSOR over the abstract game: the cubes from which the
    /// controller can force `phi`. It plays the EXACT concrete one-step game (`∀env ∃ctrl`) within each
    /// concrete state, then lifts to the cube by the may/must abstraction — the `must` (definite) region
    /// requires EVERY concrete state in the cube to be controller-winning (`∀x∈p`), the `may` (possible)
    /// region requires SOME (`∃x∈p`). When the predicates pin each concrete state (one cube per state)
    /// this reduces to the exact [`ExactModel::cpre_controllable`], so the exact engine is the
    /// differential oracle; with coarser predicates a cube where some states win and others do not is
    /// `⊥`. Sound: definite (`must`/`¬may`) verdicts transfer (Bruns–Godefroid). Requires the relation
    /// to carry [`GamePieces`] (built via [`BddBitBlaster::abstract_game`]).
    fn cpre_controllable(&self, phi: &TritBdd) -> TritBdd {
        let g = self
            .game
            .as_ref()
            .expect("cpre_controllable requires a game relation (abstract_game)");
        // The concrete controller-win region toward next-set `r_next` (over concrete x):
        // `∀env. ∃ctrl. ∃p'. A'(x, ctrl, env, p') ∧ r_next(p')` = "the controller can step into r".
        let win = |r_next: &BDDFunction| -> BDDFunction {
            use oxidd::{BooleanFunctionQuant, BooleanOperator};
            let reach = g
                .a_prime
                .apply_exists(BooleanOperator::And, r_next, &self.next_cube)
                .unwrap()
                .exists(&g.ctrl_cube)
                .unwrap();
            reach.forall(&g.env_cube).unwrap()
        };
        self.lift_game_win(
            g,
            &win(&self.to_next(phi.must())),
            &win(&self.to_next(phi.may())),
        )
    }

    /// P2.5-F — the 3-valued ENVIRONMENT PREDECESSOR (dual of [`Self::cpre_controllable`]): the cubes
    /// from which the ENVIRONMENT can force `phi` — concrete `∃env ∀ctrl` per state, lifted by the
    /// same `∀x`/`∃x` cube abstraction.
    fn cpre_environment(&self, phi: &TritBdd) -> TritBdd {
        let g = self
            .game
            .as_ref()
            .expect("cpre_environment requires a game relation (abstract_game)");
        // `∃env. ∀ctrl. ∃p'. A'(x, ctrl, env, p') ∧ r_next(p')`.
        let win = |r_next: &BDDFunction| -> BDDFunction {
            use oxidd::{BooleanFunctionQuant, BooleanOperator};
            let step = g
                .a_prime
                .apply_exists(BooleanOperator::And, r_next, &self.next_cube)
                .unwrap();
            step.forall(&g.ctrl_cube)
                .unwrap()
                .exists(&g.env_cube)
                .unwrap()
        };
        self.lift_game_win(
            g,
            &win(&self.to_next(phi.must())),
            &win(&self.to_next(phi.may())),
        )
    }

    /// Lift a concrete per-state win region to the cube: `must = ∀x∈p. win_must`, `may = ∃x∈p. win_may`,
    /// both restricted to feasible cubes. Shared by both predecessors.
    fn lift_game_win(
        &self,
        g: &GamePieces,
        win_must: &BDDFunction,
        win_may: &BDDFunction,
    ) -> TritBdd {
        use oxidd::BooleanFunctionQuant;
        let cpre_must =
            g.a.not()
                .unwrap()
                .or(win_must)
                .unwrap()
                .forall(&g.reg_cube)
                .unwrap()
                .and(&self.feasible_present)
                .unwrap();
        let cpre_may =
            g.a.and(win_may)
                .unwrap()
                .exists(&g.reg_cube)
                .unwrap()
                .and(&self.feasible_present)
                .unwrap();
        TritBdd::from_parts(cpre_must, cpre_may)
    }

    /// R-F5.5c — the may/must relations RESTRICTED to a guard's matching
    /// transitions: `R ∧ cur_ok(x) ∧ next_ok(x')`, where `cur_ok` / `next_ok`
    /// are the `req_*` / `forb_*` state-predicate filters over the present /
    /// next predicate vars. Each guard predicate names a `P_i` in `names`; its
    /// characteristic set is the present var `present[i]` (current) or its
    /// `to_next` image (next). Mirrors [`crate::mu_calculus::symbolic`]'s
    /// `SymbolicKmts::guarded_relations` on this relation's cube frame.
    ///
    /// A required predicate absent from `names` (not part of the cube's
    /// abstraction) contributes `⊥` (no cube matches → the modal is vacuous
    /// there), exactly as the explicit `guard_matches` filter; a forbidden
    /// absent predicate is vacuously satisfied (no constraint, since `¬⊥ = ⊤`).
    ///
    /// Label / controllability / step-bounded guards are NOT handled here —
    /// they are rejected up front in [`Self::eval_node`] as out-of-fragment
    /// over a predicate cube (see
    /// [`crate::mu_calculus::cube_modality_soundness_warnings`]).
    fn guarded_relations(
        &self,
        guard: &Guard,
        names: &HashMap<&str, usize>,
        r_must: &BDDFunction,
    ) -> (BDDFunction, BDDFunction) {
        // `present[i]` if the predicate is in the cube's set; `ff` otherwise
        // (a required-but-absent predicate makes the guard vacuous; a
        // forbidden-but-absent one is vacuously satisfied since `¬ff = tt`).
        let pred_set = |name: &str| -> BDDFunction {
            match names.get(name) {
                Some(&i) => self.present[i].clone(),
                None => self.ff.clone(),
            }
        };
        let mut c = self.tt.clone();
        for v in &guard.current.required {
            c = c.and(&pred_set(v)).unwrap();
        }
        for v in &guard.current.forbidden {
            c = c.and(&pred_set(v).not().unwrap()).unwrap();
        }
        for v in &guard.next.required {
            c = c.and(&self.to_next(&pred_set(v))).unwrap();
        }
        for v in &guard.next.forbidden {
            c = c.and(&self.to_next(&pred_set(v).not().unwrap())).unwrap();
        }
        (self.r_may.and(&c).unwrap(), r_must.and(&c).unwrap())
    }

    /// R-F5.4.1 — evaluate a mu-calculus `formula` symbolically over this
    /// abstract relation, returning a `(must, may)` verdict over the present
    /// predicate cubes. `pred_names[i]` names predicate `P_i` (the atomic
    /// proposition `P_i` holds exactly on the cubes where bit `i` is set —
    /// i.e. its characteristic set is the present var `p_i`).
    ///
    /// Supports the **audited-sound fragment** the cube path evaluates:
    /// `True`/`False`, predicates, `!`/`&&`/`||`, `mu`/`nu`, bare `[]`/`<>`, and
    /// (R-F5.5c) modalities guarded by `req_cur`/`forb_cur`/`req_next`/`forb_next`
    /// state predicates (each names a `P_i` in `pred_names`; see
    /// [`Self::guarded_relations`]). A label / controllability / step-bounded
    /// modality remains an honest error — out-of-fragment over a predicate cube
    /// (see [`crate::mu_calculus::cube_modality_soundness_warnings`]). Requires a
    /// relation built *with* a [`MustSemantics`] (the modal steps read
    /// `R_must`).
    pub fn evaluate(&self, formula: &Formula, pred_names: &[String]) -> Result<TritBdd, String> {
        let r_must = self.r_must.as_ref().ok_or_else(|| {
            "AbstractRelation::evaluate requires a must relation — build with a MustSemantics"
                .to_string()
        })?;
        let name_to_idx: HashMap<&str, usize> = pred_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        let mut bindings: HashMap<FormulaVarId, TritBdd> = HashMap::new();
        self.eval_node(formula, formula.root(), &name_to_idx, r_must, &mut bindings)
    }

    fn eval_node(
        &self,
        f: &Formula,
        id: crate::mu_calculus::NodeId,
        names: &HashMap<&str, usize>,
        r_must: &BDDFunction,
        bindings: &mut HashMap<FormulaVarId, TritBdd>,
    ) -> Result<TritBdd, String> {
        Ok(match f.node(id) {
            MuNode::True => self.tb_top(),
            MuNode::False => self.tb_bot(),
            MuNode::Predicate(name) => match names.get(name.as_str()) {
                // The AP's characteristic set is the present predicate var; the
                // cube valuation is definite (Sharp), so must = may = p_i.
                Some(&i) => TritBdd::from_parts(self.present[i].clone(), self.present[i].clone()),
                None => self.tb_bot(),
            },
            MuNode::Variable(v) => bindings
                .get(v)
                .cloned()
                .ok_or_else(|| format!("unbound fixpoint variable {v:?}"))?,
            MuNode::Not(n) => self.eval_node(f, *n, names, r_must, bindings)?.not(),
            MuNode::And(a, b) => {
                let av = self.eval_node(f, *a, names, r_must, bindings)?;
                let bv = self.eval_node(f, *b, names, r_must, bindings)?;
                av.and(&bv)
            }
            MuNode::Or(a, b) => {
                let av = self.eval_node(f, *a, names, r_must, bindings)?;
                let bv = self.eval_node(f, *b, names, r_must, bindings)?;
                av.or(&bv)
            }
            MuNode::Modal {
                kind,
                guard,
                target,
            } => {
                // P2.5-F — TWO-PLAYER: a controllability guard is dispatched to the 3-valued
                // controllable predecessor when the relation carries a game partition
                // (`abstract_game`). It must be a CONTROL-ONLY guard (no label / state-predicate /
                // step axis, which the game CPre does not yet combine with). Without a game partition
                // it stays an honest error (the plain verification cube has no player partition).
                if guard.control != Control::All {
                    let control_only = *guard
                        == Guard {
                            control: guard.control,
                            ..Guard::default()
                        };
                    if self.game.is_none() {
                        return Err(
                            "symbolic cube evaluator: controllability guards (`ctrl`) require a \
                             two-player game relation — build with `abstract_game`"
                                .into(),
                        );
                    }
                    if !control_only {
                        return Err(
                            "symbolic cube evaluator: a controllability (`ctrl`) modality must be \
                             control-only — labels / state-predicate / step guards are not yet \
                             combined with the game predecessor"
                                .into(),
                        );
                    }
                    let phi = self.eval_node(f, *target, names, r_must, bindings)?;
                    return Ok(match guard.control {
                        Control::Controllable => self.cpre_controllable(&phi),
                        Control::Environment => self.cpre_environment(&phi),
                        Control::All => unreachable!("handled by the outer branch"),
                    });
                }
                if guard.max_steps.is_some() {
                    return Err(
                        "symbolic cube evaluator: step-bounded modalities (`steps`) unsupported"
                            .into(),
                    );
                }
                if !guard.labels.is_empty() {
                    return Err(
                        "symbolic cube evaluator: label guards unsupported over a predicate cube \
                         (the cube carries no label structure — use `req_cur`/`req_next` state \
                         guards, or a bare `[]`/`<>`)"
                            .into(),
                    );
                }
                // R-F5.5c — the sound in-fragment guards over a cube are the
                // `req_cur`/`forb_cur`/`req_next`/`forb_next` state-predicate
                // filters. `guarded_relations` returns `(r_may, r_must)`
                // unchanged for a bare (all-empty) guard, so this path is a
                // no-op for `[]`/`<>`.
                let (gr_may, gr_must) = self.guarded_relations(guard, names, r_must);
                let phi = self.eval_node(f, *target, names, r_must, bindings)?;
                match kind {
                    ModalKind::Box => self.box_pre(&phi, &gr_may, &gr_must),
                    ModalKind::Diamond => self.diamond_pre(&phi, &gr_may, &gr_must),
                }
            }
            MuNode::Mu { var, body } => {
                self.fixpoint(f, *var, *body, names, r_must, bindings, false)?
            }
            MuNode::Nu { var, body } => {
                self.fixpoint(f, *var, *body, names, r_must, bindings, true)?
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn fixpoint(
        &self,
        f: &Formula,
        var: FormulaVarId,
        body: crate::mu_calculus::NodeId,
        names: &HashMap<&str, usize>,
        r_must: &BDDFunction,
        bindings: &mut HashMap<FormulaVarId, TritBdd>,
        greatest: bool,
    ) -> Result<TritBdd, String> {
        let mut x = if greatest {
            self.tb_top()
        } else {
            self.tb_bot()
        };
        loop {
            bindings.insert(var, x.clone());
            let next = self.eval_node(f, body, names, r_must, bindings)?;
            if next.eq_set(&x) {
                bindings.remove(&var);
                return Ok(next);
            }
            x = next;
        }
    }

    /// The trit verdict of an evaluated formula at cube `cube`.
    pub fn verdict_at(&self, verdict: &TritBdd, cube: usize) -> Trit {
        let mt = self.cube_minterm(cube, &self.present);
        if mt.and(verdict.must()).unwrap() == mt {
            Trit::True
        } else if mt.and(verdict.may()).unwrap() == mt {
            Trit::Unknown
        } else {
            Trit::False
        }
    }

    /// P2.5-F — the trit verdict at the INITIAL cube(s): the abstract cubes containing a concrete
    /// initial state (`∃x. init(x) ∧ A(x,p)`). `True` iff every such cube is definite-true
    /// (`init ⊆ must`), `False` iff none is even possibly-true (`init ∩ may = ∅`), else `⊥`. Requires a
    /// game relation (which carries `A(x,p)`).
    pub fn verdict_at_init(&self, verdict: &TritBdd, init_state: &BDDFunction) -> Trit {
        use oxidd::BooleanFunctionQuant;
        let g = self
            .game
            .as_ref()
            .expect("verdict_at_init requires a game relation (abstract_game)");
        let init_cubes = init_state.and(&g.a).unwrap().exists(&g.reg_cube).unwrap();
        if init_cubes == self.ff {
            return Trit::Unknown; // no feasible initial cube (should not happen for a real reset)
        }
        if init_cubes.and(&verdict.must().not().unwrap()).unwrap() == self.ff {
            Trit::True
        } else if init_cubes.and(verdict.may()).unwrap() == self.ff {
            Trit::False
        } else {
            Trit::Unknown
        }
    }
}

/// Map an OxiDD apply result (arena-full ⇒ `OutOfMemory`) to the engine's `String` error, so a wide
/// BDD op ABSTAINS (`Err` → the caller routes to `--engine explicit`) instead of `.unwrap()`-panicking.
/// A panic here aborts the process (SIGABRT) on the unwind of the exhausted manager — `catch_unwind`
/// cannot save it — so a hot-path op that can exhaust the arena must propagate, never `unwrap`.
#[inline]
fn oom<T, E: std::fmt::Debug>(r: Result<T, E>) -> Result<T, String> {
    r.map_err(|_e| {
        "symbolic bit-blaster: BDD arena exhausted (OxiDD OutOfMemory) — abstained on the NODE \
         budget; the cone does not compress to a tractable BDD; raise MUNUNU_BDD_ARENA_NODES, or \
         use `--engine explicit`"
            .to_string()
    })
}

impl BvTermBackend for BddBitBlaster {
    type Value = BitVec;
    type Error = String;

    fn eval_const(&mut self, value: &ConstValue, width: u32) -> Result<Self::Value, Self::Error> {
        // Wide binary literals (> 128 bits) are built DIRECTLY from the bit string, bypassing the
        // u128 `eval_const_value` (which ERRORS on a > 128-bit `Bin` — u128 overflow). Such consts
        // occur out of the property cone (verdict-irrelevant, cone widths ≤ the bit cap), but a
        // hard error would fail the whole exact MC; representing them per-bit keeps the build
        // going. `s` is the btor2 MSB-first bit string; bit `b` (LSB-first) is `s[len-1-b]`,
        // zero-extended past the string. Found: keymgr_ctrl's 256-bit key-state constants.
        if let ConstValue::Bin(s) = value {
            let bytes = s.as_bytes();
            let n = bytes.len();
            return Ok((0..width as usize)
                .map(|b| {
                    if b < n && bytes[n - 1 - b] == b'1' {
                        self.tt.clone()
                    } else {
                        self.ff.clone()
                    }
                })
                .collect());
        }
        // AR-S2 — `eval_const_value` now returns an arbitrary-width `Bv`, so
        // `bv.bit(b)` is exact for every bit (wide `Ones` / `Dec` constants
        // included), not truncated at 128.
        let bv = eval_const_value(value, width)?;
        Ok((0..width as usize)
            .map(|b| {
                if bv.bit(b as u64) {
                    self.tt.clone()
                } else {
                    self.ff.clone()
                }
            })
            .collect())
    }

    fn eval_op(
        &mut self,
        _nid: Nid,
        op: Op,
        immediates: &[u32],
        args: &[Operand],
        width: u32,
    ) -> Result<Self::Value, Self::Error> {
        // START-of-op guard — if a PRIOR op already pushed the BDD past the budget, bail BEFORE
        // running this one. Load-bearing: the op we skip could be the wide one whose single apply
        // would overrun the arena (→ OxiDD `OutOfMemory` → `.unwrap()` panic → SIGABRT on the
        // unwind of the exhausted manager). The arena headroom (see `build_with_keep`) covers the
        // one over-budget op that trips the end-of-op guard below; this start guard stops the NEXT.
        self.check_node_budget()?;
        let read = |i: usize| -> Result<BitVec, String> { self.resolve(args[i]) };

        let result: BitVec = match op {
            // ---- Bitwise (result + operands all `width` bits) ----
            Op::Not => {
                let a = read(0)?;
                self.map_bits_checked(width, |i| oom(a[i].not()))?
            }
            Op::And => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| oom(a[i].and(&b[i])))?
            }
            Op::Or => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| oom(a[i].or(&b[i])))?
            }
            Op::Xor => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| self.xor(&a[i], &b[i]))?
            }
            Op::Nand => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| oom(oom(a[i].and(&b[i]))?.not()))?
            }
            Op::Nor => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| oom(oom(a[i].or(&b[i]))?.not()))?
            }
            Op::Xnor => {
                let (a, b) = (read(0)?, read(1)?);
                self.map_bits_checked(width, |i| oom(self.xor(&a[i], &b[i])?.not()))?
            }

            // ---- Arithmetic (two's-complement, wrap to `width`) ----
            Op::Add => {
                let (a, b) = (read(0)?, read(1)?);
                self.add_bits(&a, &b, self.ff.clone(), width)?.0
            }
            Op::Sub => {
                // a − b = a + ¬b + 1
                let (a, b) = (read(0)?, read(1)?);
                let not_b: BitVec = (0..width as usize)
                    .map(|i| oom(b[i].not()))
                    .collect::<Result<BitVec, String>>()?;
                self.add_bits(&a, &not_b, self.tt.clone(), width)?.0
            }
            Op::Inc => {
                let a = read(0)?;
                self.add_bits(&a, &[], self.tt.clone(), width)?.0
            }
            Op::Dec => {
                // a − 1 = a + (all ones) + 0
                let a = read(0)?;
                let ones: BitVec = (0..width as usize).map(|_| self.tt.clone()).collect();
                self.add_bits(&a, &ones, self.ff.clone(), width)?.0
            }
            Op::Neg => {
                // −a = ¬a + 1
                let a = read(0)?;
                let not_a: BitVec = (0..width as usize)
                    .map(|i| oom(a[i].not()))
                    .collect::<Result<BitVec, String>>()?;
                self.add_bits(&not_a, &[], self.tt.clone(), width)?.0
            }
            Op::Mul => {
                // Schoolbook shift-and-add: Σ_i (b[i] ? (a << i) : 0), truncated to `width`.
                // O(width²) BDD adds — fine at the ≤ MAX_BITBLAST_BITS cone widths. Both
                // operands + the result are `width` bits; the product wraps mod 2^width.
                // Each partial + each `add_bits` is budget-checked, so a wide multiply
                // ABSTAINS at the budget rather than exhausting the arena (mununu#462).
                let (a, b) = (read(0)?, read(1)?);
                let w = width as usize;
                let mut acc: BitVec = vec![self.ff.clone(); w];
                for i in 0..w {
                    let bi = b.get(i).cloned().unwrap_or_else(|| self.ff.clone());
                    // Partial product: (a << i) masked by b[i]. Bit j = (j ≥ i) ? a[j−i] ∧ b[i] : 0.
                    let partial: BitVec = self.map_bits_checked(width, |j| {
                        if j >= i {
                            oom(a
                                .get(j - i)
                                .cloned()
                                .unwrap_or_else(|| self.ff.clone())
                                .and(&bi))
                        } else {
                            Ok(self.ff.clone())
                        }
                    })?;
                    acc = self.add_bits(&acc, &partial, self.ff.clone(), width)?.0;
                }
                acc
            }

            // ---- Variable shifts (barrel shifter, result + operands all `width` bits) ----
            Op::Sll => {
                let (a, s) = (read(0)?, read(1)?);
                self.barrel_shift(&a, &s, width, true, false)?
            }
            Op::Srl => {
                let (a, s) = (read(0)?, read(1)?);
                self.barrel_shift(&a, &s, width, false, false)?
            }
            Op::Sra => {
                let (a, s) = (read(0)?, read(1)?);
                self.barrel_shift(&a, &s, width, false, true)?
            }

            // ---- Equality / implication (1-bit result) ----
            Op::Eq | Op::Iff => {
                let (a, b) = (read(0)?, read(1)?);
                self.eq_bits(&a, &b)?
            }
            Op::Neq => {
                let (a, b) = (read(0)?, read(1)?);
                vec![oom(self.eq_bits(&a, &b)?[0].not())?]
            }
            Op::Implies => {
                // ¬(|a) ∨ (|b)
                let (a, b) = (read(0)?, read(1)?);
                let a_bool = self.or_reduce(&a)?;
                let b_bool = self.or_reduce(&b)?;
                vec![oom(oom(a_bool.not())?.or(&b_bool))?]
            }

            // ---- Unsigned comparisons (1-bit result) ----
            Op::Ugte => {
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![self.uge(&a, &b, w)?]
            }
            Op::Ult => {
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![oom(self.uge(&a, &b, w)?.not())?]
            }
            Op::Ugt => {
                // a > b ⟺ b < a ⟺ ¬(b ≥ a)
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![oom(self.uge(&b, &a, w)?.not())?]
            }
            Op::Ulte => {
                // a ≤ b ⟺ b ≥ a
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![self.uge(&b, &a, w)?]
            }

            // ---- Signed comparisons (two's-complement, 1-bit result) ----
            Op::Slt => {
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![self.slt(&a, &b, w)?]
            }
            Op::Sgt => {
                // a > b ⟺ b < a
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![self.slt(&b, &a, w)?]
            }
            Op::Sgte => {
                // a ≥ b ⟺ ¬(a < b)
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![oom(self.slt(&a, &b, w)?.not())?]
            }
            Op::Slte => {
                // a ≤ b ⟺ ¬(b < a)
                let (a, b) = (read(0)?, read(1)?);
                let w = a.len().max(b.len()) as u32;
                vec![oom(self.slt(&b, &a, w)?.not())?]
            }

            // ---- Reductions (1-bit result) ----
            Op::Redor => vec![self.or_reduce(&read(0)?)?],
            Op::Redand => vec![self.and_reduce(&read(0)?)?],
            Op::Redxor => vec![self.xor_reduce(&read(0)?)?],

            // ---- Structural rearrangement ----
            Op::Concat => {
                // {a, b}: b occupies the low bits, a the high bits.
                let (a, b) = (read(0)?, read(1)?);
                let mut out = b;
                out.extend(a);
                out.truncate(width as usize);
                out
            }
            Op::Slice => {
                // immediates = [upper, lower]; out bit i = a[lower + i].
                let a = read(0)?;
                let lower = *immediates.get(1).ok_or("slice missing lower")? as usize;
                (0..width as usize)
                    .map(|i| a.get(lower + i).cloned().unwrap_or_else(|| self.ff.clone()))
                    .collect()
            }
            Op::Uext => {
                // zero-extend to `width`.
                let a = read(0)?;
                (0..width as usize)
                    .map(|i| a.get(i).cloned().unwrap_or_else(|| self.ff.clone()))
                    .collect()
            }
            Op::Sext => {
                // sign-extend to `width` (replicate the MSB of `a`).
                let a = read(0)?;
                let msb = a.last().cloned().unwrap_or_else(|| self.ff.clone());
                (0..width as usize)
                    .map(|i| a.get(i).cloned().unwrap_or_else(|| msb.clone()))
                    .collect()
            }

            // ---- Conditional ----
            Op::Ite => {
                // c ? t : e — c is 1-bit.
                let c = read(0)?;
                let (t, e) = (read(1)?, read(2)?);
                let cond = c[0].clone();
                let ncond = oom(cond.not())?;
                // Per-bit budget check + Err-propagation. A wide `Ite` (a mux over a wide word) is
                // the heaviest arm — the whole width builds in ONE `eval_op` call, so the between-op
                // guard cannot fire mid-collect. Checking after each output bit bounds the growth
                // between checks to a single 2-input mux, so we bail (Err) at the budget with the
                // arena headroom still intact — OxiDD never actually exhausts, so it never panics.
                let mut out: BitVec = BitVec::with_capacity(width as usize);
                for i in 0..width as usize {
                    let then_i = oom(cond.and(&t[i]))?;
                    let else_i = oom(ncond.and(&e[i]))?;
                    out.push(oom(then_i.or(&else_i))?);
                    self.check_node_budget()?;
                }
                out
            }

            // Out of R-F5.3a scope — later slices / never in the FSM+counter core.
            other => {
                return Err(format!(
                    "operator {other:?} not yet supported in the R-F5.3a BDD bit-blaster"
                ));
            }
        };
        // END-of-op guard — bail cleanly (→ a graceful `Err`) if this op grew the shared BDD arena
        // past the budget. The arena is sized well above the budget, so this op (which crossed it)
        // still fit without hitting OxiDD's `OutOfMemory`; the NEXT op is stopped by the start guard.
        self.check_node_budget()?;
        Ok(result)
    }

    fn bind(&mut self, nid: Nid, value: Self::Value) {
        self.env.insert(nid, value);
    }

    fn honor_init(&self) -> bool {
        // Register bits stand for their present-state variables, not init
        // values — the walk computes the transition function symbolically.
        false
    }

    fn read_operand(&self, op: Operand) -> Option<Self::Value> {
        self.resolve(op).ok()
    }

    fn uf_substitute(&mut self, _nid: Nid, _width: u32) -> Option<Self::Value> {
        // No uninterpreted-function abstraction in R-F5.3a.
        None
    }
}

/// D1 — the EXACT (full-state, 2-valued) transition model for symbolic
/// μ-calculus model checking, with NO predicate abstraction. The state is the
/// register-bit valuation itself; inputs are free (nondeterminism). A formula's
/// satisfying set is a single [`BDDFunction`] over the register-bit vars, and the
/// modal pre-image substitutes each state bit by its next-state function then
/// quantifies the input bits (`∃` for `⟨⟩`, `∀` for `[]`). Because there is no
/// abstraction the verdict is 2-valued and definite (no `⊥`); the cost is bounded
/// by BDD size, not by the `2^|Registers|` explicit-state cap. Reuses the exact
/// same substitution [`BddBitBlaster::abstract_relation`] builds, applied to the
/// formula BDD directly rather than to predicate vars.
pub struct ExactModel {
    /// State-bit varno → its next-state function. Held registers (no `Next` line)
    /// are omitted (identity ⇒ held value); inputs are never state, never
    /// substituted (they remain free to be quantified).
    sub_vars: Vec<VarNo>,
    sub_repl: Vec<BDDFunction>,
    /// The cube of input-bit vars, quantified in the single-agent (`Control::All`) modal pre-image.
    input_cube: BDDFunction,
    /// P2.5-F — the two-player input partition. `ctrl_cube` is the cube of the CONTROLLABLE input
    /// bits (the controller's), `env_cube` the cube of the remaining (environment/uncontrolled) input
    /// bits; `ctrl_cube ∧ env_cube = input_cube`. With no partition declared (the default),
    /// `ctrl_cube = tt` (no controllable bits) and `env_cube = input_cube` — so the controllable
    /// predecessor degenerates to the box pre-image (the environment owns everything), which is the
    /// correct semantics for "no controllable inputs". Used by [`ExactModel::cpre_controllable`] /
    /// [`ExactModel::cpre_environment`] to solve the two-player game.
    ctrl_cube: BDDFunction,
    env_cube: BDDFunction,
    /// BTOR2 `constraint` (assume) conjunction over the state + input vars — `tt`
    /// when the design is unconstrained. Injected into `diamond_pre` / `box_pre`
    /// so the modal quantifier ranges only over constraint-respecting transitions.
    constraint: BDDFunction,
    tt: BDDFunction,
    ff: BDDFunction,
    /// Fixpoint ITERATION budget — the total μ/ν fixpoint iterations across one
    /// [`ExactModel::evaluate`]. A wide FREE COUNTER has a small BDD (so the node-budget
    /// guard never fires) but a `2^W`-step (reachable-diameter) fixpoint; this bails it to
    /// a clean `Err` (→ Skipped) DETERMINISTICALLY (machine-independent). It bounds the
    /// iteration COUNT, not wall-clock TIME: at ~ms per cheap preimage, reaching the ~1M
    /// default takes MINUTES — so [`deadline`](Self::deadline) is the time backstop beside it.
    /// The other blowup mode — a large BDD — is caught by the node-budget guard + catch_unwind.
    iter_budget: usize,
    /// Running total of fixpoint iterations. Interior-mutable because `evaluate` / `fixpoint`
    /// take `&self`; the exact eval is single-threaded per call.
    iters: std::cell::Cell<usize>,
    /// Wall-clock backstop DEADLINE (`Instant::now()` + [`exact_time_budget`]), SAMPLED every 256
    /// iterations in [`Self::fixpoint`] BESIDE [`iter_budget`](Self::iter_budget) (so a fast
    /// control fixpoint never reads the clock). `None` = no time bound (pure determinism, for
    /// tests via `MUNUNU_BDD_TIME_BUDGET_MS=0`).
    ///
    /// **Why a wall clock despite the iteration budget's determinism.** The iteration budget
    /// bounds COUNT; a deep free-counter (`twocount32` — two 32-bit counters, ~2^32 diameter)
    /// reaches it only after ~132 min of individually-cheap preimages, and no bit-COUNT cap can
    /// exclude it without also excluding the tractable wide-CONTROL cones the P2.5-A cap-raise
    /// exists to admit (i2c's 173-bit cone decides in 242 ms; twocount32's 65 bits < i2c's 173).
    /// So the cap cannot separate them and this deadline must. It is a SECONDARY guard: it only
    /// fires for a design the deterministic budgets abstain on ANYWAY, bounding how LONG the
    /// abstain takes, not WHETHER it happens — so a non-pathological verdict (which finishes far
    /// under it) stays machine-independent. Abstain is always sound (never a fabricated verdict),
    /// so a machine-dependent abstain BOUNDARY is a robustness, not a soundness, property. The
    /// interruptible unit is one fixpoint iteration; a single pathological in-op `apply_exists`
    /// (not observed in-corpus — the BDD-blowup form is caught by the node budget) is the residual
    /// gap a thread + `recv_timeout` would close (P2.5-A-follow).
    deadline: Option<std::time::Instant>,
}

/// P2.2c #4 — the outcome of the SOUND diameter pre-pass ([`ExactModel::reach_diameter_to`]).
/// It MEASURES the exact engine's own bounded fixpoint, so it neither over-predicts (a near
/// target saturates fast) nor under-predicts (a wide diameter of ANY counter shape ExceedsBound) —
/// the failure modes of the structural [`ModelFacts::cone_counter_diameter_log2`] proxy.
///
/// [`ModelFacts::cone_counter_diameter_log2`]: crate::adapter::btor2::model_facts::ModelFacts::cone_counter_diameter_log2
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiameterEstimate {
    /// The `EF(target)` preimage fixpoint saturated at this depth (≤ `k_max`) — the exact
    /// distance the farthest state needs to reach `target`. The full exact fixpoint converges
    /// here, so the property is cheap for the exact engine.
    Saturated(usize),
    /// Still growing at `k_max` ⇒ the backward-reach diameter to `target` EXCEEDS `k_max`; the
    /// exact fixpoint needs > `k_max` iterations (the measured diameter signal).
    ExceedsBound(usize),
}

/// The exact-engine fixpoint iteration budget: `MUNUNU_BDD_ITER_BUDGET` or a default sized to
/// catch a wide-counter diameter (`2^W`) while admitting any control fixpoint (small diameter).
fn fixpoint_iter_budget() -> usize {
    std::env::var("MUNUNU_BDD_ITER_BUDGET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1 << 20) // ~1M iterations; a counter's steps are cheap (small BDD) so this is fast to reach
}

/// The exact-engine WALL-CLOCK backstop: `MUNUNU_BDD_TIME_BUDGET_MS` or a default. See
/// [`ExactModel::deadline`] for why this exists beside the deterministic iteration budget. `0`
/// disables it (returns `None` ⇒ no time bound, for tests wanting pure determinism). The default
/// (10 s) is ~40× i2c's measured 242 ms — generous for any tractable control cone — while cutting a
/// deep-counter abstain from the iteration budget's ~minutes down to seconds.
fn exact_time_budget() -> Option<std::time::Duration> {
    let ms = std::env::var("MUNUNU_BDD_TIME_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10_000);
    (ms != 0).then(|| std::time::Duration::from_millis(ms))
}

impl BddBitBlaster {
    /// Bail (→ `Err`, a graceful abstain that routes the caller to `--engine explicit`) once the
    /// live BDD node count passes the budget. `approx_num_inner_nodes` is O(1). Called at BOTH the
    /// start and end of every `eval_op`: the end guard catches the op that crossed the budget (the
    /// arena headroom absorbs it), the start guard stops the following op from running while over
    /// budget — together they keep the arena from ever actually filling, so OxiDD never returns
    /// `OutOfMemory` (whose `.unwrap()` panic would abort the process on the unwind of the
    /// exhausted manager, uncatchable by `catch_unwind`).
    fn check_node_budget(&self) -> Result<(), String> {
        let live = self
            ._manager
            .with_manager_shared(|m| m.approx_num_inner_nodes());
        if live > self.node_budget {
            return Err(format!(
                "abstained on the NODE budget ({live} of {} live BDD nodes) — the property's cone \
                 does not compress to a tractable BDD; raise MUNUNU_BDD_ARENA_NODES (which scales \
                 the node budget), or use `--engine explicit`",
                self.node_budget
            ));
        }
        Ok(())
    }

    /// Build the [`ExactModel`] for full-state 2-valued μ-calculus MC (D1). Pinned
    /// inputs (config concretization / reset-gating) are already constants in the
    /// bit-blasted design, so the `∃`/`∀` over the *remaining* free inputs is exact
    /// for the pinned model.
    pub fn exact_model(&self) -> ExactModel {
        self.exact_model_partitioned(&std::collections::HashSet::new())
    }

    /// P2.5-F — build the [`ExactModel`] with a TWO-PLAYER input partition: input cells whose symbol is
    /// in `controllable` become the controller's (`ctrl_cube`), the rest the environment's (`env_cube`).
    /// An empty set reproduces [`Self::exact_model`] (all inputs environment / single-agent). The
    /// partition drives [`ExactModel::cpre_controllable`] / [`ExactModel::cpre_environment`]; the
    /// single-agent `Control::All` pre-image still quantifies the whole `input_cube` and is unaffected.
    pub fn exact_model_partitioned(
        &self,
        controllable: &std::collections::HashSet<String>,
    ) -> ExactModel {
        // State bit → next-state function (the same sub the abstract relation uses).
        let mut sub_vars: Vec<VarNo> = Vec::new();
        let mut sub_repl: Vec<BDDFunction> = Vec::new();
        for cell in &self.cells {
            if !cell.is_state {
                continue;
            }
            if let Some(next_fn) = self.next_funcs.get(&cell.symbol) {
                for (vn, f) in cell.varnos.iter().zip(next_fn.iter()) {
                    sub_vars.push(*vn);
                    sub_repl.push(f.clone());
                }
            }
        }
        // The input-bit cubes to quantify in the modal pre-image. `input_cube` is every input bit
        // (single-agent); `ctrl_cube` / `env_cube` split it by the two-player partition.
        let mut input_cube = self.tt.clone();
        let mut ctrl_cube = self.tt.clone();
        let mut env_cube = self.tt.clone();
        for cell in &self.cells {
            if !cell.is_state {
                let is_ctrl = controllable.contains(&cell.symbol);
                for v in &cell.vars {
                    input_cube = input_cube.and(v).unwrap();
                    if is_ctrl {
                        ctrl_cube = ctrl_cube.and(v).unwrap();
                    } else {
                        env_cube = env_cube.and(v).unwrap();
                    }
                }
            }
        }
        ExactModel {
            sub_vars,
            sub_repl,
            input_cube,
            ctrl_cube,
            env_cube,
            constraint: self.constraint_bdd.clone(),
            tt: self.tt.clone(),
            ff: self.ff.clone(),
            iter_budget: fixpoint_iter_budget(),
            iters: std::cell::Cell::new(0),
            deadline: exact_time_budget().map(|d| std::time::Instant::now() + d),
        }
    }

    /// D1.3/D1.5 — the BDD of the design's **initial states**, per the BTOR2 format:
    /// a state with an `init <state> <value>` line is pinned to that value; a state
    /// with **no `init` line is left FREE**, because the format leaves its initial
    /// value unconstrained.
    ///
    /// **mununu#498 (sibling).** This used to pin an un-`init`ed state to all-zero
    /// (the `setundef -zero` convention). That made the modelled initial set a strict
    /// SUBSET of the design's, which is an UNDER-approximation: `Violated` stayed
    /// sound (the pinned state is a genuine initial state) but `Holds` did not — φ
    /// holding at one initial state says nothing about the others. mununu therefore
    /// disagreed with `btormc` / Pono, which follow the format (the btor2tools
    /// `noninitstate.btor2` case), and `exact_bad_reachable` had to *abstain* to stay
    /// honest. Modelling the initial set exactly removes the approximation, so BOTH
    /// directions are sound and no guard is needed.
    ///
    /// The SV path is unaffected: `reset_init::inject_reset_init` injects an `init`
    /// line for EVERY state cell of a reset-gated design before the model is built
    /// (all-or-nothing), so a lifted RTL design has no free-init state. Under
    /// `--no-gate-reset` the design is *documented* to choose its own power-up, which
    /// is what this now models.
    ///
    /// `Holds` iff EVERY initial state satisfies the property.
    pub fn initial_state_bdd(&self, file: &Btor2File) -> Result<BDDFunction, String> {
        // State-line nid → the value operand of its `init` line (if any).
        let mut init_value_nid: HashMap<Nid, Nid> = HashMap::new();
        for line in &file.lines {
            if let Node::Init { state, value, .. } = &line.node {
                init_value_nid.insert(*state, value.nid());
            }
        }
        // State-line nid → the symbol `build()` assigned it. AR-S1 / #242 drift
        // guard: names go through the SAME `parser::resolve_leaf_symbol` the seam's
        // `leaf_cells` enumeration (and hence `build()`) uses, else the cell lookup
        // below misses on flattened yosys output (cell = `bit_cnt_d`, raw State
        // symbol = none → `state_<nid>`) and the register is left unconstrained — a
        // silently over-broad init.
        let symbols = crate::adapter::btor2::parser::collect_symbols(file);
        let mut sym_of_nid: HashMap<Nid, String> = HashMap::new();
        for line in &file.lines {
            if let Node::State { symbol, .. } = &line.node {
                let sym = crate::adapter::btor2::parser::resolve_leaf_symbol(
                    &symbols, line.nid, symbol, "state",
                );
                sym_of_nid.insert(line.nid, sym);
            }
        }
        let cell_of_sym: HashMap<&str, &Cell> = self
            .cells
            .iter()
            .filter(|c| c.is_state)
            .map(|c| (c.symbol.as_str(), c))
            .collect();

        let mut init = self.tt.clone();
        for line in &file.lines {
            if !matches!(line.node, Node::State { .. }) {
                continue;
            }
            let Some(sym) = sym_of_nid.get(&line.nid) else {
                continue;
            };
            let Some(cell) = cell_of_sym.get(sym.as_str()) else {
                continue;
            };
            // No `init` line ⇒ the format leaves this state's initial value
            // unconstrained, so contribute NO conjunct and leave its bits free.
            // (Pinning them to all-zero here was the mununu#498 sibling
            // under-approximation — see this method's doc comment.)
            let Some(value_bits) = init_value_nid
                .get(&line.nid)
                .and_then(|vn| self.env.get(vn))
                .cloned()
            else {
                continue;
            };
            for (var, val) in cell.vars.iter().zip(value_bits.iter()) {
                // state_bit ⟺ value_bit  =  ¬(state_bit XOR value_bit)
                let iff = oom(self.xor(var, val)?.not())?;
                init = oom(init.and(&iff))?;
                self.check_node_budget()?;
            }
        }
        Ok(init)
    }
}

impl ExactModel {
    /// `⊤` / `⊥` over the state-var frame (for fixpoint seeds + tests).
    pub fn tt(&self) -> &BDDFunction {
        &self.tt
    }
    pub fn ff(&self) -> &BDDFunction {
        &self.ff
    }

    /// `φ` pushed one step forward: each state bit ↦ its next-state function ⇒ a
    /// BDD over `(state, input)` = "the successor of this state under this input
    /// satisfies `φ`". Simultaneous substitution ⇒ the concrete next-state
    /// relation. `pub` so the D1.8 lasso extractor can pick a `φ`-preserving input.
    pub fn to_next(&self, phi: &BDDFunction) -> BDDFunction {
        if self.sub_vars.is_empty() {
            phi.clone()
        } else {
            phi.substitute(&Subst::new(self.sub_vars.clone(), self.sub_repl.clone()))
                .unwrap()
        }
    }

    /// The states where the `constraint` is satisfiable by *some* input:
    /// `∃i. constraint(s,i)`. A valid constraint-respecting run only ever visits
    /// such states (a state with no admissible input is a dead end), so callers
    /// conjoin this into a *target* set (e.g. `bad`) to require the target itself
    /// be constraint-consistent. `tt` when unconstrained.
    pub fn constraint_satisfiable(&self) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        self.constraint.exists(&self.input_cube).unwrap()
    }

    /// `⟨⟩φ` — some **constraint-respecting** successor satisfies `φ`:
    /// `∃i. constraint(s,i) ∧ to_next(φ)`. `constraint = tt` (unconstrained) makes
    /// this the plain `∃i. to_next(φ)`, so an unconstrained design is unchanged.
    pub fn diamond_pre(&self, phi: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        self.to_next(phi)
            .and(&self.constraint)
            .unwrap()
            .exists(&self.input_cube)
            .unwrap()
    }

    /// `[]φ` — **every constraint-respecting** successor satisfies `φ`:
    /// `∀i. constraint(s,i) ⟹ to_next(φ)` = `∀i. ¬constraint(s,i) ∨ to_next(φ)`.
    /// The dual of the constrained diamond; `constraint = tt` gives the plain
    /// `∀i. to_next(φ)`.
    pub fn box_pre(&self, phi: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        self.constraint
            .not()
            .unwrap()
            .or(&self.to_next(phi))
            .unwrap()
            .forall(&self.input_cube)
            .unwrap()
    }

    /// P2.5-F — the CONTROLLABLE PREDECESSOR `CPre_ctrl(φ)`: the states from which the controller can
    /// force the next state into `φ` against every environment move —
    /// `{ s : ∀ env, ∃ ctrl, constraint(s, env, ctrl) ∧ next(s, env, ctrl) ∈ φ }`. This is the
    /// two-player (synchronous, Mealy: environment moves, controller responds) analogue of
    /// [`Self::diamond_pre`], and the modal step of a controller-side game fixpoint. `Control::All`
    /// still uses `diamond_pre`; with no controllable inputs (`ctrl_cube = tt`) this degenerates to
    /// `box_pre` (the environment owns everything), the correct "controller cannot influence" reading.
    ///
    /// The controller responds AFTER seeing the environment move (`∃ctrl` inner, `∀env` outer). Under
    /// `Control::Controllable` the box/diamond kind is subsumed by the controllability structure — the
    /// same operator as the explicit reference (`evaluator.rs::modal_trit_core`), which the two-player
    /// exact engine is differentially validated against.
    pub fn cpre_controllable(&self, phi: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        // ∃ctrl: a constraint-respecting transition into φ (per state + environment move).
        let reachable = self
            .to_next(phi)
            .and(&self.constraint)
            .unwrap()
            .exists(&self.ctrl_cube)
            .unwrap();
        // ∀env: the controller has such a response to every environment move.
        reachable.forall(&self.env_cube).unwrap()
    }

    /// P2.5-F — the ENVIRONMENT PREDECESSOR `CPre_env(φ)`, the De Morgan dual of
    /// [`Self::cpre_controllable`]: the states from which the ENVIRONMENT can force `φ` against every
    /// controllable move — `{ s : ∃ env, ∀ ctrl, constraint ⟹ next ∈ φ }`. Used for the environment
    /// side of the game (e.g. extracting the counterstrategy region when the controller loses).
    pub fn cpre_environment(&self, phi: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        // ∀ctrl: every controllable response leads (constraint-respecting) into φ.
        let forced = self
            .constraint
            .not()
            .unwrap()
            .or(&self.to_next(phi))
            .unwrap()
            .forall(&self.ctrl_cube)
            .unwrap();
        // ∃env: some environment move imposes that.
        forced.exists(&self.env_cube).unwrap()
    }

    /// P2.5-F (strategy extraction) — the `(state, ctrl-input)` pairs from which, WHATEVER the
    /// environment does, the controller's move lands the successor in `keep`: `∀env. constraint ∧ next ∈
    /// keep`. `cpre_controllable(keep) = ctrl_forcing_moves(keep).∃ctrl`. A single state's slice,
    /// projected onto the controllable inputs, is the controller's forced move there (env-oblivious /
    /// positional; with no environment inputs it is the whole move).
    pub fn ctrl_forcing_moves(&self, keep: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        self.to_next(keep)
            .and(&self.constraint)
            .unwrap()
            .forall(&self.env_cube)
            .unwrap()
    }

    /// Dual — the `(state, env-input)` pairs from which, WHATEVER the controller does, the successor is
    /// in `keep`: `∀ctrl. constraint ⟹ next ∈ keep`. The environment's forcing moves, for the
    /// counterstrategy when the controller loses.
    pub fn env_forcing_moves(&self, keep: &BDDFunction) -> BDDFunction {
        use oxidd::BooleanFunctionQuant;
        self.constraint
            .not()
            .unwrap()
            .or(&self.to_next(keep))
            .unwrap()
            .forall(&self.ctrl_cube)
            .unwrap()
    }

    // ---- Phase 2b: symbolic environment-strategy synthesis ----------------------------------------
    // `AG EF good` under an environment strategy. The environment OWNS the inputs, so `◇` = ∃input
    // (`diamond_pre`) is exactly Eve's controllable move; there is no adversary among the inputs (the
    // design is deterministic given inputs). The synthesis is a 1-player (control) game:
    //   R = EF(good)              — recoverable region (good reachable via SOME input path)
    //   S = νX. R ∧ ◇X            — the env can STAY in R forever (a safety game inside R)
    // `init ⊆ S` ⟺ the environment has a (positional) strategy maintaining `AG EF good` — from every
    // state on the strategy's path `good` stays reachable. This finds recovery disciplines that no
    // constant input-hold (Phase-2a slice 1) expresses: the safe move may differ per state.

    /// `EF good = μX. good ∨ ◇X` — the RECOVERABLE region (states from which `good` is reachable under
    /// some input path). Least fixpoint from ⊥. Mirrors [`BddBitBlaster::not_ef_p`] without the final
    /// negation.
    pub fn ef_region(&self, good: &BDDFunction) -> BDDFunction {
        let mut ef = self.ff.clone();
        loop {
            let next = good.or(&self.diamond_pre(&ef)).unwrap();
            if next == ef {
                return ef;
            }
            ef = next;
        }
    }

    /// `νX. r ∧ ◇X` — the ENV-MAINTAINABLE region within `r`: states from which the environment can
    /// pick inputs to STAY in `r` forever (Eve owns all inputs, `◇` = ∃input is her move). With
    /// `r = ef_region(good)` this is the set from which an environment strategy keeps `AG EF good`.
    /// Greatest fixpoint from ⊤.
    pub fn env_maintain_region(&self, r: &BDDFunction) -> BDDFunction {
        let mut s = self.tt.clone();
        loop {
            let next = r.and(&self.diamond_pre(&s)).unwrap();
            if next == s {
                return s;
            }
            s = next;
        }
    }

    /// The `(state, input)` pairs whose constraint-respecting successor lies in `keep`:
    /// `to_next(keep) ∧ constraint`. Conjoined with a state set, it is the strategy's admissible
    /// moves from those states — the raw material for extracting a concrete strategy witness.
    pub fn move_into(&self, keep: &BDDFunction) -> BDDFunction {
        self.to_next(keep).and(&self.constraint).unwrap()
    }

    /// `μX. (region ∧ target) ∨ (region ∧ ◇X)` — states in `region` from which `target` is reachable
    /// WITHOUT leaving `region`. Least fixpoint from the region-restricted seed. Used to gate an
    /// environment strategy's non-vacuity: from `init` inside the maintainable region `S`, can the
    /// environment reach a `¬good` state while staying in `S` (i.e. does the strategy genuinely LEAVE
    /// `good`, or does it vacuously sit on `good` forever)?
    pub fn ef_within(&self, region: &BDDFunction, target: &BDDFunction) -> BDDFunction {
        let seed = target.and(region).unwrap();
        let mut ef = seed.clone();
        loop {
            let step = region.and(&self.diamond_pre(&ef)).unwrap();
            let next = seed.or(&step).unwrap();
            if next == ef {
                return ef;
            }
            ef = next;
        }
    }

    /// D1.2 — evaluate a **2-valued** μ-calculus `formula` over the exact model,
    /// returning the BDD of states that satisfy it. No abstraction ⇒ the answer is
    /// definite (no `⊥`); the whole modal-μ fragment is decided exactly, bounded
    /// only by BDD size. `atoms` maps each predicate-atom name to its full-state
    /// BDD (the caller builds these via [`BddBitBlaster::predicate_bdd`]); an
    /// unresolved atom is `⊥` (empty set), never silently true.
    ///
    /// Modalities are **bare** `[]`/`<>` in D1.2 (the fragment the liveness
    /// showcase uses). Guarded / controllability / step-bounded modalities are an
    /// honest error — state-predicate guards over the exact relation are a D1.2b
    /// follow-up (they mirror [`AbstractRelation::guarded_relations`], 2-valued).
    pub fn evaluate(
        &self,
        formula: &Formula,
        atoms: &HashMap<&str, BDDFunction>,
    ) -> Result<BDDFunction, String> {
        let mut bindings: HashMap<FormulaVarId, BDDFunction> = HashMap::new();
        self.eval_node(formula, formula.root(), atoms, &mut bindings)
    }

    /// Evaluate a specific sub-node `id` of `formula` to its state-set BDD (fresh
    /// bindings). D1.8b uses it to get the target-predicate BDD of a detected `AF p`.
    /// Sound only when `id` has no free fixpoint variables (the `AF` target doesn't).
    pub fn eval_at(
        &self,
        formula: &Formula,
        id: crate::mu_calculus::NodeId,
        atoms: &HashMap<&str, BDDFunction>,
    ) -> Result<BDDFunction, String> {
        let mut bindings: HashMap<FormulaVarId, BDDFunction> = HashMap::new();
        self.eval_node(formula, id, atoms, &mut bindings)
    }

    fn eval_node(
        &self,
        f: &Formula,
        id: crate::mu_calculus::NodeId,
        atoms: &HashMap<&str, BDDFunction>,
        bindings: &mut HashMap<FormulaVarId, BDDFunction>,
    ) -> Result<BDDFunction, String> {
        Ok(match f.node(id) {
            MuNode::True => self.tt.clone(),
            MuNode::False => self.ff.clone(),
            MuNode::Predicate(name) => atoms
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| self.ff.clone()),
            MuNode::Variable(v) => bindings
                .get(v)
                .cloned()
                .ok_or_else(|| format!("unbound fixpoint variable {v:?}"))?,
            MuNode::Not(n) => self.eval_node(f, *n, atoms, bindings)?.not().unwrap(),
            MuNode::And(a, b) => {
                let av = self.eval_node(f, *a, atoms, bindings)?;
                let bv = self.eval_node(f, *b, atoms, bindings)?;
                av.and(&bv).unwrap()
            }
            MuNode::Or(a, b) => {
                let av = self.eval_node(f, *a, atoms, bindings)?;
                let bv = self.eval_node(f, *b, atoms, bindings)?;
                av.or(&bv).unwrap()
            }
            MuNode::Modal {
                kind,
                guard,
                target,
            } => {
                // The exact engine supports bare `[]`/`<>` (single-agent) and the CONTROLLABILITY axis
                // (`Control::{Controllable, Environment}`, the two-player game — P2.5-F). Other guard
                // axes (labels / current / next / step-bounded) remain a follow-up.
                let control_only = *guard
                    == Guard {
                        control: guard.control,
                        ..Guard::default()
                    };
                if !control_only {
                    return Err(
                        "exact μ-calculus MC: only bare `[]`/`<>` and controllability (`ctrl`) \
                         modalities are supported — labels / state-predicate / step-bounded guards \
                         are a follow-up"
                            .into(),
                    );
                }
                let phi = self.eval_node(f, *target, atoms, bindings)?;
                match guard.control {
                    // Single-agent: box = ∀input, diamond = ∃input.
                    Control::All => match kind {
                        ModalKind::Box => self.box_pre(&phi),
                        ModalKind::Diamond => self.diamond_pre(&phi),
                    },
                    // Two-player: the controllability structure subsumes the box/diamond kind (matching
                    // the explicit reference `evaluator.rs::modal_trit_core`).
                    Control::Controllable => self.cpre_controllable(&phi),
                    Control::Environment => self.cpre_environment(&phi),
                }
            }
            MuNode::Mu { var, body } => self.fixpoint(f, *var, *body, atoms, bindings, false)?,
            MuNode::Nu { var, body } => self.fixpoint(f, *var, *body, atoms, bindings, true)?,
        })
    }

    /// Kleene iteration for a least (`greatest=false`, from `⊥`) or greatest
    /// (`greatest=true`, from `⊤`) fixpoint. Convergence is exact set equality —
    /// ROBDDs are canonical, so `==` is the fixpoint test, and over a finite state
    /// space it converges in ≤ |states| steps (the iteration *is* the ranking).
    fn fixpoint(
        &self,
        f: &Formula,
        var: FormulaVarId,
        body: crate::mu_calculus::NodeId,
        atoms: &HashMap<&str, BDDFunction>,
        bindings: &mut HashMap<FormulaVarId, BDDFunction>,
        greatest: bool,
    ) -> Result<BDDFunction, String> {
        let mut x = if greatest {
            self.tt.clone()
        } else {
            self.ff.clone()
        };
        loop {
            // Iteration budget — bail deterministically before a wide-counter diameter (`2^W`)
            // fixpoint hangs. Counted across ALL fixpoints in this `evaluate` (nested νμ share
            // the running total), so a bounded total work regardless of nesting.
            let n = self.iters.get() + 1;
            self.iters.set(n);
            if n > self.iter_budget {
                return Err(format!(
                    "symbolic bit-blaster: abstained on the ITERATION budget ({n} > {}) — the \
                     property's reachable diameter is too large to bit-blast; raise \
                     MUNUNU_BDD_ITER_BUDGET, or use `--engine explicit`",
                    self.iter_budget
                ));
            }
            // Wall-clock backstop beside the count budget — bail a deep-counter fixpoint (whose
            // cheap-but-many preimages reach `iter_budget` only after minutes) in seconds. The
            // per-iteration preimage is the interruptible unit; the `Instant::now()` read is
            // SAMPLED every `TIME_CHECK_STRIDE` iterations, not every one, so a fast control
            // fixpoint (converges in < a stride) never touches the clock — zero overhead on the
            // common path, ~stride×per-iter granularity on a deep loop. See [`Self::deadline`] for
            // why a wall clock is sound here despite the count budget.
            const TIME_CHECK_STRIDE: usize = 256;
            if n.is_multiple_of(TIME_CHECK_STRIDE)
                && let Some(dl) = self.deadline
                && std::time::Instant::now() > dl
            {
                return Err(format!(
                    "symbolic bit-blaster: abstained on the WALL-CLOCK budget (fixpoint still \
                     iterating at {n} steps) — the property's reachable diameter is too large to \
                     decide in the time budget; raise MUNUNU_BDD_TIME_BUDGET_MS (default 10000 ms; \
                     0 disables), or use `--engine explicit`"
                ));
            }
            bindings.insert(var, x.clone());
            let next = self.eval_node(f, body, atoms, bindings)?;
            if next == x {
                bindings.remove(&var);
                return Ok(next);
            }
            x = next;
        }
    }

    /// P2.2c #4 — the SOUND diameter PRE-PASS. Iterate the `EF(target)` least fixpoint
    /// (`μX.(target ∨ ◇X)`) from `⊥` for up to `k_max` diamond-preimage steps and report where it
    /// saturates. This runs the exact engine's OWN inner reachability fixpoint, bounded — so it
    /// *measures* the backward-reach diameter to `target` rather than guessing it from structure.
    ///
    /// It is the sound superset of [`ModelFacts::cone_counter_diameter_log2`]: a near target
    /// saturates in a few steps (`Saturated(small)`), and a wide-diameter recovery of ANY shape —
    /// an up-counter, a down-drain, a deep sequential chain — `ExceedsBound(k_max)`, with no
    /// over- or under-prediction (the counter proxy misses up-counters and over-fires on a
    /// counter-in-cone whose property does not traverse it; this cannot, because it evaluates the
    /// real transitions). Cheap: `k_max` preimages on the already-built relation, `k_max` small.
    ///
    /// USE: routing on the result is still a heuristic *cutoff* — `ExceedsBound(k_max)` soundly
    /// means the diameter > `k_max`, but concluding "the exact engine will not decide" requires
    /// `k_max` above the largest *decidable* diameter (empirically small; grinds run to the
    /// `1<<20` budget). So it is a sound estimator + a deadline/early-abort input, and — because
    /// `Saturated` means the `EF(target)` reach-set is now known — a decision short-cut on the
    /// cheap path. Not memoized (varies per target).
    ///
    /// [`ModelFacts::cone_counter_diameter_log2`]: crate::adapter::btor2::model_facts::ModelFacts::cone_counter_diameter_log2
    pub fn reach_diameter_to(&self, target: &BDDFunction, k_max: usize) -> DiameterEstimate {
        let mut x = self.ff.clone();
        for depth in 0..k_max {
            // ◇x under the exact modal preimage (∃ inputs, constraint-respecting), then
            // `target ∨ ◇x` — the monotone `EF(target)` step. Saturation `next == x` is exact
            // (ROBDDs are canonical), and the depth at saturation IS the reach diameter.
            let pre = self.diamond_pre(&x);
            let next = target.or(&pre).unwrap();
            if next == x {
                return DiameterEstimate::Saturated(depth);
            }
            x = next;
        }
        DiameterEstimate::ExceedsBound(k_max)
    }
}

/// D1.3 — the definite verdict of exact symbolic model checking: a property
/// `Holds` iff every initial state satisfies it, else it is `Violated` (there is a
/// reachable-from-init counterexample class). 2-valued — the exact engine never
/// returns `⊥`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactVerdict {
    Holds,
    Violated,
}

/// D1.4 — rewrite every register name in a [`PredicateExpr`] through `resolve`
/// (the `BtorSts::resolve_register` alias→canonical map), so an atom over a
/// user-visible name binds against the bit-blasted state cell.
fn resolve_predicate_expr_registers(
    expr: &PredicateExpr,
    resolve: &impl Fn(&str) -> String,
) -> PredicateExpr {
    match expr {
        PredicateExpr::Cmp {
            register,
            op,
            value,
        } => PredicateExpr::Cmp {
            register: resolve(register),
            op: *op,
            value: *value,
        },
        PredicateExpr::CmpReg { lhs, op, rhs } => PredicateExpr::CmpReg {
            lhs: resolve(lhs),
            op: *op,
            rhs: resolve(rhs),
        },
        PredicateExpr::CmpRegAddend {
            lhs,
            op,
            rhs,
            addend,
            width,
        } => PredicateExpr::CmpRegAddend {
            lhs: resolve(lhs),
            op: *op,
            rhs: resolve(rhs),
            addend: *addend,
            width: *width,
        },
        PredicateExpr::And(a, b) => PredicateExpr::And(
            Box::new(resolve_predicate_expr_registers(a, resolve)),
            Box::new(resolve_predicate_expr_registers(b, resolve)),
        ),
        PredicateExpr::Or(a, b) => PredicateExpr::Or(
            Box::new(resolve_predicate_expr_registers(a, resolve)),
            Box::new(resolve_predicate_expr_registers(b, resolve)),
        ),
        PredicateExpr::Select {
            array,
            index,
            op,
            value,
        } => PredicateExpr::Select {
            array: resolve(array),
            index: resolve(index),
            op: *op,
            value: *value,
        },
        PredicateExpr::Not(a) => {
            PredicateExpr::Not(Box::new(resolve_predicate_expr_registers(a, resolve)))
        }
    }
}

/// R-F5.6 — collect every register name a [`PredicateExpr`] references (already resolved to
/// canonical cell names). These are the seed atoms for the cone-of-influence keep-set that
/// restricts the bit-blaster to the property's cone (used by both the exact engine and, via
/// `symbolic_engine`, the symbolic cube-BDD engine).
pub(crate) fn collect_predicate_registers(
    expr: &PredicateExpr,
    out: &mut std::collections::HashSet<String>,
) {
    match expr {
        PredicateExpr::Cmp { register, .. } => {
            out.insert(register.clone());
        }
        PredicateExpr::CmpReg { lhs, rhs, .. } | PredicateExpr::CmpRegAddend { lhs, rhs, .. } => {
            out.insert(lhs.clone());
            out.insert(rhs.clone());
        }
        PredicateExpr::And(a, b) | PredicateExpr::Or(a, b) => {
            collect_predicate_registers(a, out);
            collect_predicate_registers(b, out);
        }
        PredicateExpr::Select { array, index, .. } => {
            // Both the array cell AND the index register must be kept in-cone.
            out.insert(array.clone());
            out.insert(index.clone());
        }
        PredicateExpr::Not(a) => collect_predicate_registers(a, out),
    }
}

/// The exact engine's cone SEED ATOMS for a formula — the register names its predicate atoms
/// reference (via [`collect_predicate_registers`]). These seed the cone-of-influence keep-set
/// that sets how many register+input bits the exact engine must bit-blast; the planner reads
/// them (with `ModelFacts::cone_bits`) to predict a cone-over-cap Skip and pick pinnable
/// inputs (P2.2). This is the UNRESOLVED extraction (no alias canonicalization) —
/// diagnostic-grade for the planner's cost estimate; the exact engine additionally resolves
/// aliases for its authoritative keep-set. An atom-free formula yields no seeds ⇒ the caller
/// treats the whole design as the cone.
pub(crate) fn formula_seed_atoms(formula: &Formula) -> Vec<String> {
    let mut regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for node in formula.nodes() {
        if let MuNode::Predicate(name) = node
            && let Ok(expr) = parse_predicate_atom_bool(name)
        {
            collect_predicate_registers(&expr, &mut regs);
        }
    }
    let mut v: Vec<String> = regs.into_iter().collect();
    v.sort();
    v
}

/// Directly decide whether ANY `bad` property of a BTOR2 design is REACHABLE from the reset
/// state — the native HWMCC safety question (a reachable `bad` = the safety property is VIOLATED,
/// "SAT" in competition terms; unreachable = "UNSAT" / safe). This bridges an ARBITRARY `bad`
/// circuit (not just a register-value predicate, which [`exact_symbolic_verdict`] requires) to the
/// exact engine: it ORs the `bad` nodes' 1-bit BDDs and evaluates `EF(bad) = μY. (bad ∨ ◇Y)` over
/// the full bit-blasted transition relation, then tests whether the modelled reset state lies in
/// it. The exact analogue of btormc's bad-reachability — the oracle the reachability differential
/// + the portfolio coverage study compare against.
///
/// Returns `Ok(true)` (reachable / SAT / unsafe), `Ok(false)` (unreachable / UNSAT / safe), or an
/// `Err` (undecided) when the design is over the bit cap, carries an unsupported op, or has
/// `constraint`/`fair` lines. **The constraint guard is a soundness requirement:** this engine
/// does not restrict reachability to constraint-satisfying runs, so on a constrained design it
/// would OVER-approximate (report a spuriously-reachable `bad` that btormc, honouring the
/// constraint, calls safe). Refusing to decide there keeps every verdict it DOES emit sound.
pub fn exact_bad_reachable(btor2_content: &str) -> Result<bool, String> {
    use crate::mu_calculus::parser as mu_parser;
    let file = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("exact bad-reachability: parse: {}", e.message))?;

    // `constraint` (assume) is now modelled soundly: the bit-blaster folds every
    // constraint into `ExactModel`'s pre-image (`diamond_pre` / `box_pre`), so the
    // `EF(bad)` fixpoint below ranges only over constraint-respecting runs — no
    // over-approximation, no refusal. `fair` is irrelevant to a finite safety
    // reachability query (it constrains infinite justice runs) and is ignored.
    let bad_ops: Vec<Operand> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Bad { signal } => Some(*signal),
            _ => None,
        })
        .collect();
    if bad_ops.is_empty() {
        return Err("exact bad-reachability: the design has no `bad` property".into());
    }

    let bb = BddBitBlaster::build(&file)?; // cap-guarded: a wide design ⇒ Err ⇒ undecided
    // OR the `bad` nodes' 1-bit BDDs into a single "some bad holds" state set.
    let mut bad = bb.ff.clone();
    for op in &bad_ops {
        let bits = bb.resolve(*op)?;
        let b = bits
            .into_iter()
            .next()
            .ok_or_else(|| "exact bad-reachability: a `bad` signal has zero width".to_string())?;
        bad = bad.or(&b).unwrap();
    }

    let model = bb.exact_model();
    // The `bad` target must itself be constraint-consistent: btormc checks the
    // constraint at the step where `bad` holds too, so reaching a state that
    // violates every constraint is not a valid counterexample. Conjoining the
    // constraint-satisfiable states leaves an unconstrained design unchanged
    // (`constraint_satisfiable() = tt`). The constrained `◇` (`diamond_pre`)
    // already restricts every intermediate step.
    let bad = bad.and(&model.constraint_satisfiable()).unwrap();
    let formula = mu_parser::parse("mu Y. (BAD or <> Y)").expect("EF(bad) formula parses");
    let mut atoms: HashMap<&str, BDDFunction> = HashMap::new();
    atoms.insert("BAD", bad);
    let reach = model.evaluate(&formula, &atoms)?;
    let init = bb.initial_state_bdd(&file)?;
    let reachable = init.and(&reach).unwrap() != bb.ff;

    // BOTH directions are sound since mununu#498's sibling fix: `initial_state_bdd`
    // models the design's initial set EXACTLY (a state with no `init` line is left
    // free, per the BTOR2 format) rather than pinning un-`init`ed registers to 0.
    // The engine therefore no longer has to refuse an UNREACHABLE verdict on a
    // free-init design — the case that used to abstain (btor2tools `noninitstate`,
    // where btormc reaches `bad` from a non-zero initial value the pinned model
    // could not see) is now DECIDED, and decided the same way btormc decides it.
    Ok(reachable)
}

/// D1.3 — exact full-state symbolic μ-calculus model checking end-to-end: parse the
/// BTOR2, bit-blast it, evaluate `formula` over the exact model (2-valued, no
/// abstraction), and return the initial-state verdict. Atoms in `formula` are
/// register-comparison predicates (`(reg op val)`) resolved via
/// [`parse_predicate_expr`] → [`BddBitBlaster::predicate_bdd`]; an atom that does
/// not parse is an error (never silently `⊥`). Bounded by BDD size, not by the
/// explicit-state cap: decides the whole modal-μ fragment (safety, `AG EF`,
/// `AF`-liveness, GR(1)) exactly, where predicate abstraction may return `⊥`.
pub fn exact_symbolic_verdict(
    btor2_content: &str,
    formula: &Formula,
) -> Result<ExactVerdict, String> {
    exact_symbolic_verdict_with_witness(btor2_content, formula).map(|(v, _)| v)
}

/// D1.8b — like [`exact_symbolic_verdict`], but on a `Violated` verdict for a bare
/// `AF p`-shaped property (`μX. (p ∨ [] X)`) it also returns a concrete
/// [`StallLasso`] counterexample: the reachable `¬p` stall witnessing that liveness
/// fails from the reset state. The witness is `None` when the verdict is `Holds`,
/// when `formula` is not that shape, or when the stall is not reachable *at* the
/// initial state (a reachable-but-not-initial stall — the `AG AF` case — is a future
/// extension of [`BddBitBlaster::exact_stall_lasso`]).
pub fn exact_symbolic_verdict_with_witness(
    btor2_content: &str,
    formula: &Formula,
) -> Result<(ExactVerdict, Option<StallLasso>), String> {
    verdict_with_witness_catching(
        btor2_content,
        formula,
        &std::collections::HashSet::new(),
        &ExactSymbolicOptions::default(),
    )
}

/// Options for the exact-symbolic engine — thread-safe per-invocation overrides
/// for behaviours that were previously env-var-only. Introduced 2026-08 to
/// support `--no-antecedent-shadow` (CLI) / `no_antecedent_shadow` (API) as
/// proper per-request opt-outs; the env-var `MUNUNU_NO_ANTECEDENT_SHADOW=1`
/// remains as a process-global escape hatch (differential-oracle / debug),
/// and either channel disabling shadow-synth wins (`false && env-var`).
#[derive(Debug, Clone, Copy)]
pub struct ExactSymbolicOptions {
    /// Whether antecedent shadow-register synthesis (mununu#476 fix) is
    /// enabled. Defaults to `true`; set to `false` to force the Phase A
    /// refusal for SVA `|=>` properties with input-derived antecedents, so a
    /// caller can differentially verify the shadow-synth verdict against the
    /// refusal path or reproduce pre-mununu#476 behaviour.
    pub antecedent_shadow_enabled: bool,
}

impl Default for ExactSymbolicOptions {
    fn default() -> Self {
        Self {
            antecedent_shadow_enabled: true,
        }
    }
}

/// Opts-accepting variant of [`exact_symbolic_verdict`]. Same semantics; the
/// only per-call knob today is [`ExactSymbolicOptions::antecedent_shadow_enabled`].
pub fn exact_symbolic_verdict_with_options(
    btor2_content: &str,
    formula: &Formula,
    opts: &ExactSymbolicOptions,
) -> Result<ExactVerdict, String> {
    verdict_with_witness_catching(
        btor2_content,
        formula,
        &std::collections::HashSet::new(),
        opts,
    )
    .map(|(v, _)| v)
}

/// Opts-accepting variant of [`exact_symbolic_verdict_with_witness`].
pub fn exact_symbolic_verdict_with_witness_and_options(
    btor2_content: &str,
    formula: &Formula,
    opts: &ExactSymbolicOptions,
) -> Result<(ExactVerdict, Option<StallLasso>), String> {
    verdict_with_witness_catching(
        btor2_content,
        formula,
        &std::collections::HashSet::new(),
        opts,
    )
}

/// P2.5-F — the SOUND-POSTURE two-player game model: exclude the CLOCK and RESET from the adversary.
/// The raw lifted BTOR2 carries `clk`/`rst` as free primary inputs, so a two-player game treats them as
/// ADVERSARIAL — the environment can FREEZE THE CLOCK (hold `clk` constant → the design never advances)
/// or HOLD RESET (→ the FSM sticks in reset), making the reachability game spuriously unrealizable for a
/// MODELING reason, not a functional one. In real synchronous verification the clock ticks and reset is a
/// controlled posture. This transform models that: inject the post-reset `init`
/// ([`crate::adapter::btor2::reset_init::inject_reset_init`]), then PIN the detected reset(s) to their
/// inactive level and the clock(s) to a constant — so the game is over the FUNCTIONAL inputs only. Returns
/// the content unchanged when there is no detected reset/clock (a pure-functional design is already sound).
///
/// **Soundness:** reset-released + reset-init IS the design's normal operational start (the same
/// operational model the recoverability path uses); the clock is near-dangling in an `async2sync`-lifted
/// design, so pinning it is a no-op on the transition. The resulting verdict/strategy is the genuine
/// FUNCTIONAL game, free of the clock-freeze / reset-hold artifacts. Opt-in (the raw model is preserved
/// for callers that want the literal all-inputs-adversarial reading).
pub fn game_sound_posture_model(btor2_content: &str) -> String {
    use crate::adapter::btor2::ast::Node;
    let Ok(file) = crate::adapter::btor2::parser::parse(btor2_content) else {
        return btor2_content.to_string();
    };
    let symbols = crate::adapter::btor2::parser::collect_symbols(&file);
    // Detected reset(s): (input name, inactive level). Post-reset `init` first, so pinning the reset
    // inactive does not leave a HAVOC initial state (the reset would otherwise set the start state).
    let resets = crate::adapter::fsm_scan::detect_resets(&file, &symbols);
    let m = crate::adapter::btor2::reset_init::inject_reset_init(btor2_content, &resets)
        .unwrap_or_else(|_| btor2_content.to_string());
    let mut pins: Vec<(String, u64)> = resets;
    // Clock(s): pin to 0 — near-dangling in async2sync, so this excludes any residual clock-gating from
    // the adversary without changing functional behaviour.
    let is_clock = |s: &str| -> bool {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "clk" | "clock" | "ck" | "i_clk" | "clk_i" | "iclk" | "clki"
        )
    };
    for line in &file.lines {
        if let Node::Input { .. } = &line.node
            && let Some(name) = symbols.get(&line.nid)
            && is_clock(name)
        {
            pins.push((name.clone(), 0));
        }
    }
    if pins.is_empty() {
        return m;
    }
    crate::adapter::btor2::pin::pin_inputs_to_constants(&m, &pins).0
}

/// P2.5-F — decide a Control-tagged μ-calculus `formula` on the TWO-PLAYER exact game, with
/// `controllable` naming the controller's input signals (the rest are the environment's). The
/// formula's `<(ctrl=controllable)>` / `[(ctrl=controllable)]` modalities are evaluated by the
/// controllable predecessor ([`ExactModel::cpre_controllable`]); `Control::All` modalities remain the
/// single-agent pre-image. `Holds` iff the controller wins from the initial state (the specification is
/// realizable under the partition). Same engine, cone restriction, and soundness posture as
/// [`exact_symbolic_verdict`]; the explicit engine (`gr1.rs` / `evaluator.rs`) is the differential
/// oracle.
pub fn exact_two_player_verdict(
    btor2_content: &str,
    formula: &Formula,
    controllable: &[&str],
) -> Result<ExactVerdict, String> {
    let set: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    verdict_with_witness_catching(
        btor2_content,
        formula,
        &set,
        &ExactSymbolicOptions::default(),
    )
    .map(|(v, _)| v)
}

/// P2.5-F — is the controllable-reachability game to `good` REALIZABLE? Builds the reachability formula
/// `μX. (good ∨ ⟨ctrl⟩X)` and decides it via [`exact_two_player_verdict`]. Unlike
/// [`exact_two_player_strategy`] (which extracts a state-INDEXED strategy and so requires `good` to name a
/// STATE register), this needs only the VERDICT — so `good` may be a combinational OUTPUT or a RELATIONAL
/// atom (`full_o == 1`, `wptr == rptr`), exactly the datapath targets a FIFO-class design uses. `good` is
/// any atom the exact engine's `predicate_bdd` resolves (state cell, named combinational output, or a
/// `REG op REG`/`REG op VALUE` comparison). `Ok(true)` = the controller wins from the initial state.
pub fn exact_two_player_reach_realizable(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Result<bool, String> {
    let formula_str = format!("mu X. (({good}) || <(ctrl = controllable)> X)");
    let formula = crate::mu_calculus::parser::parse(&formula_str)
        .map_err(|e| format!("exact two-player reach: parsing `{formula_str}`: {e:?}"))?;
    Ok(exact_two_player_verdict(btor2_content, &formula, controllable)? == ExactVerdict::Holds)
}

/// P2.5-F — is the controllable-RECURRENCE (Büchi) game "visit `good` INFINITELY OFTEN" REALIZABLE?
/// Builds the recurrence formula `νZ. μY. ((good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y)` and decides it via
/// [`exact_two_player_verdict`] — the nested `νμ` is the genuine Büchi winning region (the exact engine
/// evaluates the two-player nested fixpoint soundly; validated by known answer in
/// `tests/exact_two_player_game.rs::controllable_recurrence_holds_iff_environment_cannot_starve`). Like
/// [`exact_two_player_reach_realizable`], `good` may be any atom `predicate_bdd` resolves (state cell,
/// combinational output, or a relation — a FIFO's `full_o == 1`). `Ok(true)` = the controller can force
/// `good` recurrently against every environment strategy from the initial state.
///
/// This is the OBJECTIVE primitive; a fairness ENVIRONMENT assumption `GF a → GF good` (rescuing an
/// unrealizable recurrence game) is a SEPARATE follow-up that requires the transition-relativized CPre
/// (Klein–Pnueli / Bloem GR(1) input-fairness) — a naive νμν with an input-predicate conjunct is UNSOUND
/// because an input is not part of the state.
pub fn exact_two_player_buchi_realizable(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Result<bool, String> {
    let formula_str = format!(
        "nu Z. (mu Y. ((({good}) && <(ctrl = controllable)> Z) || <(ctrl = controllable)> Y))"
    );
    let formula = crate::mu_calculus::parser::parse(&formula_str)
        .map_err(|e| format!("exact two-player recurrence: parsing `{formula_str}`: {e:?}"))?;
    Ok(exact_two_player_verdict(btor2_content, &formula, controllable)? == ExactVerdict::Holds)
}

/// P2.5-F — is the RECURRENCE game `GF good` REALIZABLE UNDER the FAIRNESS environment assumption
/// `GF assume` (the GR(1) 1-pair objective `GF assume → GF good`)?
///
/// **Soundness — why the assumption is LATCHED, not written into the formula.** `assume` is typically a
/// PRIMARY-INPUT predicate (`incr_rptr_i == 0`); an input is not part of the state, so it cannot appear
/// directly in a state-fairness fixpoint (a naive νμν with an input-predicate conjunct would be
/// unsound — the input is ∀/∃-quantified by the surrounding CPre and the fixpoint types are muddled).
/// We latch the assumption event into a fresh 1-bit state `mununu_assume_prev`
/// ([`crate::adapter::btor2::bad_monitor::emit_latched_predicate_state`]) — a pure monitor for which
/// `GF(mununu_assume_prev == 1) ⟺ GF assume` — and decide the STATE-fairness GR(1) 1-pair game
///
/// ```text
/// νZ. μY. νX. ( (good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y ∨ (mununu_assume_prev == 0 ∧ ⟨ctrl⟩X) )
/// ```
///
/// EXACTLY via [`exact_two_player_verdict`] (Piterman–Pnueli–Sa'ar). The νX "stall" branch captures the
/// system winning for free when the assumption is only finitely satisfied; when the environment plays
/// `assume` infinitely often the stall is disabled infinitely often, forcing μ-progress to `good`. A
/// definite verdict is exact and transfers to the concrete game. With a trivially-true assumption the
/// νX branch vanishes and this reduces to [`exact_two_player_buchi_realizable`].
///
/// engine: `exact-symbolic` ROBDD (OxiDD), triple-nested νμν two-player fixpoint over the
/// assumption-latched model. `Ok(true)` = the controller wins `GF assume → GF good`.
pub fn exact_two_player_gr1_realizable(
    btor2_content: &str,
    good: &str,
    assume: &crate::adapter::btor2::concrete_oracle::OracleAtom,
    controllable: &[&str],
) -> Result<bool, String> {
    let latched = crate::adapter::btor2::bad_monitor::emit_latched_predicate_state(
        btor2_content,
        assume,
        false,
    )
    .map_err(|e| format!("exact two-player GR(1): latching the assumption: {e}"))?;
    let latch = crate::adapter::btor2::bad_monitor::ASSUME_PREV_SYMBOL;
    let formula_str = format!(
        "nu Z. (mu Y. (nu X. ((({good}) && <(ctrl = controllable)> Z) || <(ctrl = controllable)> Y \
         || (({latch} == 0) && <(ctrl = controllable)> X))))"
    );
    let formula = crate::mu_calculus::parser::parse(&formula_str)
        .map_err(|e| format!("exact two-player GR(1): parsing `{formula_str}`: {e:?}"))?;
    Ok(exact_two_player_verdict(&latched, &formula, controllable)? == ExactVerdict::Holds)
}

/// P2.5-F — is `GF good` REALIZABLE under a CONJUNCTION of fairness assumptions
/// `GF a_1 ∧ … ∧ GF a_m` (the multi-pair GR(1) objective `(⋀_i GF a_i) → GF good`)? Used when NO single
/// justice assumption rescues an unrealizable recurrence game but a SET of them does (the fairness analog
/// of [`crate::adapter::recoverability`]'s reach conjunctive slice).
///
/// **Soundness.** Each input assumption `a_i` is LATCHED into its own monitor state
/// `mununu_assume_prev_i` (same pure-monitor argument as [`exact_two_player_gr1_realizable`], one latch
/// per assumption), so `GF(mununu_assume_prev_i == 1) ⟺ GF a_i`. The controller then wins the
/// STATE-fairness MULTI-pair GR(1) game (Piterman–Pnueli–Sa'ar), decided EXACTLY by
/// [`exact_two_player_verdict`]:
///
/// ```text
/// νZ. μY. ⋁_i νX_i. ( (good ∧ ⟨ctrl⟩Z) ∨ ⟨ctrl⟩Y ∨ (mununu_assume_prev_i == 0 ∧ ⟨ctrl⟩X_i) )
/// ```
///
/// The `⋁_i` over per-assumption `νX_i` "stall" branches encodes "the system wins if `good` recurs OR
/// SOME assumption `a_i` is satisfied only finitely often" (`(⋀ GF a_i) → GF good` ≡ `GF good ∨ ⋁_i
/// FG ¬a_i`). For `m = 1` it reduces exactly to [`exact_two_player_gr1_realizable`]; for `m = 0` (no
/// assumption) to [`exact_two_player_buchi_realizable`]. `Ok(true)` = the controller wins under the
/// conjunction. engine: `exact-symbolic` ROBDD (OxiDD) over the `m`-latch model.
pub fn exact_two_player_gr1_conjunction_realizable(
    btor2_content: &str,
    good: &str,
    assumes: &[crate::adapter::btor2::concrete_oracle::OracleAtom],
    controllable: &[&str],
) -> Result<bool, String> {
    use crate::adapter::btor2::bad_monitor::{
        ASSUME_PREV_SYMBOL, emit_latched_predicate_state_named,
    };
    if assumes.is_empty() {
        return exact_two_player_buchi_realizable(btor2_content, good, controllable);
    }
    let ctrl = "<(ctrl = controllable)>";
    let mut content = btor2_content.to_string();
    let mut disjuncts: Vec<String> = Vec::with_capacity(assumes.len());
    for (i, a) in assumes.iter().enumerate() {
        let sym = format!("{ASSUME_PREV_SYMBOL}_{i}");
        content = emit_latched_predicate_state_named(&content, a, &sym, false).map_err(|e| {
            format!("exact two-player GR(1) conjunction: latching assumption {i}: {e}")
        })?;
        // One νX_i stall branch per assumption; X_i is a fresh fixpoint variable.
        disjuncts.push(format!(
            "(nu X{i}. ((({good}) && {ctrl} Z) || {ctrl} Y || (({sym} == 0) && {ctrl} X{i})))"
        ));
    }
    let formula_str = format!("nu Z. (mu Y. ({}))", disjuncts.join(" || "));
    let formula = crate::mu_calculus::parser::parse(&formula_str).map_err(|e| {
        format!("exact two-player GR(1) conjunction: parsing `{formula_str}`: {e:?}")
    })?;
    Ok(exact_two_player_verdict(&content, &formula, controllable)? == ExactVerdict::Holds)
}

/// P2.5-F — decide a Control-tagged μ-calculus `formula` on the TWO-PLAYER KMTS (3-valued predicate
/// cube) game: the scale backend of the unified verifier (past the exact BDD cap). `predicates` is the
/// abstraction (name → atom); `controllable` names the controller's inputs. Returns the 3-valued verdict
/// at the initial cube — `True` (controller wins / spec realizable), `False` (environment wins), or
/// `Unknown` (the abstraction is too coarse; a CEGAR refinement of the predicates is the follow-up).
///
/// **Soundness (Bruns–Godefroid).** Definite (`True`/`False`) verdicts transfer to the concrete game at
/// every alternation depth. The controllable predecessor plays the exact concrete `∀env ∃ctrl` game
/// within each concrete state and lifts by the `∀x`/`∃x` cube abstraction
/// ([`AbstractRelation::cpre_controllable`]); with predicates that pin each state it equals the exact
/// [`exact_two_player_verdict`], which is the differential oracle.
pub fn kmts_two_player_verdict(
    btor2_content: &str,
    formula: &Formula,
    predicates: &[(String, PredicateExpr)],
    controllable: &[&str],
    must_semantics: MustSemantics,
) -> Result<Trit, String> {
    let file = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("adapter/btor2/kmts two-player: {}", e.message))?;
    let exprs: Vec<PredicateExpr> = predicates.iter().map(|(_, e)| e.clone()).collect();
    let names: Vec<String> = predicates.iter().map(|(n, _)| n.clone()).collect();
    // R-F5.6 cone restriction from the predicate registers (mirrors the single-agent cube path).
    let mut seed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &exprs {
        collect_predicate_registers(e, &mut seed);
    }
    let seed_atoms: Vec<String> = seed.into_iter().collect();
    let keep = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));
    let bb = BddBitBlaster::build_with_keep(&file, keep.as_ref())?;
    let ctrl: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    let rel = bb.abstract_game(&exprs, must_semantics, &ctrl)?;
    let verdict = rel.evaluate(formula, &names)?;
    let init = bb.initial_state_bdd(&file)?;
    Ok(rel.verdict_at_init(&verdict, &init))
}

/// P2.5-F (game-CEGAR) — decide the two-player game with automatic predicate REFINEMENT: when the
/// abstraction is too coarse (the initial cube is `⊥`), add the controllable/environment PRE-IMAGE
/// (`CPre`) regions of the current predicates and re-evaluate, until the verdict is definite or
/// `max_refinements` is reached. Returns `(verdict, refinements_used)`.
///
/// **Soundness + convergence.** Every refinement predicate is a state region, so adding it only FINES
/// the abstraction (monotone; a definite verdict never flips — Bruns–Godefroid). The pre-image
/// refinement is the game attractor's layers, so for a decidable reachability game the abstraction
/// becomes definite once the layers containing the initial state are represented — the CEGAR converges
/// to the exact verdict ([`exact_two_player_verdict`] is the oracle). When no new predicate is produced
/// the abstraction is pre-image-closed (the game is fully represented) and the current verdict stands.
///
/// **Scope.** The refinement heuristic is WP-layering (sound + convergent but, on a deep attractor, as
/// many predicates as the diameter — no asymptotic win over `exact` yet). Interpolation-based predicate
/// discovery (the RELEVANT predicate, not every layer) is the scaling follow-up.
pub fn kmts_two_player_verdict_cegar(
    btor2_content: &str,
    formula: &Formula,
    predicates: &[(String, PredicateExpr)],
    controllable: &[&str],
    must_semantics: MustSemantics,
    max_refinements: usize,
) -> Result<(Trit, usize), String> {
    let file = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("adapter/btor2/kmts two-player CEGAR: {}", e.message))?;
    let exprs: Vec<PredicateExpr> = predicates.iter().map(|(_, e)| e.clone()).collect();
    let names: Vec<String> = predicates.iter().map(|(n, _)| n.clone()).collect();
    let mut seed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &exprs {
        collect_predicate_registers(e, &mut seed);
    }
    let seed_atoms: Vec<String> = seed.into_iter().collect();
    let keep = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));
    let bb = BddBitBlaster::build_with_keep(&file, keep.as_ref())?;
    let ctrl: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    let exact = bb.exact_model_partitioned(&ctrl); // the concrete pre-image (WP) operator for refinement
    let init = bb.initial_state_bdd(&file)?;

    // The predicate set: the named formula atoms first (evaluate binds atom name → index by position),
    // then the anonymous CEGAR refinement predicates.
    let mut pred_bdds: Vec<BDDFunction> = exprs
        .iter()
        .map(|e| bb.predicate_bdd(e))
        .collect::<Result<Vec<_>, _>>()?;

    for r in 0..=max_refinements {
        let rel = bb.abstract_game_bdd(&pred_bdds, must_semantics, &ctrl)?;
        let verdict = rel.evaluate(formula, &names)?;
        let v = rel.verdict_at_init(&verdict, &init);
        if v != Trit::Unknown {
            return Ok((v, r));
        }
        // Refine: add the controllable pre-image of each current predicate — the game attractor's next
        // layer. (The pre-images form a chain, so at most one new predicate per iteration: no cube
        // blow-up. Refining with the operator matching the formula's control modality — `cpre_environment`
        // for an `<env>` formula — is the general form; the reachability-controllable formulas here use
        // `cpre_controllable`.)
        let mut added = false;
        let current = pred_bdds.clone();
        for p in &current {
            let wp = exact.cpre_controllable(p);
            if !pred_bdds.contains(&wp) {
                pred_bdds.push(wp);
                added = true;
            }
        }
        if !added {
            // Pre-image-closed: the game is fully represented; the ⊥ is genuine for this game shape.
            return Ok((Trit::Unknown, r));
        }
    }
    Ok((Trit::Unknown, max_refinements))
}

/// Shared `catch_unwind` BACKSTOP for the node-budget guard. A single BDD op (`Op::Mul`, a wide
/// variable shift) can overflow the fixed OxiDD arena *inside* one `eval_op`, before the per-op
/// node-budget check fires — the `.unwrap()` on OxiDD's `OutOfMemory` then panics. That is a clean
/// unwrap-panic (the op released the manager lock before returning `Err`), and the manager is local to
/// the inner call and dropped on unwind, so catching it and degrading to a Skipped-class error is SOUND
/// (no shared state is poisoned; the verdict is never fabricated — only turned into a clean Err →
/// Skipped). Gradual growth is caught earlier + cheaper by the node-budget guard in `eval_op`.
fn verdict_with_witness_catching(
    btor2_content: &str,
    formula: &Formula,
    controllable: &std::collections::HashSet<String>,
    opts: &ExactSymbolicOptions,
) -> Result<(ExactVerdict, Option<StallLasso>), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exact_symbolic_verdict_with_witness_inner(btor2_content, formula, controllable, opts)
    }))
    .unwrap_or_else(|_| {
        Err(
            "symbolic bit-blaster: BDD arena overflow while building the transition relation \
             — abstained on the NODE budget (the cone does not compress to a tractable BDD); \
             raise MUNUNU_BDD_ARENA_NODES, or use `--engine explicit`"
                .to_string(),
        )
    })
}

fn exact_symbolic_verdict_with_witness_inner(
    btor2_content: &str,
    formula: &Formula,
    controllable: &std::collections::HashSet<String>,
    opts: &ExactSymbolicOptions,
) -> Result<(ExactVerdict, Option<StallLasso>), String> {
    let file = crate::adapter::btor2::parser::parse(btor2_content)
        .map_err(|e| format!("adapter/btor2/exact MC: {}", e.message))?;

    // mununu#476 — antecedent shadow-register synthesis. If the formula matches
    // the canonical SVA `|=>` shape (Or(Not(Predicate(A)), Modal{Box, _})),
    // synthesise a shadow state register for every antecedent atom `A` whose
    // combinational cone reaches primary inputs. The augmented BTOR2 file +
    // rewritten formula are then handed to the rest of this fn. Atoms that
    // cannot be shadowed (see `RefusalReason`) fall through to the Phase A
    // refusal below. See `docs/design/antecedent-shadow-synthesis.md` for the
    // full design + soundness argument.
    //
    // Two orthogonal opt-out channels; EITHER disabling wins:
    //   1. `opts.antecedent_shadow_enabled = false` — per-invocation opt-out
    //      threaded from CLI (`--no-antecedent-shadow`) / API
    //      (`no_antecedent_shadow: true`). Thread-safe, per-request.
    //   2. `MUNUNU_NO_ANTECEDENT_SHADOW=1` env var — process-global escape
    //      hatch for differential-oracle / debug use.
    let (file, rewritten_formula) = {
        let opt_out =
            !opts.antecedent_shadow_enabled || std::env::var("MUNUNU_NO_ANTECEDENT_SHADOW").is_ok();
        let antecedents = crate::mu_calculus::detect_pipeimplies_antecedent_atoms(formula);
        if !opt_out && !antecedents.is_empty() {
            let synth = crate::adapter::btor2::antecedent_shadow::synthesize_shadows(
                &file,
                &antecedents,
                crate::adapter::btor2::antecedent_shadow::ShadowSynthOpts::default(),
            );
            if synth.shadows.is_empty() {
                (file, None)
            } else {
                let rewritten = crate::mu_calculus::substitute_predicates(formula, &synth.renames);
                tracing::info!(
                    n_shadows = synth.shadows.len(),
                    "antecedent shadow-synth: rewrote {} input-derived antecedent atom(s) into shadow registers ({} atoms fell through to fallback refusal)",
                    synth.shadows.len(),
                    synth.refused.len(),
                );
                (synth.augmented, Some(rewritten))
            }
        } else {
            (file, None)
        }
    };
    let formula: &Formula = rewritten_formula.as_ref().unwrap_or(formula);

    // Register-name resolution: a user-visible name (`bit_cnt_q`) maps to the
    // canonical state-cell name the bit-blast binds against (`bit_cnt_d` after
    // yosys async2sync/flatten aliasing). Use the SOUND (Strict) alias resolution,
    // NOT the Loose "nearest state in the cone" BFS: Strict follows only pure
    // aliases (`Output` / `Uext`-`Sext`-by-0 / reset-mux `Ite`) to a state cell, so
    // a register alias still resolves, but a combinational-FUNCTION output
    // (`done = (state == 2)`, a 1-bit signal over the 2-bit `state`) hits the `==`
    // Op and resolves to `None` — the atom then KEEPS its own name and binds to the
    // 1-bit combinational signal (`named_signals`) via `signal_bits`, NOT the
    // wrong-width driving register. The Loose BFS rewrote `done` → `state`, so
    // `done == K` was evaluated as `state == K` (`EF (done == 2)` spuriously
    // `Holds`) — a soundness bug on ANY combinational-function output atom. The
    // exact engine is the differential oracle and must use the sound resolution.
    let resolve = |name: &str| -> String {
        crate::adapter::btor2::parser::resolve_to_canonical_name(
            &file,
            name,
            crate::adapter::btor2::parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };

    // R-F5.6 — resolve each distinct formula atom to a canonical [`PredicateExpr`] BEFORE
    // building the bit-blaster, and collect the register names they reference. Those seed the
    // cone-of-influence keep-set so the bit-blaster bit-blasts only the property's cone
    // (out-of-cone registers/inputs pinned to constants) instead of the whole design — the
    // R-F5.6 scaling fix. An atom-free formula (no seeds) keeps the full-design behaviour.
    let mut exprs: Vec<(String, PredicateExpr)> = Vec::new();
    let mut seed_regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for node in formula.nodes() {
        if let MuNode::Predicate(name) = node {
            if exprs.iter().any(|(n, _)| n == name) {
                continue;
            }
            // The exact engine reads mu-calculus atoms directly, so a bare-boolean
            // atom (`sig`, no comparison) is admitted as `sig != 0` — the "signal is
            // true" reading. (The seeder keeps the strict parser; see
            // `parse_predicate_atom_bool`.)
            let expr = parse_predicate_atom_bool(name)
                .map_err(|e| format!("exact MC: atom `{name}` is not a predicate: {e}"))?;
            let expr = resolve_predicate_expr_registers(&expr, &resolve);
            collect_predicate_registers(&expr, &mut seed_regs);
            exprs.push((name.clone(), expr));
        }
    }

    // R-F5.6 cone + array/$mem lift. Compute the property's cone-of-influence keep-set
    // BEFORE enumerating leaf cells, then auto-havoc any memory whose reads are OUT of that
    // cone so the exact bit-blaster (which cannot bit-blast an array sort) still decides an
    // array-datapath design whose CONTROL cone is array-free — an S-box ROM, a register file
    // the property never reads.
    //
    // SOUNDNESS: an out-of-cone memory does not feed the property, so replacing its reads with
    // free inputs (pinned out-of-cone by the cone restriction) cannot change the verdict. We
    // only havoc when EVERY detected memory is out of cone; an IN-cone memory is left in place
    // (havoc over-approximates the read, which is unsound for liveness on the read contents),
    // so `leaf_cells` errors on its array sort → honest Skip rather than an unsound verdict.
    // `havoc_rewrite_memories` preserves every non-array NID (Reads become same-NID Inputs), so
    // the keep-set computed on the original file stays valid against the rewritten file.
    let seed_atoms: Vec<String> = seed_regs.iter().cloned().collect();
    let keep_set = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));
    // F1 — enum re-encode (before the array-havoc; both preserve NIDs, so the keep-set
    // computed above stays valid). Compress a state register with a provably small set of
    // reachable CONSTANT values (`W` bits → `⌈log₂K⌉`) so its `2^W`-wide bit-blast fits the
    // `MAX_BITBLAST_BITS` cap; a BIJECTION on the reachable states ⇒ verdict-preserving.
    // One-hot FSMs are the common case; binary-encoded FSMs (`{0, 3, 5}`) compress the same
    // way. SKIP a register a property ATOM reads BY VALUE (`c_state == start_a`) —
    // re-encoding changes its value semantics, which would silently break the atom; leaving
    // that one uncompressed keeps the atom sound. `enum_reencode` additionally abstains on
    // any unhandled use of the register.
    let file = {
        let syms = crate::adapter::btor2::parser::collect_symbols(&file);
        let mut f = file;
        for meta in crate::adapter::btor2::bit_blast::detect_enum_states(&f) {
            if syms.get(&meta.nid).is_some_and(|s| seed_regs.contains(s)) {
                continue;
            }
            if let Some(reenc) = crate::adapter::btor2::bit_blast::enum_reencode(&f, &meta) {
                f = reenc;
            }
        }
        f
    };
    let file = {
        let memories = crate::adapter::btor2::bit_blast::detect_btor2_memories(&file);
        let all_out_of_cone = !memories.is_empty()
            && keep_set
                .as_ref()
                .is_some_and(|keep| memories.iter().all(|m| !keep.contains(&m.nid)));
        if all_out_of_cone {
            let havoc: std::collections::HashSet<Nid> = memories.iter().map(|m| m.nid).collect();
            crate::adapter::btor2::bit_blast::havoc_rewrite_memories(&file, &havoc)
                .map_err(|e| format!("exact MC: out-of-cone memory havoc: {e:?}"))?
        } else {
            file
        }
    };
    // STS-IR seam on the (possibly havoc'd) file — the single canonical leaf enumeration.
    let sts = crate::adapter::sts_ir::BtorSts::new(&file);

    // Input-atom soundness guard. This engine leaves primary inputs FREE (they are
    // quantified out by the modalities — see the module header: "inputs stay free and
    // are quantified out"). A formula atom that *pins* a primary input therefore cannot
    // be evaluated as a state predicate: the input copy read by the atom is decoupled
    // from the input copy driving the adjacent `[]`/`<>` transition. This is exactly the
    // SVA `input |=> next-state` shape — the antecedent input and the transition input
    // are the same physical signal in the same cycle, but the engine treats them as
    // independent, yielding a spurious verdict. Refuse to decide rather than emit an
    // unsound Holds/Violated. The default `explicit` cube engine handles input-antecedent
    // properties correctly (it shadow-registers the antecedent). Proper fix in this
    // engine = guarded modalities / an antecedent shadow register — deferred (deep).
    let input_leaf_names: std::collections::HashSet<String> = sts
        .leaf_cells()
        .map_err(|e| format!("exact MC: {e}"))?
        .into_iter()
        .filter(|c| !c.is_state)
        .map(|c| c.name)
        .collect();
    if let Some(bad) = seed_regs.iter().find(|r| input_leaf_names.contains(*r)) {
        return Err(format!(
            "exact MC: atom references primary input `{bad}`. The exact-symbolic engine \
             leaves inputs free/quantified, so a temporal property that pins an input \
             (e.g. an SVA `input |=> next-state` lift) decouples the antecedent from its \
             consequent and would report an unsound verdict. Use the default `explicit` \
             cube engine, or lift the antecedent through a shadow register."
        ));
    }

    // mununu#476 — transitive combinational fan-in guard. When the atom name is NOT
    // itself a primary input but its combinational cone REACHES primary inputs (stopping
    // at register boundaries), pinning the atom's value pins a function of those inputs
    // — the same decoupling problem as pinning an input directly. Without this, a
    // combinational-of-input signal like `y = a && (b == K)` used in an SVA `|=>`
    // antecedent yields a definite `Violated (1 cell)` on correct RTL (see monono's
    // `wb_mem_client`: `mem_rvalid_mine = mem_rvalid && (mem_rid == CLIENT_ID)`).
    // Reuses [`crate::adapter::btor2::parser::cone_inputs`], which walks the
    // combinational cone and stops at `Node::State` — so a combinational-of-STATE
    // signal (which the previous refusal comment explicitly permits) is NOT flagged.
    //
    // Name-to-NID resolution has to cover THREE symbol sources:
    //   1. Input / State symbols (via `collect_symbols`, Pass 1)
    //   2. Op-symbol aliases that `collect_symbols` attaches to their driving state
    //      (Pass 2 — Yosys's `uext _ _ 0 NAME` alias pattern)
    //   3. Output-node symbols pointing at a combinational signal — NOT covered by
    //      `collect_symbols` because they carry no state-alias intent. This is exactly
    //      the case the guard needs to catch (a combinational-of-input `Output`).
    let mut name_to_nid: std::collections::HashMap<String, crate::adapter::btor2::ast::Nid> =
        crate::adapter::btor2::parser::collect_symbols(&file)
            .into_iter()
            .map(|(nid, name)| (name, nid))
            .collect();
    for line in &file.lines {
        if let crate::adapter::btor2::ast::Node::Output {
            symbol: Some(s),
            signal,
        } = &line.node
        {
            // An Output symbol refers to the underlying signal, not to the Output line's
            // own NID. Use it only if there isn't already an Input/State mapping for the
            // same name (Input/State takes precedence for the same-name edge case).
            name_to_nid.entry(s.clone()).or_insert(signal.nid());
        }
    }
    for reg in &seed_regs {
        // Skip atoms already flagged by the exact-name refusal above.
        if input_leaf_names.contains(reg) {
            continue;
        }
        let Some(&nid) = name_to_nid.get(reg) else {
            continue;
        };
        let derived_inputs = crate::adapter::btor2::parser::cone_inputs(&file, nid);
        if !derived_inputs.is_empty() {
            let inputs = derived_inputs
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "exact MC: atom `{reg}` is combinationally driven by primary input(s) {inputs}. \
                 The exact-symbolic engine leaves inputs free/quantified, so a temporal property \
                 whose antecedent depends on inputs decouples the antecedent from its consequent \
                 and would report an unsound verdict. Use the default `explicit` cube engine, or \
                 lift the antecedent through a shadow register."
            ));
        }
    }

    // monono#partsel — the same soundness posture for an atom that binds to a
    // signal reaching an anonymous free INPUT (without being a bare input itself).
    // The yosys-slang lift of a plain-vector partial register assignment
    // (`q[hi:lo] <= d`) models `q`'s unwritten bits as free inputs (havoc) and
    // aliases its name to the `concat` that mixes them; `q == 0` then reads those
    // free inputs and the fixpoint mis-decides (a spurious `Holds` on a register
    // that is reachably non-zero). Refuse rather than emit an unsound verdict — the
    // verify-auto driver skips such a property up front, and this guards the exact
    // engine's other callers (the differential oracle, `btor2 verify`). A faithful
    // state cell (read_verilog / sv2v) and an anonymous split sub-register the
    // NID-indexed COI keeps (packed-2-D `q[idx] <= d`) are both NOT flagged — the
    // latter now decides.
    if let Some(bad) = seed_regs
        .iter()
        .find(|r| crate::adapter::btor2::parser::signal_reaches_anonymous_input(&file, r))
    {
        return Err(format!(
            "exact MC: atom `{bad}` binds to a signal whose unwritten bits the RTL front-end \
             left undriven (modelled as free inputs) — the yosys-slang lift of a plain-vector \
             partial assignment (`{bad}[hi:lo] <= …`). A state predicate over it reads those \
             havoc inputs and would be unsound, so it is refused. Re-lift with the \
             read_verilog / sv2v front-end, which models `{bad}` faithfully."
        ));
    }

    // P2.5-F — validate the two-player partition. Every declared controllable input must be a real
    // primary input of the design. A name matching nothing would otherwise fall back SILENTLY to the
    // environment (a sound but pessimistic all-environment reading, and not the partition the caller
    // asked for) — error instead. On a flattened design the BTOR2 inputs ARE the top module's primary
    // inputs (Yosys `flatten` resolves declared module-to-module wires to internal signals), so a name
    // that is not here is either a typo or an internal signal driven by another module — not a top input.
    if !controllable.is_empty() {
        let mut unknown: Vec<&str> = controllable
            .iter()
            .filter(|c| !input_leaf_names.contains(c.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            let mut inputs: Vec<&str> = input_leaf_names.iter().map(String::as_str).collect();
            inputs.sort_unstable();
            return Err(format!(
                "exact two-player MC: declared controllable input(s) {unknown:?} are not primary \
                 inputs of the design (primary inputs: {inputs:?}). Declare a real top-level input; \
                 for a multi-module design ensure it is a top input, not one driven by another \
                 module's output (which flatten resolves to an internal signal, not a free input)."
            ));
        }
    }

    let bb = BddBitBlaster::build_with_keep(&file, keep_set.as_ref())?;
    let exact = if controllable.is_empty() {
        bb.exact_model()
    } else {
        bb.exact_model_partitioned(controllable)
    };

    // Resolve each atom's full-state BDD against the (cone-restricted) bit-blaster.
    let mut resolved: Vec<(String, BDDFunction)> = Vec::new();
    for (name, expr) in &exprs {
        let bdd = bb.predicate_bdd(expr)?;
        resolved.push((name.clone(), bdd));
    }
    let atoms: HashMap<&str, BDDFunction> = resolved
        .iter()
        .map(|(n, b)| (n.as_str(), b.clone()))
        .collect();

    let sat = exact.evaluate(formula, &atoms)?;
    let init = bb.initial_state_bdd(&file)?;
    // Holds iff init ⊆ sat, i.e. no initial state violates φ.
    let violating = init.and(&sat.not().unwrap()).unwrap();
    if violating == *exact.ff() {
        return Ok((ExactVerdict::Holds, None));
    }
    // Violated — attach a stall lasso when the property is a liveness shape (bare
    // `AF p` or `AG AF p`). The target `p` is evaluated from the same atom set;
    // `exact_reachable_stall_lasso` finds a stall reachable from reset (subsuming the
    // stall-at-reset case), so it covers both shapes. Best-effort: `None` if the shape
    // isn't recognised or no stall is reachable.
    // First the `AF`/`AG AF` stall lasso; failing that, the `AG EF p` recoverability trap path
    // (a reachable state from which `p` is unreachable). Best-effort: `None` if neither shape
    // matches or the witness region is unreachable.
    let witness = detect_af_target(formula)
        .and_then(|p_id| exact.eval_at(formula, p_id, &atoms).ok())
        .and_then(|p_bdd| bb.exact_reachable_stall_lasso(&file, &p_bdd))
        .or_else(|| {
            detect_ag_ef_target(formula)
                .and_then(|p_id| exact.eval_at(formula, p_id, &atoms).ok())
                .and_then(|p_bdd| bb.exact_reachable_trap_path(&file, &p_bdd))
        });
    Ok((ExactVerdict::Violated, witness))
}

/// Phase 2b — the outcome of a symbolic environment-strategy synthesis for `AG EF good`.
#[derive(Debug, Clone)]
pub enum EnvStrategyOutcome {
    /// The environment CAN maintain recoverability: every initial state lies in the env-maintainable
    /// region `S = νX. EF(good) ∧ ◇X`, so a positional strategy (pick an input keeping the system in
    /// `S`) keeps `AG EF good` from every reachable state on its path. `first_move` is a concrete
    /// witness of σ: an input choice at an initial state that stays in `S`.
    Maintainable {
        note: String,
        first_move: std::collections::BTreeMap<String, u128>,
    },
    /// No environment strategy suffices — from some initial state the adversary can force a permanent
    /// trap (`init ⊄ S`). This is the NEGATED game's verdict (`EF AG ¬good`); `trap_path` is the forced
    /// counter-strategy witness (reach a state from which `good` is unreachable), when extractable.
    NotMaintainable { trap_path: Option<StallLasso> },
    /// Not applicable / indeterminate: `good` is not a `REG op VALUE` atom, or the model could not be
    /// built (over the exact cap, in-cone array, input-atom, …). The caller falls back / abstains.
    Inapplicable(String),
}

/// Phase 2b — SYMBOLIC environment-strategy synthesis for `AG EF good` over the bit-blasted BDD.
///
/// Reduction (see [`ExactModel::env_maintain_region`]): `R = EF(good)` is the recoverable region; the
/// env can maintain `AG EF good` iff `init ⊆ S = νX. R ∧ ◇X` (it can stay in `R` forever, `◇` = ∃input
/// being the environment's move). `init ⊆ S` ⇒ [`EnvStrategyOutcome::Maintainable`] with a first-move
/// witness; else [`EnvStrategyOutcome::NotMaintainable`] — the DUAL/NEGATED game (`EF AG ¬good`), whose
/// adversary counter-strategy is the reachable-trap path. Finds POSITIONAL strategies that no constant
/// input-hold (Phase-2a slice 1) expresses (the safe move may differ per state).
///
/// **Engine (§16):** `exact-symbolic` full-state ROBDD (OxiDD) — reuses [`ExactModel`]'s symbolic
/// transition relation + `diamond_pre` (∃input CPre) + the `EF`/`ν` fixpoints. Tool-free, sidecar-free,
/// host-runnable. Bounded by the exact engine's cone/bit cap (an over-cap cone ⇒ `Inapplicable`).
pub fn exact_env_strategy(btor2_content: &str, good: &str) -> Result<EnvStrategyOutcome, String> {
    use crate::adapter::btor2::parser;
    let file = parser::parse(btor2_content)
        .map_err(|e| format!("adapter/btor2/env-strategy: {}", e.message))?;
    let resolve = |name: &str| -> String {
        parser::resolve_to_canonical_name(
            &file,
            name,
            parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };
    // `good` must be a comparison / boolean atom (same admission as the exact verdict path).
    let expr = match parse_predicate_atom_bool(good) {
        Ok(e) => resolve_predicate_expr_registers(&e, &resolve),
        Err(e) => return Ok(EnvStrategyOutcome::Inapplicable(format!("`{good}`: {e}"))),
    };
    // Cone-of-influence keep-set from the atom's registers (mirrors the verdict path's scaling).
    let mut seed: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_predicate_registers(&expr, &mut seed);
    let seed_atoms: Vec<String> = seed.into_iter().collect();
    let keep_set = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));

    let bb = match BddBitBlaster::build_with_keep(&file, keep_set.as_ref()) {
        Ok(bb) => bb,
        Err(e) => return Ok(EnvStrategyOutcome::Inapplicable(e)),
    };
    let exact = bb.exact_model();
    let good_bdd = match bb.predicate_bdd(&expr) {
        Ok(b) => b,
        Err(e) => return Ok(EnvStrategyOutcome::Inapplicable(e)),
    };
    let init = bb.initial_state_bdd(&file)?;

    let r = exact.ef_region(&good_bdd);
    let s = exact.env_maintain_region(&r);
    // init ⊆ S ⟺ init ∧ ¬S is empty.
    let escaping = init.and(&s.not().unwrap()).unwrap();
    if escaping == *exact.ff() {
        // NON-VACUITY (strategy-intrinsic): the maintained region must let the environment LEAVE
        // `good` — else `S ⊆ good` and the "strategy" vacuously sits on `good` forever (AG EF good is
        // trivially true because you are always AT good). Require `init` to reach a `¬good` state while
        // staying in `S`. (Free-model non-vacuity is not enough: the strategy, not the free design,
        // decides whether `good` is left.)
        let not_good = good_bdd.not().unwrap();
        let can_leave = init.and(&exact.ef_within(&s, &not_good)).unwrap();
        if can_leave == *exact.ff() {
            return Ok(EnvStrategyOutcome::Inapplicable(format!(
                "an environment strategy would keep the design in `{good}` forever — vacuous, not a \
                 meaningful recovery"
            )));
        }
        // The environment can maintain recoverability. Witness a first move: an initial state in S
        // with an admissible input keeping the successor in S — `init ∧ S ∧ move_into(S)`.
        let move_set = init.and(&s).unwrap().and(&exact.move_into(&s)).unwrap();
        let first_move = if move_set == *exact.ff() {
            std::collections::BTreeMap::new()
        } else {
            bb.input_assignment(&bb.pick_full_assignment(&move_set))
        };
        Ok(EnvStrategyOutcome::Maintainable {
            note: format!(
                "the environment can maintain `AG EF({good})`: from every initial state it can choose \
                 inputs keeping the design in the recoverable region `EF({good})` forever (a positional \
                 strategy, not necessarily a constant hold)"
            ),
            first_move,
        })
    } else {
        // The adversary forces a permanent trap from some initial state — the NEGATED game. Emit the
        // forced-trap counter-strategy witness (reachable state from which `good` is unreachable).
        let trap_path = bb.exact_reachable_trap_path(&file, &good_bdd);
        Ok(EnvStrategyOutcome::NotMaintainable { trap_path })
    }
}

/// P2.5-E T1 — env-strategy EXISTENCE for ANY μ-calculus property, over the bit-blasted BDD.
///
/// An environment strategy can enforce `formula` from the initial state iff `init ⊆ W`, where
/// `W = evaluate(env_enforce(formula))` is the winning region of the 1-player control game (the
/// environment owns every transition; the `□→◇` [`env_enforce`] rewrite makes each modality the
/// environment's move). Since `exact_symbolic_verdict` already returns `Holds` exactly when `init ⊆
/// evaluate(φ)`, existence is simply `exact_symbolic_verdict(env_enforce(formula)) == Holds`.
///
/// Returns `Ok(Some(true))` (a strategy exists), `Ok(Some(false))` (the adversary can force `¬formula` —
/// solve `invert(formula)`'s game for the counter-strategy region), or `Ok(None)` when the exact engine
/// cannot decide (over the cone/bit cap, in-cone array, input-atom — the caller abstains). GENERALIZES
/// [`exact_env_strategy`] (recoverability shape), which stays as the differential oracle + the witness/
/// non-vacuity path; strategy-WITNESS extraction for arbitrary `formula` is T2. CONDITIONAL-ONLY: this
/// never changes a canonical verdict.
///
/// **Engine (§16):** `exact-symbolic` full-state ROBDD (OxiDD) — `evaluate(env_enforce(φ))` reuses the
/// existing `diamond_pre` (∃input = the env's move) + Kleene fixpoint; no `Control`-honoring needed in the
/// 1-player case (⟨ctrl⟩ ≡ ◇ when the env owns all inputs). Tool-free, host-runnable.
pub fn exact_env_strategy_exists(
    btor2_content: &str,
    formula: &Formula,
) -> Result<Option<bool>, String> {
    let enforced = crate::mu_calculus::env_enforce::env_enforce(formula);
    match exact_symbolic_verdict(btor2_content, &enforced) {
        Ok(ExactVerdict::Holds) => Ok(Some(true)),
        Ok(ExactVerdict::Violated) => Ok(Some(false)),
        // The exact engine abstained (over-cap / in-cone array / input-atom) — undecided, not a strategy.
        Err(_) => Ok(None),
    }
}

/// Shared setup for the env-strategy witness: parse + resolve `good` to a state-atom BDD (via the sound
/// Strict alias resolution) + build the cone-restricted exact bit-blaster. `None` when `good` is not a
/// resolvable `REG op VALUE` atom or the model cannot be built (over-cap / array / input-atom).
fn build_atom_model(
    btor2_content: &str,
    good: &str,
) -> Option<(BddBitBlaster, Btor2File, BDDFunction)> {
    use crate::adapter::btor2::parser;
    let file = parser::parse(btor2_content).ok()?;
    let resolve = |name: &str| -> String {
        parser::resolve_to_canonical_name(
            &file,
            name,
            parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };
    let expr = resolve_predicate_expr_registers(&parse_predicate_atom_bool(good).ok()?, &resolve);
    let mut seed: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_predicate_registers(&expr, &mut seed);
    let seed_atoms: Vec<String> = seed.into_iter().collect();
    let keep_set = (!seed_atoms.is_empty())
        .then(|| crate::adapter::btor2::dep_graph::cone_leaf_nids(&file, &seed_atoms));
    let bb = BddBitBlaster::build_with_keep(&file, keep_set.as_ref()).ok()?;
    let good_bdd = bb.predicate_bdd(&expr).ok()?;
    Some((bb, file, good_bdd))
}

/// P2.5-E T2 — extract a concrete env-strategy WITNESS for `AG EF good`: the positional input discipline
/// that drives the design to `good`, as a `(state, input)` winning play ([`BddBitBlaster::reach_good_play`]).
/// The trace exhibits a POSITIONAL strategy directly — the same input at different values in different
/// states (the shape no constant hold can express). `None` when `good` is not a resolvable atom, the
/// model can't be built, or `good` is unreachable from the initial state.
///
/// **Engine (§16):** `exact-symbolic` ROBDD — the `EF(good)` attractor + concrete minterm descent.
pub fn exact_env_strategy_witness(btor2_content: &str, good: &str) -> Option<StallLasso> {
    let (bb, file, good_bdd) = build_atom_model(btor2_content, good)?;
    bb.reach_good_play(&file, &good_bdd)
}

/// P2.5-E — a full POSITIONAL environment strategy for `AG EF good`: a memoryless strategy over the
/// design's reachable states. Where [`exact_env_strategy_witness`] returns ONE play (witnessing `EF good`
/// from the initial state), this returns the `AG` part — the driving discipline for *every* reachable
/// state, as a state-indexed map. It is the reusable object: an environment model, a directed-test seed,
/// or the environment half of an assume-guarantee contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PositionalStrategy {
    /// The state register the strategy is indexed by (the register named in the `good` atom).
    pub state_register: String,
    /// One row per reachable value of `state_register`, ordered by rank then value.
    pub entries: Vec<StrategyEntry>,
}

/// One control-state row of a [`PositionalStrategy`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StrategyEntry {
    /// The value of the strategy's state register in this row.
    pub state_value: u128,
    /// Attractor distance to `good` (`0` = the state already satisfies `good`).
    pub rank: u32,
    /// The inputs the strategy FORCES in this state (name → value). An input that some rank-decreasing
    /// move leaves free is omitted, so a nonempty map is the minimal driving discipline. The SAME input
    /// appearing with DIFFERENT forced values across rows is the positional obligation, made explicit as
    /// a map rather than read off a single trace.
    pub forced_inputs: BTreeMap<String, u128>,
}

/// P2.5-E — synthesize the full positional environment strategy for `AG EF good` (see
/// [`PositionalStrategy`]). `None` when `good` is not a resolvable `REG op VALUE` state atom over a state
/// register, the model can't be built (over-cap / array / input-atom), or `good` is unreachable from the
/// initial state (no strategy exists).
///
/// **Engine (§16):** `exact-symbolic` ROBDD — the `EF(good)` attractor supplies each state's rank;
/// concrete forward reachability from the initial state enumerates the real reachable states (so the free
/// reset does not make every encoding "winning"); each row's forced inputs are projected from the
/// rank-decreasing moves.
pub fn exact_env_positional_strategy(
    btor2_content: &str,
    good: &str,
) -> Option<PositionalStrategy> {
    use crate::adapter::btor2::parser;
    let (bb, file, good_bdd) = build_atom_model(btor2_content, good)?;
    let resolve = |name: &str| -> String {
        parser::resolve_to_canonical_name(
            &file,
            name,
            parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };
    let expr = resolve_predicate_expr_registers(&parse_predicate_atom_bool(good).ok()?, &resolve);
    let mut regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_predicate_registers(&expr, &mut regs);
    let reg = regs.into_iter().next()?;
    bb.env_positional_strategy(&file, &good_bdd, &reg)
}

/// One `(environment-input guard → forced controllable inputs)` move of a Mealy controller
/// ([`MealyStrategy`]). The game is Mealy — the environment moves and the controller RESPONDS
/// ([`ExactModel::cpre_controllable`], `∀env ∃ctrl`) — so the controller's move may depend on the
/// current environment input. `env_inputs` is empty for an env-independent (Moore) move.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MealyMove {
    /// The environment-input valuation this move responds to; empty = env-independent (holds for every
    /// environment move).
    pub env_inputs: BTreeMap<String, u128>,
    /// The controllable inputs the controller forces in response.
    pub forced_ctrl: BTreeMap<String, u128>,
}

/// One control-state row of a [`MealyStrategy`]: the controller's response(s) at that control value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MealyEntry {
    /// The value of the strategy's state register.
    pub state_value: u128,
    /// The attractor rank (distance to `good`); 0 = already `good` (no move needed).
    pub rank: u32,
    /// The controller's move(s). A single `env_inputs`-empty move = a Moore (env-independent) response;
    /// several moves = a genuinely reactive response, one per environment input valuation.
    pub moves: Vec<MealyMove>,
    /// `false` = the reactive-move enumeration hit its bound and `moves` is a partial cover (only for
    /// wide-environment reactive states; never for the Moore fast-path). No silent truncation.
    pub complete: bool,
}

/// P2.5-F — a Mealy CONTROLLER strategy for a realizable reachability game: at each control state the
/// controller forces the controllable inputs (possibly reacting to the environment input — the game is
/// `∀env ∃ctrl`) to a rank-decreasing move. Contrast [`PositionalStrategy`], which the ENVIRONMENT
/// counterstrategy uses (the environment is the first-mover, so its winning strategy is positional).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MealyStrategy {
    /// The state register the strategy is indexed by.
    pub state_register: String,
    /// One row per reachable control-state value, sorted by rank then value.
    pub entries: Vec<MealyEntry>,
}

impl MealyStrategy {
    /// The positional (Moore) projection, when every entry's response is env-independent (a single
    /// `env_inputs`-empty move) — i.e. the controller needs no reaction to the environment. `None` when
    /// any state requires a genuinely reactive (env-dependent) move, since no state-only strategy exists
    /// there. With all inputs controllable (no environment) this always succeeds and equals the 1-player
    /// [`exact_env_positional_strategy`].
    pub fn as_positional(&self) -> Option<PositionalStrategy> {
        let mut entries = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            let forced = match e.moves.as_slice() {
                [] => BTreeMap::new(),
                [m] if m.env_inputs.is_empty() => m.forced_ctrl.clone(),
                _ => return None, // reactive: no env-independent positional move
            };
            entries.push(StrategyEntry {
                state_value: e.state_value,
                rank: e.rank,
                forced_inputs: forced,
            });
        }
        Some(PositionalStrategy {
            state_register: self.state_register.clone(),
            entries,
        })
    }
}

/// P2.5-F — a synthesized TWO-PLAYER strategy for the controllable-reachability game `μX. (good ∨
/// ⟨ctrl⟩X)`: the CONTROLLER's Mealy strategy when it is realizable, or the ENVIRONMENT's positional
/// COUNTERSTRATEGY when it is not (by determinacy, exactly one holds). The asymmetry is intrinsic to the
/// Mealy game (`∀env ∃ctrl`): the environment is the first-mover, so its winning strategy is positional
/// (state-indexed); the controller responds, so its strategy may depend on the environment input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TwoPlayerStrategy {
    /// The controller wins from the initial state (spec realizable) — its Mealy strategy. Serializes as
    /// `{"kind": "controller_strategy", "state_register": …, "entries": […]}`.
    ControllerStrategy(MealyStrategy),
    /// The environment wins (unrealizable) — its positional counterstrategy, forcing environment inputs
    /// (robust to every controllable move) to keep the play out of the controller's winning region.
    /// Serializes as `{"kind": "environment_counterstrategy", "state_register": …, "entries": […]}`.
    EnvironmentCounterstrategy(PositionalStrategy),
}

impl TwoPlayerStrategy {
    /// `true` when the controller wins from the initial state (the spec is realizable) and this carries
    /// its strategy; `false` when the environment wins (unrealizable) and this carries the counterstrategy.
    pub fn realizable(&self) -> bool {
        matches!(self, TwoPlayerStrategy::ControllerStrategy(_))
    }
}

/// P2.5-F — synthesize the two-player strategy for the controllable-reachability game to `good` with
/// `controllable` naming the controller's inputs. Returns the CONTROLLER's Mealy strategy when the
/// controller wins from the initial state (== [`exact_two_player_verdict`] holds on `μX. good ∨ ⟨ctrl⟩X`),
/// or the ENVIRONMENT's positional counterstrategy when it does not (`TwoPlayerStrategy::realizable`
/// distinguishes them). The surface entry point behind `mununu btor2 game` / `POST /api/v1/btor2/game`.
///
/// `Err` when `good` is not a resolvable `REG op VALUE` state atom, the model can't be built (over the
/// exact cap / array / input-atom), the atom references no state register, or a declared `controllable`
/// name is not a real primary input — the same partition validation as [`exact_two_player_verdict`], so a
/// typo or an internally-driven signal is rejected rather than silently reinterpreted as environment.
///
/// **Engine (§16):** `exact-symbolic` ROBDD — the controllable attractor (`cpre_controllable`) supplies
/// ranks; concrete forward reachability enumerates the real states. The controller's moves come from
/// [`ExactModel::ctrl_forcing_moves`] (env-independent, Moore fast-path) or, where the controller must
/// react, the per-environment move relation ([`ExactModel::move_into`]); the environment counterstrategy
/// from [`ExactModel::env_forcing_moves`].
pub fn exact_two_player_strategy(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Result<TwoPlayerStrategy, String> {
    use crate::adapter::btor2::parser;
    let (bb, file, good_bdd) = build_atom_model(btor2_content, good).ok_or_else(|| {
        format!(
            "exact two-player strategy: `{good}` is not a resolvable `REG op VALUE` state atom, or the \
             model can't be built (over the exact cap / array / input-atom)"
        )
    })?;
    // Validate the partition: every declared controllable input must be a real primary input, else it
    // would fall back SILENTLY to the environment (the same rule + rationale as `exact_two_player_verdict`).
    if !controllable.is_empty() {
        let inputs: std::collections::HashSet<String> = crate::adapter::sts_ir::BtorSts::new(&file)
            .leaf_cells()
            .map_err(|e| format!("exact two-player strategy: {e}"))?
            .into_iter()
            .filter(|c| !c.is_state)
            .map(|c| c.name)
            .collect();
        let mut unknown: Vec<&str> = controllable
            .iter()
            .copied()
            .filter(|c| !inputs.contains(*c))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            let mut names: Vec<&str> = inputs.iter().map(String::as_str).collect();
            names.sort_unstable();
            return Err(format!(
                "exact two-player strategy: declared controllable input(s) {unknown:?} are not primary \
                 inputs of the design (primary inputs: {names:?})"
            ));
        }
    }
    let resolve = |name: &str| -> String {
        parser::resolve_to_canonical_name(
            &file,
            name,
            parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };
    let expr = resolve_predicate_expr_registers(
        &parse_predicate_atom_bool(good).map_err(|e| format!("exact two-player strategy: {e}"))?,
        &resolve,
    );
    let mut regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_predicate_registers(&expr, &mut regs);
    let reg = regs.into_iter().next().ok_or_else(|| {
        format!("exact two-player strategy: `{good}` references no state register")
    })?;
    let ctrl: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    bb.two_player_strategy(&file, &good_bdd, &reg, &ctrl)
        .ok_or_else(|| {
            format!("exact two-player strategy: `{reg}` is not a state register in the design")
        })
}

/// P2.5-F (b) — the CONTROLLER's Mealy strategy for a REALIZABLE RECURRENCE (Büchi) game `GF good` — how
/// the controller forces `good` infinitely often. Returns `Ok(Some(ControllerStrategy))` when the game is
/// realizable from the initial state, `Ok(None)` when it is unrealizable (the actionable witness is then
/// the environment starvation lasso, [`exact_two_player_recurrence_stall_lasso`]) or the target register
/// is absent, and `Err` when `good` is not a resolvable `REG op VALUE` STATE atom (a combinational-output
/// / relational recurrence target has no state-indexed strategy — the verdict + fairness discovery still
/// apply), the model can't be built, or a declared `controllable` name is not a primary input.
///
/// **Engine (§16):** `exact-symbolic` ROBDD — the Büchi winning region `W = νZ. Attr_ctrl(good ∧ CPre_ctrl
/// Z)` and a memoryless controller strategy that attracts to `R = good ∧ CPre_ctrl W` then re-enters `W`
/// (positional determinacy of Büchi games). The reach analog is [`exact_two_player_strategy`].
pub fn exact_two_player_buchi_strategy(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Result<Option<TwoPlayerStrategy>, String> {
    use crate::adapter::btor2::parser;
    let (bb, file, good_bdd) = build_atom_model(btor2_content, good).ok_or_else(|| {
        format!(
            "exact two-player recurrence strategy: `{good}` is not a resolvable `REG op VALUE` state \
             atom, or the model can't be built (over the exact cap / array / input-atom)"
        )
    })?;
    // Validate the partition: every declared controllable input must be a real primary input (same rule +
    // rationale as `exact_two_player_strategy` — else it would fall back SILENTLY to the environment).
    if !controllable.is_empty() {
        let inputs: std::collections::HashSet<String> = crate::adapter::sts_ir::BtorSts::new(&file)
            .leaf_cells()
            .map_err(|e| format!("exact two-player recurrence strategy: {e}"))?
            .into_iter()
            .filter(|c| !c.is_state)
            .map(|c| c.name)
            .collect();
        let mut unknown: Vec<&str> = controllable
            .iter()
            .copied()
            .filter(|c| !inputs.contains(*c))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            let mut names: Vec<&str> = inputs.iter().map(String::as_str).collect();
            names.sort_unstable();
            return Err(format!(
                "exact two-player recurrence strategy: declared controllable input(s) {unknown:?} are \
                 not primary inputs of the design (primary inputs: {names:?})"
            ));
        }
    }
    let resolve = |name: &str| -> String {
        parser::resolve_to_canonical_name(
            &file,
            name,
            parser::ResolveStrictness::Strict {
                allow_reset_mux: true,
            },
        )
        .unwrap_or_else(|| name.to_string())
    };
    let expr = resolve_predicate_expr_registers(
        &parse_predicate_atom_bool(good)
            .map_err(|e| format!("exact two-player recurrence strategy: {e}"))?,
        &resolve,
    );
    let mut regs: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_predicate_registers(&expr, &mut regs);
    let reg = regs.into_iter().next().ok_or_else(|| {
        format!("exact two-player recurrence strategy: `{good}` references no state register")
    })?;
    let ctrl: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    Ok(bb.two_player_buchi_strategy(&file, &good_bdd, &reg, &ctrl))
}

/// P2.5-F (b) — the ENVIRONMENT STARVATION LASSO for an **unrealizable** RECURRENCE game `GF good`: a
/// concrete play — `prefix` from reset into a repeating `¬good` `cycle`, with the per-step `inputs` the
/// environment plays — witnessing that the environment can force `good` to be visited only FINITELY
/// often, so `GF good` fails and no controller realizes the Büchi objective. This is the actionable
/// counterpart of the `unrealizable` recurrence verdict ("here is the trace where withholding the ack
/// starves `good` forever"): it is Verilator-replayable (drive `inputs`, observe `good` false on the
/// cycle) and is a **sound proof of unrealizability** on its own — a returned lasso means the game is
/// unrealizable.
///
/// **Witness region:** `stall_env = νZ. (¬good ∧ CPre_env Z)` — the states from which the environment
/// can keep `¬good` FOREVER against every controller move — reached from the initial state under
/// env-forcing (`μY. (stall_env ∨ CPre_env Y)`). Returns `None` when the game is realizable, when the
/// env cannot force a reachable `¬good`-forever region (`stall_env` unreachable), or when
/// unrealizability is a subtler co-Büchi with no such region (the full env co-Büchi strategy is a
/// follow-up). So a `Some` lasso is the common "the FSM is parked in a `¬good` wait state" case; a
/// `None` does not by itself certify realizability — read `exact_two_player_buchi_realizable` for that.
///
/// **Engine (§16):** `exact-symbolic` ROBDD over the input-partitioned model
/// ([`BddBitBlaster::exact_model_partitioned`]): `stall_env` and the reach layers are `cpre_environment`
/// greatest/least fixpoints; the concrete lasso is walked from the initial state, each step picking a
/// `(ctrl, env)` pair whose successor stays in the target layer (the env's forcing response to the
/// walked controller move), reusing [`BddBitBlaster::walk_stall_cycle`]. Same partition validation as
/// [`exact_two_player_strategy`] — a `controllable` name that is not a primary input is rejected.
pub fn exact_two_player_recurrence_stall_lasso(
    btor2_content: &str,
    good: &str,
    controllable: &[&str],
) -> Result<Option<StallLasso>, String> {
    let (bb, file, good_bdd) = build_atom_model(btor2_content, good).ok_or_else(|| {
        format!(
            "exact recurrence stall lasso: `{good}` is not a resolvable `REG op VALUE` state atom, or \
             the model can't be built (over the exact cap / array / input-atom)"
        )
    })?;
    // Validate the partition: every declared controllable input must be a real primary input, else it
    // would fall back SILENTLY to the environment (the same rule + rationale as `exact_two_player_verdict`).
    if !controllable.is_empty() {
        let inputs: std::collections::HashSet<String> = crate::adapter::sts_ir::BtorSts::new(&file)
            .leaf_cells()
            .map_err(|e| format!("exact recurrence stall lasso: {e}"))?
            .into_iter()
            .filter(|c| !c.is_state)
            .map(|c| c.name)
            .collect();
        let mut unknown: Vec<&str> = controllable
            .iter()
            .copied()
            .filter(|c| !inputs.contains(*c))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            let mut names: Vec<&str> = inputs.iter().map(String::as_str).collect();
            names.sort_unstable();
            return Err(format!(
                "exact recurrence stall lasso: declared controllable input(s) {unknown:?} are not \
                 primary inputs of the design (primary inputs: {names:?})"
            ));
        }
    }
    let ctrl: std::collections::HashSet<String> =
        controllable.iter().map(|s| s.to_string()).collect();
    Ok(bb.two_player_recurrence_stall_lasso(&file, &good_bdd, &ctrl))
}

/// D1.8b/b-2 — detect a liveness shape and return the node id of its target `p`.
/// Recognised (both disjunct/conjunct orders; modalities must be bare `[]`):
/// - bare `AF p` = `μX. (p ∨ [] X)`,
/// - `AG AF p` = `νY. ((μX. (p ∨ [] X)) ∧ [] Y)`.
///
/// `None` for any other shape.
fn detect_af_target(formula: &Formula) -> Option<crate::mu_calculus::NodeId> {
    let root = formula.root();
    // Bare `AF p`.
    if let Some(p) = af_target_at(formula, root) {
        return Some(p);
    }
    // `AG AF p` = `νY. (AF_p ∧ [] Y)`: strip the `AG` wrapper, then match `AF p`.
    let MuNode::Nu { var, body } = formula.node(root) else {
        return None;
    };
    let MuNode::And(a, b) = formula.node(*body) else {
        return None;
    };
    let af = if is_box_over_var(formula, *a, var) {
        *b
    } else if is_box_over_var(formula, *b, var) {
        *a
    } else {
        return None;
    };
    af_target_at(formula, af)
}

/// Match `μX. (p ∨ [] X)` at `node` → the node id of `p` (the non-recursive disjunct).
fn af_target_at(
    formula: &Formula,
    node: crate::mu_calculus::NodeId,
) -> Option<crate::mu_calculus::NodeId> {
    let MuNode::Mu { var, body } = formula.node(node) else {
        return None;
    };
    let MuNode::Or(a, b) = formula.node(*body) else {
        return None;
    };
    if is_box_over_var(formula, *a, var) {
        Some(*b)
    } else if is_box_over_var(formula, *b, var) {
        Some(*a)
    } else {
        None
    }
}

/// Is `node` a bare `[] v` (unguarded box over the fixpoint variable `v`)?
fn is_box_over_var(
    formula: &Formula,
    node: crate::mu_calculus::NodeId,
    v: &crate::mu_calculus::FormulaVarId,
) -> bool {
    matches!(
        formula.node(node),
        MuNode::Modal { kind: ModalKind::Box, guard, target }
            if *guard == Guard::default()
                && matches!(formula.node(*target), MuNode::Variable(w) if w == v)
    )
}

/// Is `node` a bare `<> v` (unguarded diamond over the fixpoint variable `v`)?
fn is_diamond_over_var(
    formula: &Formula,
    node: crate::mu_calculus::NodeId,
    v: &crate::mu_calculus::FormulaVarId,
) -> bool {
    matches!(
        formula.node(node),
        MuNode::Modal { kind: ModalKind::Diamond, guard, target }
            if *guard == Guard::default()
                && matches!(formula.node(*target), MuNode::Variable(w) if w == v)
    )
}

/// P3 — detect `AG EF p = νY. ((μX. (p ∨ <> X)) ∧ [] Y)` (the recoverability shape) and return
/// the node id of its target `p`. Mirrors [`detect_af_target`] but the inner μ uses a bare
/// DIAMOND (`<> X` = `∃input`), not a box. `None` for any other shape.
fn detect_ag_ef_target(formula: &Formula) -> Option<crate::mu_calculus::NodeId> {
    let root = formula.root();
    let MuNode::Nu { var, body } = formula.node(root) else {
        return None;
    };
    let MuNode::And(a, b) = formula.node(*body) else {
        return None;
    };
    let ef = if is_box_over_var(formula, *a, var) {
        *b
    } else if is_box_over_var(formula, *b, var) {
        *a
    } else {
        return None;
    };
    ef_target_at(formula, ef)
}

/// Match `μX. (p ∨ <> X)` (= `EF p`) at `node` → the node id of `p` (the non-recursive
/// disjunct). The diamond twin of [`af_target_at`].
fn ef_target_at(
    formula: &Formula,
    node: crate::mu_calculus::NodeId,
) -> Option<crate::mu_calculus::NodeId> {
    let MuNode::Mu { var, body } = formula.node(node) else {
        return None;
    };
    let MuNode::Or(a, b) = formula.node(*body) else {
        return None;
    };
    if is_diamond_over_var(formula, *a, var) {
        Some(*b)
    } else if is_diamond_over_var(formula, *b, var) {
        Some(*a)
    } else {
        None
    }
}

/// A `Violated` bare `EF p = μX. (p ∨ ◇X)` (reachability) means exactly that the
/// target `p` is **UNREACHABLE** from the reset state — there is no counterexample
/// *trace* (no path reaches `p`), so the actionable witness is the *target itself*:
/// "the design never reaches `p`". Under cut points the model is an over-approximation
/// (freeing a net only ADDS transitions ⇒ MORE reachability), so a `p` that is
/// unreachable even in the over-approximation is unreachable in the concrete RTL too —
/// the unreachability transfers **soundly**.
///
/// If `formula` is that bare-`EF` shape, return the predicate-atom strings that make up
/// its target `p` (naming the unreachable target for a repair witness); `None` for any
/// other shape. Complements the `AF`/`AG AF` stall lasso and the `AG EF` trap path,
/// which witness *liveness/recoverability* failures with a concrete path.
pub(crate) fn ef_target_atoms(formula: &Formula) -> Option<Vec<String>> {
    let p = ef_target_at(formula, formula.root())?;
    let mut atoms = Vec::new();
    collect_target_atoms(formula, p, &mut atoms);
    (!atoms.is_empty()).then_some(atoms)
}

/// Collect the distinct predicate-atom strings under a formula sub-node (the `EF`
/// target `p`), descending through boolean structure only. Modal/fixpoint nodes are
/// not expected inside a reachability target and are ignored.
fn collect_target_atoms(
    formula: &Formula,
    node: crate::mu_calculus::NodeId,
    out: &mut Vec<String>,
) {
    match formula.node(node) {
        MuNode::Predicate(name) if !out.contains(name) => out.push(name.clone()),
        MuNode::And(a, b) | MuNode::Or(a, b) => {
            collect_target_atoms(formula, *a, out);
            collect_target_atoms(formula, *b, out);
        }
        MuNode::Not(a) => collect_target_atoms(formula, *a, out),
        _ => {}
    }
}

/// If `formula` is a recoverability `AG EF good` (`νY. ((μX. (good ∨ ◇X)) ∧ □Y)`),
/// return the predicate atoms of its `good` target — so a caller can route the
/// property to the scalable ranking/recoverability engine (which decides a
/// well-founded datapath descent, e.g. a down-counter to a value, that the cube
/// leaves ⊥). `None` for any other shape. Complements [`ef_target_atoms`] (bare `EF`).
pub(crate) fn ag_ef_good_atoms(formula: &Formula) -> Option<Vec<String>> {
    let p = detect_ag_ef_target(formula)?;
    let mut atoms = Vec::new();
    collect_target_atoms(formula, p, &mut atoms);
    (!atoms.is_empty()).then_some(atoms)
}

/// D1.8c — build the sound μ-calculus formula that verifies the GR(1) response
/// property `GF assume → GF guarantee` over the **exact** engine. `assume` and
/// `guarantee` are predicate expressions (e.g. `"req == 1"`).
///
/// The property FAILS iff the system has a reachable **bad cycle** — a cycle that
/// hits `assume` infinitely often (`GF assume`) yet avoids `guarantee` forever
/// (`FG ¬guarantee`). The generated formula is `¬EF badcycle`, where
///
/// ```text
/// badcycle = νZ. μY. ((assume ∧ ¬guarantee ∧ ◇Z) ∨ (¬guarantee ∧ ◇Y))
/// EF X     = μW. (X ∨ ◇W)
/// ```
///
/// `badcycle` is the Emerson–Lei fair-cycle set — an infinite `¬guarantee` path
/// recurring through `assume` — and `◇` is `∃input` in the exact model, so this reads
/// "∃ a bad input sequence". Fed to [`exact_symbolic_verdict`] (or a
/// `@mununu_guarantee` annotation on the exact-symbolic verify-auto path), the exact
/// engine reports **Holds** iff the reset state cannot reach any bad cycle — i.e. the
/// GR(1) property genuinely holds on all paths (a *definite*, sound liveness verdict
/// where the existential LTL `GF` translation `AG EF` and predicate abstraction do
/// not).
///
/// Scope: a single `assume`/`guarantee` pair (the GR(1) response core). Generalised
/// (Streett) multi-fairness `⋀ GF aᵢ → ⋀ GF bⱼ` is a future extension.
pub fn gr1_response_formula(assume: &str, guarantee: &str) -> String {
    // badcycle = νZ. μY. ((A ∧ ¬B ∧ ◇Z) ∨ (¬B ∧ ◇Y))
    let bad = format!(
        "nu Z. (mu Y. (((({assume}) and (not ({guarantee}))) and <> Z) \
         or ((not ({guarantee})) and <> Y)))"
    );
    // ¬EF badcycle = ¬ μW. (badcycle ∨ ◇W)
    format!("not (mu W. (({bad}) or <> W))")
}

/// D1.8 — a **stall lasso**: the concrete counterexample witness for a `Violated`
/// `AF p` (all-paths-eventually-`p`) property from the exact engine. `AF p` fails
/// exactly when some initial state can avoid `p` forever — reach a `¬p` cycle
/// staying in `¬p` (the "stall"). The lasso is that path: a `prefix` from the
/// initial state to the cycle entry, then the repeating `cycle`. Each state is a
/// concrete valuation of the state registers.
/// The ranked reachable states of a strategy extraction: the `(state, rank)` pairs reachable from the
/// initial state within the winner's region, plus the per-state-register-value minimum rank. Shared
/// between [`BddBitBlaster::reachable_ranked_states`] (which produces it) and
/// [`BddBitBlaster::controller_mealy_from_layers`] (which consumes it).
type RankedStates = (Vec<(BTreeMap<String, u128>, u32)>, BTreeMap<u128, u32>);

#[derive(Debug, Clone)]
pub struct StallLasso {
    /// States from the initial state up to (excluding) the cycle entry. Empty when
    /// the initial state is itself on the cycle.
    pub prefix: Vec<BTreeMap<String, u128>>,
    /// The repeating `¬p` cycle; `cycle[0]` is the state the last one steps back to.
    /// Empty only in the (guarded) non-total-model deadlock case.
    pub cycle: Vec<BTreeMap<String, u128>>,
    /// P3 — the INPUT assignment that drives each transition of the concatenated
    /// `prefix ++ cycle` path (`inputs[i]` is the input at path-state `i`), for RTL replay.
    /// Empty when a builder does not record inputs (e.g. hand-authored test lassos); the
    /// [`StallLasso`] identity ignores it (see the manual `PartialEq`), so equality is over
    /// the state path only.
    pub inputs: Vec<BTreeMap<String, u128>>,
}

// StallLasso identity is the STATE path (prefix ++ cycle); `inputs` is replay metadata that
// does not affect equality (a lasso witnessing the same path is the same witness).
impl PartialEq for StallLasso {
    fn eq(&self, other: &Self) -> bool {
        self.prefix == other.prefix && self.cycle == other.cycle
    }
}
impl Eq for StallLasso {}

/// One register's concrete value in a [`StallLassoView`] state (serialized `{register, value}`,
/// matching the API `CexCellView`). Values fit in `u64` under the exact bit-blast cap.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LassoCell {
    pub register: String,
    pub value: u64,
}

/// The serde-able surface form of a [`StallLasso`] — reset → `prefix` → repeating `cycle`, plus the
/// per-transition `inputs` (the env-forcing move at each step), each state/input an ordered
/// `{register, value}` list. Shared by the CLI `btor2 game` JSON and the API `Btor2GameResponse`
/// so the two surfaces cannot drift. `inputs[i]` drives the transition out of concatenated-path
/// state `i`; it is the ACTIONABLE signal for an environment-starvation witness ("the env holds
/// `ack = 0` forever").
#[derive(Debug, Clone, serde::Serialize)]
pub struct StallLassoView {
    pub prefix: Vec<Vec<LassoCell>>,
    pub cycle: Vec<Vec<LassoCell>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Vec<LassoCell>>,
}

impl StallLasso {
    /// Convert to the serde surface form ([`StallLassoView`]); `u128` register/input values are
    /// narrowed to `u64` (they fit under the exact engine's bit-blast cap), as the recoverability
    /// counterexample surface already does.
    pub fn to_view(&self) -> StallLassoView {
        let states = |rows: &[BTreeMap<String, u128>]| -> Vec<Vec<LassoCell>> {
            rows.iter()
                .map(|st| {
                    st.iter()
                        .map(|(register, value)| LassoCell {
                            register: register.clone(),
                            value: *value as u64,
                        })
                        .collect()
                })
                .collect()
        };
        StallLassoView {
            prefix: states(&self.prefix),
            cycle: states(&self.cycle),
            inputs: states(&self.inputs),
        }
    }
}

impl BddBitBlaster {
    /// D1.8 — extract a [`StallLasso`] witnessing that `AF p` is **Violated** from the
    /// design's initial state, or `None` when `AF p` holds (no reachable stall).
    ///
    /// `p` is the target-predicate BDD (build it with [`predicate_bdd`], over state
    /// registers). The method computes `stall = EG ¬p = νZ. (¬p ∧ ◇Z)` — the states
    /// with an infinite `p`-avoiding path, exactly `¬⟦AF p⟧` — over the exact model.
    /// If the initial state ([`initial_state_bdd`]) lies in `stall`, it walks a
    /// concrete path inside `stall`: at each step it greedily picks (from the BDD) an
    /// input whose successor stays in `stall`, advances with the concrete
    /// [`eval_step`] simulator, and stops when a state repeats — the cycle. Bounded by
    /// the stall's size, so it terminates.
    ///
    /// The witness is **self-validating**: replay it through `eval_step` and confirm
    /// every state is `¬p` and the cycle closes.
    ///
    /// [`predicate_bdd`]: BddBitBlaster::predicate_bdd
    /// [`initial_state_bdd`]: BddBitBlaster::initial_state_bdd
    /// [`eval_step`]: BddBitBlaster::eval_step
    pub fn exact_stall_lasso(&self, file: &Btor2File, p: &BDDFunction) -> Option<StallLasso> {
        let exact = self.exact_model();
        let stall = self.eg_not_p(&exact, p);
        // A stall state that is also initial ⇒ AF p is violated from reset.
        let init = self.initial_state_bdd(file).ok()?;
        let bad = init.and(&stall).unwrap();
        if bad == self.ff {
            return None;
        }
        let s = self.pick_state_assignment(&bad);
        Some(self.walk_stall_cycle(&exact, &stall, s, Vec::new(), Vec::new()))
    }

    /// D1.8b-2 — extract a [`StallLasso`] witnessing that `AG AF p` is **Violated**:
    /// a `¬p` stall is *reachable* from the reset state (not necessarily at reset).
    /// Generalises [`exact_stall_lasso`] (which requires the stall *at* reset): the
    /// lasso's `prefix` is now the reset→stall reach path followed by the `¬p` cycle.
    /// `None` when no stall is reachable (`AG AF p` holds).
    ///
    /// Layered reachability: `L_0 = EG¬p`, `L_k = L_{k-1} ∨ ◇L_{k-1}` (states that can
    /// reach a stall in ≤ k steps). `init ∩ L_∞ ≠ ∅` iff a stall is reachable. The
    /// reach phase descends the layers — each step lands one layer closer via a
    /// concrete [`eval_step`] — then the shared cycle walk closes the `¬p` cycle.
    ///
    /// [`exact_stall_lasso`]: BddBitBlaster::exact_stall_lasso
    /// [`eval_step`]: BddBitBlaster::eval_step
    pub fn exact_reachable_stall_lasso(
        &self,
        file: &Btor2File,
        p: &BDDFunction,
    ) -> Option<StallLasso> {
        let exact = self.exact_model();
        let stall = self.eg_not_p(&exact, p);
        if stall == self.ff {
            return None;
        }
        // Reach layers: L_0 = stall, L_k = L_{k-1} ∨ ◇L_{k-1}.
        let mut layers = vec![stall.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.diamond_pre(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let can_reach = layers.last().unwrap().clone();
        let init = self.initial_state_bdd(file).ok()?;
        let bad = init.and(&can_reach).unwrap();
        if bad == self.ff {
            return None;
        }
        // Reach phase: from an initial state that can reach a stall, descend one layer
        // per step until inside the stall, recording the reset→stall prefix.
        let mut s = self.pick_state_assignment(&bad);
        let mut prefix: Vec<BTreeMap<String, u128>> = Vec::new();
        let mut inputs: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if self.state_minterm(&s).and(&stall).unwrap() != self.ff {
                break; // reached the stall
            }
            prefix.push(s.clone());
            // s ∈ L_k \ L_{k-1} (k minimal) ⇒ s ∈ ◇L_{k-1}: a successor lands in L_{k-1}.
            let k = (1..layers.len())
                .find(|&k| self.state_minterm(&s).and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(1);
            let good = self
                .state_minterm(&s)
                .and(&exact.to_next(&layers[k - 1]))
                .unwrap();
            if good == self.ff {
                return Some(StallLasso {
                    prefix,
                    cycle: Vec::new(),
                    inputs,
                });
            }
            let full = self.pick_full_assignment(&good);
            inputs.push(self.input_assignment(&full));
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            s = ns;
        }
        Some(self.walk_stall_cycle(&exact, &stall, s, prefix, inputs))
    }

    /// P3 — extract a concrete path witnessing that `AG EF p` is **Violated**: a state
    /// reachable from reset from which `p` is UNREACHABLE (the "trap" `¬EF p`), plus the
    /// reset→trap prefix. `¬EF p` is absorbing (if you cannot reach `p`, no successor can
    /// either), so the witness needs no cycle walk — the trap state is a single-state
    /// absorbing `cycle`. `None` when the trap is unreachable (`AG EF p` holds).
    ///
    /// Reuses the layered reachability of [`exact_reachable_stall_lasso`] with `trap = ¬EF p`
    /// in place of `stall = EG¬p`; the reach phase descends one layer per concrete
    /// [`eval_step`] until inside the trap.
    ///
    /// [`exact_reachable_stall_lasso`]: BddBitBlaster::exact_reachable_stall_lasso
    /// [`eval_step`]: BddBitBlaster::eval_step
    pub fn exact_reachable_trap_path(
        &self,
        file: &Btor2File,
        p: &BDDFunction,
    ) -> Option<StallLasso> {
        let exact = self.exact_model();
        let trap = self.not_ef_p(&exact, p);
        if trap == self.ff {
            return None; // EF p holds everywhere ⇒ AG EF p holds
        }
        // Reach layers: L_0 = trap, L_k = L_{k-1} ∨ ◇L_{k-1}.
        let mut layers = vec![trap.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.diamond_pre(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let init = self.initial_state_bdd(file).ok()?;
        let bad = init.and(layers.last().unwrap()).unwrap();
        if bad == self.ff {
            return None; // the trap is unreachable ⇒ AG EF p holds
        }
        // Reach phase: descend one layer per step until inside the trap, recording the prefix.
        let mut s = self.pick_state_assignment(&bad);
        let mut prefix: Vec<BTreeMap<String, u128>> = Vec::new();
        let mut inputs: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if self.state_minterm(&s).and(&trap).unwrap() != self.ff {
                // Reached the trap: `s` witnesses `¬EF p`; it is absorbing (a 1-state cycle).
                return Some(StallLasso {
                    prefix,
                    cycle: vec![s],
                    inputs,
                });
            }
            prefix.push(s.clone());
            let k = (1..layers.len())
                .find(|&k| self.state_minterm(&s).and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(1);
            let good = self
                .state_minterm(&s)
                .and(&exact.to_next(&layers[k - 1]))
                .unwrap();
            if good == self.ff {
                return Some(StallLasso {
                    prefix,
                    cycle: Vec::new(),
                    inputs,
                });
            }
            let full = self.pick_full_assignment(&good);
            inputs.push(self.input_assignment(&full));
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            s = ns;
        }
        None
    }

    /// P2.5-E T2 — a concrete WINNING PLAY for `AG EF good`: the environment's positional strategy
    /// exhibited as a `(state, input)` trajectory from the initial state that REACHES `p` (`good`). It is
    /// the DUAL of [`Self::exact_reachable_trap_path`] (which descends toward the trap): here we descend
    /// the `EF(good)` attractor toward `good`, and at each state pick an input that strictly decreases the
    /// reachability rank. A POSITIONAL discipline surfaces directly in the trace — the SAME input takes
    /// different values in different states (e.g. `boot_req=1` at Idle, `boot_req=0` at BootDone) — which
    /// no constant hold can express. `prefix`/`inputs` are the play; `cycle` holds the reached `good`
    /// state. `None` if `good` is unreachable from the initial state.
    ///
    /// **Engine (§16):** `exact-symbolic` ROBDD — the `EF` attractor layers (`diamond_pre` = ∃input) +
    /// the concrete minterm descent (`pick_full_assignment`/`eval_step`), reusing the trap-path machinery.
    pub fn reach_good_play(&self, file: &Btor2File, p: &BDDFunction) -> Option<StallLasso> {
        let exact = self.exact_model();
        // EF(good) attractor layers toward `good`: L_0 = good, L_k = L_{k-1} ∨ ◇L_{k-1}.
        let mut layers = vec![p.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.diamond_pre(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let init = self.initial_state_bdd(file).ok()?;
        let reachable = init.and(layers.last().unwrap()).unwrap();
        if reachable == self.ff {
            return None; // good unreachable from init
        }
        let mut s = self.pick_state_assignment(&reachable);
        let mut prefix: Vec<BTreeMap<String, u128>> = Vec::new();
        let mut inputs: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if self.state_minterm(&s).and(p).unwrap() != self.ff {
                return Some(StallLasso {
                    prefix,
                    cycle: vec![s],
                    inputs,
                });
            }
            prefix.push(s.clone());
            // Current attractor rank `k`; pick an input stepping into layer `k-1` (rank strictly down).
            let k = (1..layers.len())
                .find(|&k| self.state_minterm(&s).and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(1);
            let step = self
                .state_minterm(&s)
                .and(&exact.to_next(&layers[k - 1]))
                .unwrap();
            if step == self.ff {
                return None; // no rank-decreasing move (should not happen inside the attractor)
            }
            let full = self.pick_full_assignment(&step);
            inputs.push(self.input_assignment(&full));
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            s = ns;
        }
        None
    }

    /// P2.5-E — the full positional strategy (see [`PositionalStrategy`]). The `EF(good)` attractor gives each
    /// state its rank; concrete forward reachability from the initial state enumerates the real reachable
    /// states; the forced inputs per control state are projected from the rank-decreasing moves.
    fn env_positional_strategy(
        &self,
        file: &Btor2File,
        good: &BDDFunction,
        reg_name: &str,
    ) -> Option<PositionalStrategy> {
        let exact = self.exact_model();
        // EF(good) attractor layers: L_0 = good, L_k = L_{k-1} ∨ ◇L_{k-1}. rank(s) = min k: s ∈ L_k.
        let mut layers = vec![good.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.diamond_pre(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let winning = layers.last().unwrap().clone();
        let init = self.initial_state_bdd(file).ok()?;
        if init.and(&winning).unwrap() == self.ff {
            return None; // good unreachable from init — no strategy
        }
        // The strategy is indexed by this state register.
        if !self
            .cells
            .iter()
            .any(|c| c.is_state && c.symbol == reg_name)
        {
            return None;
        }
        // rank(s): the least attractor layer containing the concrete state s.
        let rank_of = |s: &BTreeMap<String, u128>| -> u32 {
            let mt = self.state_minterm(s);
            (0..layers.len())
                .find(|&k| mt.and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(0) as u32
        };
        // Concrete forward reachability from init (under all inputs) — stays on the design's real states,
        // so the free reset does not make every encoding "winning" (a symbolic winning-region walk would).
        let mut frontier: Vec<BTreeMap<String, u128>> = Vec::new();
        {
            let mut r = init.clone();
            for _ in 0..1024 {
                if r == self.ff {
                    break;
                }
                let s = self.pick_state_assignment(&r);
                r = r.and(&self.state_minterm(&s).not().unwrap()).unwrap();
                frontier.push(s);
            }
        }
        let mut visited: std::collections::HashSet<BTreeMap<String, u128>> =
            std::collections::HashSet::new();
        let mut reached: Vec<(BTreeMap<String, u128>, u32)> = Vec::new();
        let mut guard = 0usize;
        while let Some(s) = frontier.pop() {
            guard += 1;
            if guard > 200_000 {
                break; // reachable-set guard (should never fire for a control FSM)
            }
            if !visited.insert(s.clone()) {
                continue;
            }
            let k = rank_of(&s);
            for ns in self.concrete_successors(&exact, &s) {
                if !visited.contains(&ns) {
                    frontier.push(ns);
                }
            }
            reached.push((s, k));
        }
        // The min rank per state-register value (its shortest distance to `good`).
        let mut min_rank: BTreeMap<u128, u32> = BTreeMap::new();
        for (s, k) in &reached {
            let v = *s.get(reg_name).unwrap_or(&0);
            min_rank
                .entry(v)
                .and_modify(|r| *r = (*r).min(*k))
                .or_insert(*k);
        }
        // The forced-input discipline is projected from the rank-decreasing moves of the MIN-RANK
        // states of each control value (its fastest-progress behaviour). Aggregating over higher-rank
        // states of the same control value — which may branch differently on OTHER registers (e.g. a
        // wipe-round flag) — would cancel the forcing; the min-rank moves are the clean discipline.
        let mut moves_by_val: BTreeMap<u128, BDDFunction> = BTreeMap::new();
        for (s, k) in &reached {
            let v = *s.get(reg_name).unwrap_or(&0);
            if *k == 0 || *k != min_rank[&v] {
                continue;
            }
            let m = self
                .state_minterm(s)
                .and(&exact.move_into(&layers[(*k - 1) as usize]))
                .unwrap();
            moves_by_val
                .entry(v)
                .and_modify(|acc| *acc = acc.or(&m).unwrap())
                .or_insert(m);
        }
        let mut entries: Vec<StrategyEntry> = min_rank
            .iter()
            .map(|(&v, &rank)| {
                let mut forced = BTreeMap::new();
                if let Some(mv) = moves_by_val.get(&v)
                    && *mv != self.ff
                {
                    for cell in self.cells.iter().filter(|c| !c.is_state) {
                        if let Some(val) = self.forced_cell_value(mv, cell) {
                            forced.insert(cell.symbol.clone(), val);
                        }
                    }
                }
                StrategyEntry {
                    state_value: v,
                    rank,
                    forced_inputs: forced,
                }
            })
            .collect();
        entries.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.state_value.cmp(&b.state_value)));
        Some(PositionalStrategy {
            state_register: reg_name.to_string(),
            entries,
        })
    }

    /// P2.5-F (b) — the ENVIRONMENT STARVATION LASSO for an unrealizable RECURRENCE game `GF good` (see
    /// [`exact_two_player_recurrence_stall_lasso`] for the contract). Mirrors [`Self::exact_reachable_stall_lasso`]
    /// but on the input-partitioned model with `cpre_environment` in place of `diamond_pre`: the stall is
    /// `stall_env = νZ. (¬good ∧ CPre_env Z)` (env forces `¬good` forever), reached under env-forcing.
    /// Returns `None` when the env cannot force a reachable `¬good`-forever region (realizable, or a
    /// subtler co-Büchi). Each concrete step (descent and cycle) plays the environment's `∀ctrl`-robust
    /// move ([`ExactModel::env_forcing_moves`]) with an arbitrary controller response, so the recorded
    /// `inputs` are the FAITHFUL "env forces `¬good` no matter what the controller does" trace — the ack
    /// the environment withholds — not a lazy coincidence, and `¬good` holds on the whole cycle.
    fn two_player_recurrence_stall_lasso(
        &self,
        file: &Btor2File,
        good: &BDDFunction,
        controllable: &std::collections::HashSet<String>,
    ) -> Option<StallLasso> {
        let exact = self.exact_model_partitioned(controllable);
        // stall_env = νZ. (¬good ∧ CPre_env Z): the env forces ¬good forever (∀ctrl ∃env), so `good` is
        // never visited again once here — the co-Büchi core.
        let not_good = good.not().unwrap();
        let mut stall = self.tt.clone();
        loop {
            let next = not_good.and(&exact.cpre_environment(&stall)).unwrap();
            if next == stall {
                break;
            }
            stall = next;
        }
        if stall == self.ff {
            return None; // the env can force ¬good-forever nowhere → no simple starvation witness.
        }
        // Reach layers under env-forcing: L_0 = stall_env, L_k = L_{k-1} ∨ CPre_env(L_{k-1}).
        let mut layers = vec![stall.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.cpre_environment(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let init = self.initial_state_bdd(file).ok()?;
        let bad = init.and(layers.last().unwrap()).unwrap();
        if bad == self.ff {
            return None; // the env cannot force the play into the stall from reset.
        }
        // One env-forcing concrete step from `s`: pick the env's `∀ctrl`-robust move into `target`
        // (`env_forcing_moves`), intersected with `to_next(target)` so the chosen `(ctrl, env)` actually
        // lands in `target` (constraint-respecting), then step via `eval_step`. Non-empty whenever
        // `s ∈ CPre_env(target)`. Returns `(input, next_state)`.
        let env_step = |s: &BTreeMap<String, u128>,
                        target: &BDDFunction|
         -> Option<(BTreeMap<String, u128>, BTreeMap<String, u128>)> {
            let moves = self
                .state_minterm(s)
                .and(&exact.env_forcing_moves(target))
                .unwrap()
                .and(&exact.to_next(target))
                .unwrap();
            if moves == self.ff {
                return None;
            }
            let full = self.pick_full_assignment(&moves);
            let input = self.input_assignment(&full);
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            Some((input, ns))
        };
        // Reach phase: descend one env-forcing layer per concrete step, recording the reset→stall prefix.
        let mut s = self.pick_state_assignment(&bad);
        let mut prefix: Vec<BTreeMap<String, u128>> = Vec::new();
        let mut inputs: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if self.state_minterm(&s).and(&stall).unwrap() != self.ff {
                break; // reached the stall
            }
            prefix.push(s.clone());
            // s ∈ L_k \ L_{k-1} (k minimal) ⇒ s ∈ CPre_env(L_{k-1}): the env can force a successor there.
            let k = (1..layers.len())
                .find(|&k| self.state_minterm(&s).and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(1);
            match env_step(&s, &layers[k - 1]) {
                Some((inp, ns)) => {
                    inputs.push(inp);
                    s = ns;
                }
                None => {
                    return Some(StallLasso {
                        prefix,
                        cycle: Vec::new(),
                        inputs,
                    });
                }
            }
        }
        // Cycle phase: stay in the stall under env-forcing until a state repeats — the ¬good cycle.
        let mut cyc: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if let Some(j) = cyc.iter().position(|prev| *prev == s) {
                let cycle = cyc.split_off(j);
                prefix.extend(cyc); // pre-cycle stall states → prefix
                return Some(StallLasso {
                    prefix,
                    cycle,
                    inputs,
                });
            }
            cyc.push(s.clone());
            match env_step(&s, &stall) {
                Some((inp, ns)) => {
                    inputs.push(inp);
                    s = ns;
                }
                None => {
                    prefix.extend(cyc);
                    return Some(StallLasso {
                        prefix,
                        cycle: Vec::new(),
                        inputs,
                    });
                }
            }
        }
        prefix.extend(cyc);
        Some(StallLasso {
            prefix,
            cycle: Vec::new(),
            inputs,
        })
    }

    /// Concrete forward reachability from the initial state, kept to `region`, tagging each reachable
    /// in-region state with its attractor `rank` (the least `k` with the state in `layers[k]`; `0` for a
    /// state in no layer — e.g. the environment's ¬winning region under the controller's attractor
    /// layers). Returns the ranked states plus, per `reg_name` value, its minimum rank (fastest
    /// progress). The FORWARD walk (not a symbolic pre-image) keeps the free reset from making every
    /// encoding reachable. Shared by the reachability and Büchi strategy extractors.
    fn reachable_ranked_states(
        &self,
        file: &Btor2File,
        exact: &ExactModel,
        region: &BDDFunction,
        layers: &[BDDFunction],
        reg_name: &str,
    ) -> RankedStates {
        let rank_of = |s: &BTreeMap<String, u128>| -> u32 {
            let mt = self.state_minterm(s);
            (0..layers.len())
                .find(|&k| mt.and(&layers[k]).unwrap() != self.ff)
                .unwrap_or(0) as u32
        };
        // An OOM-abstained init (mununu#462) degrades to the empty state set → an
        // empty ranking → the caller finds no strategy (a sound abstain, not a panic).
        let init = self
            .initial_state_bdd(file)
            .unwrap_or_else(|_| self.ff.clone());
        let mut frontier: Vec<BTreeMap<String, u128>> = Vec::new();
        {
            let mut r = init.clone();
            for _ in 0..1024 {
                if r == self.ff {
                    break;
                }
                let s = self.pick_state_assignment(&r);
                r = r.and(&self.state_minterm(&s).not().unwrap()).unwrap();
                frontier.push(s);
            }
        }
        let mut visited: std::collections::HashSet<BTreeMap<String, u128>> =
            std::collections::HashSet::new();
        let mut reached: Vec<(BTreeMap<String, u128>, u32)> = Vec::new();
        let mut guard = 0usize;
        while let Some(s) = frontier.pop() {
            guard += 1;
            if guard > 200_000 {
                break;
            }
            if !visited.insert(s.clone()) {
                continue;
            }
            for ns in self.concrete_successors(exact, &s) {
                if !visited.contains(&ns) {
                    frontier.push(ns);
                }
            }
            // Only states in the winner's region carry a strategy row.
            if self.state_minterm(&s).and(region).unwrap() != self.ff {
                let k = rank_of(&s);
                reached.push((s, k));
            }
        }
        // Min rank per state-register value (its fastest progress, as in the 1-player extractor).
        let mut min_rank: BTreeMap<u128, u32> = BTreeMap::new();
        for (s, k) in &reached {
            let v = *s.get(reg_name).unwrap_or(&0);
            min_rank
                .entry(v)
                .and_modify(|r| *r = (*r).min(*k))
                .or_insert(*k);
        }
        (reached, min_rank)
    }

    /// Build the controller's [`MealyStrategy`] from precomputed attractor `layers`, the ranked reachable
    /// `reached` states and their per-value `min_rank`. `recur_target`: `None` = rank-0 states are
    /// terminal (a REACH goal — no move); `Some(w)` = rank-0 states force a move into `w` (a BÜCHI recur
    /// point — visit `good`, then re-enter the winning region so `good` recurs). At each min-rank control
    /// value force a rank-decreasing (or, at rank 0 with a recur target, region-re-entering) move:
    /// env-independent when one exists (Moore fast-path, [`ExactModel::ctrl_forcing_moves`]), else one
    /// response per environment input ([`ExactModel::move_into`] — a genuinely reactive controller, since
    /// the controller is the `∀env ∃ctrl` responder).
    fn controller_mealy_from_layers(
        &self,
        exact: &ExactModel,
        reg_name: &str,
        controllable: &std::collections::HashSet<String>,
        layers: &[BDDFunction],
        recur_target: Option<&BDDFunction>,
        ranked: &RankedStates,
    ) -> MealyStrategy {
        let (reached, min_rank) = ranked;
        const MAX_REACTIVE_MOVES: usize = 256;
        let is_ctrl = |c: &&Cell| -> bool { !c.is_state && controllable.contains(&c.symbol) };
        let is_env = |c: &&Cell| -> bool { !c.is_state && !controllable.contains(&c.symbol) };
        // Per value: the union of its min-rank state minterms + that value's (uniform) rank.
        let mut states_by_val: BTreeMap<u128, (BDDFunction, u32)> = BTreeMap::new();
        for (s, k) in reached {
            let v = *s.get(reg_name).unwrap_or(&0);
            if *k != min_rank[&v] {
                continue;
            }
            let mt = self.state_minterm(s);
            states_by_val
                .entry(v)
                .and_modify(|(acc, _)| *acc = acc.or(&mt).unwrap())
                .or_insert((mt, *k));
        }
        let mut entries: Vec<MealyEntry> = Vec::new();
        for (&v, (mt, rank)) in &states_by_val {
            // The set the forced move must land in: the next-lower attractor layer (rank ≥ 1), or — at a
            // rank-0 state with a recur target — the winning region itself (Büchi recur). A rank-0 reach
            // goal (no recur target) needs no move.
            let target = if *rank == 0 {
                match recur_target {
                    None => {
                        entries.push(MealyEntry {
                            state_value: v,
                            rank: 0,
                            moves: Vec::new(),
                            complete: true,
                        });
                        continue;
                    }
                    Some(w) => w,
                }
            } else {
                &layers[(*rank - 1) as usize]
            };
            // Moore fast-path: a single controllable move robust to every environment input.
            let moore = mt.and(&exact.ctrl_forcing_moves(target)).unwrap();
            if moore != self.ff {
                let mut forced_ctrl = BTreeMap::new();
                for cell in self.cells.iter().filter(is_ctrl) {
                    if let Some(val) = self.forced_cell_value(&moore, cell) {
                        forced_ctrl.insert(cell.symbol.clone(), val);
                    }
                }
                entries.push(MealyEntry {
                    state_value: v,
                    rank: *rank,
                    moves: vec![MealyMove {
                        env_inputs: BTreeMap::new(),
                        forced_ctrl,
                    }],
                    complete: true,
                });
                continue;
            }
            // Reactive: enumerate one controllable response per environment-input valuation. The state is
            // in the attractor, so `∀env ∃ctrl` — every environment column has a response.
            let mut rem = mt.and(&exact.move_into(target)).unwrap();
            let mut moves: Vec<MealyMove> = Vec::new();
            for _ in 0..MAX_REACTIVE_MOVES {
                if rem == self.ff {
                    break;
                }
                let full = self.pick_full_assignment(&rem);
                let mut env_inputs = BTreeMap::new();
                let mut env_mt = self.tt.clone();
                for cell in self.cells.iter().filter(is_env) {
                    let val = *full.get(&cell.symbol).unwrap_or(&0);
                    env_inputs.insert(cell.symbol.clone(), val);
                    for (b, var) in cell.vars.iter().enumerate() {
                        let lit = if (val >> b) & 1 == 1 {
                            var.clone()
                        } else {
                            var.not().unwrap()
                        };
                        env_mt = env_mt.and(&lit).unwrap();
                    }
                }
                let mut forced_ctrl = BTreeMap::new();
                for cell in self.cells.iter().filter(is_ctrl) {
                    forced_ctrl.insert(cell.symbol.clone(), *full.get(&cell.symbol).unwrap_or(&0));
                }
                moves.push(MealyMove {
                    env_inputs,
                    forced_ctrl,
                });
                rem = rem.and(&env_mt.not().unwrap()).unwrap();
            }
            entries.push(MealyEntry {
                state_value: v,
                rank: *rank,
                moves,
                complete: rem == self.ff, // false = the reactive fan-out hit the bound (no silent cap)
            });
        }
        entries.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.state_value.cmp(&b.state_value)));
        MealyStrategy {
            state_register: reg_name.to_string(),
            entries,
        }
    }

    /// P2.5-F (b) — the CONTROLLER's Mealy strategy for a REALIZABLE recurrence (Büchi) game `GF good`
    /// (force `good` infinitely often). `None` when the game is unrealizable (the initial state is not in
    /// the Büchi winning region — the actionable witness is then the environment starvation lasso, see
    /// [`exact_two_player_recurrence_stall_lasso`]) or `reg_name` is not a state register.
    ///
    /// **Engine (§16):** `exact-symbolic` ROBDD. The Büchi winning region is `W = νZ. Attr_ctrl(good ∧
    /// CPre_ctrl Z)` (Emerson–Lei); the memoryless controller strategy attracts to the recur set `R =
    /// good ∧ CPre_ctrl W` and, from `R`, re-enters `W`, so `R ⊆ good` is visited infinitely often
    /// (positional determinacy of Büchi games). Reuses [`Self::controller_mealy_from_layers`] with the
    /// attractor layers to `R` and a recur target of `W`.
    fn two_player_buchi_strategy(
        &self,
        file: &Btor2File,
        good: &BDDFunction,
        reg_name: &str,
        controllable: &std::collections::HashSet<String>,
    ) -> Option<TwoPlayerStrategy> {
        if !self
            .cells
            .iter()
            .any(|c| c.is_state && c.symbol == reg_name)
        {
            return None;
        }
        let exact = self.exact_model_partitioned(controllable);
        // Büchi winning region W = νZ. Attr_ctrl(good ∧ CPre_ctrl Z): start at ⊤ and shrink.
        let mut w = self.tt.clone();
        loop {
            let recur = good.and(&exact.cpre_controllable(&w)).unwrap();
            let mut attr = recur;
            loop {
                let next = attr.or(&exact.cpre_controllable(&attr)).unwrap();
                if next == attr {
                    break;
                }
                attr = next;
            }
            if attr == w {
                break;
            }
            w = attr;
        }
        let init = self.initial_state_bdd(file).ok()?;
        if init.and(&w).unwrap() == self.ff {
            return None; // unrealizable Büchi → the env starvation lasso is the witness, not a strategy
        }
        // Attractor layers to the recur set R = good ∧ CPre_ctrl W (L_0 = R, …, L_last = W).
        let recur = good.and(&exact.cpre_controllable(&w)).unwrap();
        let mut layers = vec![recur];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.cpre_controllable(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let ranked = self.reachable_ranked_states(file, &exact, &w, &layers, reg_name);
        Some(TwoPlayerStrategy::ControllerStrategy(
            self.controller_mealy_from_layers(
                &exact,
                reg_name,
                controllable,
                &layers,
                Some(&w),
                &ranked,
            ),
        ))
    }

    /// P2.5-F — synthesize the two-player strategy for the controllable-reachability game to `good`, with
    /// `controllable` the controller's input names. Solves the attractor `μX. good ∨ CPre_ctrl(X)`; the
    /// controller wins iff `init ⊆ attractor`. Returns the WINNER's strategy, and the Mealy/positional
    /// asymmetry is intrinsic to the game (`CPre_ctrl = ∀env ∃ctrl` — the environment moves, the
    /// controller responds):
    /// - **realizable** (controller wins): a [`MealyStrategy`]. At each reachable winning state force a
    ///   rank-decreasing controllable move: env-independent when one exists ([`ExactModel::ctrl_forcing_moves`]
    ///   — the Moore fast-path), else ONE response per environment input ([`ExactModel::move_into`] — a
    ///   genuinely reactive controller, since the controller is the responder). With all inputs
    ///   controllable this is Moore everywhere and `MealyStrategy::as_positional` equals the 1-player
    ///   [`Self::env_positional_strategy`].
    /// - **unrealizable** (environment wins): a positional [`PositionalStrategy`] — the environment is the
    ///   first-mover, so its winning strategy is state-indexed. Each reachable state OUTSIDE the attractor
    ///   forces ENVIRONMENT inputs that keep the play out of it ([`ExactModel::env_forcing_moves`],
    ///   ∀ctrl-robust). A safety/maintain strategy, so every row has rank 0.
    ///
    /// `None` when `reg_name` is not a state register.
    fn two_player_strategy(
        &self,
        file: &Btor2File,
        good: &BDDFunction,
        reg_name: &str,
        controllable: &std::collections::HashSet<String>,
    ) -> Option<TwoPlayerStrategy> {
        let exact = self.exact_model_partitioned(controllable);
        // Controllable-reachability attractor: L_0 = good, L_k = L_{k-1} ∨ CPre_ctrl(L_{k-1}).
        // rank(s) = min k : s ∈ L_k. The controller wins from init iff init ⊆ the fixpoint.
        let mut layers = vec![good.clone()];
        loop {
            let prev = layers.last().unwrap();
            let next = prev.or(&exact.cpre_controllable(prev)).unwrap();
            if &next == prev {
                break;
            }
            layers.push(next);
        }
        let winning = layers.last().unwrap().clone();
        if !self
            .cells
            .iter()
            .any(|c| c.is_state && c.symbol == reg_name)
        {
            return None;
        }
        let init = self.initial_state_bdd(file).ok()?;
        let realizable = init.and(&winning).unwrap() != self.ff;
        // The winner's region: the controller lives inside the attractor; the environment lives outside it
        // (by determinacy of the reachability game, ¬attractor is the environment's winning region).
        let region = if realizable {
            winning.clone()
        } else {
            winning.not().unwrap()
        };
        let ranked = self.reachable_ranked_states(file, &exact, &region, &layers, reg_name);
        let is_env = |c: &&Cell| -> bool { !c.is_state && !controllable.contains(&c.symbol) };
        if realizable {
            // CONTROLLER (Mealy): a rank-decreasing forced move at each reachable winning state; the
            // rank-0 (`good`) states are terminal (`recur_target = None` — a REACH goal, not a Büchi
            // recur). `cpre_controllable = ∀env ∃ctrl`, so the responder may be reactive.
            Some(TwoPlayerStrategy::ControllerStrategy(
                self.controller_mealy_from_layers(
                    &exact,
                    reg_name,
                    controllable,
                    &layers,
                    None,
                    &ranked,
                ),
            ))
        } else {
            // ENVIRONMENT counterstrategy — positional (the environment is the first-mover). At each
            // reachable state outside the attractor force env inputs keeping the play out of it
            // (`env_forcing_moves`, robust to every controllable move). The `∀ctrl` is the correct dual of
            // `cpre_controllable`'s `∀env ∃ctrl`, so this exists iff the controller has no strategy.
            let (reached, min_rank) = &ranked;
            let mut moves_by_val: BTreeMap<u128, BDDFunction> = BTreeMap::new();
            for (s, k) in reached {
                let v = *s.get(reg_name).unwrap_or(&0);
                if *k != min_rank[&v] {
                    continue;
                }
                let m = self
                    .state_minterm(s)
                    .and(&exact.env_forcing_moves(&region))
                    .unwrap();
                moves_by_val
                    .entry(v)
                    .and_modify(|acc| *acc = acc.or(&m).unwrap())
                    .or_insert(m);
            }
            let mut entries: Vec<StrategyEntry> = min_rank
                .iter()
                .map(|(&v, &rank)| {
                    let mut forced = BTreeMap::new();
                    if let Some(mv) = moves_by_val.get(&v)
                        && *mv != self.ff
                    {
                        for cell in self.cells.iter().filter(is_env) {
                            if let Some(val) = self.forced_cell_value(mv, cell) {
                                forced.insert(cell.symbol.clone(), val);
                            }
                        }
                    }
                    StrategyEntry {
                        state_value: v,
                        rank,
                        forced_inputs: forced,
                    }
                })
                .collect();
            entries.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.state_value.cmp(&b.state_value)));
            Some(TwoPlayerStrategy::EnvironmentCounterstrategy(
                PositionalStrategy {
                    state_register: reg_name.to_string(),
                    entries,
                },
            ))
        }
    }

    /// The distinct concrete next-states reachable from `s` under some constraint-respecting input.
    /// Enumerates by picking a full `(state = s, input)` assignment, stepping it, then subtracting every
    /// input that leads to that successor — so the loop count is the number of DISTINCT successors (few).
    fn concrete_successors(
        &self,
        exact: &ExactModel,
        s: &BTreeMap<String, u128>,
    ) -> Vec<BTreeMap<String, u128>> {
        let mut rem = self.state_minterm(s).and(&self.constraint_bdd).unwrap();
        let mut outs = Vec::new();
        for _ in 0..4096 {
            if rem == self.ff {
                break;
            }
            let full = self.pick_full_assignment(&rem);
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            rem = rem
                .and(&exact.to_next(&self.state_minterm(&ns)).not().unwrap())
                .unwrap();
            outs.push(ns);
        }
        outs
    }

    /// The value `cell` is forced to across the (state,input) set `set` (assumed non-empty), or `None`
    /// if any of `cell`'s bits can take either value there — i.e. the cell is not (fully) constrained.
    fn forced_cell_value(&self, set: &BDDFunction, cell: &Cell) -> Option<u128> {
        let mut val = 0u128;
        for (b, var) in cell.vars.iter().enumerate() {
            let with1 = set.and(var).unwrap();
            let with0 = set.and(&var.not().unwrap()).unwrap();
            if with1 != self.ff && with0 != self.ff {
                return None;
            }
            if with1 != self.ff {
                val |= 1u128 << b;
            }
        }
        Some(val)
    }

    /// `¬EF p = ¬(μX. (p ∨ ◇X))` over the exact model — the "trap" region: states from which
    /// `p` is unreachable. `◇` is `∃input` (`diamond_pre`), so `EF p` is the states with SOME
    /// path to `p`. Least fixpoint from ⊥.
    fn not_ef_p(&self, exact: &ExactModel, p: &BDDFunction) -> BDDFunction {
        let mut ef = self.ff.clone();
        loop {
            let next = p.or(&exact.diamond_pre(&ef)).unwrap();
            if next == ef {
                break;
            }
            ef = next;
        }
        ef.not().unwrap()
    }

    /// `stall = EG ¬p = νZ. (¬p ∧ ◇Z)` over the exact model — the states with an
    /// infinite `p`-avoiding path (`¬⟦AF p⟧`). Greatest fixpoint from ⊤.
    fn eg_not_p(&self, exact: &ExactModel, p: &BDDFunction) -> BDDFunction {
        let not_p = p.not().unwrap();
        let mut stall = self.tt.clone();
        loop {
            let next = not_p.and(&exact.diamond_pre(&stall)).unwrap();
            if next == stall {
                break;
            }
            stall = next;
        }
        stall
    }

    /// P3 — the INPUT part of a full (state+input) assignment: the values of the design's
    /// input cells only. This is the input that drives the transition, recorded for RTL replay.
    fn input_assignment(&self, full: &HashMap<String, u128>) -> BTreeMap<String, u128> {
        self.cells
            .iter()
            .filter(|c| !c.is_state)
            .filter_map(|c| full.get(&c.symbol).map(|v| (c.symbol.clone(), *v)))
            .collect()
    }

    /// Walk a concrete `¬p` path inside `stall` from `s` until a state repeats — the
    /// cycle. `prefix` (the already-walked reach path) is prepended to the result; any
    /// pre-cycle stall states join it. `inputs` accumulates the input driving each recorded
    /// transition (for RTL replay). Each step greedily picks a stall-preserving input and
    /// advances via [`eval_step`]. Bounded by the stall's size; a deadlock yields an open lasso.
    ///
    /// [`eval_step`]: BddBitBlaster::eval_step
    fn walk_stall_cycle(
        &self,
        exact: &ExactModel,
        stall: &BDDFunction,
        mut s: BTreeMap<String, u128>,
        mut prefix: Vec<BTreeMap<String, u128>>,
        mut inputs: Vec<BTreeMap<String, u128>>,
    ) -> StallLasso {
        let mut cyc: Vec<BTreeMap<String, u128>> = Vec::new();
        for _ in 0..1_000_000 {
            if let Some(j) = cyc.iter().position(|prev| *prev == s) {
                let cycle = cyc.split_off(j);
                prefix.extend(cyc); // pre-cycle stall states → prefix
                return StallLasso {
                    prefix,
                    cycle,
                    inputs,
                };
            }
            cyc.push(s.clone());
            // The successors-in-stall set for state = s (inputs free) is
            // `state_minterm(s) ∧ to_next(stall)` — non-empty because s ∈ stall =
            // ¬p ∧ ◇stall guarantees a stall-successor under some input.
            let good = self.state_minterm(&s).and(&exact.to_next(stall)).unwrap();
            if good == self.ff {
                prefix.extend(cyc);
                return StallLasso {
                    prefix,
                    cycle: Vec::new(),
                    inputs,
                };
            }
            // Pick a full (state = s, input) assignment, step concretely, and keep
            // held registers (no `Next` line) at their current value.
            let full = self.pick_full_assignment(&good);
            inputs.push(self.input_assignment(&full));
            let mut ns = s.clone();
            for (reg, val) in self.eval_step(&full) {
                ns.insert(reg, val);
            }
            s = ns;
        }
        prefix.extend(cyc);
        StallLasso {
            prefix,
            cycle: Vec::new(),
            inputs,
        }
    }

    /// The minterm fixing only the STATE bits to `state` (input bits left free).
    fn state_minterm(&self, state: &BTreeMap<String, u128>) -> BDDFunction {
        let mut mt = self.tt.clone();
        for cell in &self.cells {
            if !cell.is_state {
                continue;
            }
            let v = state.get(&cell.symbol).copied().unwrap_or(0);
            for (b, var) in cell.vars.iter().enumerate() {
                let lit = if (v >> b) & 1 == 1 {
                    var.clone()
                } else {
                    var.not().unwrap()
                };
                mt = mt.and(&lit).unwrap();
            }
        }
        mt
    }

    /// Greedily pick one satisfying STATE assignment from a non-empty `set` (state
    /// registers only; inputs left free).
    fn pick_state_assignment(&self, set: &BDDFunction) -> BTreeMap<String, u128> {
        self.pick_assignment(set, true).into_iter().collect()
    }

    /// Greedily pick one satisfying FULL assignment (state + input) from `set`.
    fn pick_full_assignment(&self, set: &BDDFunction) -> HashMap<String, u128> {
        self.pick_assignment(set, false)
    }

    /// Greedy minterm pick: for each selected cell's bits, keep the value that leaves
    /// the residual set non-empty. `state_only` restricts to state cells.
    fn pick_assignment(&self, set: &BDDFunction, state_only: bool) -> HashMap<String, u128> {
        let mut cur = set.clone();
        let mut out: HashMap<String, u128> = HashMap::new();
        for cell in &self.cells {
            if state_only && !cell.is_state {
                continue;
            }
            let mut val = 0u128;
            for (b, var) in cell.vars.iter().enumerate() {
                let with1 = cur.and(var).unwrap();
                if with1 != self.ff {
                    cur = with1;
                    val |= 1u128 << b;
                } else {
                    cur = cur.and(&var.not().unwrap()).unwrap();
                }
            }
            out.insert(cell.symbol.clone(), val);
        }
        out
    }

    /// Live inner-node count of the shared BDD arena — the ACTUAL footprint of everything built
    /// (the transition-system next-state functions, etc.). O(1). Test-measurement helper.
    #[cfg(test)]
    pub(crate) fn live_bdd_nodes(&self) -> usize {
        self._manager
            .with_manager_shared(|m| m.approx_num_inner_nodes())
    }
}

/// `(unique BDD nodes in f, BDD height of f)`. Both ACTUAL (traversal-measured), not theoretical.
/// Height = the longest root→terminal path in DECISION nodes, computed by memoized cofactor recursion
/// (`cofactors()` splits on f's top variable; terminals return 0). Test-measurement helper.
///
/// `mutable_key_type`: `BDDFunction` has interior mutability (its manager ref) but its `Hash`/`Eq`
/// are by stable node identity, so it is a sound `HashMap` key.
#[cfg(test)]
#[allow(clippy::mutable_key_type)]
pub(crate) fn bdd_nodes_height(f: &BDDFunction) -> (usize, usize) {
    use oxidd::Function; // cofactors(), node_count()
    use std::collections::HashMap;
    fn height(f: &BDDFunction, memo: &mut HashMap<BDDFunction, usize>) -> usize {
        if let Some(&v) = memo.get(f) {
            return v;
        }
        let v = match f.cofactors() {
            None => 0, // terminal (⊤ / ⊥)
            Some((t, e)) => 1 + height(&t, memo).max(height(&e, memo)),
        };
        memo.insert(f.clone(), v);
        v
    }
    (f.node_count(), height(f, &mut HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast::simulate_one_step;
    use crate::adapter::btor2::parser;

    /// MEASUREMENT — the ACTUAL BDD (node count + height), not the theoretical state count, of the
    /// canonical wide-counter control primitive, plus its discoverable predicates. A K-bit reloading
    /// down-counter `cnt' = (cnt==0) ? MAX : cnt-1` has 2^K reachable STATES, but its BDD transition
    /// relation is small — this measures the actual OxiDD representation to show that the wall is NOT
    /// the BDD size (so variable reordering, which only shrinks BDD size, cannot help it — the wall
    /// is the 2^K fixpoint DIAMETER, which no ordering changes). `MUNUNU_PROBE_BTOR2` additionally
    /// measures a real lift's transition-system BDD.
    #[test]
    #[ignore = "measurement — actual BDD node count + height of the counter transition system"]
    fn measure_bdd_actual_size() {
        fn counter_btor2(k: u32) -> String {
            let max = (1u128 << k) - 1;
            format!(
                "1 sort bitvec 1\n2 sort bitvec {k}\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
                 6 constd 2 {max}\n7 init 2 3 6\n8 eq 1 3 4\n9 sub 2 3 5\n10 ite 2 8 6 9\n11 next 2 3 10\n"
            )
        }
        eprintln!("=== ACTUAL BDD of a K-bit reloading down-counter's transition relation ===");
        eprintln!(
            "  reachable STATES = 2^K, but we measure the BDD REPRESENTATION (exact node_count + height):"
        );
        eprintln!(
            "  {:>3} {:>18} {:>14} {:>16}",
            "K", "transition-BDD nodes", "height (nodes)", "states 2^K"
        );
        for k in [4u32, 8, 12, 16, 20, 24] {
            let f = parser::parse(&counter_btor2(k)).expect("parse");
            let bb = BddBitBlaster::build(&f).expect("counter fits the bit cap");
            let nf = bb.next_funcs.get("cnt").expect("cnt next-fn");
            // exact transition-relation size = distinct BDD nodes across all next-state bits;
            // height = the deepest per-bit root→terminal path (the MSB bit depends on all K bits).
            let sum_nodes: usize = nf.iter().map(|b| bdd_nodes_height(b).0).sum();
            let height = nf.iter().map(|b| bdd_nodes_height(b).1).max().unwrap_or(0);
            eprintln!(
                "  {:>3} {:>18} {:>14} {:>16}",
                k,
                sum_nodes,
                height,
                1u64 << k
            );
        }
        eprintln!(
            "  ⇒ MEASURED: the counter's BDD is SMALL — live nodes + Σ next-fn nodes grow ~poly(K), \
             height ~K (linear), while the reachable STATE count is 2^K. So the counter's hardness is \
             NOT its BDD size; it is the 2^K reachable DIAMETER (the μ/ν pre-image fixpoint needs ~2^K \
             iterations — the reason the #369 fixpoint-iteration budget exists, distinct from the node \
             budget). Variable reordering shrinks BDD size, which is already small, and does NOT change \
             the iteration count ⇒ reordering CANNOT improve a counter's tractability."
        );

        // Discoverable predicates for the counter (what a predicate-abstraction refinement can name).
        let f = parser::parse(&counter_btor2(16)).expect("parse");
        let counters = crate::adapter::btor2::bit_blast::detect_down_counter(&f);
        eprintln!("=== discoverable predicates for the K=16 counter ===");
        eprintln!(
            "  detect_down_counter: {} counter(s); threshold predicate(s): {:?}",
            counters.len(),
            counters
                .iter()
                .map(|c| format!("cnt == {}", c.threshold))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "  ⇒ a counter yields exactly ONE natural predicate (cnt == threshold). That is enough to \
             STATE the recovery target but NOT to DECIDE it: no bounded predicate set captures a 2^K-step \
             descent, so the cube abstains — the descent is decided by the RANKING certificate (a \
             well-founded measure), not by adding predicates. So 'more discoverable predicates' is not \
             the lever either."
        );

        // Optional: a real lift's transition-system BDD (build + actual size, or the build wall).
        if let Ok(path) = std::env::var("MUNUNU_PROBE_BTOR2") {
            let content = std::fs::read_to_string(&path).expect("read btor2");
            let rf = parser::parse(&content).expect("parse real");
            eprintln!(
                "=== real lift {} ===",
                path.rsplit('/').next().unwrap_or(&path)
            );
            match BddBitBlaster::build(&rf) {
                Ok(bb) => {
                    let live = bb.live_bdd_nodes();
                    let height = bb
                        .next_funcs
                        .values()
                        .flat_map(|v| v.iter())
                        .map(|b| bdd_nodes_height(b).1)
                        .max()
                        .unwrap_or(0);
                    eprintln!(
                        "  built: live BDD nodes = {live}, transition-fn height (max) = {height}"
                    );
                    // Crux: the transition BDD is small — so does the exact FIXPOINT decide it once
                    // built? Call the EXACT engine DIRECTLY (no cube fallback) so the verdict is
                    // exact-only: Err = exact abstained (cap/node/iteration budget), never cube.
                    if let Ok(target) = std::env::var("MUNUNU_PROBE_GOOD") {
                        let formula_str = format!("nu Y. ((mu X. (({target}) || <> X)) && [] Y)");
                        let formula = crate::mu_calculus::parser::parse(&formula_str)
                            .expect("formula parses");
                        let t0 = std::time::Instant::now();
                        let v = exact_symbolic_verdict(&content, &formula);
                        eprintln!(
                            "  EXACT-ONLY AG EF ({target}): {v:?}  [{} ms]",
                            t0.elapsed().as_millis()
                        );
                    }
                }
                Err(e) => eprintln!("  BDD build wall: {e}"),
            }
            // The ALTERNATIVE tool for a wide / BDD-blowing datapath cone: PREDICATE ABSTRACTION
            // (cube + smt-hyper-must). It has its own path (no full-state bit-blast), so it runs
            // whether or not the exact BDD built — the "right tool vs reordering" comparison.
            if let Ok(target) = std::env::var("MUNUNU_PROBE_GOOD") {
                let tc = std::time::Instant::now();
                let cube = crate::adapter::recoverability::verify_recoverability_scalable(
                    &content,
                    &target,
                    &[],
                );
                eprintln!(
                    "  CUBE (predicate-abstraction) AG EF ({target}): {cube:?}  [{} ms]",
                    tc.elapsed().as_millis()
                );
            }
        }
    }

    /// P2.2c #4 — the SOUND diameter pre-pass MEASURES the backward-reach distance to the target,
    /// so it neither over- nor under-predicts (the two failure modes of the structural counter
    /// proxy). On a K-bit reloading down-counter draining to 0, the diameter to `cnt == 0` is
    /// 2^K − 1: a `k_max` below it reports `ExceedsBound`; above it, `Saturated` at the true depth.
    #[test]
    fn reach_diameter_measures_the_true_counter_distance() {
        // 4-bit reloading down-counter (init = 15, drains to 0, reloads) — diameter to cnt==0 = 15.
        let src = "1 sort bitvec 1\n2 sort bitvec 4\n3 state 2 cnt\n4 zero 2\n5 one 2\n\
                   6 constd 2 15\n7 init 2 3 6\n8 eq 1 3 4\n9 sub 2 3 5\n10 ite 2 8 6 9\n11 next 2 3 10\n";
        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let target = bb
            .predicate_bdd(&PredicateExpr::Cmp {
                register: "cnt".into(),
                op: CmpOp::Eq,
                value: 0,
            })
            .expect("target bdd");
        let model = bb.exact_model();

        // k_max BELOW the diameter → ExceedsBound (the measured diameter signal — this is what the
        // structural counter proxy could only GUESS, and got wrong for up-counters / near targets).
        assert_eq!(
            model.reach_diameter_to(&target, 8),
            DiameterEstimate::ExceedsBound(8),
            "an 8-step bound cannot reach cnt==0 from the far end of a 4-bit drain"
        );
        // k_max ABOVE the diameter → Saturated at the TRUE distance (~15), and it is measured, not
        // assumed — the pre-pass ran the exact engine's own EF fixpoint, bounded.
        match model.reach_diameter_to(&target, 64) {
            DiameterEstimate::Saturated(d) => assert!(
                (12..=17).contains(&d),
                "a 4-bit drain saturates at diameter ~15, measured: {d}"
            ),
            e => panic!("expected Saturated within the generous bound, got {e:?}"),
        }
    }

    /// A.4 — `ef_target_atoms` names the reachability target of a bare `EF p`
    /// (`μX. (p ∨ ◇X)`), whose violation is the "target unreachable" repair witness,
    /// and returns `None` for the liveness/recoverability shapes (which carry a concrete
    /// path witness instead).
    #[test]
    fn ef_target_atoms_names_bare_ef_reachability_target() {
        use crate::mu_calculus::parser as mu_parser;
        // bare `EF p` = μX.(p ∨ ◇X): the reachability target is the single atom.
        let ef = mu_parser::parse("mu X. ((round_counter == 31) or <> X)").expect("EF parses");
        let atoms = ef_target_atoms(&ef).expect("bare EF yields a target");
        assert_eq!(atoms.len(), 1);
        assert!(atoms[0].contains("round_counter"));
        // `AG EF p` (recoverability, νμ) is NOT a bare EF — its failure is a trap PATH,
        // not an unreachable target, so no unreachable-target witness here.
        let agef = mu_parser::parse("nu Y. ((mu X. ((busy == 0) or <> X)) and [] Y)")
            .expect("AG EF parses");
        assert_eq!(ef_target_atoms(&agef), None);
        // `AF p` (box inner) is a liveness/stall shape, not reachability.
        let af = mu_parser::parse("mu X. ((done == 1) or [] X)").expect("AF parses");
        assert_eq!(ef_target_atoms(&af), None);
    }

    /// Ranking-wiring — `ag_ef_good_atoms` names the `good` target of a recoverability
    /// `AG EF good` (`νY. ((μX. (good ∨ ◇X)) ∧ □Y)`), so a caller can route it to the
    /// scalable ranking/recoverability engine. `None` for a bare `EF` (that is the
    /// unreachable-target case) or any non-recoverability shape.
    #[test]
    fn ag_ef_good_atoms_names_recoverability_target() {
        use crate::mu_calculus::parser as mu_parser;
        // `AG EF (cnt == 0)` — a down-counter/timer recoverability target.
        let agef = mu_parser::parse("nu Y. ((mu X. ((cnt == 0) or <> X)) and [] Y)")
            .expect("AG EF parses");
        let atoms = ag_ef_good_atoms(&agef).expect("AG EF yields a good target");
        assert_eq!(atoms, vec!["cnt == 0".to_string()]);
        // relational good `AG EF (wptr == rptr)` (FIFO drain) — the register-vs-register atom.
        let rel = mu_parser::parse("nu Y. ((mu X. ((wptr == rptr) or <> X)) and [] Y)")
            .expect("relational AG EF parses");
        assert_eq!(
            ag_ef_good_atoms(&rel),
            Some(vec!["wptr == rptr".to_string()])
        );
        // a bare `EF` is NOT the recoverability shape (it is the unreachable-target case).
        let ef = mu_parser::parse("mu X. ((cnt == 0) or <> X)").expect("EF parses");
        assert_eq!(ag_ef_good_atoms(&ef), None);
    }

    /// Run the BDD bit-blaster against the concrete simulator over an
    /// exhaustive enumeration of the given (symbol, bit-width) leaves.
    fn assert_matches_simulator(
        src: &str,
        leaves: &[(&str, u32)],
        states: &[&str],
        inputs: &[&str],
    ) {
        let file = parser::parse(src).expect("parse fixture");
        let bb = BddBitBlaster::build(&file).expect("build bit-blaster");

        let total_bits: u32 = leaves.iter().map(|(_, w)| *w).sum();
        assert!(total_bits <= 16, "keep the exhaustive sweep small");

        for combo in 0..(1u128 << total_bits) {
            // Decode `combo` into a per-symbol value assignment.
            let mut assignment: HashMap<String, u128> = HashMap::new();
            let mut offset = 0u32;
            for (sym, w) in leaves {
                let mask = (1u128 << w) - 1;
                assignment.insert((*sym).to_string(), (combo >> offset) & mask);
                offset += w;
            }

            // Symbolic step.
            let symbolic = bb.eval_step(&assignment);

            // Concrete oracle.
            let regs: HashMap<String, u128> = states
                .iter()
                .map(|s| ((*s).to_string(), assignment[*s]))
                .collect();
            let inps: HashMap<String, u128> = inputs
                .iter()
                .map(|s| ((*s).to_string(), assignment[*s]))
                .collect();
            let concrete = simulate_one_step(&file, &regs, &inps).expect("concrete step");

            for st in states {
                assert_eq!(
                    symbolic.get(*st).copied(),
                    concrete.get(*st).copied(),
                    "next-state mismatch for `{st}` at assignment {assignment:?}"
                );
            }
        }
    }

    /// Saturating counter + companion register: exercises `state`, `one`,
    /// `ones`, `add`, `sub`, `eq`, `ult`, `and`, `ite`.
    #[test]
    fn rf5_3a_matches_simulator_saturating_counter() {
        let src = r#"
1 sort bitvec 3
2 sort bitvec 1
3 state 1 cnt
4 state 1 acc
5 input 2 en
6 one 1
7 ones 1
8 add 1 3 6
9 eq 2 3 7
10 ite 1 9 3 8
11 ite 1 5 10 3
12 next 1 3 11
13 sub 1 4 6
14 and 1 4 3
15 ult 2 4 3
16 ite 1 15 13 14
17 next 1 4 16
"#;
        assert_matches_simulator(
            src,
            &[("cnt", 3), ("acc", 3), ("en", 1)],
            &["cnt", "acc"],
            &["en"],
        );
    }

    /// Bit-manipulation heavy: exercises `not`, `inc`, `dec`, `neg`, `slice`,
    /// `uext`, `sext`, `concat`, `xor`, `or`, `redor`, `ite`. The next-state
    /// funnels every wide intermediate back to a 4-bit register, so the
    /// exhaustive check on `r'` transitively validates the whole cone.
    #[test]
    fn rf5_3a_matches_simulator_bit_manipulation() {
        let src = r#"
1 sort bitvec 4
2 sort bitvec 1
3 sort bitvec 2
4 sort bitvec 8
5 state 1 r
6 input 2 sel
7 not 1 5
8 inc 1 5
9 dec 1 5
10 neg 1 5
11 slice 3 5 3 2
12 uext 4 5 4
13 sext 4 5 4
14 concat 4 5 5
15 slice 1 14 7 4
16 xor 1 7 8
17 or 1 9 10
18 redor 2 5
19 ite 1 6 16 17
20 ite 1 18 15 19
21 next 1 5 20
"#;
        assert_matches_simulator(src, &[("r", 4), ("sel", 1)], &["r"], &["sel"]);
    }

    /// Unsigned comparator family + reductions on their own: `ult`, `ulte`,
    /// `ugt`, `ugte`, `neq`, `redand`, `redxor`. The seven 1-bit results are
    /// packed (via correctly-sized `concat`s) into two 3-bit words that an
    /// `ite` funnels back into the register, so the exhaustive check on `x'`
    /// validates every comparator.
    #[test]
    fn rf5_3a_matches_simulator_comparators() {
        let src = r#"
1 sort bitvec 3
2 sort bitvec 1
3 sort bitvec 2
4 state 1 x
5 input 1 y
6 ult 2 4 5
7 ulte 2 4 5
8 ugt 2 4 5
9 ugte 2 4 5
10 neq 2 4 5
11 redand 2 4
12 redxor 2 4
13 concat 3 6 7
14 concat 1 13 8
15 concat 3 9 10
16 concat 1 15 11
17 ite 1 12 14 16
18 next 1 4 17
"#;
        assert_matches_simulator(src, &[("x", 3), ("y", 3)], &["x"], &["y"]);
    }

    // ---- R-F5.3b: predicate BDDs vs `PredicateExpr::eval` ----

    /// Two 3-bit registers `a`, `b` with no transition logic — enough to hang
    /// predicate BDDs on the register-bit vars.
    fn two_register_blaster() -> BddBitBlaster {
        let src = r#"
1 sort bitvec 3
2 state 1 a
3 state 1 b
"#;
        let file = parser::parse(src).expect("parse");
        BddBitBlaster::build(&file).expect("build")
    }

    /// Exhaustively check that `predicate_bdd(expr)`, restricted to every
    /// (a, b) assignment, equals `expr.eval(regs)`.
    fn assert_predicate_matches_eval(bb: &BddBitBlaster, expr: &PredicateExpr) {
        let bdd = bb.predicate_bdd(expr).expect("compile predicate");
        for a in 0..8u128 {
            for b in 0..8u128 {
                let regs: HashMap<String, u128> = [("a".to_string(), a), ("b".to_string(), b)]
                    .into_iter()
                    .collect();
                assert_eq!(
                    bb.eval_predicate_at(&bdd, &regs),
                    expr.eval(&regs),
                    "predicate {expr:?} disagrees with eval at a={a} b={b}"
                );
            }
        }
    }

    #[test]
    fn rf5_3b_predicate_bdd_matches_eval_comparisons() {
        let bb = two_register_blaster();
        for op in [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Gt,
            CmpOp::Ge,
        ] {
            for value in 0..9u64 {
                // value 0..8 in range; value 8 exceeds the 3-bit register width.
                assert_predicate_matches_eval(
                    &bb,
                    &PredicateExpr::Cmp {
                        register: "a".to_string(),
                        op,
                        value,
                    },
                );
            }
        }
    }

    #[test]
    fn rf5_3b_predicate_bdd_matches_eval_literal_exceeds_width() {
        // A literal wider than the register: `a == 9` is always false, `a < 9`
        // always true — the zero-extend-to-common-width path must reproduce
        // `eval`'s unmasked u128 comparison.
        let bb = two_register_blaster();
        for (op, value) in [
            (CmpOp::Eq, 9u64),
            (CmpOp::Lt, 9),
            (CmpOp::Ge, 8),
            (CmpOp::Gt, 7),
        ] {
            assert_predicate_matches_eval(
                &bb,
                &PredicateExpr::Cmp {
                    register: "a".to_string(),
                    op,
                    value,
                },
            );
        }
    }

    #[test]
    fn rf5_3b_predicate_bdd_matches_eval_register_relations() {
        let bb = two_register_blaster();
        for op in [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Gt,
            CmpOp::Ge,
        ] {
            assert_predicate_matches_eval(
                &bb,
                &PredicateExpr::CmpReg {
                    lhs: "a".to_string(),
                    op,
                    rhs: "b".to_string(),
                },
            );
        }
    }

    #[test]
    fn rf5_3b_predicate_bdd_matches_eval_addend_wraps() {
        // `a == (b + k) mod 8` for every addend k — exercises the wrap boundary.
        let bb = two_register_blaster();
        for addend in 0..8u64 {
            assert_predicate_matches_eval(
                &bb,
                &PredicateExpr::CmpRegAddend {
                    lhs: "a".to_string(),
                    op: CmpOp::Eq,
                    rhs: "b".to_string(),
                    addend,
                    width: 3,
                },
            );
        }
        // A relational inequality with an addend too.
        assert_predicate_matches_eval(
            &bb,
            &PredicateExpr::CmpRegAddend {
                lhs: "a".to_string(),
                op: CmpOp::Ge,
                rhs: "b".to_string(),
                addend: 2,
                width: 3,
            },
        );
    }

    #[test]
    fn rf5_3b_predicate_bdd_matches_eval_boolean_combinators() {
        let bb = two_register_blaster();
        let a_lt_b = PredicateExpr::CmpReg {
            lhs: "a".to_string(),
            op: CmpOp::Lt,
            rhs: "b".to_string(),
        };
        let a_eq_3 = PredicateExpr::eq("a", 3);
        let b_ge_5 = PredicateExpr::Cmp {
            register: "b".to_string(),
            op: CmpOp::Ge,
            value: 5,
        };
        // (a < b ∧ a == 3) ∨ ¬(b ≥ 5)
        let compound = PredicateExpr::Or(
            Box::new(PredicateExpr::And(Box::new(a_lt_b), Box::new(a_eq_3))),
            Box::new(PredicateExpr::Not(Box::new(b_ge_5))),
        );
        assert_predicate_matches_eval(&bb, &compound);
    }

    // ---- R-F5.3c: abstract may relation vs brute-force concrete may-edges ----

    /// Build the symbolic abstract may relation and assert it equals the
    /// brute-force concrete may-relation: for every present cube `ci` and next
    /// cube `cj`, `ci → cj` is symbolically a may-edge iff *some* concrete
    /// state in `ci` steps (under *some* input) to a concrete state in `cj`.
    /// The brute force enumerates every (register, input) assignment and runs
    /// the concrete `simulate_one_step` — fully independent of the symbolic path.
    fn assert_may_relation_matches_bruteforce(
        src: &str,
        predicates: &[PredicateExpr],
        states: &[(&str, u32)],
        inputs: &[(&str, u32)],
    ) {
        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let rel = bb
            .abstract_may_relation(predicates)
            .expect("build may relation");
        let k = predicates.len();

        let cube_of = |regs: &HashMap<String, u128>| -> usize {
            let mut c = 0usize;
            for (i, p) in predicates.iter().enumerate() {
                if p.eval(regs) {
                    c |= 1 << i;
                }
            }
            c
        };

        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        let total_input_bits: u32 = inputs.iter().map(|(_, w)| *w).sum();
        assert!(
            total_state_bits + total_input_bits <= 14,
            "keep the sweep small"
        );

        let mut expected: std::collections::HashSet<(usize, usize)> = Default::default();
        for scombo in 0..(1u128 << total_state_bits) {
            let mut regs: HashMap<String, u128> = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (scombo >> off) & mask);
                off += w;
            }
            let present_cube = cube_of(&regs);
            for icombo in 0..(1u128 << total_input_bits) {
                let mut inps: HashMap<String, u128> = HashMap::new();
                let mut ioff = 0u32;
                for (name, w) in inputs {
                    let mask = (1u128 << w) - 1;
                    inps.insert((*name).to_string(), (icombo >> ioff) & mask);
                    ioff += w;
                }
                let next = simulate_one_step(&file, &regs, &inps).expect("concrete step");
                // Held registers keep their current value (BTOR2 convention).
                let mut regs_next = regs.clone();
                regs_next.extend(next);
                expected.insert((present_cube, cube_of(&regs_next)));
            }
        }

        for ci in 0..(1usize << k) {
            for cj in 0..(1usize << k) {
                assert_eq!(
                    rel.may_holds(ci, cj),
                    expected.contains(&(ci, cj)),
                    "may-edge {ci} -> {cj} disagrees with brute force"
                );
            }
        }
    }

    /// 2-bit saturating counter with an enable, predicates `{cnt == 0, cnt ≥ 2}`.
    /// Exercises the single-register substitution path.
    #[test]
    fn rf5_3c_may_relation_saturating_counter() {
        let src = r#"
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
"#;
        let predicates = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        assert_may_relation_matches_bruteforce(src, &predicates, &[("cnt", 2)], &[("en", 1)]);
    }

    /// Two 2-bit registers stepping together, with a **relational** predicate
    /// `cnt == acc` plus `cnt ≥ 2` and `acc == 0`. Exercises substitution over
    /// two registers that each carry a next-state function, and a `CmpReg`.
    #[test]
    fn rf5_3c_may_relation_two_registers_relational_predicate() {
        // cnt' = (cnt + 1) mod 4 ; acc' = en ? cnt : acc
        let src = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 state 1 acc
5 input 2 en
6 one 1
7 add 1 3 6
8 ite 1 5 3 4
9 next 1 3 7
10 next 1 4 8
"#;
        let predicates = [
            PredicateExpr::CmpReg {
                lhs: "cnt".to_string(),
                op: CmpOp::Eq,
                rhs: "acc".to_string(),
            },
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
            PredicateExpr::eq("acc", 0),
        ];
        assert_may_relation_matches_bruteforce(
            src,
            &predicates,
            &[("cnt", 2), ("acc", 2)],
            &[("en", 1)],
        );
    }

    // ---- R-F5.3d: abstract must relation vs brute-force concrete must-edges ----

    /// Build the symbolic abstract must relation under `semantics` and assert it
    /// equals the brute-force concrete must-relation. `∀∃`: `ci → cj` is a
    /// must-edge iff *every* concrete state in `ci` has *some* input landing in
    /// `cj`. `∀∀`: every state, under *every* input, lands in `cj`. An empty
    /// (unsatisfiable) source cube is vacuously a must-edge to every cube — the
    /// symbolic `∀` and the brute-force `.all()` over zero states agree.
    fn assert_must_relation_matches_bruteforce(
        src: &str,
        predicates: &[PredicateExpr],
        states: &[(&str, u32)],
        inputs: &[(&str, u32)],
        semantics: MustSemantics,
    ) {
        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let rel = bb
            .abstract_relation(predicates, Some(semantics))
            .expect("build must relation");
        let k = predicates.len();

        let cube_of = |regs: &HashMap<String, u128>| -> usize {
            let mut c = 0usize;
            for (i, p) in predicates.iter().enumerate() {
                if p.eval(regs) {
                    c |= 1 << i;
                }
            }
            c
        };

        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        let total_input_bits: u32 = inputs.iter().map(|(_, w)| *w).sum();
        assert!(
            total_state_bits + total_input_bits <= 14,
            "keep the sweep small"
        );

        // Per concrete state: the set of next-cubes reachable over all inputs,
        // grouped by the state's present cube.
        let mut by_cube: HashMap<usize, Vec<std::collections::HashSet<usize>>> = HashMap::new();
        for scombo in 0..(1u128 << total_state_bits) {
            let mut regs: HashMap<String, u128> = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (scombo >> off) & mask);
                off += w;
            }
            let present_cube = cube_of(&regs);
            let mut reach: std::collections::HashSet<usize> = Default::default();
            for icombo in 0..(1u128 << total_input_bits) {
                let mut inps: HashMap<String, u128> = HashMap::new();
                let mut ioff = 0u32;
                for (name, w) in inputs {
                    let mask = (1u128 << w) - 1;
                    inps.insert((*name).to_string(), (icombo >> ioff) & mask);
                    ioff += w;
                }
                let next = simulate_one_step(&file, &regs, &inps).expect("concrete step");
                let mut regs_next = regs.clone();
                regs_next.extend(next);
                reach.insert(cube_of(&regs_next));
            }
            by_cube.entry(present_cube).or_default().push(reach);
        }

        for ci in 0..(1usize << k) {
            for cj in 0..(1usize << k) {
                // An infeasible source cube (no concrete state maps to it) has
                // NO must-edge — the relation restricts must-edges to feasible
                // sources so `R_must ⊆ R_may` holds globally. A feasible cube
                // uses the ∀∃ / ∀∀ definition over its inhabitants.
                let expected = match by_cube.get(&ci) {
                    None => false,
                    Some(reaches) => match semantics {
                        MustSemantics::ForallExists => reaches.iter().all(|r| r.contains(&cj)),
                        MustSemantics::ForallForall => {
                            reaches.iter().all(|r| r.len() == 1 && r.contains(&cj))
                        }
                    },
                };
                assert_eq!(
                    rel.must_holds(ci, cj),
                    expected,
                    "must-edge {ci} -> {cj} ({semantics:?}) disagrees with brute force"
                );
            }
        }
    }

    const SATURATING_COUNTER_BTOR2: &str = r#"
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
"#;

    const TWO_REGISTER_BTOR2: &str = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 state 1 acc
5 input 2 en
6 one 1
7 add 1 3 6
8 ite 1 5 3 4
9 next 1 3 7
10 next 1 4 8
"#;

    #[test]
    fn rf5_3d_must_relation_saturating_counter() {
        let predicates = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        for sem in [MustSemantics::ForallExists, MustSemantics::ForallForall] {
            assert_must_relation_matches_bruteforce(
                SATURATING_COUNTER_BTOR2,
                &predicates,
                &[("cnt", 2)],
                &[("en", 1)],
                sem,
            );
        }
    }

    #[test]
    fn rf5_3d_must_relation_two_registers_relational_predicate() {
        let predicates = [
            PredicateExpr::CmpReg {
                lhs: "cnt".to_string(),
                op: CmpOp::Eq,
                rhs: "acc".to_string(),
            },
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
            PredicateExpr::eq("acc", 0),
        ];
        for sem in [MustSemantics::ForallExists, MustSemantics::ForallForall] {
            assert_must_relation_matches_bruteforce(
                TWO_REGISTER_BTOR2,
                &predicates,
                &[("cnt", 2), ("acc", 2)],
                &[("en", 1)],
                sem,
            );
        }
    }

    /// Gated counter: `cnt` decrements only when the *untracked* register `tb`
    /// is 1, else holds; `tb' = setb` (free input). Mirrors uart_tx's `bit_cnt_q`
    /// gated by the `tick_baud_q` register. The predicate set is only `cnt == 0`
    /// (so `tb` is untracked). With the state `(cnt=1, tb=1)` FORCED to `cnt=0`
    /// regardless of input, the `∀∃` must-edge `{cnt≠0} → {cnt≠0}` must NOT hold
    /// (that state escapes). This is the exact structure where the verify-auto
    /// symbolic engine returned an (apparently unsound) definite verdict on
    /// `AG AF (bit_cnt==0)` — the brute-force oracle is ground truth.
    const GATED_COUNTER_BTOR2: &str = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 state 2 tb
5 input 2 setb
6 one 1
7 zero 1
8 eq 2 3 7
9 not 2 8
10 and 2 4 9
11 sub 1 3 6
12 ite 1 10 11 3
13 next 1 3 12
14 next 2 4 5
"#;

    #[test]
    fn rf5_soundness_gated_counter_must_relation_matches_bruteforce() {
        let predicates = [PredicateExpr::eq("cnt", 0)];
        for sem in [MustSemantics::ForallExists, MustSemantics::ForallForall] {
            assert_must_relation_matches_bruteforce(
                GATED_COUNTER_BTOR2,
                &predicates,
                &[("cnt", 2), ("tb", 1)],
                &[("setb", 1)],
                sem,
            );
        }
    }

    // ---- D1.1: exact full-state 2-valued modal pre-image vs brute force ----

    /// The `ExactModel`'s `box_pre`/`diamond_pre` over the exact (input-
    /// nondeterministic) concrete relation, checked cell-for-cell against a
    /// brute-force concrete pre-image. `φ` is the satisfying set of `pred` (a
    /// full-state BDD via `predicate_bdd`); the brute force enumerates every
    /// `(state, input)`, simulates one step, and forms `{x : ∃i. pred(next(x,i))}`
    /// (⟨⟩) and `{x : ∀i. pred(next(x,i))}` ([]) as BDDs of state minterms. BDD
    /// equality is exact (ROBDD canonical).
    fn assert_exact_modal_matches_bruteforce(
        src: &str,
        pred: &PredicateExpr,
        states: &[(&str, u32)],
        inputs: &[(&str, u32)],
    ) {
        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let exact = bb.exact_model();
        let phi = bb.predicate_bdd(pred).expect("phi bdd");
        let dia = exact.diamond_pre(&phi);
        let boxed = exact.box_pre(&phi);

        // The minterm BDD (over state vars) for one register valuation.
        let state_minterm = |regs: &HashMap<String, u128>| -> BDDFunction {
            let mut m = bb.tt.clone();
            for cell in &bb.cells {
                if !cell.is_state {
                    continue;
                }
                let val = regs.get(&cell.symbol).copied().unwrap_or(0);
                for (b, var) in cell.vars.iter().enumerate() {
                    let lit = if (val >> b) & 1 == 1 {
                        var.clone()
                    } else {
                        var.not().unwrap()
                    };
                    m = m.and(&lit).unwrap();
                }
            }
            m
        };

        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        let total_input_bits: u32 = inputs.iter().map(|(_, w)| *w).sum();
        assert!(
            total_state_bits + total_input_bits <= 14,
            "keep the sweep small"
        );

        let mut expected_dia = bb.ff.clone();
        let mut expected_box = bb.ff.clone();
        for scombo in 0..(1u128 << total_state_bits) {
            let mut regs: HashMap<String, u128> = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (scombo >> off) & mask);
                off += w;
            }
            let mut any = false;
            let mut all = true;
            for icombo in 0..(1u128 << total_input_bits) {
                let mut inps: HashMap<String, u128> = HashMap::new();
                let mut ioff = 0u32;
                for (name, w) in inputs {
                    let mask = (1u128 << w) - 1;
                    inps.insert((*name).to_string(), (icombo >> ioff) & mask);
                    ioff += w;
                }
                let next = simulate_one_step(&file, &regs, &inps).expect("concrete step");
                let mut regs_next = regs.clone();
                regs_next.extend(next);
                let sat = pred.eval(&regs_next);
                any = any || sat;
                all = all && sat;
            }
            let m = state_minterm(&regs);
            if any {
                expected_dia = expected_dia.or(&m).unwrap();
            }
            if all {
                expected_box = expected_box.or(&m).unwrap();
            }
        }
        assert!(
            dia == expected_dia,
            "diamond_pre disagrees with brute force"
        );
        assert!(boxed == expected_box, "box_pre disagrees with brute force");
    }

    #[test]
    fn d1_1_exact_modal_gated_counter() {
        // The gated counter (cnt gated by untracked tb) — the exact model sees tb,
        // so the pre-image is precise where predicate abstraction was ⊥.
        assert_exact_modal_matches_bruteforce(
            GATED_COUNTER_BTOR2,
            &PredicateExpr::eq("cnt", 0),
            &[("cnt", 2), ("tb", 1)],
            &[("setb", 1)],
        );
    }

    #[test]
    fn d1_1_exact_modal_saturating_counter() {
        for pred in [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ] {
            assert_exact_modal_matches_bruteforce(
                SATURATING_COUNTER_BTOR2,
                &pred,
                &[("cnt", 2)],
                &[("en", 1)],
            );
        }
    }

    // ---- D1.2: exact full-state 2-valued μ-evaluator vs evaluate_tri ----

    /// The exact `ExactModel::evaluate` (2-valued, full-state BDD) agrees with the
    /// reference `evaluate_tri` on the equivalent **Sharp concrete** Kripke
    /// structure (states = concrete register valuations, Sharp edges over every
    /// input). Both are exact ⇒ definite; a state is in the exact set iff
    /// `evaluate_tri` returns `True` there. Includes `AF`/`AG AF` — the liveness
    /// the *abstraction* path answers ⊥ but the exact path decides.
    fn assert_exact_eval_matches_tri(
        src: &str,
        predicates: &[(&str, PredicateExpr)],
        states: &[(&str, u32)],
        inputs: &[(&str, u32)],
        formulas: &[&str],
    ) {
        use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, TransitionModality, Tristate};
        use crate::mu_calculus::evaluator::{Environment, evaluate_tri};
        use crate::mu_calculus::trit::Trit;

        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let exact = bb.exact_model();
        let atoms: HashMap<&str, BDDFunction> = predicates
            .iter()
            .map(|(n, e)| (*n, bb.predicate_bdd(e).expect("pred bdd")))
            .collect();

        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        let total_input_bits: u32 = inputs.iter().map(|(_, w)| *w).sum();
        assert!(
            total_state_bits + total_input_bits <= 14,
            "keep the sweep small"
        );
        let n = 1usize << total_state_bits;

        let decode = |combo: u128| -> HashMap<String, u128> {
            let mut regs = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (combo >> off) & mask);
                off += w;
            }
            regs
        };
        let encode = |regs: &HashMap<String, u128>| -> usize {
            let mut combo = 0usize;
            let mut off = 0u32;
            for (name, w) in states {
                let v = regs.get(*name).copied().unwrap_or(0) as usize;
                combo |= (v & ((1usize << w) - 1)) << off;
                off += w;
            }
            combo
        };

        // Explicit Sharp Kripke structure over the 2^bits concrete states.
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for j in 0..n {
            b.state(format!("s{j}"));
        }
        b.initial("s0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..n)
            .map(|j| b.state_id_or_insert(format!("s{j}")).unwrap())
            .collect();
        for (j, &id) in ids.iter().enumerate() {
            let regs = decode(j as u128);
            for (name, e) in predicates {
                let v = if e.eval(&regs) {
                    Tristate::KleeneT
                } else {
                    Tristate::KleeneF
                };
                b.with_3valued_predicate(id, *name, v);
            }
        }
        for (j, &id) in ids.iter().enumerate() {
            let regs = decode(j as u128);
            for icombo in 0..(1u128 << total_input_bits) {
                let mut inps = HashMap::new();
                let mut ioff = 0u32;
                for (name, w) in inputs {
                    let mask = (1u128 << w) - 1;
                    inps.insert((*name).to_string(), (icombo >> ioff) & mask);
                    ioff += w;
                }
                let next = simulate_one_step(&file, &regs, &inps).expect("step");
                let mut regs_next = regs.clone();
                regs_next.extend(next);
                let jt = encode(&regs_next);
                b.transition_ids_with_modality(id, &[step], ids[jt], TransitionModality::Sharp);
            }
        }
        let clts = b.build().expect("clts builds");

        let state_minterm = |regs: &HashMap<String, u128>| -> BDDFunction {
            let mut m = bb.tt.clone();
            for cell in &bb.cells {
                if !cell.is_state {
                    continue;
                }
                let val = regs.get(&cell.symbol).copied().unwrap_or(0);
                for (bit, var) in cell.vars.iter().enumerate() {
                    let lit = if (val >> bit) & 1 == 1 {
                        var.clone()
                    } else {
                        var.not().unwrap()
                    };
                    m = m.and(&lit).unwrap();
                }
            }
            m
        };

        let env = Environment::new(clts.state_count());
        for fs in formulas {
            let formula = crate::mu_calculus::parser::parse(fs).expect("formula parses");
            let tri = evaluate_tri(&formula, &clts, &env).expect("evaluate_tri");
            let ex = exact.evaluate(&formula, &atoms).expect("exact evaluate");
            for j in 0..n {
                let regs = decode(j as u128);
                let m = state_minterm(&regs);
                let in_exact = m.and(&ex).unwrap() == m;
                let tri_true = tri.verdict_at(j) == Trit::True;
                assert_eq!(
                    in_exact,
                    tri_true,
                    "formula `{fs}` at s{j} ({regs:?}): exact∈={in_exact} tri={:?}",
                    tri.verdict_at(j)
                );
            }
        }
    }

    #[test]
    fn d1_2_exact_eval_gated_counter_matches_tri() {
        // `cnt` gated by untracked `tb`: AF(cnt==0) is definite-False at states that
        // can loop with tb=0 (setb kept 0), definite elsewhere — the exact path
        // decides it where abstraction was ⊥.
        assert_exact_eval_matches_tri(
            GATED_COUNTER_BTOR2,
            &[("p", PredicateExpr::eq("cnt", 0))],
            &[("cnt", 2), ("tb", 1)],
            &[("setb", 1)],
            &[
                "[] p",
                "<> p",
                "not p",
                "nu X. (p and [] X)",                   // AG p
                "mu X. (p or <> X)",                    // EF p
                "mu X. (p or [] X)",                    // AF p  (the liveness)
                "nu X. ((mu Y. (p or [] Y)) and [] X)", // AG AF p
                "nu Y. ((mu X. (p or <> X)) and [] Y)", // AG EF p
            ],
        );
    }

    #[test]
    fn d1_2_exact_eval_saturating_counter_matches_tri() {
        assert_exact_eval_matches_tri(
            SATURATING_COUNTER_BTOR2,
            &[
                ("p", PredicateExpr::eq("cnt", 0)),
                (
                    "q",
                    PredicateExpr::Cmp {
                        register: "cnt".to_string(),
                        op: CmpOp::Ge,
                        value: 2,
                    },
                ),
            ],
            &[("cnt", 2)],
            &[("en", 1)],
            &[
                "[] q",
                "<> q",
                "p or q",
                "mu X. (q or [] X)",                    // AF q
                "nu X. ((mu Y. (q or [] Y)) and [] X)", // AG AF q
                "nu Y. ((mu X. (q or <> X)) and [] Y)", // AG EF q
            ],
        );
    }

    // ---- D1.3: init-state verdict + the exact_symbolic_verdict entry point ----

    /// A 2-bit wrapping up-counter with an enable, RESET to 0 via a BTOR2 `init`
    /// line — so `initial_state_bdd` pins `cnt == 0` and the verdict is a real
    /// initial-state check.
    const RESET_COUNTER_BTOR2: &str = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 input 2 en
5 one 1
6 zero 1
7 add 1 3 5
8 ite 1 4 7 3
9 next 1 3 8
10 init 1 3 6
"#;

    #[test]
    fn d1_3_exact_verdict_reset_counter() {
        // Verdicts are AT the initial state (cnt == 0), computed exactly.
        let cases = [
            ("(cnt == 0)", ExactVerdict::Holds), // init is 0
            ("not (cnt == 0)", ExactVerdict::Violated),
            ("[] (cnt == 0)", ExactVerdict::Violated), // successor cnt=1 possible
            ("mu X. ((cnt == 3) or <> X)", ExactVerdict::Holds), // EF cnt==3 (reachable)
            ("mu X. ((cnt == 3) or [] X)", ExactVerdict::Violated), // AF cnt==3 (en=0 stalls at 0)
            (
                "nu Y. ((mu X. ((cnt == 0) or <> X)) and [] Y)", // AG EF cnt==0 (wraps back)
                ExactVerdict::Holds,
            ),
        ];
        for (fs, expected) in cases {
            let formula = crate::mu_calculus::parser::parse(fs).expect("formula parses");
            let v = exact_symbolic_verdict(RESET_COUNTER_BTOR2, &formula).expect("verdict");
            assert_eq!(v, expected, "formula `{fs}`");
        }
    }

    /// Without an `init` line the initial-state set is unconstrained (every state),
    /// so `Holds` means the property holds from EVERY state. `AG AF (cnt==0)` on
    /// the *saturating* counter is `Violated` (cnt saturates at 3 and never returns
    /// to 0) — a definite verdict the abstraction path could only give as ⊥.
    #[test]
    fn d1_3_exact_verdict_no_init_is_global() {
        let f = crate::mu_calculus::parser::parse("nu X. ((mu Y. ((cnt == 0) or [] Y)) and [] X)")
            .expect("formula");
        assert_eq!(
            exact_symbolic_verdict(SATURATING_COUNTER_BTOR2, &f).expect("verdict"),
            ExactVerdict::Violated,
        );
    }

    // ---- Array/$mem lift: auto-havoc out-of-cone memories on the exact path ----

    /// Oracle: a 1-bit `phase` toggle (init 0, `phase' = !phase`) with NO memory.
    /// This is the ground-truth verdict source for the differential below.
    const PHASE_TOGGLE_NO_MEM: &str = r#"
1 sort bitvec 1
2 state 1 phase
3 zero 1
4 init 1 2 3
5 not 1 2
6 next 1 2 5
"#;

    /// Same `phase` toggle PLUS an array `rf_reg` (32×8 register file) that the
    /// property never reads — its `read` feeds only a dead `eq`. The memory is OUT
    /// of `phase`'s cone, so the exact engine auto-havocs it and decides the phase
    /// property. Before the lift, `leaf_cells` errored on the array sort (Skip).
    const PHASE_TOGGLE_OUT_OF_CONE_MEM: &str = r#"
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
14 state 1 phase
15 zero 1
16 init 1 14 15
17 not 1 14
18 next 1 14 17
"#;

    /// A register `dout` LOADS from the memory read (`dout' = rf_reg[addr]`), so the
    /// memory IS in `dout`'s cone. Havoc would over-approximate the read (unsound for
    /// liveness on memory contents), so the engine must NOT havoc — it leaves the
    /// array in place and `leaf_cells` errors → honest Skip (Err), not a verdict.
    const DOUT_READS_IN_CONE_MEM: &str = r#"
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
12 state 3 dout
13 next 3 12 11
"#;

    #[test]
    fn array_lift_out_of_cone_memory_decides_and_matches_oracle() {
        // Every phase property must decide IDENTICALLY with and without the
        // out-of-cone memory — the differential-oracle check that auto-havoc is
        // verdict-preserving. (Without the lift, the `_MEM` column errored.)
        let cases = [
            ("(phase == 0)", ExactVerdict::Holds), // init is 0
            ("mu X. ((phase == 1) or <> X)", ExactVerdict::Holds), // EF phase==1 (toggles)
            ("[] (phase == 0)", ExactVerdict::Violated), // successor phase=1
            (
                "nu Y. ((mu X. ((phase == 0) or <> X)) and [] Y)", // AG EF phase==0 (toggles back)
                ExactVerdict::Holds,
            ),
        ];
        for (fs, expected) in cases {
            let formula = crate::mu_calculus::parser::parse(fs).expect("formula parses");
            let oracle = exact_symbolic_verdict(PHASE_TOGGLE_NO_MEM, &formula)
                .expect("mem-free oracle verdict");
            assert_eq!(oracle, expected, "oracle `{fs}`");
            let lifted = exact_symbolic_verdict(PHASE_TOGGLE_OUT_OF_CONE_MEM, &formula)
                .expect("out-of-cone memory should decide, not Skip");
            assert_eq!(lifted, oracle, "array-lifted `{fs}` must match the oracle");
        }
    }

    #[test]
    fn array_lift_in_cone_memory_skips_rather_than_havoc() {
        // `dout` reads the memory, so the memory is in the property's cone. The
        // engine must NOT auto-havoc (unsound for liveness on read contents) — it
        // returns an honest Skip (Err), never a verdict.
        let formula =
            crate::mu_calculus::parser::parse("mu X. ((dout == 0) or <> X)").expect("formula");
        let r = exact_symbolic_verdict(DOUT_READS_IN_CONE_MEM, &formula);
        assert!(
            r.is_err(),
            "in-cone memory must Skip (Err), got a verdict: {r:?}"
        );
    }

    /// A real Yosys-emitted BTOR2 (lowRISC/ibex 32×32 register file). Every output
    /// (`rdata_a_o`/`rdata_b_o`) reads the `mem` array, so any property atom over a
    /// read output puts the memory IN the cone — the in-cone guard must decline to
    /// havoc and Skip (Err), NOT emit an unsound verdict, on genuine yosys `write`/
    /// `read`/`init` array emission (not just the hand-authored fixtures above).
    const IBEX_REGFILE_YOSYS_BTOR2: &str =
        include_str!("../../../tests/data/ibex_register_file_fpga_16x4.btor2");

    #[test]
    fn array_lift_real_yosys_regfile_read_output_skips() {
        let formula =
            crate::mu_calculus::parser::parse("mu X. ((rdata_a_o == 0) or <> X)").expect("formula");
        let r = exact_symbolic_verdict(IBEX_REGFILE_YOSYS_BTOR2, &formula);
        assert!(
            r.is_err(),
            "a property reading the register-file memory must Skip (Err), got: {r:?}"
        );
    }

    // Frozen-register regression (2026-07-05) — a register that LOADS a nonzero
    // value from a free input `ld` (`reg_next = ld ? 10 : reg`). Two shapes that
    // MUST decide identically:
    //   (A) the state line carries the symbol directly (`state 2 bit_cnt_q`);
    //   (B) the state line is UNNAMED and the user-visible name survives only on a
    //       `uext _ NID 0 NAME` alias (`state 2` + `uext 2 4 0 bit_cnt_q`) — the
    //       exact shape yosys emits after flatten + async2sync + dffunmap.
    // The bit-blaster names its cells via `collect_symbols` (alias-aware), so both
    // cells are named `bit_cnt_q`. The bug: `next_funcs` was keyed off the RAW
    // state symbol (`state_<nid>` for (B)), so `next_funcs.get(&cell.symbol)` missed
    // and (B)'s register was left with NO next-state function — silently FROZEN at
    // its init value. `EF(bit_cnt_q != 0)` then read `Violated` (a reachable state
    // reported unreachable) for (B) while (A) correctly read `Holds`. From any
    // state, asserting `ld` reaches `bit_cnt_q = 10`, so the sound verdict is
    // `Holds` for BOTH.
    const FROZEN_REG_ATOM_ON_STATE: &str = r#"
1 sort bitvec 1
2 sort bitvec 4
3 input 1 ld
4 state 2 bit_cnt_q
5 const 2 0000
6 const 2 1010
7 ite 2 3 6 4
8 next 2 4 7
"#;
    const FROZEN_REG_ATOM_ON_UEXT_ALIAS: &str = r#"
1 sort bitvec 1
2 sort bitvec 4
3 input 1 ld
4 state 2
5 const 2 0000
6 const 2 1010
7 ite 2 3 6 4
8 next 2 4 7
9 uext 2 4 0 bit_cnt_q
"#;

    #[test]
    fn exact_next_func_binds_to_uext_aliased_register_not_frozen() {
        // The regression: named-state and uext-aliased-state must decide the SAME.
        let ef = crate::mu_calculus::parser::parse("mu Y. ((not (bit_cnt_q == 0)) or <> Y)")
            .expect("EF formula");
        let named = exact_symbolic_verdict(FROZEN_REG_ATOM_ON_STATE, &ef).expect("verdict");
        let aliased = exact_symbolic_verdict(FROZEN_REG_ATOM_ON_UEXT_ALIAS, &ef).expect("verdict");
        assert_eq!(
            named,
            ExactVerdict::Holds,
            "EF(bit_cnt_q != 0) is reachable (assert `ld`), so a named state decides Holds"
        );
        assert_eq!(
            aliased, named,
            "a uext-ALIASED state must decide identically to a named one — not a frozen \
             register (Violated) from a `next_funcs` key that missed the alias resolution"
        );
        // The AF-liveness companion also agrees: with `ld` free, `ld=0` forever holds
        // the loaded value, so `AG AF(bit_cnt_q == 0)` is a real Violated on BOTH.
        let agaf = crate::mu_calculus::parser::parse(
            "nu X. ((mu Y. ((bit_cnt_q == 0) or [] Y)) and [] X)",
        )
        .expect("AGAF formula");
        assert_eq!(
            exact_symbolic_verdict(FROZEN_REG_ATOM_ON_UEXT_ALIAS, &agaf).expect("verdict"),
            exact_symbolic_verdict(FROZEN_REG_ATOM_ON_STATE, &agaf).expect("verdict"),
            "AG AF must also decide identically for named vs uext-aliased state"
        );
    }

    /// monono#partsel — the yosys-slang lift of a PARTIAL register assignment
    /// (`a_q[11:8] <= val`) as it actually reaches the engine. slang models only
    /// the *written* slice as a `state` cell (nid 19, 4-bit), turns the register's
    /// *unwritten* bits into ANONYMOUS free `input`s (nid 17, 8-bit; nid 21, 4-bit),
    /// and aliases the register name `a_q` (`uext … 0 a_q`) to the 16-bit `concat`
    /// that mixes them. A predicate over `a_q` therefore reads free inputs and the
    /// exact fixpoint used to answer a spurious `Holds` for `AG (a_q == 0)` (and
    /// `!o_partsel`) even though `a_q` is reachably non-zero — the exact soundness
    /// bug monono's planted-bug canary caught. `b_q`, written in full, lifts to one
    /// faithful state cell and decides correctly.
    ///
    /// This is the post-reset-init + reset-pinned BTOR2 (the shape verify-auto hands
    /// the exact engine), captured verbatim from the `mununu-sva` slang lift so the
    /// regression runs in `make ci` without slang. Expected: `a_q` REFUSED (Err) —
    /// no silent verdict — while `b_q` decides `Violated`.
    const SLANG_PARTSEL_PARTIAL_WRITE: &str = r#"1 sort bitvec 1
2 input 1 clk
3 one 1 rst_n
4 sort bitvec 4
5 input 4 val
6 sort bitvec 16
7 const 6 0000000000000000
8 state 6
9 ite 6 3 8 7
10 redor 1 9
11 output 10 o_concat
12 state 6
13 ite 6 3 12 7
14 redor 1 13
15 output 14 o_lowconcat
16 sort bitvec 8
17 input 16
18 const 4 0000
19 state 4
20 ite 4 3 19 18
21 input 4
22 sort bitvec 12
23 concat 22 20 17
24 concat 6 21 23
25 redor 1 24
26 output 25 o_partsel
27 state 6
28 ite 6 3 27 7
29 redor 1 28
30 output 29 o_plain
31 uext 6 24 0 a_q
32 uext 6 9 0 b_q
33 uext 6 13 0 c_q
34 uext 6 28 0 d_q
35 slice 16 9 7 0
36 concat 22 5 35
37 slice 4 9 15 12
38 concat 6 37 36
39 ite 6 3 38 7
40 next 6 8 39
41 const 22 000000000000
42 concat 6 41 5
43 ite 6 3 42 7
44 next 6 12 43
45 ite 4 3 5 18
46 next 4 19 45
47 ite 6 3 42 7
48 next 6 27 47
49 constd 6 0
50 init 6 8 49
51 constd 6 0
52 init 6 12 51
53 constd 4 0
54 init 4 19 53
55 constd 6 0
56 init 6 27 55
"#;

    #[test]
    fn exact_refuses_partsel_register_over_undriven_inputs() {
        let file = parser::parse(SLANG_PARTSEL_PARTIAL_WRITE).expect("parse");

        // The soundness guard's discriminator: `a_q`'s bound signal reaches an
        // anonymous (undriven-register-bit) input; `b_q` (a faithful state cell)
        // does not. The combinational output `o_partsel` (a function of `a_q`) is
        // caught too; `o_concat` (a function of the faithful `b_q`) is not.
        assert!(
            parser::signal_reaches_anonymous_input(&file, "a_q"),
            "a_q binds to a concat mixing undriven free inputs"
        );
        assert!(
            !parser::signal_reaches_anonymous_input(&file, "b_q"),
            "b_q is a faithful state cell — never flagged"
        );
        assert!(parser::signal_reaches_anonymous_input(&file, "o_partsel"));
        assert!(!parser::signal_reaches_anonymous_input(&file, "o_concat"));

        // The exact engine REFUSES `a_q` (Err) rather than emit the pre-fix spurious
        // `Holds`; it still decides the faithful `b_q` as `Violated` (b_q takes `val`
        // into its middle bits, so it is reachably non-zero).
        let ag_a = crate::mu_calculus::parser::parse("nu X. ((a_q == 0) and [] X)").unwrap();
        let ag_b = crate::mu_calculus::parser::parse("nu X. ((b_q == 0) and [] X)").unwrap();
        assert!(
            exact_symbolic_verdict(SLANG_PARTSEL_PARTIAL_WRITE, &ag_a).is_err(),
            "AG(a_q==0) must be REFUSED, not a silent Holds — the register's bits are \
             partly undriven free inputs in the slang lift"
        );
        assert_eq!(
            exact_symbolic_verdict(SLANG_PARTSEL_PARTIAL_WRITE, &ag_b),
            Ok(ExactVerdict::Violated),
            "AG(b_q==0) is a faithful register that reaches non-zero ⇒ Violated"
        );
    }

    /// monono#partsel COI fix — a PACKED 2-D partial write (`p[idx] <= d`) splits
    /// the register into ANONYMOUS `state` sub-cells with NO free inputs (`p`
    /// aliases a width-2 `concat` of two width-1 anonymous states, so the width
    /// filter drops the name off both). The OLD symbol-keyed cone-of-influence
    /// could not keep an un-named cell, so it pinned the sub-cells to their init 0
    /// → `p` frozen at 0 → a spurious `Holds` for `AG (p == 0)`. The NID-indexed
    /// COY ([`crate::adapter::btor2::dep_graph::cone_leaf_nids`]) now KEEPS the
    /// anonymous sub-cells, so `AG (p == 0)` DECIDES `Violated` (`p[0] <= d` makes
    /// it reachably non-zero) — no free input, so the guard does not refuse it.
    /// `q` (a faithful named register) also decides.
    #[test]
    fn exact_decides_packed_split_register_via_nid_cone() {
        // p = concat(anon-state-5, anon-state-6); alias `uext 4 7 0 p` is width 2,
        // states are width 1 → width filter drops `p` off them → both anonymous.
        // p[0] (state 5) <= d (a named input), p[1] (state 6) <= 0; both init 0.
        // `q` (state 14) is a faithful named 1-bit register that toggles.
        const PACKED_SPLIT: &str = "1 sort bitvec 1
2 one 1 rst_n
3 input 1 d
4 sort bitvec 2
5 state 1
6 state 1
7 concat 4 5 6
8 uext 4 7 0 p
9 zero 1
10 next 1 5 3
11 next 1 6 9
12 init 1 5 9
13 init 1 6 9
14 state 1 q
15 not 1 14
16 next 1 14 15
17 init 1 14 9
";
        let file = parser::parse(PACKED_SPLIT).expect("parse");
        // `p` reaches anonymous split sub-registers but NO free input — the
        // input-only guard does not fire, so the property is DECIDED, not refused.
        assert!(
            !parser::signal_reaches_anonymous_input(&file, "p"),
            "an anonymous-state split (no free input) is kept by the COI, not refused"
        );
        assert!(!parser::signal_reaches_anonymous_input(&file, "q"));
        // Exact DECIDES `AG(p==0)` = Violated: the NID cone keeps the anonymous
        // sub-cells, so `p[0] <= d` (d free) reaches a non-zero `p`.
        let ag_p = crate::mu_calculus::parser::parse("nu X. ((p == 0) and [] X)").unwrap();
        let ef_q = crate::mu_calculus::parser::parse("mu Y. ((q == 1) or <> Y)").unwrap();
        assert_eq!(
            exact_symbolic_verdict(PACKED_SPLIT, &ag_p),
            Ok(ExactVerdict::Violated),
            "AG(p==0) must now DECIDE Violated — the NID cone keeps the anonymous \
             split sub-cells instead of freezing them"
        );
        assert_eq!(
            exact_symbolic_verdict(PACKED_SPLIT, &ef_q),
            Ok(ExactVerdict::Holds),
            "EF(q==1) on the faithful named register also decides"
        );
    }

    /// A binary constant WIDER than 128 bits (out of the property cone) must not fail the exact
    /// MC — it is built per-bit from the string, not parsed to u128. `wide` is a 130-bit const
    /// register the FSM never reads; the property is over the 1-bit `fsm`. Regression for the
    /// keymgr_ctrl "bad binary literal" Skip (256-bit key-state constants).
    #[test]
    fn exact_verdict_tolerates_over_128_bit_binary_constant() {
        // 130-bit `wide` const held in a register out of the `fsm` cone; `fsm` toggles 0↔1.
        let bits130 = "1".to_string() + &"0".repeat(129); // 130-char MSB-first binary literal
        let btor2 = format!(
            "1 sort bitvec 1\n2 sort bitvec 130\n3 const 2 {bits130}\n4 state 1 fsm\n5 state 2 wide\n\
             6 not 1 4\n7 next 1 4 6\n8 next 2 5 3\n9 zero 1\n10 init 1 4 9\n11 init 2 5 3\n"
        );
        let formula = crate::mu_calculus::parser::parse("mu Y. ((fsm == 1) or <> Y)")
            .expect("EF formula parses");
        // Must DECIDE (not error on the 130-bit const): fsm reaches 1 ⇒ EF(fsm==1) holds.
        assert_eq!(
            exact_symbolic_verdict(&btor2, &formula),
            Ok(ExactVerdict::Holds),
            "a > 128-bit out-of-cone binary constant must not fail the exact MC"
        );
    }

    /// `exact_bad_reachable` — a reachable `bad` (a 3-bit counter reaches `0b111`) is SAT.
    /// The btor2tools `count2` shape: init 0, `next = s + 1`, `bad = (s == 7)`.
    #[test]
    fn exact_bad_reachable_true_on_reaching_counter() {
        const COUNT2: &str = r#"
1 sort bitvec 3
2 zero 1
3 state 1
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 ones 1
9 sort bitvec 1
10 eq 9 3 8
11 bad 10
"#;
        assert_eq!(
            exact_bad_reachable(COUNT2),
            Ok(true),
            "a 3-bit counter from 0 reaches 0b111 ⇒ bad is reachable (SAT)"
        );
    }

    /// `exact_bad_reachable` — an unreachable `bad` is UNSAT: a register held at 0 can never
    /// equal 1, so `bad = (s == 1)` is unreachable from the reset state.
    #[test]
    fn exact_bad_reachable_false_on_stuck_register() {
        const STUCK: &str = r#"
1 sort bitvec 1
2 zero 1
3 state 1 s
4 init 1 3 2
5 next 1 3 3
6 one 1
7 eq 1 3 6
8 bad 7
"#;
        assert_eq!(
            exact_bad_reachable(STUCK),
            Ok(false),
            "s is held at 0 ⇒ (s == 1) is never reachable (UNSAT / safe)"
        );
    }

    /// `exact_bad_reachable` — the soundness guard: a design with a `constraint` line is
    /// UNDECIDED (Err), never an over-approximate verdict (this engine does not restrict
    /// reachability to constraint-satisfying runs).
    /// `exact_bad_reachable` — a `constraint` over a free input does not block an
    /// already-unreachable `bad`: `s` held at 0, `bad = (s == 1)` is unreachable
    /// regardless, so the modelled-constraint verdict is UNSAT (safe), not a refusal.
    #[test]
    fn exact_bad_reachable_models_constraint_unreachable() {
        const CONSTRAINED: &str = r#"
1 sort bitvec 1
2 zero 1
3 state 1 s
4 init 1 3 2
5 next 1 3 3
6 one 1
7 eq 1 3 6
8 bad 7
9 input 1 c
10 constraint 9
"#;
        assert_eq!(
            exact_bad_reachable(CONSTRAINED),
            Ok(false),
            "constraint over a free input; bad=(s==1) is unreachable ⇒ safe (decided, not refused)"
        );
    }

    /// `exact_bad_reachable` — a `constraint` that FORBIDS the bad state blocks it:
    /// a 3-bit counter reaches `0b111`, but `constraint (s != 7)` makes every
    /// constraint-respecting run avoid `s == 7`, so `bad = (s == 7)` is unreachable
    /// under the assumption (UNSAT). Without constraint modelling this would be SAT.
    #[test]
    fn exact_bad_reachable_constraint_blocks_bad() {
        const BLOCKED: &str = r#"
1 sort bitvec 3
2 zero 1
3 state 1
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 ones 1
9 sort bitvec 1
10 eq 9 3 8
11 bad 10
12 neq 9 3 8
13 constraint 12
"#;
        assert_eq!(
            exact_bad_reachable(BLOCKED),
            Ok(false),
            "constraint (s != 7) forbids the bad state ⇒ bad unreachable under the assumption"
        );
    }

    /// `exact_bad_reachable` — a satisfiable `constraint` over a free input does NOT
    /// block a reachable `bad`: the input can be chosen to satisfy the constraint at
    /// every step, so the 3-bit counter still reaches `0b111` (SAT).
    #[test]
    fn exact_bad_reachable_constraint_satisfiable_still_reachable() {
        const SAT: &str = r#"
1 sort bitvec 3
2 zero 1
3 state 1
4 init 1 3 2
5 one 1
6 add 1 3 5
7 next 1 3 6
8 ones 1
9 sort bitvec 1
10 eq 9 3 8
11 bad 10
12 input 9 c
13 constraint 12
"#;
        assert_eq!(
            exact_bad_reachable(SAT),
            Ok(true),
            "constraint (free input c) is satisfiable with c=1 every step ⇒ bad still reachable"
        );
    }

    /// mununu#498 (sibling) — a design with UNINITIALISED (free-init) state is now
    /// DECIDED, and decided the way `btormc` decides it.
    ///
    /// `s` is held (`next = s`) with no `init` line, so the BTOR2 format leaves its
    /// initial value free and `s = 1` at cycle 0 is a genuine initial state: `bad`
    /// IS reachable. The engine used to pin `s` to 0, find `bad` unreachable, and
    /// then have to ABSTAIN to avoid emitting an unsound "safe" (this test formerly
    /// asserted the abstention). Modelling the initial set exactly removes both the
    /// wrong answer and the need to refuse.
    #[test]
    fn exact_bad_reachable_decides_free_init_design_like_btormc() {
        const FREE_INIT: &str = r#"
1 sort bitvec 1
2 state 1 s
3 next 1 2 2
4 one 1
5 eq 1 2 4
6 bad 5
"#;
        assert_eq!(
            exact_bad_reachable(FREE_INIT),
            Ok(true),
            "`s` is free at cycle 0, so `bad = (s == 1)` is reachable — the verdict btormc \
             gives, and one the pin-to-zero model could not reach"
        );
    }

    /// mununu#498 (sibling), against the third-party oracle case. `btor2tools`'
    /// `noninitstate.btor2` exists precisely to test free-init semantics: `state0`
    /// and `state1` carry no `init`, and a `constraint` forces them DIFFERENT in the
    /// first step, so `bad = (state0 == state1)` is reachable — `btormc` returns
    /// `sat`.
    ///
    /// Pinning both to 0 made them EQUAL at step 0, which contradicts that
    /// constraint; the exact engine could not see the real initial states and had to
    /// abstain, leaving `btormc` / Pono / SPACER to carry the portfolio. It now
    /// decides, and agrees.
    #[test]
    fn exact_decides_btor2tools_noninitstate_like_btormc() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/btor2/btor2tools_suite/noninitstate.btor2"
        );
        let content = std::fs::read_to_string(path).expect("read noninitstate.btor2");
        assert_eq!(
            exact_bad_reachable(&content),
            Ok(true),
            "btormc reports `sat` on this design; the exact engine must now agree rather \
             than abstain — a differential disagreement here is the mununu#498 sibling \
             under-approximation returning"
        );
    }

    /// D1.7 — the exact engine DEGRADES GRACEFULLY on a design too large to
    /// bit-blast. `BddBitBlaster::build` rejects a design whose register+input
    /// bit count exceeds the effective cap with a clean `Err` — and does so
    /// BEFORE allocating the BDD manager, so there is no OoM / hang. `build` is the
    /// first thing `exact_symbolic_verdict` calls, so the whole exact path returns
    /// `Err` (which `verify_auto`'s exact branch maps to a `Skipped` property, per
    /// `e2e_sysrst`-style graceful degradation). This locks that OoM-safety
    /// guarantee for the exact path as a fast, non-docker regression. The fixture is
    /// 300-bit — over the [`AUTO_CAP_CEILING`] (192) the no-env auto-cap will admit,
    /// so it still degrades. (The 41–192-bit admit path is covered by
    /// `auto_cap_admits_modestly_wide_concrete_cone`.)
    #[test]
    fn d1_7_exact_verdict_over_bit_cap_degrades_gracefully() {
        // One 300-bit register (300 > AUTO_CAP_CEILING = 192), no inputs. The formula is
        // immaterial: the cap fires in `build`, before atom resolution or fixpoint
        // evaluation, so a caller degrades to Skipped rather than OoM.
        const WIDE_BTOR2: &str = r#"
1 sort bitvec 300
2 sort bitvec 1
3 state 1 wide
4 one 1
5 add 1 3 4
6 next 1 3 5
"#;
        let formula = crate::mu_calculus::parser::parse("(wide == 0)").expect("formula parses");
        let err = exact_symbolic_verdict(WIDE_BTOR2, &formula)
            .expect_err("300-bit design exceeds the auto-cap ceiling (192) → clean Err");
        assert!(
            err.contains("register+input bits") && err.contains("300"),
            "the error must name the bit count + cap so a caller can degrade to \
             Skipped; got: {err}"
        );
        // #467 — the abstention names WHICH budget was hit and the KNOB to raise, so
        // a consumer acts without blind trial-and-error across the three budgets.
        assert!(
            err.contains("BIT CAP") && err.contains("MUNUNU_BDD_MAX_BITS"),
            "the abstention must name the budget (BIT CAP) + its knob \
             (MUNUNU_BDD_MAX_BITS); got: {err}"
        );
    }

    /// Auto-cap-management (verification-execution-planner Phase 4.6) — with NO explicit
    /// `MUNUNU_BDD_MAX_BITS`, a concrete cone that modestly exceeds the conservative 40-bit
    /// default is admitted up to [`AUTO_CAP_CEILING`] (192), so exact decides the CONCRETE model
    /// directly instead of Skipping. Locks (a) the pure `default_cap_for_cone` thresholds and
    /// (b) that a 48-bit concrete cone — which the old 40-bit default rejected (see `d1_7`,
    /// which used exactly this width before the ceiling shipped) — now DECIDES. No env mutation:
    /// the positive case relies on the default (env-unset) path; if `MUNUNU_BDD_MAX_BITS` is set
    /// in the environment the assertion is skipped rather than falsely failing.
    #[test]
    fn auto_cap_admits_modestly_wide_concrete_cone() {
        // Pure threshold math (no env, no BDD): the default floor is never lowered; a 41–192-bit
        // cone raises the cap to fit; a > 192-bit cone is clamped to the ceiling (⇒ Err upstream).
        assert_eq!(default_cap_for_cone(8), 40, "≤ floor ⇒ the 40-bit floor");
        assert_eq!(default_cap_for_cone(40), 40, "exactly the floor");
        assert_eq!(
            default_cap_for_cone(48),
            48,
            "41–192 ⇒ raised to fit the cone"
        );
        assert_eq!(
            default_cap_for_cone(173),
            173,
            "i2c's control cone ⇒ admitted (P2.5-A)"
        );
        assert_eq!(default_cap_for_cone(192), 192, "exactly the ceiling");
        assert_eq!(
            default_cap_for_cone(300),
            192,
            "> ceiling ⇒ clamped (build then Errs). NB the cap is NOT the tractability gate — a \
             deep counter (twocount32, 65-bit cone < i2c's 173) is admitted and bounded by the \
             wall-clock deadline, not the cap; see AUTO_CAP_CEILING + ExactModel::deadline",
        );

        // End-to-end: a 48-bit counter cone the old default Skipped now bit-blasts and decides.
        // `wide` inits to 0 and increments, so `(wide == 0)` holds in the initial state — a
        // definite verdict only reachable if the cone was admitted (not Skipped).
        if std::env::var_os("MUNUNU_BDD_MAX_BITS").is_some() {
            return; // an explicit cap overrides the auto-raise; don't assert against it
        }
        const CONE48: &str = r#"
1 sort bitvec 48
2 sort bitvec 1
3 state 1 wide
4 zero 1
5 init 1 3 4
6 one 1
7 add 1 3 6
8 next 1 3 7
"#;
        let formula = crate::mu_calculus::parser::parse("(wide == 0)").expect("formula parses");
        assert_eq!(
            exact_symbolic_verdict(CONE48, &formula),
            Ok(ExactVerdict::Holds),
            "a 48-bit concrete cone (> the 40-bit floor, ≤ the 64-bit ceiling) must be \
             admitted by the auto-cap and decided, not Skipped"
        );
    }

    /// Safety machinery (node-budget guard + `catch_unwind`) — at the DEFAULT cap, a 40-bit
    /// multiplier (`p' = x * y`, 20-bit inputs) has an exponential BDD that overflows the 2M
    /// arena during `eval_op`. The engine must degrade to a clean `Err` (→ Skipped), NEVER
    /// OoM-panic. Asserts only that a `Result` comes back — the value (Err on overflow, or Ok
    /// if it happens to fit) is immaterial; the point is no panic escapes. No env mutation.
    /// mununu#498 — a BTOR2 `state` with NO `next` line is a FREE variable at
    /// every step (the format's free variable; Yosys emits exactly this for
    /// `$anyseq`, which is what `cutpoint` leaves behind). It must NOT be modelled
    /// as a held register: doing so pins it to its absent-therefore-zero init and
    /// freezes it, turning a cut point's documented OVER-approximation into an
    /// UNDER-approximation.
    ///
    /// FSM: `st` leaves 0 only when the freed net `f` is 1.
    ///   - `f` free  ⇒ `EF (st == 1)` HOLDS and `AG (st == 0)` VIOLATED.
    ///   - `f` frozen at 0 (the bug) ⇒ exactly the opposite, both wrong.
    ///
    /// Both directions are asserted: the safety direction alone would pass an
    /// implementation that special-cased `AG` (mununu#498 discussion).
    const ANYSEQ_GATE: &str = r#"
1 sort bitvec 1
2 state 1 f
3 state 1 st
4 zero 1
5 one 1
6 ite 1 2 5 3
7 next 1 3 6
8 init 1 3 4
"#;

    #[test]
    fn state_without_next_is_free_each_step_not_a_held_register() {
        // `f` has no `next` line ⇒ free. `st` starts 0 and becomes 1 whenever f=1.
        let reach = crate::mu_calculus::parser::parse("mu Z.((st == 1) || <> Z)").expect("parse");
        assert_eq!(
            exact_symbolic_verdict(ANYSEQ_GATE, &reach).expect("verdict"),
            ExactVerdict::Holds,
            "a state with no `next` is FREE, so the gated transition is reachable; \
             modelling it as a register frozen at its zero init makes this VIOLATED \
             — the mununu#498 under-approximation"
        );
    }

    #[test]
    fn state_without_next_does_not_freeze_a_safety_property_into_holding() {
        // The same bug seen from the safety side: with `f` frozen at 0 the FSM never
        // leaves 0 and `AG (st == 0)` wrongly HOLDS.
        let ag = crate::mu_calculus::parser::parse("nu Y.((st == 0) && [] Y)").expect("parse");
        assert_eq!(
            exact_symbolic_verdict(ANYSEQ_GATE, &ag).expect("verdict"),
            ExactVerdict::Violated,
            "`st` can leave 0 because the no-`next` state is free; a HOLDS here is the \
             mununu#498 unsound direction — a safety property passing on a model that \
             cannot move"
        );
    }

    #[test]
    fn anyconst_shaped_state_with_self_next_stays_classified_as_a_register() {
        // `$anyconst` is a DIFFERENT cell from `$anyseq` and must NOT be swept up by
        // the mununu#498 fix: Yosys emits it as `next(k) = k`, so it HAS a next line
        // and stays a state (held). Guards against the fix over-reaching into "every
        // uninitialised state is free".
        //
        // Asserted at the CLASSIFICATION seam rather than through a verdict, on
        // purpose: a verdict here would also depend on the SEPARATE `no init ⇒ pinned
        // to zero` rule, which makes an `$anyconst` a constant ZERO rather than an
        // arbitrary constant. That is its own soundness question with its own blast
        // radius (RTL registers are legitimately pinned to their reset value), it is
        // NOT fixed here, and entangling it would make this test assert something the
        // fix does not claim.
        const ANYCONST_GATE: &str = r#"
1 sort bitvec 1
2 state 1 k
3 next 1 2 2
4 state 1 st
5 zero 1
6 one 1
7 ite 1 2 6 4
8 next 1 4 7
9 init 1 4 5
"#;
        let file = crate::adapter::btor2::parser::parse(ANYCONST_GATE).expect("parse");
        let sts = crate::adapter::sts_ir::BtorSts::new(&file);
        let cells = sts.leaf_cells().expect("leaf cells");
        let by_name = |n: &str| cells.iter().find(|c| c.name == n).expect("cell").is_state;
        assert!(
            by_name("k"),
            "an `$anyconst`-shaped state (next = itself) stays a REGISTER"
        );
        assert!(by_name("st"), "a normal register stays a register");
    }

    #[test]
    fn anyseq_shaped_state_without_next_is_classified_as_an_input() {
        // The other side of the same seam: `f` has no `next`, so it must classify as
        // an INPUT (free each step, quantifiable) rather than a register.
        let file = crate::adapter::btor2::parser::parse(ANYSEQ_GATE).expect("parse");
        let sts = crate::adapter::sts_ir::BtorSts::new(&file);
        let cells = sts.leaf_cells().expect("leaf cells");
        let by_name = |n: &str| cells.iter().find(|c| c.name == n).expect("cell").is_state;
        assert!(
            !by_name("f"),
            "a state with no `next` is a FREE variable and must classify as an input"
        );
        assert!(by_name("st"), "the real register is unaffected");
        // And the seam's two views agree with the classification.
        use crate::adapter::sts_ir::SymbolicTransitionSystem;
        let inputs: Vec<String> = sts.input_vars().into_iter().map(|v| v.name).collect();
        let states: Vec<String> = sts.state_vars().into_iter().map(|v| v.name).collect();
        assert!(
            inputs.contains(&"f".to_string()),
            "f in input_vars: {inputs:?}"
        );
        assert!(
            states.contains(&"st".to_string()),
            "st in state_vars: {states:?}"
        );
        assert!(
            !states.contains(&"f".to_string()),
            "f must NOT be in state_vars"
        );
    }

    #[test]
    fn wide_multiplier_bails_gracefully_never_panics() {
        const MUL40: &str = r#"
1 sort bitvec 20
2 sort bitvec 40
3 input 1 x
4 input 1 y
5 uext 2 3 20
6 uext 2 4 20
7 mul 2 5 6
8 state 2 p
9 next 2 8 7
10 zero 2
11 init 2 8 10
"#;
        let formula = crate::mu_calculus::parser::parse("(p == 0)").expect("formula");
        // Must return a Result (Ok or Err) without panicking — `catch_unwind` converts any
        // arena overflow into an Err. A panic here fails the test, which is the guarantee.
        let _ = exact_symbolic_verdict(MUL40, &formula);
    }

    /// Regression for the `sv check-fsm` SIGABRT on wide RTL (uart/i2c_slave, 2026-07-30): the
    /// FSM-scan reachability member (`fsm_encoding_scan` → `decide_reach_portfolio` →
    /// [`exact_bad_reachable`]) bit-blasts the design's transition relation, and a wide op could
    /// exhaust the BDD arena in a SINGLE apply — before the between-op node-budget guard. OxiDD then
    /// returned `OutOfMemory`, the `.unwrap()` PANICKED, and unwinding dropped the exhausted manager,
    /// which ABORTED the process (SIGABRT) — uncatchable by `catch_unwind`, so the whole scan
    /// crashed. The fix (per-bit budget guard + `Err`-propagation in the wide arms, start/end op
    /// guards, and arena headroom above the budget) keeps the arena from ever actually filling, so
    /// this MUST return a Result (Ok/Err) without panicking or aborting. A panic/abort fails the test.
    #[test]
    fn exact_bad_reachable_wide_design_bails_gracefully_never_aborts() {
        const WIDE_MUL_BAD: &str = r#"
1 sort bitvec 20
2 sort bitvec 40
3 sort bitvec 1
4 input 1 x
5 input 1 y
6 uext 2 4 20
7 uext 2 5 20
8 mul 2 6 7
9 state 2 p
10 next 2 9 8
11 zero 2
12 init 2 9 11
13 ones 2
14 eq 3 9 13
15 bad 14
"#;
        let _ = exact_bad_reachable(WIDE_MUL_BAD);
    }

    /// R-F5.6 — cone-of-influence restriction lifts the bit cap when the property's cone is
    /// small even though the FULL design is over the cap. `fsm` (2-bit) cycles 1→2→3→0→…; `wide`
    /// (300-bit) is an out-of-cone counter `fsm` never reads. The full build hits the cap (302 >
    /// the 192-bit auto-cap ceiling), but `exact_symbolic_verdict` restricts to the cone of
    /// `fsm == 0` = {fsm} (pinning `wide` to a constant) and DECIDES `EF (fsm == 0)` = Holds.
    /// `wide` is 300-bit so the full design exceeds the auto-cap ceiling — COI, not the
    /// auto-raise, is what makes this decidable. Locks that (a) COI is wired into the exact path,
    /// and (b) pinning the out-of-cone datapath is verdict-preserving — the whole point of
    /// R-F5.6, as a fast non-docker regression.
    #[test]
    fn rf5_6_coi_lifts_bit_cap_on_out_of_cone_datapath() {
        const FSM_PLUS_WIDE_CTR: &str = r#"
1 sort bitvec 2
2 sort bitvec 300
3 state 1 fsm
4 one 1
5 add 1 3 4
6 next 1 3 5
7 zero 1
8 init 1 3 4
9 state 2 wide
10 one 2
11 add 2 9 10
12 next 2 9 11
"#;
        // The full design is 2 + 300 = 302 bits — over the 192-bit auto-cap ceiling.
        let file = parser::parse(FSM_PLUS_WIDE_CTR).expect("parse");
        let full_err = BddBitBlaster::build(&file).err();
        assert!(
            full_err.as_ref().is_some_and(|e| e.contains("302")),
            "full 302-bit build must still hit the cap; got {full_err:?}",
        );
        // The exact verdict restricts to the {fsm} cone (2 bits), pinning `wide` → decidable.
        // `fsm` inits to 1 and cycles to 0, so `EF (fsm == 0)` holds from the initial state.
        let formula =
            crate::mu_calculus::parser::parse("mu Y. ((fsm == 0) or <> Y)").expect("formula");
        let verdict = exact_symbolic_verdict(FSM_PLUS_WIDE_CTR, &formula)
            .expect("COI restricts to the 2-bit fsm cone ⇒ decidable, not capped");
        assert_eq!(
            verdict,
            ExactVerdict::Holds,
            "fsm cycles to 0 ⇒ EF(fsm==0) holds; the 45-bit out-of-cone `wide` is pinned",
        );
    }

    /// The bit-blasted `Mul` (shift-and-add) over a SYMBOLIC operand (not a constant fold):
    /// `p' = x * 3` on a 4-bit input `x`. `EF (p == 6)` holds (x=2 ⇒ 6 mod 16), so the
    /// multiplier is exercised end-to-end. Locks the `Op::Mul` arm that unblocks the
    /// prim_packer_fifo / prim_fifo_sync drainability properties in the corpus.
    #[test]
    fn mul_bit_blaster_shift_and_add() {
        const MUL_BTOR2: &str = r#"
1 sort bitvec 4
2 input 1 x
3 const 1 0011
4 mul 1 2 3
5 state 1 p
6 next 1 5 4
7 const 1 0000
8 init 1 5 7
"#;
        let formula =
            crate::mu_calculus::parser::parse("mu Y. ((p == 6) or <> Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(MUL_BTOR2, &formula).expect("mul decides"),
            ExactVerdict::Holds,
            "p' = x*3; x=2 ⇒ p==6 reachable ⇒ EF(p==6) holds",
        );
    }

    /// The bit-blasted variable `Srl` barrel shifter: `p' = 12 >> s` over a 4-bit shift input
    /// `s`. `12 >> s ∈ {12,6,3,1,0}`, so `EF(p==3)` holds (s=2) but `EF(p==5)` is VIOLATED (5 is
    /// never reachable) — a precise check of the shift amount/direction/fill. Unblocks the
    /// prim_packer_fifo / prim_fifo_sync drainability cones (which use `Srl`).
    #[test]
    fn srl_bit_blaster_barrel_shift() {
        const SRL_BTOR2: &str = r#"
1 sort bitvec 4
2 const 1 1100
3 input 1 s
4 srl 1 2 3
5 state 1 p
6 next 1 5 4
7 const 1 0000
8 init 1 5 7
"#;
        let reachable =
            crate::mu_calculus::parser::parse("mu Y. ((p == 3) or <> Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(SRL_BTOR2, &reachable).expect("srl decides"),
            ExactVerdict::Holds,
            "12 >> 2 == 3 ⇒ EF(p==3) holds",
        );
        let unreachable =
            crate::mu_calculus::parser::parse("mu Y. ((p == 5) or <> Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(SRL_BTOR2, &unreachable).expect("srl decides"),
            ExactVerdict::Violated,
            "12 >> s is never 5 ⇒ EF(p==5) is violated (validates exact shift semantics)",
        );
    }

    /// The bit-blasted signed `Slt`: `p' = (x <s 0)` over a 4-bit input `x`. `EF(p==1)` holds
    /// because x ∈ {8..15} are NEGATIVE two's-complement (−8..−1) — a verdict an UNSIGNED
    /// comparison would get wrong (x <u 0 is never true ⇒ it would report Violated). Unblocks
    /// prim_fifo_sync (its drainability cone uses `Slt`).
    #[test]
    fn slt_bit_blaster_signed_less_than() {
        const SLT_BTOR2: &str = r#"
1 sort bitvec 1
2 sort bitvec 4
3 input 2 x
4 const 2 0000
5 slt 1 3 4
6 state 1 p
7 next 1 6 5
8 const 1 0
9 init 1 6 8
"#;
        let formula =
            crate::mu_calculus::parser::parse("mu Y. ((p == 1) or <> Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(SLT_BTOR2, &formula).expect("slt decides"),
            ExactVerdict::Holds,
            "x in 8..15 are signed-negative so x <s 0 and EF(p==1) holds (unsigned would fail)",
        );
    }

    /// A predicate binds to a named COMBINATIONAL signal, not just a state register: `o = p + 1`
    /// is a module OUTPUT (no `next` line), and `EF(o == 3)` holds (p reaches 2 ⇒ o = 3). Locks
    /// the `named_signals` binding that unblocks corpus atoms over combinational outputs
    /// (`depth_o`, `gnt_o`) which are not post-synthesis registers.
    #[test]
    fn predicate_binds_combinational_output() {
        const COMB_BTOR2: &str = r#"
1 sort bitvec 4
2 state 1 p
3 one 1
4 add 1 2 3
5 output 4 o
6 next 1 2 4
7 const 1 0000
8 init 1 2 7
"#;
        let formula =
            crate::mu_calculus::parser::parse("mu Y. ((o == 3) or <> Y)").expect("formula");
        assert_eq!(
            exact_symbolic_verdict(COMB_BTOR2, &formula).expect("combinational atom binds"),
            ExactVerdict::Holds,
            "o = p+1 is a combinational output; p reaches 2 ⇒ o==3 ⇒ EF(o==3) holds",
        );
    }

    /// Regression (2026-07-20): a 1-bit combinational-FUNCTION output atom must bind to the
    /// 1-bit signal, NOT the wider driving register. `done = (st == 2)` over a 2-bit `st`
    /// cycling 0→1→2→0 — `done` is a strict one-cycle pulse. The Loose "nearest state in the
    /// cone" resolver rewrote `done` → `st`, so `done == K` was evaluated as `st == K` (2-bit):
    /// `EF (done == 2)` spuriously `Holds` and `AG (done → AX (done == 0))` spuriously
    /// `Violated`, with `done == 0` (`st == 0`) disagreeing with `!(done == 1)` (`st != 1`).
    /// The Strict alias resolver returns `None` for the `==` combinational op, so the atom
    /// binds to the 1-bit `done` (`named_signals`) — the sound verdict.
    #[test]
    fn combinational_output_atom_binds_at_its_own_width() {
        const PULSE_FSM: &str = r#"
1 sort bitvec 1
2 sort bitvec 2
3 state 2 st
4 const 2 10
5 const 2 00
6 one 2
7 add 2 3 6
8 eq 1 3 4
9 ite 2 8 5 7
10 next 2 3 9
11 output 8 done
12 init 2 3 5
"#;
        let parse = |s: &str| crate::mu_calculus::parser::parse(s).expect("formula");
        // `done` is 1-bit: it is NEVER 2 (that value belongs to `st`, the driving register).
        assert_eq!(
            exact_symbolic_verdict(PULSE_FSM, &parse("mu Y. ((done == 2) or <> Y)")).unwrap(),
            ExactVerdict::Violated,
            "1-bit combinational `done`: EF(done==2) must be Violated (was Holds when `done` \
             misresolved to the 2-bit `st`)",
        );
        // `done` ∈ {0,1}: never "neither 0 nor 1".
        assert_eq!(
            exact_symbolic_verdict(
                PULSE_FSM,
                &parse("mu Y. ((not (done == 0) and not (done == 1)) or <> Y)"),
            )
            .unwrap(),
            ExactVerdict::Violated,
            "a 1-bit `done` is always 0 or 1",
        );
        // `done` is a strict one-cycle pulse ⇒ AG(done → AX(done==0)) Holds; and now that
        // `done` binds at 1 bit, the `done==0` and `!(done==1)` encodings agree.
        let pulse_eq0 = "nu Y. ((not (done == 1) or [] (done == 0)) and [] Y)";
        let pulse_ne1 = "nu Y. ((not (done == 1) or [] (not (done == 1))) and [] Y)";
        assert_eq!(
            exact_symbolic_verdict(PULSE_FSM, &parse(pulse_eq0)).unwrap(),
            ExactVerdict::Holds,
            "done pulses one cycle ⇒ AG(done→AX(done==0)) Holds (was spurious Violated)",
        );
        assert_eq!(
            exact_symbolic_verdict(PULSE_FSM, &parse(pulse_ne1)).unwrap(),
            exact_symbolic_verdict(PULSE_FSM, &parse(pulse_eq0)).unwrap(),
            "`done==0` and `!(done==1)` must agree for a 1-bit combinational output",
        );
    }

    /// Soundness guard: an atom that PINS a primary input is REFUSED (`Err`), never
    /// decided. The exact-symbolic engine leaves inputs free/quantified, so a temporal
    /// property with an input in an atom (the SVA `input |=> next-state` shape) decouples
    /// the antecedent input from the transition input and would report an unsound verdict.
    /// Contrast `predicate_binds_combinational_output`: an atom over a combinational
    /// OUTPUT (derived from state) is fine — only *primary inputs* are refused.
    #[test]
    fn exact_symbolic_refuses_primary_input_atom() {
        // `q' = clr` (a 1-bit latch of the primary input `clr`); the formula's atom pins
        // the input `clr` directly.
        const INPUT_ATOM_BTOR2: &str = r#"
1 sort bitvec 1
2 input 1 clr
3 state 1 q
4 const 1 0
5 init 1 3 4
6 next 1 3 2
"#;
        let formula = crate::mu_calculus::parser::parse("mu Y. (clr or <> Y)").expect("formula");
        let err = exact_symbolic_verdict(INPUT_ATOM_BTOR2, &formula)
            .expect_err("an atom pinning a primary input must be refused, not decided");
        assert!(
            err.contains("primary input") && err.contains("clr"),
            "diagnostic should name the offending input `clr`: {err}"
        );
    }

    /// mununu#476 — a NAMED combinational signal whose cone reaches primary inputs
    /// (without crossing a register boundary) is refused **when the formula does not
    /// match the `|=>` shape** (bare `EF`, `AF`, etc.). This preserves soundness for
    /// arbitrary user-authored mu-calc formulas whose semantics are not the SVA `|=>`
    /// obligation. When the formula DOES match `|=>`, shadow-synthesis takes over
    /// and the property decides (see
    /// [`shadow_synth_flips_derived_input_antecedent_to_decided`]).
    ///
    /// Fixture mirrors monono's `wb_mem_client`: `mem_rvalid_mine = mem_rvalid && got`
    /// where both `mem_rvalid` and `got` are primary inputs.
    ///
    /// Contrast [`exact_symbolic_refuses_primary_input_atom`] (name IS an input) and
    /// [`predicate_binds_combinational_output`] (combinational of STATE — admitted).
    #[test]
    fn exact_symbolic_refuses_derived_input_atom() {
        // `mem_rvalid_mine = mem_rvalid && got` — a named combinational Output whose
        // cone reaches two primary inputs, no state. `q` is a spare latch so the model
        // has state; the formula never reads it. Uses the `Node::Output` form (matching
        // `predicate_binds_combinational_output` above) rather than an Op inline symbol,
        // because Op-inline symbols are consumed as state aliases by `collect_symbols`.
        const DERIVED_INPUT_BTOR2: &str = r#"
1 sort bitvec 1
2 input 1 mem_rvalid
3 input 1 got
4 state 1 q
5 const 1 0
6 init 1 4 5
7 and 1 2 3
8 output 7 mem_rvalid_mine
9 next 1 4 2
"#;
        let formula =
            crate::mu_calculus::parser::parse("mu Y. (mem_rvalid_mine or <> Y)").expect("formula");
        let err = exact_symbolic_verdict(DERIVED_INPUT_BTOR2, &formula).expect_err(
            "an atom over a combinational signal reaching primary inputs must be refused, \
             not decided",
        );
        assert!(
            err.contains("combinationally driven by primary input"),
            "diagnostic should name the transitive-fan-in case: {err}"
        );
        assert!(
            err.contains("mem_rvalid_mine"),
            "diagnostic should name the offending atom `mem_rvalid_mine`: {err}"
        );
        assert!(
            err.contains("mem_rvalid") && err.contains("got"),
            "diagnostic should list every source primary input (mem_rvalid, got): {err}"
        );
    }

    /// mununu#476 — Option C. The same monono `wb_mem_client` shape (an atom
    /// combinationally driven by primary inputs) DECIDES when the surrounding
    /// formula matches the canonical SVA `|=>` lift shape
    /// `nu X. ((¬A ∨ □B) ∧ □X)`: the engine detects the shape, synthesises
    /// a shadow state cell that samples `A` per cycle, rewrites the atom to
    /// the shadow, and evaluates on the augmented model. This is the fix that
    /// closes the coverage gap the Phase A refusal left open.
    ///
    /// The BTOR2 sets `q' = 0` unconditionally, so `[] (q == 0)` always Holds
    /// and the whole property Holds after shadow-synth. Pre-Option C this
    /// property would have been Skipped (via the Phase A refusal on
    /// `mem_rvalid_mine`); post-Option C it Holds.
    ///
    /// Contrast [`exact_symbolic_refuses_derived_input_atom`] (same atom, but a
    /// bare `mu Y. (a or <> Y)` — no `|=>` shape → refusal path still fires).
    #[test]
    fn shadow_synth_flips_derived_input_antecedent_to_decided() {
        // Same combinational-of-inputs shape as monono's `wb_mem_client`, plus
        // a spare state `q` whose next is const 0 (so `q == 0` always holds).
        const BTOR2: &str = r#"
1 sort bitvec 1
2 input 1 mem_rvalid
3 input 1 got
4 state 1 q
5 const 1 0
6 init 1 4 5
7 and 1 2 3
8 output 7 mem_rvalid_mine
9 next 1 4 5
"#;
        // Canonical SVA `mem_rvalid_mine |=> (q == 0)` lift shape.
        let formula = crate::mu_calculus::parser::parse(
            "nu X. ((not mem_rvalid_mine or [] (q == 0)) and [] X)",
        )
        .expect("formula");
        let verdict = exact_symbolic_verdict(BTOR2, &formula)
            .expect("shadow-synth on an SVA `|=>` shape should decide, not refuse");
        assert_eq!(
            verdict,
            ExactVerdict::Holds,
            "with shadow-synth, `mem_rvalid_mine |=> (q == 0)` where q'=0 always Holds",
        );

        // Opt-out via the thread-safe options channel (`--no-antecedent-shadow`
        // CLI / `no_antecedent_shadow: true` API — mununu#476 item 4). Restores
        // the Phase A refusal even for `|=>` shapes. Env-var opt-out
        // (`MUNUNU_NO_ANTECEDENT_SHADOW=1`) is the equivalent process-global
        // channel; either channel disabling shadow-synth wins.
        let err = exact_symbolic_verdict_with_options(
            BTOR2,
            &formula,
            &ExactSymbolicOptions {
                antecedent_shadow_enabled: false,
            },
        )
        .expect_err("opt-out must revert to the Phase A transitive-fan-in refusal");
        assert!(
            err.contains("combinationally driven by primary input"),
            "opt-out refusal should still be the Phase A message: {err}"
        );
    }

    /// mununu#475 follow-up (item 3 of the shadow-synth follow-up list) —
    /// multi-atom `|=>` antecedent. `(valid_and_ready && valid_and_ack) |=> (q == 0)`
    /// where both atoms are COMBINATIONAL-OF-INPUTS (the typical monono `wb_mem_client`
    /// shape multiplied by two) and `q` is a spare state pinned to 0. The detector
    /// now walks into the `And` under the antecedent-side `Not` and returns BOTH
    /// atoms; shadow-synth synthesises TWO shadow registers
    /// (`_mununu_antshadow_0`, `_mununu_antshadow_1`), one per atom; the rewritten
    /// formula uses both shadows and the property decides. Pre-follow-up this
    /// would have Skipped with the transitive refusal (multi-atom antecedent was
    /// out-of-scope).
    ///
    /// **Bare-primary-input atoms in a multi-atom antecedent are still refused**
    /// (per `RefusalReason::IsPrimaryInput`) — the author-confirmation case
    /// remains a safety measure even at the leaf level; the caller can rewrite
    /// `valid |=> C` as `valid_gated = valid` + `valid_gated |=> C` if that's
    /// truly the intent.
    #[test]
    fn shadow_synth_flips_multi_atom_antecedent_to_decided() {
        // `valid_and_ready = valid && ready` and `valid_and_ack = valid && ack`
        // — both combinational-of-inputs Outputs (yosys-style Output form,
        // matches the monono `mem_rvalid_mine` pattern). `q` is a spare state
        // whose next is const 0 → `q == 0` always Holds.
        const BTOR2: &str = r#"
1 sort bitvec 1
2 input 1 valid
3 input 1 ready
4 input 1 ack
5 state 1 q
6 const 1 0
7 init 1 5 6
8 and 1 2 3
9 output 8 valid_and_ready
10 and 1 2 4
11 output 10 valid_and_ack
12 next 1 5 6
"#;
        let formula = crate::mu_calculus::parser::parse(
            "nu X. ((not (valid_and_ready and valid_and_ack) or [] (q == 0)) and [] X)",
        )
        .expect("formula");
        let verdict = exact_symbolic_verdict(BTOR2, &formula)
            .expect("multi-atom shadow-synth on an SVA `|=>` shape should decide, not refuse");
        assert_eq!(
            verdict,
            ExactVerdict::Holds,
            "with shadow-synth on both leaves, `(valid_and_ready && valid_and_ack) |=> (q == 0)` \
             where q'=0 always Holds",
        );
    }

    /// P3 — the trap-path witness for a `Violated` `AG EF (st==0)` recoverability property.
    /// `st` cycles 0→1→2→0, but `esc` drives it to the TERMINAL trap `st==3` (self-loop), from
    /// which `st==0` is unreachable — so `AG EF (st==0)` is Violated, and the witness is a
    /// concrete reset→trap path ending in the absorbing trap state. Self-validating: it replays
    /// the prefix through `eval_step` and confirms the final state is the trap `st==3`.
    #[test]
    fn p3_ag_ef_trap_path_witness() {
        const TRAP_FSM: &str = r#"
1 sort bitvec 1
2 sort bitvec 2
3 input 1 esc
4 state 2 st
5 const 2 11
6 const 2 00
7 one 2
8 add 2 4 7
9 const 2 10
10 eq 1 4 9
11 ite 2 10 6 8
12 eq 1 4 5
13 ite 2 12 5 11
14 ite 2 3 5 13
15 next 2 4 14
16 init 2 4 6
"#;
        let formula =
            crate::mu_calculus::parser::parse("nu Y. ((mu X. ((st == 0) or <> X)) and [] Y)")
                .expect("formula");
        let (verdict, witness) =
            exact_symbolic_verdict_with_witness(TRAP_FSM, &formula).expect("exact verdict");
        assert_eq!(
            verdict,
            ExactVerdict::Violated,
            "st==3 is a reachable terminal trap ⇒ AG EF (st==0) is Violated",
        );
        let w = witness.expect("a Violated AG EF must yield a reachable-trap-path witness");
        // The witness ends in the absorbing trap `st==3` (its single-state cycle).
        let trap_state = w
            .cycle
            .last()
            .or_else(|| w.prefix.last())
            .expect("non-empty witness");
        assert_eq!(
            trap_state.get("st").copied(),
            Some(3),
            "the witness ends in the terminal trap st==3; got {trap_state:?}",
        );
        // The prefix starts at the reset state (st==0) — a genuine reset→trap path.
        if let Some(first) = w.prefix.first() {
            assert_eq!(
                first.get("st").copied(),
                Some(0),
                "the witness prefix starts at the reset state st==0",
            );
        }
        // SELF-VALIDATE the recorded inputs: replaying `inputs[i]` from `prefix[i]` via the
        // concrete eval_step must land on the next path state — i.e. the witness's input
        // sequence genuinely reproduces the reset→trap path (the basis for RTL replay).
        assert_eq!(
            w.inputs.len(),
            w.prefix.len(),
            "one recorded input per prefix transition",
        );
        let file = parser::parse(TRAP_FSM).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let path: Vec<_> = w.prefix.iter().chain(w.cycle.iter()).collect();
        for (i, inp) in w.inputs.iter().enumerate() {
            // full = state(path[i]) + input(inp)
            let mut full: HashMap<String, u128> =
                path[i].iter().map(|(k, v)| (k.clone(), *v)).collect();
            for (k, v) in inp {
                full.insert(k.clone(), *v);
            }
            let stepped = bb.eval_step(&full);
            let next_st = stepped.get("st").copied().unwrap_or(path[i]["st"]);
            assert_eq!(
                Some(next_st),
                path[i + 1].get("st").copied(),
                "replaying input {i} from st={} must reach the next path state",
                path[i]["st"],
            );
        }
    }

    /// D1.8 — the stall-lasso witness for a `Violated` `AF (cnt==3)` on the reset
    /// counter. `en=0` holds `cnt`, so from the reset state (`cnt=0`) there is a
    /// `¬(cnt==3)` path that never reaches 3 — `AF (cnt==3)` fails, and the witness
    /// is a concrete lasso. The test is **self-validating**: it replays every lasso
    /// edge through the concrete `eval_step` simulator (there must be an input
    /// realising each step) and confirms every state is `¬p` and the cycle closes.
    #[test]
    fn d1_8_exact_stall_lasso_reset_counter() {
        let file = parser::parse(RESET_COUNTER_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let p = bb
            .predicate_bdd(&PredicateExpr::eq("cnt", 3))
            .expect("predicate cnt==3");

        let lasso = bb
            .exact_stall_lasso(&file, &p)
            .expect("AF(cnt==3) is Violated on the reset counter → a stall lasso exists");
        assert!(!lasso.cycle.is_empty(), "the stall must close into a cycle");

        // The witness starts at the reset state (cnt = 0).
        let first = lasso
            .prefix
            .first()
            .or_else(|| lasso.cycle.first())
            .expect("non-empty lasso");
        assert_eq!(
            first.get("cnt"),
            Some(&0),
            "lasso starts at the reset state"
        );

        // Every lasso state avoids p (cnt != 3).
        for st in lasso.prefix.iter().chain(lasso.cycle.iter()) {
            assert_ne!(st.get("cnt"), Some(&3), "every stall state is ¬(cnt==3)");
        }

        // Self-validate: every consecutive edge (including the cycle's closing edge
        // back to cycle[0]) is realised by SOME concrete input via eval_step.
        let step_exists = |from: &BTreeMap<String, u128>, to: &BTreeMap<String, u128>| -> bool {
            (0..=1u128).any(|en| {
                let mut asg: HashMap<String, u128> =
                    from.iter().map(|(k, v)| (k.clone(), *v)).collect();
                asg.insert("en".to_string(), en);
                let next = bb.eval_step(&asg);
                next.get("cnt").copied().unwrap_or(0) == *to.get("cnt").unwrap()
            })
        };
        let mut path: Vec<&BTreeMap<String, u128>> =
            lasso.prefix.iter().chain(lasso.cycle.iter()).collect();
        // Close the loop: the last cycle state steps back to cycle[0].
        path.push(&lasso.cycle[0]);
        for w in path.windows(2) {
            assert!(
                step_exists(w[0], w[1]),
                "lasso edge {:?} -> {:?} must be a real transition",
                w[0],
                w[1]
            );
        }
    }

    /// D1.8 — `AF (cnt==0)` HOLDS on the reset counter from cnt=0 (it's already 0),
    /// so there is no stall and no lasso.
    #[test]
    fn d1_8_exact_stall_lasso_none_when_af_holds() {
        let file = parser::parse(RESET_COUNTER_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let p = bb
            .predicate_bdd(&PredicateExpr::eq("cnt", 0))
            .expect("predicate cnt==0");
        // cnt==0 holds at the initial state, so AF(cnt==0) holds ⇒ no stall witness.
        assert!(bb.exact_stall_lasso(&file, &p).is_none());
    }

    /// D1.8b — the verdict entry point also yields the stall lasso for a Violated
    /// bare `AF p`, and `None` when it holds.
    #[test]
    fn d1_8b_verdict_with_witness_af_violated() {
        let af = crate::mu_calculus::parser::parse("mu X. ((cnt == 3) or [] X)").expect("parse");
        let (v, w) =
            exact_symbolic_verdict_with_witness(RESET_COUNTER_BTOR2, &af).expect("verdict");
        assert_eq!(v, ExactVerdict::Violated);
        let lasso = w.expect("Violated AF ⇒ a stall lasso witness");
        assert!(!lasso.cycle.is_empty());
        for st in lasso.prefix.iter().chain(lasso.cycle.iter()) {
            assert_ne!(st.get("cnt"), Some(&3), "every stall state is ¬(cnt==3)");
        }

        // `AF (cnt==0)` holds at reset (cnt is already 0) ⇒ no witness.
        let holds = crate::mu_calculus::parser::parse("mu X. ((cnt == 0) or [] X)").expect("parse");
        let (v2, w2) =
            exact_symbolic_verdict_with_witness(RESET_COUNTER_BTOR2, &holds).expect("verdict");
        assert_eq!(v2, ExactVerdict::Holds);
        assert!(w2.is_none());
    }

    // D1.8b-2 — a counter where the stall is REACHABLE but not initial: from cnt=0 the
    // next state is forced to 1 (so `AF (cnt==1)` HOLDS at reset), but `cnt=2`/`cnt=3`
    // can hold under `!go` forever (a `¬(cnt==1)` stall), reachable via 0→1→2. So
    // `AG AF (cnt==1)` is Violated by a reachable-not-initial stall.
    const REACH_STALL_BTOR2: &str = r#"
1 sort bitvec 2
2 sort bitvec 1
3 state 1 cnt
4 input 2 go
5 zero 1
6 one 1
7 eq 2 3 5
8 add 1 3 6
9 ite 1 4 8 3
10 ite 1 7 6 9
11 next 1 3 10
12 init 1 3 5
"#;

    /// D1.8b-2 — `exact_reachable_stall_lasso` finds a stall reachable from reset that
    /// `exact_stall_lasso` (stall-at-reset only) misses. The lasso's cycle is `¬p`; the
    /// prefix is the reset→stall reach path (and MAY pass through a `p` state — the
    /// point of `AG AF p` is that `p` holds only finitely).
    #[test]
    fn d1_8b2_reachable_stall_lasso() {
        let file = parser::parse(REACH_STALL_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let p = bb
            .predicate_bdd(&PredicateExpr::eq("cnt", 1))
            .expect("predicate cnt==1");

        // Stall-at-reset finds nothing: from cnt=0 every path reaches cnt=1.
        assert!(
            bb.exact_stall_lasso(&file, &p).is_none(),
            "AF(cnt==1) holds at reset — no stall AT reset"
        );

        // Reachable-stall finds the AG-AF witness.
        let lasso = bb
            .exact_reachable_stall_lasso(&file, &p)
            .expect("a ¬(cnt==1) stall is reachable from reset");
        assert!(
            !lasso.cycle.is_empty(),
            "the reachable stall must close a cycle"
        );
        assert_eq!(
            lasso.prefix.first().and_then(|st| st.get("cnt")),
            Some(&0),
            "the lasso starts at the reset state"
        );
        // The CYCLE avoids p forever (only the prefix may touch p finitely).
        for st in &lasso.cycle {
            assert_ne!(st.get("cnt"), Some(&1), "the stall cycle is ¬(cnt==1)");
        }
        // Self-validate every edge (incl. the closing edge) via the concrete simulator.
        let step_exists = |from: &BTreeMap<String, u128>, to: &BTreeMap<String, u128>| -> bool {
            (0..=1u128).any(|go| {
                let mut asg: HashMap<String, u128> =
                    from.iter().map(|(k, v)| (k.clone(), *v)).collect();
                asg.insert("go".to_string(), go);
                bb.eval_step(&asg).get("cnt").copied().unwrap_or(0) == *to.get("cnt").unwrap()
            })
        };
        let mut path: Vec<&BTreeMap<String, u128>> =
            lasso.prefix.iter().chain(lasso.cycle.iter()).collect();
        path.push(&lasso.cycle[0]);
        for w in path.windows(2) {
            assert!(
                step_exists(w[0], w[1]),
                "lasso edge {:?} -> {:?} must be a real transition",
                w[0],
                w[1]
            );
        }
    }

    /// D1.8b-2 — the verdict entry point surfaces the reachable-stall witness for a
    /// Violated `AG AF p`.
    #[test]
    fn d1_8b2_verdict_with_witness_ag_af_violated() {
        let ag_af =
            crate::mu_calculus::parser::parse("nu Y. ((mu X. ((cnt == 1) or [] X)) and [] Y)")
                .expect("parse");
        let (v, w) =
            exact_symbolic_verdict_with_witness(REACH_STALL_BTOR2, &ag_af).expect("verdict");
        assert_eq!(v, ExactVerdict::Violated, "AG AF(cnt==1) is Violated");
        let lasso = w.expect("Violated AG AF ⇒ a reachable-stall witness");
        assert!(!lasso.cycle.is_empty());
        for st in &lasso.cycle {
            assert_ne!(st.get("cnt"), Some(&1));
        }
    }

    /// D1.8c — the generated GR(1) response formula `GF a → GF b` gives the correct
    /// **definite** verdict over the exact engine on hand-verified models. Validates
    /// the soundness-critical alternating-fixpoint structure of `gr1_response_formula`
    /// against known GR(1) truth.
    #[test]
    fn d1_8c_gr1_response_formula_verdicts() {
        let formula = crate::mu_calculus::parser::parse(&gr1_response_formula("s == 0", "s == 1"))
            .expect("gr1 formula parses");

        // TOGGLE: forced s0⇄s1. Every path hits s==0 and s==1 infinitely, so
        // `GF(s==0) → GF(s==1)` HOLDS (no ¬(s==1) cycle exists).
        const TOGGLE: &str = r#"
1 sort bitvec 1
2 state 1 s
3 not 1 2
4 next 1 2 3
5 zero 1
6 init 1 2 5
"#;
        assert_eq!(
            exact_symbolic_verdict(TOGGLE, &formula).expect("verdict"),
            ExactVerdict::Holds,
            "forced toggle: GF(s==0) → GF(s==1) holds"
        );

        // STAYLOOP: s0 (s==0, ¬(s==1)) self-loops under `stay`, else advances to s1.
        // `stay=1` forever keeps s==0 (assume recurs) and never reaches s==1 — a
        // reachable assume-recurring guarantee-avoiding cycle ⇒ VIOLATED.
        const STAYLOOP: &str = r#"
1 sort bitvec 1
2 state 1 s
3 input 1 stay
4 zero 1
5 one 1
6 eq 1 2 4
7 ite 1 3 4 5
8 ite 1 6 7 4
9 next 1 2 8
10 init 1 2 4
"#;
        assert_eq!(
            exact_symbolic_verdict(STAYLOOP, &formula).expect("verdict"),
            ExactVerdict::Violated,
            "stay-loop: an assume-recurring, guarantee-avoiding cycle is reachable"
        );

        // Vacuous: an unsatisfiable assume (`s == 3` over a 1-bit register) makes
        // `GF assume` false, so `GF assume → GF guarantee` holds trivially — no bad
        // cycle can recur through an unreachable assume.
        let vacuous = crate::mu_calculus::parser::parse(&gr1_response_formula("s == 3", "s == 1"))
            .expect("parse");
        assert_eq!(
            exact_symbolic_verdict(STAYLOOP, &vacuous).expect("verdict"),
            ExactVerdict::Holds,
            "unsatisfiable assume ⇒ GF assume → GF guarantee holds vacuously"
        );
    }

    /// D1.4 — `resolve_predicate_expr_registers` rewrites every register name in a
    /// `PredicateExpr` tree through the resolver (idempotent on names it doesn't
    /// remap).
    #[test]
    fn d1_4_resolve_predicate_expr_registers_rewrites() {
        let remap = |n: &str| -> String {
            if n == "cnt_q" {
                "cnt_d".to_string()
            } else {
                n.to_string()
            }
        };
        let expr = PredicateExpr::And(
            Box::new(PredicateExpr::eq("cnt_q", 0)),
            Box::new(PredicateExpr::CmpReg {
                lhs: "cnt_q".to_string(),
                op: CmpOp::Eq,
                rhs: "acc".to_string(),
            }),
        );
        let out = resolve_predicate_expr_registers(&expr, &remap);
        match out {
            PredicateExpr::And(a, b) => {
                assert!(matches!(*a, PredicateExpr::Cmp { register, .. } if register == "cnt_d"));
                assert!(
                    matches!(*b, PredicateExpr::CmpReg { lhs, rhs, .. } if lhs == "cnt_d" && rhs == "acc")
                );
            }
            _ => panic!("expected And"),
        }
    }

    /// D1 HEADLINE (docker-gated) — exact ROBDD μ-calculus MC *decides* on real
    /// OpenTitan `uart_tx` an `AF`-liveness that predicate abstraction returns ⊥
    /// for: `AG AF (bit_cnt_q == 0)` — "a transmission in progress always completes"
    /// — is **Violated** (definite), and the engine returns a concrete stall lasso.
    /// The counter does NOT always drain: `bit_cnt_q` loads on a write, and a
    /// persistently-asserted `wr` (or a stalled `tick_baud_x16`) holds it non-zero
    /// forever — a real liveness failure. The value of the exact engine is that it
    /// decides this **definitely with a counterexample** where the predicate-cube
    /// path answers ⊥ (an `AF` needs a ranking predicate abstraction cannot
    /// synthesize).
    ///
    /// REGRESSION for the frozen-register fix (2026-07-05): this asserted `Holds`
    /// until the `next_funcs` keying bug was fixed. `bit_cnt_q`'s name survives only
    /// on a `uext` alias of an unnamed state (post flatten/async2sync/dffunmap); the
    /// bug keyed `next_funcs` off the raw state symbol, so the lookup missed and the
    /// register was silently FROZEN at its 0 init — making `AG AF` a *vacuous*
    /// `Holds`. With the register actually transitioning, the true verdict is
    /// `Violated`. Companion verdicts remain definite + sound: `AG EF`
    /// (recoverability — idle always reachable, since reset drains the counter) and
    /// `EF (bit_cnt==0)` are `Holds`; `AG (bit_cnt < 12)` (a real bounded-counter
    /// safety invariant) is `Holds`.
    /// `#[ignore]`; the SV is read at run time so `make ci` only compiles it.
    #[test]
    #[ignore = "needs sv2v + yosys — run in the mununu-sva docker image"]
    fn e2e_d1_uart_tx_exact_liveness_verdict() {
        use crate::adapter::yosys::{YosysOptions, sv_to_btor2_with_blackboxes};

        let sv = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/verify/m1_opentitan_uart_tx/source/uart_tx.sv"
        ))
        .expect("read uart_tx.sv");
        let yopts = YosysOptions {
            top: Some("uart_tx".to_string()),
            use_sv2v: true,
            ..Default::default()
        };
        let btor2 = sv_to_btor2_with_blackboxes(&sv, &yopts)
            .expect("extract uart_tx")
            .btor2;
        // Anchor the model at reset, exactly as `verify_auto` does before handing a
        // design to this engine. `sv_to_btor2_with_blackboxes` is the raw lift: Yosys
        // `async2sync` puts the reset in the next-state mux and emits NO `init` line,
        // so without this step the BTOR2 does not say where the design starts — and
        // since mununu#498's sibling fix the engine honours that and starts anywhere,
        // which makes a bounded-counter invariant like `AG (bit_cnt_q < 12)` fail from
        // a fabricated power-up of 12..15. Injecting the reset init is what makes this
        // the model `sv verify-auto` actually builds, rather than one no shipped path
        // produces. `rst_ni` is active-low, so its INACTIVE level is 1.
        let btor2 = crate::adapter::btor2::reset_init::inject_reset_init(
            &btor2,
            &[("rst_ni".to_string(), 1u64)],
        )
        .expect("inject reset init");
        let verdict = |f: &str| -> ExactVerdict {
            let ff = crate::mu_calculus::parser::parse(f).expect("formula parses");
            exact_symbolic_verdict(&btor2, &ff).expect("definite verdict, binds")
        };

        // THE HEADLINE: `AG AF (bit_cnt_q == 0)` — "a transmission always completes"
        // — is decided **Violated** (definite liveness failure) on real OpenTitan
        // RTL: a persistently-asserted `wr` (or a stalled tick) holds the counter
        // non-zero forever. The predicate-abstraction path answers this ⊥ (no
        // ranking); exact ROBDD MC decides it definitely, with a stall lasso. This
        // is the frozen-register-fix regression: a false `Holds` here means the
        // `next_funcs` alias-keying bug regressed and `bit_cnt_q` is frozen again.
        assert_eq!(
            verdict("nu X. ((mu Y. ((bit_cnt_q == 0) or [] Y)) and [] X)"),
            ExactVerdict::Violated,
            "AG AF (bit_cnt_q==0) is Violated on uart_tx (a stalled/persistently-written \
             counter never drains) — NOT the vacuous Holds of a frozen register"
        );
        // Companion sanity (all definite + sound): `AG EF` (recoverability — idle is
        // always reachable because reset drains the counter) and `EF (bit_cnt==0)`
        // hold; `AG (bit_cnt < 12)` is a real bounded-counter safety invariant.
        assert_eq!(
            verdict("nu Y. ((mu X. ((bit_cnt_q == 0) or <> X)) and [] Y)"),
            ExactVerdict::Holds,
        );
        assert_eq!(
            verdict("mu Y. ((bit_cnt_q == 0) or <> Y)"),
            ExactVerdict::Holds,
        );
        assert_eq!(
            verdict("nu X. ((bit_cnt_q < 12) and [] X)"),
            ExactVerdict::Holds,
            "the transmit bit counter is bounded (never ≥ 12)"
        );
    }

    /// Evaluate `AG AF (cnt==0)` over the gated counter and print the per-cube
    /// verdict. `c1 = {cnt≠0}` has may-edges to both `c0` and `c1` but (per the
    /// brute-force-verified relation) NO must-edge, so `AF (cnt==0)` at `c1` must
    /// be ⊥ (`must=false`, `may=true`), hence `AG AF` is ⊥ — NOT a definite
    /// VIOLATED. If this prints `False` at a feasible cube, the evaluation is the
    /// unsoundness source; if `⊥`, the verify-auto VIOLATED comes from elsewhere
    /// (pinning / shadow / compound / final-verdict projection).
    #[test]
    fn rf5_soundness_gated_counter_ag_af_is_bottom_not_violated() {
        let predicates = [PredicateExpr::eq("cnt", 0)];
        let file = parser::parse(GATED_COUNTER_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let rel = bb
            .abstract_relation(&predicates, Some(MustSemantics::ForallExists))
            .expect("rel");
        let names = ["done".to_string()];
        let formula = crate::mu_calculus::parser::parse("nu X. ((mu Y. (done or [] Y)) and [] X)")
            .expect("formula");
        let v = rel.evaluate(&formula, &names).expect("eval");
        for c in rel.feasible_cubes() {
            let verdict = rel.verdict_at(&v, c);
            eprintln!("AG AF (cnt==0) @ cube {c}: {verdict:?}");
            // No feasible cube may be definite-False: the concrete design can
            // always eventually reach cnt==0 (or stay), so a definite VIOLATED
            // would be unsound. ⊥ or True are acceptable.
            assert_ne!(
                verdict,
                crate::mu_calculus::trit::Trit::False,
                "cube {c}: AG AF (cnt==0) is definite-False — unsound over the gated counter"
            );
        }
    }

    /// The KMTS invariant `R_must ⊆ R_may` over **feasible** source cubes: every
    /// must-edge out of a satisfiable cube is a may-edge (for both semantics).
    /// An infeasible (unsatisfiable) cube is excluded — it is vacuously a
    /// must-edge to every cube yet has no may-edge, and the downstream lift
    /// never materialises such a cube as an abstract state.
    #[test]
    fn rf5_3d_must_subseteq_may_over_feasible_cubes() {
        let predicates = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        let file = parser::parse(SATURATING_COUNTER_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        for sem in [MustSemantics::ForallExists, MustSemantics::ForallForall] {
            let rel = bb
                .abstract_relation(&predicates, Some(sem))
                .expect("relation");
            let k = rel.num_predicates();
            for ci in 0..(1usize << k) {
                // A feasible cube has at least one may-out-edge (every concrete
                // state steps somewhere); an infeasible cube has none.
                let feasible = (0..(1usize << k)).any(|cj| rel.may_holds(ci, cj));
                if !feasible {
                    continue;
                }
                for cj in 0..(1usize << k) {
                    if rel.must_holds(ci, cj) {
                        assert!(
                            rel.may_holds(ci, cj),
                            "must ⊄ may: {ci} -> {cj} ({sem:?}) is a must-edge but not a may-edge"
                        );
                    }
                }
            }
        }
    }

    // ---- R-F5.4.1: symbolic cube evaluator ≡ evaluate_tri ----

    /// Build the abstract relation, materialise the equivalent explicit
    /// cube-KMTS (feasible cubes as states, may/must edges + definite cube AP
    /// labels), and assert the symbolic evaluator agrees with `evaluate_tri`
    /// on every `formula_str` at every feasible cube.
    fn assert_symbolic_eval_matches_tri(
        src: &str,
        predicates: &[PredicateExpr],
        names: &[&str],
        states: &[(&str, u32)],
        formulas: &[&str],
    ) {
        use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, TransitionModality, Tristate};
        use crate::mu_calculus::evaluator::{Environment, evaluate_tri};

        let file = parser::parse(src).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let rel = bb
            .abstract_relation(predicates, Some(MustSemantics::ForallExists))
            .expect("relation");

        let cube_of = |regs: &HashMap<String, u128>| -> usize {
            let mut c = 0usize;
            for (i, p) in predicates.iter().enumerate() {
                if p.eval(regs) {
                    c |= 1 << i;
                }
            }
            c
        };

        // Feasible cubes: those inhabited by at least one concrete register state.
        let total_state_bits: u32 = states.iter().map(|(_, w)| *w).sum();
        let mut feasible_set: std::collections::BTreeSet<usize> = Default::default();
        for scombo in 0..(1u128 << total_state_bits) {
            let mut regs: HashMap<String, u128> = HashMap::new();
            let mut off = 0u32;
            for (name, w) in states {
                let mask = (1u128 << w) - 1;
                regs.insert((*name).to_string(), (scombo >> off) & mask);
                off += w;
            }
            feasible_set.insert(cube_of(&regs));
        }
        let feasible: Vec<usize> = feasible_set.into_iter().collect();
        let n = feasible.len();

        // Explicit cube-KMTS over the feasible cubes.
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for j in 0..n {
            b.state(format!("c{j}"));
        }
        b.initial("c0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..n)
            .map(|j| b.state_id_or_insert(format!("c{j}")).unwrap())
            .collect();
        for (j, &ci) in feasible.iter().enumerate() {
            let mut set_vars: Vec<&str> = Vec::new();
            for (i, nm) in names.iter().enumerate() {
                let held = (ci >> i) & 1 == 1;
                let v = if held {
                    Tristate::KleeneT
                } else {
                    Tristate::KleeneF
                };
                b.with_3valued_predicate(ids[j], *nm, v);
                if held {
                    set_vars.push(*nm);
                }
            }
            // R-F5.5c — also populate the 2-valued state-variable bitset. The cube
            // AP labels are definite (KleeneT/KleeneF), so this is consistent with
            // the 3-valued map above and a no-op for the unguarded fragment. It is
            // required for guarded modalities: `evaluate_tri`'s `GuardPartitions`
            // reads `state_variable_bitset` (not `state_3valued_predicates`) when
            // matching `req_cur`/`forb_cur`/`req_next`/`forb_next`.
            b.with_variables_for_state(ids[j], set_vars);
        }
        for (j, &ci) in feasible.iter().enumerate() {
            for (l, &cj) in feasible.iter().enumerate() {
                if rel.must_holds(ci, cj) {
                    b.transition_ids_with_modality(
                        ids[j],
                        &[step],
                        ids[l],
                        TransitionModality::Sharp,
                    );
                } else if rel.may_holds(ci, cj) {
                    b.transition_ids_with_modality(
                        ids[j],
                        &[step],
                        ids[l],
                        TransitionModality::MayOnly,
                    );
                }
            }
        }
        let clts = b.build().expect("clts builds");

        let names_owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let env = Environment::new(clts.state_count());
        for formula_str in formulas {
            let formula = crate::mu_calculus::parser::parse(formula_str).expect("formula parses");
            let tri = evaluate_tri(&formula, &clts, &env).expect("evaluate_tri");
            let sym = rel
                .evaluate(&formula, &names_owned)
                .expect("symbolic evaluate");
            for (j, &ci) in feasible.iter().enumerate() {
                assert_eq!(
                    rel.verdict_at(&sym, ci),
                    tri.verdict_at(j),
                    "cube {ci} (state c{j}), formula `{formula_str}`"
                );
            }
        }
    }

    /// The full unguarded fragment (bare `[]`/`<>`, boolean ops, nested νμ) on
    /// the saturating counter, predicates `p = (cnt == 0)`, `q = (cnt ≥ 2)`.
    #[test]
    fn rf5_4_1_symbolic_eval_matches_tri_saturating_counter() {
        let predicates = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        let formulas = [
            "[] p",
            "<> p",
            "[] q",
            "<> q",
            "not q",
            "p or q",
            "p and q",
            "nu X. p and [] X",                   // AG p
            "mu X. q or <> X",                    // EF q
            "nu X. (not p) and [] X",             // AG ¬p
            "nu Y. (mu X. (q or <> X)) and [] Y", // AG EF q (nested νμ)
        ];
        assert_symbolic_eval_matches_tri(
            SATURATING_COUNTER_BTOR2,
            &predicates,
            &["p", "q"],
            &[("cnt", 2)],
            &formulas,
        );
    }

    /// Two registers stepping together, with a relational predicate — exercises
    /// a KMTS with `MayOnly` edges (input nondeterminism) so the `⊥` verdicts
    /// are non-trivial.
    #[test]
    fn rf5_4_1_symbolic_eval_matches_tri_two_registers() {
        let predicates = [
            PredicateExpr::CmpReg {
                lhs: "cnt".to_string(),
                op: CmpOp::Eq,
                rhs: "acc".to_string(),
            },
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
            PredicateExpr::eq("acc", 0),
        ];
        let formulas = [
            "[] p",
            "<> p",
            "<> (p and q)",
            "nu X. p and [] X",
            "mu X. r or <> X",
            "nu Y. (mu X. (p or <> X)) and [] Y",
        ];
        assert_symbolic_eval_matches_tri(
            TWO_REGISTER_BTOR2,
            &predicates,
            &["p", "q", "r"],
            &[("cnt", 2), ("acc", 2)],
            &formulas,
        );
    }

    /// R-F5.5c — modalities guarded by `req_cur`/`forb_cur`/`req_next`/`forb_next`
    /// state predicates agree cell-for-cell with `evaluate_tri` on the explicit
    /// cube-KMTS (the guard filters modal steps by source/target cube exactly as
    /// the explicit `guard_matches`). Saturating counter, `p = (cnt == 0)`,
    /// `q = (cnt ≥ 2)`.
    #[test]
    fn rf5_5c_symbolic_eval_matches_tri_guarded_saturating_counter() {
        let predicates = [
            PredicateExpr::eq("cnt", 0),
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
        ];
        let formulas = [
            // next-state guards
            "[ req_next = {q} ] p",
            "< req_next = {q} > p",
            "[ forb_next = {q} ] p",
            "< forb_next = {p} > q",
            // current-state guards
            "[ req_cur = {p} ] q",
            "< req_cur = {q} > p",
            // combined next guards (require q, forbid p in the target)
            "[ req_next = {q}, forb_next = {p} ] p",
            // guarded modality inside a fixpoint
            "nu X. p and [ req_next = {q} ] X",
            "mu X. q or < req_next = {p} > X",
            // nested νμ with a guarded outer box
            "nu Y. (mu X. (q or <> X)) and [ req_cur = {p} ] Y",
            // unguarded still works alongside guarded
            "nu X. p and [] X",
        ];
        assert_symbolic_eval_matches_tri(
            SATURATING_COUNTER_BTOR2,
            &predicates,
            &["p", "q"],
            &[("cnt", 2)],
            &formulas,
        );
    }

    /// R-F5.5c — the same guarded fragment over a KMTS carrying `MayOnly` edges
    /// (two registers, relational predicate), so guarded verdicts include
    /// non-trivial `⊥`s that must still match `evaluate_tri`.
    #[test]
    fn rf5_5c_symbolic_eval_matches_tri_guarded_two_registers() {
        let predicates = [
            PredicateExpr::CmpReg {
                lhs: "cnt".to_string(),
                op: CmpOp::Eq,
                rhs: "acc".to_string(),
            },
            PredicateExpr::Cmp {
                register: "cnt".to_string(),
                op: CmpOp::Ge,
                value: 2,
            },
            PredicateExpr::eq("acc", 0),
        ];
        let formulas = [
            "[ req_next = {q} ] p",
            "< req_next = {r} > p",
            "[ forb_next = {p} ] q",
            "[ req_cur = {r} ] p",
            "nu X. p and [ req_next = {q} ] X",
            "mu X. r or < req_next = {p} > X",
            "nu Y. (mu X. (p or <> X)) and [ req_cur = {r} ] Y",
        ];
        assert_symbolic_eval_matches_tri(
            TWO_REGISTER_BTOR2,
            &predicates,
            &["p", "q", "r"],
            &[("cnt", 2), ("acc", 2)],
            &formulas,
        );
    }

    /// R-F5.5c — label / controllability / step-bounded guards are out-of-fragment
    /// over a predicate cube (see `cube_modality_soundness_warnings`) and are an
    /// honest error, never a wrong/vacuous verdict.
    #[test]
    fn rf5_5c_label_control_step_guards_are_rejected() {
        let predicates = [PredicateExpr::eq("cnt", 0)];
        let file = parser::parse(SATURATING_COUNTER_BTOR2).expect("parse");
        let bb = BddBitBlaster::build(&file).expect("build");
        let rel = bb
            .abstract_relation(&predicates, Some(MustSemantics::ForallExists))
            .expect("relation");
        let names = ["p".to_string()];

        for (formula_str, why) in [
            ("[ ctrl = controllable ] p", "controllability guard"),
            ("< ( steps <= 2 ) > p", "step-bounded modality"),
            ("[ labels = {step} ] p", "label guard"),
        ] {
            let formula = crate::mu_calculus::parser::parse(formula_str).expect("formula parses");
            assert!(
                rel.evaluate(&formula, &names).is_err(),
                "{why} must be an honest error over a cube, not a verdict: `{formula_str}`"
            );
        }
    }

    // ---- Phase 2b: symbolic environment-strategy synthesis ----------------------------------------

    // A POSITIONAL-strategy trap: st cycles A(0)→B(1)→C(2)→A, but the SAFE input DIFFERS per state —
    // x=0 at A, x=1 at B, x=0 at C — and the WRONG input at any state drops to TRAP(3, absorbing). So
    // no CONSTANT input-hold recovers (x=0 traps at B; x=1 traps at A), but a POSITIONAL environment
    // strategy stays in the recoverable region {A,B,C}. Free-input `AG EF(st==0)` is VIOLATED.
    const POSITIONAL_TRAP: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 x
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 4 5
11 eq 2 4 7
12 eq 2 4 8
13 ite 1 3 9 7
14 ite 1 3 8 9
15 ite 1 3 9 5
16 ite 1 12 15 9
17 ite 1 11 14 16
18 ite 1 10 13 17
19 next 1 4 18
";

    /// Phase 2b — the symbolic env-strategy synthesizes a POSITIONAL strategy that maintains
    /// `AG EF(st==0)` on `POSITIONAL_TRAP`, where NO constant input-hold works (the marginal reach
    /// over Phase-2a slice 1). `init ⊆ S` ⇒ Maintainable with a first-move witness.
    #[test]
    fn env_strategy_finds_positional_maintenance_where_no_constant_hold_works() {
        match exact_env_strategy(POSITIONAL_TRAP, "st == 0").expect("synthesis runs") {
            EnvStrategyOutcome::Maintainable { first_move, .. } => {
                assert!(
                    first_move.contains_key("x"),
                    "the first move should choose the input `x`: {first_move:?}"
                );
            }
            other => panic!("expected Maintainable (positional strategy exists): {other:?}"),
        }
        // Cross-check the marginal reach: a single-input CONSTANT hold does NOT maintain it — the
        // free-input verdict is VIOLATED and neither x=0 nor x=1 keeps the design recoverable.
        assert_eq!(
            crate::adapter::recoverability::verify_recoverability(POSITIONAL_TRAP, "st == 0").ok(),
            Some(crate::verdict::PropertyVerdict::Violated),
            "free-input the positional trap is VIOLATED (a constant hold cannot fix it)"
        );
    }

    /// Phase 2b — an UNRECOVERABLE design (`st` unconditionally drops to an absorbing TRAP, `st==0`
    /// only at reset): NO environment strategy maintains recovery (`init ⊄ S`), so the synthesis
    /// reports NotMaintainable — the negated game's forced-trap counter-strategy.
    #[test]
    fn env_strategy_reports_not_maintainable_on_forced_trap() {
        // A(0) --any x--> TRAP(1, absorbing); good = st==0 reachable only initially.
        const FORCED_TRAP: &str = "\
1 sort bitvec 1
2 input 1 x
3 state 1 st
4 zero 1
5 init 1 3 4
6 one 1
7 next 1 3 6
";
        match exact_env_strategy(FORCED_TRAP, "st == 0").expect("synthesis runs") {
            EnvStrategyOutcome::NotMaintainable { .. } => {}
            other => panic!("expected NotMaintainable (forced trap, no strategy): {other:?}"),
        }
    }

    /// Phase 2b SOUNDNESS CONTROL — a strategy that would keep the design in `good` forever is VACUOUS
    /// and must NOT be reported. `go=0` keeps `st` at IDLE(0) forever, so the maintained region ⊆ good
    /// (the env never leaves good) ⇒ Inapplicable, not a spurious Maintainable.
    #[test]
    fn env_strategy_rejects_vacuous_stay_in_good() {
        // st=A(0) --go=1--> B(1) --> TRAP(3); good = st==0. Holding go=0 keeps st at 0 forever (vacuous).
        const STALLER: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 state 1 st
4 zero 1
5 init 1 3 4
6 input 2 go
7 one 1
8 constd 1 2
9 constd 1 3
10 eq 2 3 4
11 eq 2 3 7
12 ite 1 6 7 4
13 ite 1 11 9 3
14 ite 1 10 12 13
15 next 1 3 14
";
        assert!(
            matches!(
                exact_env_strategy(STALLER, "st == 0").expect("runs"),
                EnvStrategyOutcome::Inapplicable(_)
            ),
            "a stay-in-good strategy is vacuous ⇒ must not be reported as Maintainable"
        );
    }

    /// P2.5-E T1 — env-strategy EXISTENCE, GENERALIZED. `exact_env_strategy_exists` decides env-enforce
    /// for ANY μ-calculus formula. On the recoverability formula it AGREES with the bespoke
    /// `exact_env_strategy` (subsumption: Maintainable ⟺ Some(true)); it also decides a SAFETY property
    /// (env-can-stay-out-of-the-trap) — a shape the recoverability-specific synth cannot express.
    #[test]
    fn env_strategy_exists_generalizes_over_mu_calculus() {
        use crate::mu_calculus::parser::parse;
        // (a) Recoverability formula on POSITIONAL_TRAP — subsumes exact_env_strategy (Maintainable).
        let recov = parse("nu Y. ((mu X. ((st == 0) || <> X)) && [] Y)").expect("parse");
        assert_eq!(
            exact_env_strategy_exists(POSITIONAL_TRAP, &recov).expect("runs"),
            Some(true),
            "env can enforce AG EF(st==0) via a positional strategy"
        );
        assert!(
            matches!(
                exact_env_strategy(POSITIONAL_TRAP, "st == 0").expect("runs"),
                EnvStrategyOutcome::Maintainable { .. }
            ),
            "the general existence must AGREE with the bespoke recoverability synth"
        );
        // (b) SAFETY property (generality) — the env can keep `st` out of the TRAP(3) forever.
        let safety = parse("nu X. ((! (st == 3)) && [] X)").expect("parse");
        assert_eq!(
            exact_env_strategy_exists(POSITIONAL_TRAP, &safety).expect("runs"),
            Some(true),
            "env can stay out of the trap (a safety objective, not recoverability)"
        );
        // (c) NO strategy — a forced trap: st=A(0) --any--> TRAP(1), so the env cannot avoid `st==1`.
        const FORCED: &str = "1 sort bitvec 1\n2 input 1 x\n3 state 1 st\n4 zero 1\n5 init 1 3 4\n6 one 1\n7 next 1 3 6\n";
        let avoid = parse("nu X. ((! (st == 1)) && [] X)").expect("parse");
        assert_eq!(
            exact_env_strategy_exists(FORCED, &avoid).expect("runs"),
            Some(false),
            "the env cannot avoid the forced trap ⇒ no strategy"
        );
    }

    /// P2.5-E T2 — the strategy WITNESS (winning play) exhibits a POSITIONAL discipline. POS_REACH: from
    /// A(0) reach GOAL(2) requires `x=1` at A (→B) then `x=0` at B (→GOAL) — opposite input values in two
    /// states, no constant hold works. `exact_env_strategy_witness` returns the play showing exactly that.
    #[test]
    fn env_strategy_witness_exhibits_positional_play() {
        const POS_REACH: &str = "\
1 sort bitvec 2
2 sort bitvec 1
3 input 2 x
4 state 1 st
5 zero 1
6 init 1 4 5
7 one 1
8 constd 1 2
9 eq 2 4 5
10 eq 2 4 7
11 ite 1 3 7 5
12 ite 1 3 7 8
13 ite 1 10 12 8
14 ite 1 9 11 13
15 next 1 4 14
";
        let play =
            exact_env_strategy_witness(POS_REACH, "st == 2").expect("a winning play to GOAL");
        let at = |s: u128| -> Option<u128> {
            play.prefix
                .iter()
                .zip(play.inputs.iter())
                .find_map(|(st, inp)| {
                    let v = st.iter().find(|(k, _)| k.contains("st")).map(|(_, v)| *v)?;
                    (v == s).then(|| inp.get("x").copied()).flatten()
                })
        };
        assert_eq!(at(0), Some(1), "positional: x=1 at A(0) to advance to B");
        assert_eq!(at(1), Some(0), "positional: x=0 at B(1) to reach GOAL");
    }

    /// Phase 2b — a non-`REG op VALUE` / unresolvable atom is Inapplicable (the caller abstains),
    /// never a spurious strategy.
    #[test]
    fn env_strategy_inapplicable_on_non_atom() {
        assert!(matches!(
            exact_env_strategy(POSITIONAL_TRAP, "not a valid atom").expect("runs"),
            EnvStrategyOutcome::Inapplicable(_)
        ));
    }
}
