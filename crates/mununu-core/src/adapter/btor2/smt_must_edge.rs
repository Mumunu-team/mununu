//! R.2.5b session 2 (2026-06-08) — SMT-backed must-edge query.
//!
//! The SMT-proved must-edge relation (replacing the removed session-1
//! sampling-based `SamplingConfluence` promotion) via a Z3 BV theory
//! check. Given a source
//! cube + a target cube + the BTOR2 transition relation, the query
//! asks:
//!
//! ```text
//! ∀ (state ⊨ src_cube). ∀ inputs. (transition(state, inputs, next) ⟹ next ⊨ tgt_cube)
//! ```
//!
//! This is **stronger** than the standard KMTS must-edge definition
//! (∀ state ⊨ src. ∃ input. next ⊨ tgt) — the session 2 MVP proves
//! "deterministic transition into tgt regardless of input." The
//! stronger definition is sound for KMTS preservation (every promoted
//! must-edge is also a standard must-edge); the MVP simply produces
//! *fewer* must-edges than the standard form would. The standard
//! ∀∃ form is queued for the R.2.5b session-2 follow-up. Both forms
//! are sound; the MVP trades some precision for a simpler encoding.
//!
//! The query is encoded as the UNSAT check of the negation:
//!
//! ```text
//! src_constraints(state_curr) ∧ transition ∧ ¬tgt_constraints(state_next)
//! ```
//!
//! If Z3 returns UNSAT, no (state, input) combination exists that
//! starts in src and escapes tgt — the must-edge holds. If SAT,
//! some (state, input) combination escapes, so the must-edge does
//! not hold. UNKNOWN (timeout) is treated as "does not hold" —
//! conservative for soundness (UNDER-approximation of R_must is
//! sound for KMTS preservation).
//!
//! # SOUNDNESS
//!
//! - Every promoted Sharp edge is sound — Z3's UNSAT verdict on the
//!   negation guarantees the must condition (in the MVP's stronger
//!   form). Missing must-edges (Z3 returns SAT or UNKNOWN) are
//!   conservative — they leave the edge as MayOnly, which is the
//!   safe direction.
//! - The encoder ([`crate::adapter::sidecar::predicate_image::btor2_encode`])
//!   is exact for every operator it supports. Operators outside its
//!   support set are rejected at encode time; the must-edge check
//!   simply returns [`SmtMustVerdict::Unknown`] in that case.

use std::collections::HashMap;

use crate::adapter::btor2::ast::Nid;
use crate::adapter::sidecar::predicate_image::btor2_encode::{Btor2SmtView, PrimedEnv, SignalKind};

/// Verdict from a single SMT must-edge query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtMustVerdict {
    /// Z3 UNSAT on the negation — the must-edge condition holds
    /// (the MVP's stronger ∀∀ form). Caller promotes MayOnly →
    /// Sharp.
    Must,
    /// Z3 SAT on the negation — some (state, input) escapes the
    /// target cube. The must-edge does not hold (in the MVP form);
    /// caller leaves the edge as MayOnly.
    NotMust,
    /// Z3 UNKNOWN (timeout, unsupported operator, missing register)
    /// — the verdict is indeterminate. Caller treats as NotMust
    /// (conservative for must-edge soundness).
    Unknown,
}

/// Build a `state_curr` BV equality constraint for one predicate
/// over the given cube bit (`1` ⇒ predicate true ⇒ register == value;
/// `0` ⇒ predicate false ⇒ register != value).
///
/// Returns `None` when the predicate's register has no matching
/// state-cell BV in the encoded view (typically because the BTOR2
/// file does not declare the register or because it was rewritten
/// away by an earlier pass).
/// ③b — an optional **deterministic** Z3 `rlimit` (resource-unit budget) for the cube's
/// must-edge SMT queries, from `MUNUNU_CUBE_SMT_RLIMIT` (`None` = unset = no bound, the
/// historical behaviour). Unlike the wall-clock `timeout`, `rlimit` is machine-independent,
/// so the ⊥-vs-decided boundary reproduces across machines. When a query exceeds it, Z3
/// returns `Unknown`, which the must-edge inference reads as **NotMust** — a *weaker* must
/// relation, hence a *more conservative* (more ⊥) but always SOUND abstraction. This turns a
/// cube that grinds on a wide combinational cone (e.g. i2c's 196-bit freed-input cone) into a
/// fast, deterministic ⊥ instead of a hang.
fn cube_smt_rlimit() -> Option<u32> {
    parse_cube_smt_rlimit(std::env::var("MUNUNU_CUBE_SMT_RLIMIT").ok())
}

/// Pure parse of the `MUNUNU_CUBE_SMT_RLIMIT` value (extracted so it is unit-testable
/// without touching the process environment): a positive integer → `Some(r)`; unset,
/// zero, negative, or non-numeric → `None` (no bound).
fn parse_cube_smt_rlimit(v: Option<String>) -> Option<u32> {
    v.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&r| r > 0)
}

fn build_predicate_constraint(bv: &z3::ast::BV, value: u64, polarity: bool) -> z3::ast::Bool {
    let width = bv.get_size();
    // R.2.5b session 2 — mask the unsigned value to the BV width.
    // Widths < 64 truncate; width == 64 passes through.
    let mask: u64 = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let bits = value & mask;
    let val_bv = z3::ast::BV::from_u64(bits, width);
    let eq = bv.eq(&val_bv);
    if polarity { eq } else { eq.not() }
}

/// Build a register-name → state-cell-NID lookup from the encoded
/// view's signal table. The keys are the BTOR2 symbol strings
/// (matching [`crate::adapter::btor2::kmts_lift::PredicateSpec::register`]).
pub fn build_register_nid_map(view: &Btor2SmtView) -> HashMap<String, Nid> {
    let mut map = HashMap::new();
    for sig in &view.signals {
        if sig.kind == SignalKind::State
            && let Some(symbol) = &sig.symbol
        {
            map.insert(symbol.clone(), sig.nid);
        }
    }
    map
}

/// H.B / H.U.1d — like [`build_register_nid_map`] but also maps **input** and
/// **combinational** signal symbols to their NID, so a predicate over a primary
/// input (free cube dimension, `docs/design/free-input-atoms.md`) or over a
/// combinational node (the uniform predicate-image term path) resolves.
///
/// - **Inputs** (H.B): source-pinned / target-free (the next-cycle input is not a
///   one-step variable) — realized in [`build_pred_constraint_uniform`].
/// - **Combinational** (H.U.1d): resolved to the combinational node's NID, whose
///   current-/next-cycle BVs are [`Btor2SmtView::signal_bvs`] / the primed cache;
///   the uniform rule then treats it as a determined-function-of-state cube
///   dimension. Latent until the seeder routes a combinational atom as a cube
///   dimension (H.U.2) — extending the map here is behaviour-preserving for
///   state/input predicate sets (the extra entries go unused).
///
/// **State takes precedence**, then input, then combinational, on a name
/// collision (`or_insert`). State-only callers use [`build_register_nid_map`],
/// so their behaviour is unchanged.
pub fn build_register_nid_map_with_inputs(view: &Btor2SmtView) -> HashMap<String, Nid> {
    let mut map = build_register_nid_map(view);
    for sig in &view.signals {
        if sig.kind == SignalKind::Input
            && let Some(symbol) = &sig.symbol
        {
            map.entry(symbol.clone()).or_insert(sig.nid);
        }
    }
    for sig in &view.signals {
        if sig.kind == SignalKind::Combinational
            && let Some(symbol) = &sig.symbol
        {
            map.entry(symbol.clone()).or_insert(sig.nid);
        }
    }
    map
}

/// R.2.5b session 2 — run the SMT must-edge query for one
/// (source-cube, target-cube) pair.
///
/// `src_bits` and `tgt_bits` are bitmasks: bit `i` of `src_bits` is
/// `1` iff predicate `predicates[i]` holds in the source cube;
/// likewise for the target cube. The view's
/// [`Btor2SmtView::transition`] is conjoined with the source +
/// negated-target constraints; UNSAT proves the must-edge.
///
/// `nid_map` maps each predicate's `register` symbol to its
/// state-cell NID; predicates whose register is absent from the
/// map cause the query to return [`SmtMustVerdict::Unknown`].
///
/// `timeout_ms` is applied per-query via Z3's `solver.timeout`
/// parameter; UNKNOWN verdicts include timeout cases.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub fn smt_per_target_must_check<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    let mut src_constraints: Vec<z3::ast::Bool> = Vec::new();
    let mut tgt_constraints: Vec<z3::ast::Bool> = Vec::new();

    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let tgt_polarity = (tgt_bits >> i) & 1 == 1;
        // B.1 — compound-aware; a missing register stays a conservative Unknown.
        let (Some(src_c), Some(tgt_c)) = (
            build_pred_constraint(view, nid_map, pred, false, src_polarity),
            build_pred_constraint(view, nid_map, pred, true, tgt_polarity),
        ) else {
            return SmtMustVerdict::Unknown;
        };
        src_constraints.push(src_c);
        tgt_constraints.push(tgt_c);
    }

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);

    solver.assert(&view.transition);
    for c in &src_constraints {
        solver.assert(c);
    }
    // ¬tgt_constraints = ¬(∧ tgt_i) = ∨ ¬tgt_i
    let tgt_negs: Vec<z3::ast::Bool> = tgt_constraints.iter().map(|c| c.not()).collect();
    let tgt_negs_refs: Vec<&z3::ast::Bool> = tgt_negs.iter().collect();
    let neg_tgt = if tgt_negs_refs.is_empty() {
        // Empty predicate set — target cube is universal (all bits
        // 0 in a 0-predicate world). Then ¬tgt is `false`, so the
        // conjunction is unsatisfiable trivially → Must.
        z3::ast::Bool::from_bool(false)
    } else {
        z3::ast::Bool::or(&tgt_negs_refs)
    };
    solver.assert(&neg_tgt);

    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

/// Is the source cube's predicate conjunction PROVEN satisfiable: `∃ s. ⋀_i src_i`?
///
/// Returns `true` only when Z3 finds a concrete state satisfying every source
/// predicate of the cube (`SatResult::Sat`). An **infeasible** (empty) cube
/// (`Unsat`) corresponds to no concrete state, and the concrete transition
/// relation is total — so the must-checks ([`smt_per_target_must_check`] and the
/// STS-IR seam's ∀∃ / hyper forms) all fabricate a vacuous `Must` out of it
/// (`src ∧ trans ∧ ¬tgt` is trivially UNSAT when `src` is UNSAT). That vacuous
/// `Sharp` edge is the root of the R.2.5b false-`Violated` / `TritBdd` panic on
/// `$past` models (`docs/design/past-shadow-soundness.md` §6): a cube like
/// `din == V ∧ din__past == V'` with contradictory dimensions on one register is
/// empty, yet promotes a must-edge. Callers gate must-promotion on this check so
/// no must-edge is created out of a proven-empty cube.
///
/// SOUNDNESS — the conservative direction is **not** to promote. `Unsat` (proven
/// empty) and `Unknown` / an unresolved predicate register (cannot prove feasible)
/// both return `false`: dropping a must-edge is always sound (must is an
/// under-approximation used by diamonds and refutation; fewer must-edges can only
/// weaken a definite verdict to ⊥, never flip it). Cube feasibility is a small
/// quantifier-free BV/AUFBV query, so `Unknown` is vanishingly rare in practice.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub fn smt_source_cube_proven_feasible<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> bool
where
    P: PredicateLike,
{
    let mut src_constraints: Vec<z3::ast::Bool> = Vec::new();
    for (i, pred) in predicates.iter().enumerate() {
        let polarity = (src_bits >> i) & 1 == 1;
        // An unresolved register — cannot prove feasible; conservatively drop
        // (the must-check itself already returns Unknown for the same reason).
        let Some(c) = build_pred_constraint(view, nid_map, pred, false, polarity) else {
            return false;
        };
        src_constraints.push(c);
    }

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl);
    }
    solver.set_params(&params);
    for c in &src_constraints {
        solver.assert(c);
    }
    // Only a definite SAT proves the cube non-empty; Unsat / Unknown → drop.
    matches!(solver.check(), z3::SatResult::Sat)
}

/// Tiny trait surface so the must-edge check can consume either
/// `PredicateSpec` (from `kmts_lift`) or test-local predicate types
/// without taking on a kmts_lift dependency here.
pub trait PredicateLike {
    fn register(&self) -> &str;
    fn value(&self) -> u64;
    /// B.1 — when this predicate is a **compound** (a boolean combination of
    /// register comparisons rather than the simple `register == value` atom),
    /// the [`PredicateExpr`] to encode. `None` (the default) ⇒ the simple atom
    /// path, which uses `register()` / `value()` exactly as before. A compound
    /// predicate is still one cube dimension; only its truth encoding changes.
    fn expr(&self) -> Option<&crate::adapter::btor2::predicate_expr::PredicateExpr> {
        None
    }
}

impl PredicateLike for crate::adapter::btor2::kmts_lift::PredicateSpec {
    fn register(&self) -> &str {
        &self.register
    }
    fn value(&self) -> u64 {
        self.value
    }
}

/// B.1 — build one predicate's constraint over the source (`slot_next ==
/// false`, `state_curr` BVs) or target (`slot_next == true`, `state_next` BVs)
/// cube, compound-aware. For a simple predicate (`expr() == None`) this is
/// exactly [`build_predicate_constraint`] — behaviour-preserving. For a
/// compound it builds the recursive constraint via
/// [`crate::adapter::btor2::predicate_expr::PredicateExpr::build_constraint`]
/// over a register→BV lookup and applies the cube polarity. Returns `None` if
/// any referenced register is absent from the view (the caller picks the
/// conservative verdict: `May` for may-checks, `Unknown` for must-checks).
fn build_pred_constraint<P: PredicateLike>(
    view: &Btor2SmtView,
    nid_map: &HashMap<String, Nid>,
    pred: &P,
    slot_next: bool,
    polarity: bool,
) -> Option<z3::ast::Bool> {
    match pred.expr() {
        None => {
            let nid = *nid_map.get(pred.register())?;
            // H.B — free-input predicate: the NID is a primary input, not a
            // state cell. Pin the current-cycle input on the source cube; leave
            // the target free (the next-cycle input is not a variable of the
            // one-step relation, so the input dimension is unconstrained on the
            // target → the edge enumeration reaches every input flavour of the
            // successor — the "for all input sequences" over-approximation).
            if let Some(in_bv) = view.inputs.get(&nid) {
                if slot_next {
                    return Some(z3::ast::Bool::from_bool(true));
                }
                return Some(build_predicate_constraint(in_bv, pred.value(), polarity));
            }
            let bv = if slot_next {
                view.next_state(nid)?
            } else {
                view.curr_state(nid)?
            };
            Some(build_predicate_constraint(bv, pred.value(), polarity))
        }
        Some(e) => {
            let lookup = |reg: &str| -> Option<z3::ast::BV> {
                let nid = *nid_map.get(reg)?;
                let bv = if slot_next {
                    view.next_state(nid)?
                } else {
                    view.curr_state(nid)?
                };
                Some(bv.clone())
            };
            // SEL — array-content lookup: array cells are absent from the BV
            // `nid_map` (not cube dimensions), so resolve by name via
            // `array_name_nid` to the curr/next Z3 `Array` handle, so a `Select`
            // seed's `select(arr, idx)` binds against the same source/target frame
            // as the BV atoms.
            let arr_lookup = |arr: &str| -> Option<z3::ast::Array> {
                let nid = *view.array_name_nid.get(arr)?;
                let a = if slot_next {
                    view.state_next_arr.get(&nid)?
                } else {
                    view.state_curr_arr.get(&nid)?
                };
                Some(a.clone())
            };
            let raw = e.build_constraint_arr(&lookup, &arr_lookup)?;
            Some(if polarity { raw } else { raw.not() })
        }
    }
}

/// H.U.1a — **the uniform rule, source side.** A term's value over the current
/// cycle `(s, i)`: a state leaf (`state_curr`), an input leaf (`inputs`), or a
/// combinational op (`signal_bvs`, the encoder's per-node cache). One lookup, no
/// per-atom-kind branch (cf. [`build_pred_constraint`]'s three branches).
fn term_source_bv(view: &Btor2SmtView, nid: Nid) -> Option<&z3::ast::BV> {
    view.state_curr
        .get(&nid)
        .or_else(|| view.inputs.get(&nid))
        .or_else(|| view.signal_bvs.get(&nid))
}

/// H.U.1a — **the uniform rule, target side.** A term's value over the NEXT
/// cycle `(s', i')`: a state leaf (`state_next`), an input leaf (the fresh `i'`
/// of [`PrimedEnv`]), or a combinational op (the primed node cache). Same shape
/// as [`term_source_bv`], over the primed projection.
fn term_target_bv<'a>(
    view: &'a Btor2SmtView,
    primed: &'a PrimedEnv,
    nid: Nid,
) -> Option<&'a z3::ast::BV> {
    view.state_next
        .get(&nid)
        .or_else(|| primed.inputs.get(&nid))
        .or_else(|| primed.cache.get(&nid))
}

/// H.U.1a — uniform variant of [`build_pred_constraint`]: ONE rule for every
/// atom kind. A predicate's register name resolves to a term nid (`nid_map`),
/// and the term's value is taken over the current cycle (`slot_next == false`,
/// [`term_source_bv`]) or the next cycle (`slot_next == true`,
/// [`term_target_bv`] over the primed cache `(s', i')`). The per-kind branches
/// of `build_pred_constraint` collapse:
/// - a **state** atom → `state_curr` / `state_next` (identical to the per-kind
///   path by construction — a register leaf's primed value IS `state_next`);
/// - a **free input** atom → `inputs` on the source (pinned), `true` on the
///   target (FREE — the H.B design's "environment picks any next input"),
///   identical to `build_pred_constraint`'s input branch for both may and must;
/// - a **compound** → `PredicateExpr::build_constraint` over the same uniform
///   term lookup;
/// - a **combinational** node (out-of-fragment for the per-kind path → `None` →
///   conservative `May`) → its real `(s, i)` / `(s', i')` term — the new
///   capability, latent until a combinational predicate is routed as a cube
///   dimension (H.U.2).
///
/// Returns `None` if a referenced register is absent from `nid_map` OR has no
/// term BV (caller picks the conservative verdict), matching `build_pred_constraint`.
fn build_pred_constraint_uniform<P: PredicateLike>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    nid_map: &HashMap<String, Nid>,
    pred: &P,
    slot_next: bool,
    polarity: bool,
    pin_input_target: bool,
) -> Option<z3::ast::Bool> {
    match pred.expr() {
        None => {
            let nid = *nid_map.get(pred.register())?;
            // H.B design (free-input-atoms.md §"Transition semantics"): a free
            // input's TARGET dimension is FREE ("the environment picks any next
            // input"); the must keeps the pinned input on the SOURCE and leaves
            // it existential on the target — a sound under-approximation. Encode
            // that as the literal `true` on the target, identical to
            // `build_pred_constraint`'s input branch (and `∃`-equivalent on the
            // may-check).
            //
            // `pin_input_target` overrides this for the nested-∀i′ hyper-must
            // (combinational-input-atoms.md §6.2): there the next input `i′` is
            // NOT free — it is universally quantified, so a target predicate over
            // an input (or a combinational function `g(s′,i′)` of one) must be
            // PINNED to its actual next-cycle value (`primed.inputs` / the primed
            // node cache) via `term_target_bv`. Leaving it free is exactly the
            // §5.1 target-free unsoundness the nested query exists to fix.
            if !pin_input_target && slot_next && view.inputs.contains_key(&nid) {
                return Some(z3::ast::Bool::from_bool(true));
            }
            let bv = if slot_next {
                term_target_bv(view, primed, nid)?
            } else {
                term_source_bv(view, nid)?
            };
            Some(build_predicate_constraint(bv, pred.value(), polarity))
        }
        Some(e) => {
            // Compounds are state-only (the lift's compound gate enforces
            // `expr.registers().all(is_state)`), so every leaf resolves to a
            // state cell — `state_curr` / `state_next` via the uniform lookup,
            // identical to `build_pred_constraint`'s compound branch.
            let term_bv = |reg: &str| -> Option<z3::ast::BV> {
                let nid = *nid_map.get(reg)?;
                if slot_next {
                    term_target_bv(view, primed, nid).cloned()
                } else {
                    term_source_bv(view, nid).cloned()
                }
            };
            // SEL — array-content lookup. Array cells are absent from the BV `nid_map`
            // (not cube dimensions), so resolve the array by name via `array_name_nid`;
            // arrays have no primed cache, so the next-cycle array is `state_next_arr`.
            let arr_lookup = |arr: &str| -> Option<z3::ast::Array> {
                let nid = *view.array_name_nid.get(arr)?;
                let a = if slot_next {
                    view.state_next_arr.get(&nid)?
                } else {
                    view.state_curr_arr.get(&nid)?
                };
                Some(a.clone())
            };
            let raw = e.build_constraint_arr(&term_bv, &arr_lookup)?;
            Some(if polarity { raw } else { raw.not() })
        }
    }
}

/// H.U.1a — uniform variant of [`smt_per_target_may_check`]. Identical `∃`
/// query (`SAT(transition ∧ src ∧ tgt)`) but builds each constraint with the
/// uniform rule ([`build_pred_constraint_uniform`]) — the target side reads the
/// primed node cache instead of the per-kind `next_state` / `true` branches.
///
/// **May-equivalent to the per-kind path on every existing predicate kind.** For
/// a state atom the target BV is `state_next` (identical). For a free input the
/// per-kind path used the literal `true` on the target, whereas this uses the
/// fresh `i'` BV — but `i'` appears in NO other constraint and NOT in the
/// transition, so `∃ i'. (i' == value)` is always satisfiable ⟺ the per-kind
/// `true`; the `∃` verdict is unchanged. For a state-register compound the
/// primed leaf BVs are `state_next` (identical). The combinational case is the
/// only behavioural difference (precise vs conservative `May`), and no caller
/// passes a combinational predicate as a cube dimension yet, so the production
/// may-edge set is unchanged — validated by the full lift / e2e suites + the
/// `uniform_may_matches_per_kind` differential.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub(crate) fn smt_per_target_may_check_uniform<P>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMayVerdict
where
    P: PredicateLike,
{
    let mut constraints: Vec<z3::ast::Bool> = Vec::new();
    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let tgt_polarity = (tgt_bits >> i) & 1 == 1;
        let (Some(src_c), Some(tgt_c)) = (
            build_pred_constraint_uniform(view, primed, nid_map, pred, false, src_polarity, false),
            build_pred_constraint_uniform(view, primed, nid_map, pred, true, tgt_polarity, false),
        ) else {
            return SmtMayVerdict::May;
        };
        constraints.push(src_c);
        constraints.push(tgt_c);
    }

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);
    solver.assert(&view.transition);
    for c in &constraints {
        solver.assert(c);
    }
    match solver.check() {
        z3::SatResult::Sat => SmtMayVerdict::May,
        z3::SatResult::Unsat => SmtMayVerdict::NotMay,
        z3::SatResult::Unknown => SmtMayVerdict::May,
    }
}

/// R.2.5b session-2 follow-up (2026-06-09) — build per-predicate
/// constraints for both the source cube (over `state_curr` BVs) and
/// the target cube (over `state_next` BVs). Shared helper so the
/// standard ∀∃ check + the hyper-must check don't duplicate the
/// per-predicate plumbing. Returns `None` if any predicate's
/// register isn't in `nid_map` (caller treats as Unknown verdict).
fn build_src_tgt_constraints<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
) -> Option<(Vec<z3::ast::Bool>, Vec<z3::ast::Bool>)>
where
    P: PredicateLike,
{
    let mut src = Vec::with_capacity(predicates.len());
    let mut tgt = Vec::with_capacity(predicates.len());
    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let tgt_polarity = (tgt_bits >> i) & 1 == 1;
        // B.1 — compound-aware (simple atom is the `expr() == None` branch).
        src.push(build_pred_constraint(
            view,
            nid_map,
            pred,
            false,
            src_polarity,
        )?);
        tgt.push(build_pred_constraint(
            view,
            nid_map,
            pred,
            true,
            tgt_polarity,
        )?);
    }
    Some((src, tgt))
}

/// R.2.5b session-2 follow-up (2026-06-09) — conjoin a vec of
/// per-predicate constraints into a single Bool. Returns
/// `Bool::from_bool(true)` for an empty input (vacuous conjunction).
fn conj_bool(constraints: &[z3::ast::Bool]) -> z3::ast::Bool {
    if constraints.is_empty() {
        z3::ast::Bool::from_bool(true)
    } else {
        let refs: Vec<&z3::ast::Bool> = constraints.iter().collect();
        z3::ast::Bool::and(&refs)
    }
}

/// mununu#512 — build the `∀`-quantified inner body for a must-edge query,
/// SUBSTITUTING the next state instead of quantifying over it.
///
/// The next state is FUNCTIONALLY DETERMINED: BTOR2 `next` is a function per state
/// cell. The original encoding introduced fresh `state_next` / `state_next_arr`
/// constants, CONSTRAINED them with `transition`, and then `∀`-bound them —
/// semantically equivalent, and noted as such in the code, but binding the next-state
/// ARRAY moves the query from QF_AUFBV (decidable) into AUFBV + `∀`-array (not). Z3
/// answers `Unknown`, `Unknown` is read as `NotMust`, and the νµ starves for
/// must-edges on every array-bearing design.
///
/// Substituting `state_next := next_expr(state_curr, inputs)` removes the variables,
/// so no `∀`-array remains and the `transition` conjunct is unnecessary — it is the
/// definition being substituted. The remaining `∀` ranges over input bit-vectors only.
///
/// **SOUNDNESS: a completeness fix, not a semantic change.** The substituted query is
/// equivalent to the quantified one, so a proven `Must` is genuinely a must-edge; the
/// only movement is `Unknown → Must`. When the view carries no recorded expressions
/// (built without `transition`), the previous encoding is used unchanged rather than
/// silently weakening the query.
fn must_inner_body_and_bounds(
    view: &Btor2SmtView,
    tgt_conj: &z3::ast::Bool,
) -> (z3::ast::Bool, Vec<z3::ast::BV>, Vec<z3::ast::Array>) {
    use z3::ast::Ast as _;
    if view.state_next_expr.is_empty() && view.state_next_arr_expr.is_empty() {
        let body = z3::ast::Bool::and(&[&view.transition, tgt_conj]).not();
        return (
            body,
            universal_bound_bvs(view),
            universal_bound_arrays(view),
        );
    }
    let mut body = tgt_conj.clone();
    let bv_subs: Vec<(z3::ast::BV, z3::ast::BV)> = view
        .state_next
        .iter()
        .filter_map(|(nid, var)| {
            view.state_next_expr
                .get(nid)
                .map(|e| (var.clone(), e.clone()))
        })
        .collect();
    if !bv_subs.is_empty() {
        let pairs: Vec<(&z3::ast::BV, &z3::ast::BV)> =
            bv_subs.iter().map(|(a, b)| (a, b)).collect();
        body = body.substitute(&pairs);
    }
    let arr_subs: Vec<(z3::ast::Array, z3::ast::Array)> = view
        .state_next_arr
        .iter()
        .filter_map(|(nid, var)| {
            view.state_next_arr_expr
                .get(nid)
                .map(|e| (var.clone(), e.clone()))
        })
        .collect();
    if !arr_subs.is_empty() {
        let pairs: Vec<(&z3::ast::Array, &z3::ast::Array)> =
            arr_subs.iter().map(|(a, b)| (a, b)).collect();
        body = body.substitute(&pairs);
    }
    (
        body.not(),
        view.inputs.values().cloned().collect(),
        Vec::new(),
    )
}

/// R.2.5b session-2 follow-up (2026-06-09) — collect the universal-
/// quantification bound vars (inputs + state_next BVs) for the ∀∃
/// must-edge check. Returns a Vec of cloned BVs whose references the
/// caller wraps as `&dyn Ast` for `forall_const`.
fn universal_bound_bvs(view: &Btor2SmtView) -> Vec<z3::ast::BV> {
    let mut bvs: Vec<z3::ast::BV> = Vec::new();
    for bv in view.inputs.values() {
        bvs.push(bv.clone());
    }
    for bv in view.state_next.values() {
        bvs.push(bv.clone());
    }
    bvs
}

/// SEL — the next-step ARRAY handles that must ALSO be universally bound in the ∀∃
/// must-edge check. The transition constrains each memory cell's next value
/// (`s_next_arr == write(s_arr, …)` / `ite(…)`), so the `state_next_arr` variable is a
/// next-state component exactly like a `state_next` BV. Omitting it leaves the array's
/// next-state FREE under the `∀`, which lets the solver pick an array that falsifies the
/// transition — trivially satisfying `¬(transition ∧ tgt)` and spuriously reporting
/// `NotMust` for EVERY array-bearing source. Binding it makes the `∀` range over the full
/// next-state (a sound completeness fix: it can only turn spurious `NotMust` into a
/// Z3-proven `Must`). Empty under `Theory::BvOnly`.
fn universal_bound_arrays(view: &Btor2SmtView) -> Vec<z3::ast::Array> {
    view.state_next_arr.values().cloned().collect()
}

/// R.2.5b session-2 follow-up (2026-06-09) — standard ∀∃ form of
/// the SMT must-edge query for one (source-cube, target-cube) pair.
///
/// **Semantics (per [`docs/design/kmts-theory.md`] standard KMTS):**
/// the must-edge `(src, label, tgt)` holds iff
///
/// ```text
/// ∀ state ⊨ src. ∃ inputs. ∃ state_next. (transition(state, inputs, state_next) ∧ state_next ⊨ tgt)
/// ```
///
/// This is **more permissive** than the MVP's ∀∀ form
/// ([`smt_per_target_must_check`]) — the ∀∀ form requires every
/// input combination to reach tgt; the ∀∃ standard form only
/// requires SOME input combination per concrete source state. Every
/// must-edge the ∀∀ form proves is also proved by the ∀∃ form, so
/// the ∀∃ form produces a STRICT SUPERSET of Sharp promotions.
///
/// **Encoding via Z3 quantifier alternation:**
///
/// Negation of the must-edge condition:
/// `∃ state ⊨ src. ∀ inputs. ∀ state_next. ¬(transition ∧ state_next ⊨ tgt)`
///
/// We build the inner formula and SAT-check:
/// `src_constraints(state_curr) ∧ forall_const([inputs, state_next], ¬(transition ∧ tgt_constraints))`
///
/// - SAT ⇒ negation holds ⇒ NotMust.
/// - UNSAT ⇒ negation refuted ⇒ Must.
/// - Unknown ⇒ Unknown (timeout / unsupported operator).
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub fn smt_per_target_must_check_standard<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    let Some((src_constraints, tgt_constraints)) =
        build_src_tgt_constraints(view, src_bits, tgt_bits, predicates, nid_map)
    else {
        return SmtMustVerdict::Unknown;
    };

    // mununu#512 — SUBSTITUTE the next state instead of quantifying over it.
    //
    // The next state is FUNCTIONALLY DETERMINED: BTOR2 `next` is a function per
    // state cell, so `state_next` is not an independent variable. The previous
    // encoding introduced fresh `state_next` / `state_next_arr` constants, CONSTRAINED
    // them with `transition`, and then `∀`-bound them. That is semantically
    // equivalent, but binding the next-state ARRAY moves the query from QF_AUFBV
    // (decidable) into AUFBV + ∀-array (not) — Z3 answers `Unknown`, `Unknown` is
    // read as `NotMust`, and the νµ starves for must-edges on every array-bearing
    // design.
    //
    // Substituting `state_next := next_expr(state_curr, inputs)` removes the
    // variables entirely, so no `∀`-array remains and the `transition` conjunct is
    // unnecessary (it is the definition being substituted). The remaining `∀` is over
    // finite input bit-vectors.
    //
    // SOUNDNESS: this is a COMPLETENESS fix, not a semantic change. The substituted
    // query is equivalent to the quantified one, so a proven `Must` is genuinely a
    // must-edge; the only movement is `Unknown → Must`.
    use z3::ast::Ast as _;
    let tgt_conj = conj_bool(&tgt_constraints);
    let (inner_body, bound_refs_owned) =
        if view.state_next_expr.is_empty() && view.state_next_arr_expr.is_empty() {
            // No recorded expressions (a view built without `transition`) — keep the
            // previous encoding rather than silently weaken the query.
            let body = z3::ast::Bool::and(&[&view.transition, &tgt_conj]).not();
            let mut bvs = universal_bound_bvs(view);
            bvs.retain(|_| true);
            (body, (bvs, universal_bound_arrays(view)))
        } else {
            let mut body = tgt_conj.clone();
            // BV next-state variables → their defining expressions.
            let bv_subs: Vec<(z3::ast::BV, z3::ast::BV)> = view
                .state_next
                .iter()
                .filter_map(|(nid, var)| {
                    view.state_next_expr
                        .get(nid)
                        .map(|e| (var.clone(), e.clone()))
                })
                .collect();
            if !bv_subs.is_empty() {
                let pairs: Vec<(&z3::ast::BV, &z3::ast::BV)> =
                    bv_subs.iter().map(|(a, b)| (a, b)).collect();
                body = body.substitute(&pairs);
            }
            // Array next-state variables → their defining `ite`/`store` expressions.
            let arr_subs: Vec<(z3::ast::Array, z3::ast::Array)> = view
                .state_next_arr
                .iter()
                .filter_map(|(nid, var)| {
                    view.state_next_arr_expr
                        .get(nid)
                        .map(|e| (var.clone(), e.clone()))
                })
                .collect();
            if !arr_subs.is_empty() {
                let pairs: Vec<(&z3::ast::Array, &z3::ast::Array)> =
                    arr_subs.iter().map(|(a, b)| (a, b)).collect();
                body = body.substitute(&pairs);
            }
            // Only the INPUTS remain universally bound — finite bit-vectors.
            let inputs: Vec<z3::ast::BV> = view.inputs.values().cloned().collect();
            (body.not(), (inputs, Vec::new()))
        };
    let (bound_bvs, bound_arrs) = bound_refs_owned;
    let mut bound_refs: Vec<&dyn z3::ast::Ast> =
        bound_bvs.iter().map(|bv| bv as &dyn z3::ast::Ast).collect();
    bound_refs.extend(bound_arrs.iter().map(|a| a as &dyn z3::ast::Ast));
    let universal = z3::ast::forall_const(&bound_refs, &[], &inner_body);

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);

    // Assert src_constraints (state_curr is existentially free at
    // the top level — the outer ∃ in the negation).
    for c in &src_constraints {
        solver.assert(c);
    }
    solver.assert(&universal);

    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

/// R.2.5b session-2 follow-up (2026-06-09) — SMT-backed hyper-must
/// check over a target SET T.
///
/// **Semantics (per [`docs/design/kmts-theory.md`] §7.4 generalised
/// KMTS hyper-must):** the hyper-must edge `(src, label, T)` holds iff
///
/// ```text
/// ∀ state ⊨ src. ∃ inputs. ∃ t ∈ T. ∃ state_next. (transition(state, inputs, state_next) ∧ state_next ⊨ t)
/// ```
///
/// The abstraction guarantees some t ∈ T is reached, but not
/// necessarily the same t across concrete states. This is the
/// standard hyper-must reading from Shoham–Grumberg LMCS 2007 §4.
///
/// **Encoding:** same shape as the per-target ∀∃ check, but with
/// the target-side constraint being a disjunction over the candidate
/// targets in `target_bits_set`:
///
/// `src_constraints ∧ forall_const([inputs, state_next], ¬(transition ∧ ⋁_t tgt_t_constraints))`
///
/// - SAT ⇒ some state in src can escape every t ∈ T ⇒ NotMust.
/// - UNSAT ⇒ every state in src has some (input, t ∈ T) witness ⇒ Must.
/// - Unknown ⇒ Unknown.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub fn smt_hyper_must_check<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    target_bits_set: &[u64],
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    if target_bits_set.is_empty() {
        // Empty target set — no possible witnesses; trivially not a must.
        return SmtMustVerdict::NotMust;
    }

    // Build src constraints once (state_curr is shared across the
    // disjunction's target constraints).
    let Some((src_constraints, _placeholder)) = build_src_tgt_constraints(
        view, src_bits, 0, // tgt_bits placeholder — not used below; we rebuild per target
        predicates, nid_map,
    ) else {
        return SmtMustVerdict::Unknown;
    };

    // Build a target-conjunction per candidate, then OR them together.
    let mut tgt_disjuncts: Vec<z3::ast::Bool> = Vec::with_capacity(target_bits_set.len());
    for &tgt_bits in target_bits_set {
        let Some((_, tgt_constraints)) =
            build_src_tgt_constraints(view, src_bits, tgt_bits, predicates, nid_map)
        else {
            return SmtMustVerdict::Unknown;
        };
        tgt_disjuncts.push(conj_bool(&tgt_constraints));
    }
    let tgt_disjuncts_refs: Vec<&z3::ast::Bool> = tgt_disjuncts.iter().collect();
    let any_tgt = z3::ast::Bool::or(&tgt_disjuncts_refs);

    // mununu#512 — substitute the next state rather than quantify over it.
    let (inner_body, bound_bvs, bound_arrs) = must_inner_body_and_bounds(view, &any_tgt);
    let mut bound_refs: Vec<&dyn z3::ast::Ast> =
        bound_bvs.iter().map(|bv| bv as &dyn z3::ast::Ast).collect();
    bound_refs.extend(bound_arrs.iter().map(|a| a as &dyn z3::ast::Ast));
    let universal = z3::ast::forall_const(&bound_refs, &[], &inner_body);

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);

    for c in &src_constraints {
        solver.assert(c);
    }
    solver.assert(&universal);

    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

/// Nested-∀i′ hyper-must (combinational-input-atoms.md §6.2) — the sound
/// treatment of a **combinational-of-input** (or raw-input) predicate in
/// **target position**, where the shipped `smt_hyper_must_check` is unsound
/// because it leaves the next input free (target-free, refuted by §5.1).
///
/// **Semantics.** The hyper-must edge `src →□ T` (`T` a target *set*) holds iff
///
/// ```text
/// ∀ (s,i) ∈ γ(src).  ∀ i′.  ∃ c′ ∈ T.  (δ(s,i), i′) ∈ γ(c′)
/// ```
///
/// — for every source pair in the cube AND every *next* input `i′`, the
/// determined successor `(δ(s,i), i′)` lands in some target cube of `T`. The
/// next input `i′` is **universally** quantified, nested inside the transition
/// (§5.2: an existential `i′` is angelic and unsound). A target predicate that
/// reads `i′` — an input, or a combinational `g(s′,i′)` — is therefore pinned to
/// its actual next-cycle value via the `PrimedEnv` (`pin_input_target = true`),
/// not left free.
///
/// **Why a target *set* (Proposition 5).** A singleton `T = {c′}` forces
/// `g(s′,i′) = c′(p)` for *all* `i′`; if `g` genuinely depends on `i′` no
/// singleton is reached for every `i′`, so the set must include both polarities.
///
/// **Encoding.** The must condition is `∀(s,i). ∀i′. ⋁_{c′∈T} succ∈γ(c′)`, whose
/// negation is *fully existential* — no quantifier alternation is needed:
///
/// ```text
/// SAT?  src(state_curr, inputs) ∧ transition ∧ ⋀_{c′∈T} ¬(succ ∈ γ(c′))
/// ```
///
/// where `succ ∈ γ(c′)` is built with the uniform rule over the primed image
/// `(state_next, i′)`. SAT ⇒ some `(s,i,i′)` escapes every `c′` ⇒ `NotMust`;
/// UNSAT ⇒ `Must` (Proposition 4 — a sound GKMTS hyper-must under-approximation).
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
///
/// Slice 1 of the nested-∀i′ track: the SMT core + its Prop 1/4/5 differential
/// guards. It is exercised by the tests below but not yet wired into the
/// production `MustEdgeInference` dispatch (a follow-up slice adds a
/// `SmtNestedHyperMust` variant + the target-position seeder routing), so it is
/// dead in non-test builds until then.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn smt_nested_hyper_must_check<P>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    src_bits: u64,
    target_bits_set: &[u64],
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    if target_bits_set.is_empty() {
        // No candidate target — no witness can cover the demonic i′.
        return SmtMustVerdict::NotMust;
    }

    // Source constraints: (s,i) ⊨ src, over the current-cycle image. These are
    // asserted at top level (the source pair is existential in the negation).
    let mut src_constraints = Vec::with_capacity(predicates.len());
    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let Some(c) =
            build_pred_constraint_uniform(view, primed, nid_map, pred, false, src_polarity, false)
        else {
            return SmtMustVerdict::Unknown;
        };
        src_constraints.push(c);
    }

    // For each candidate target cube c′: ¬(successor ∈ γ(c′)), i.e. NOT all of
    // c′'s predicates hold at (state_next, i′). The target side PINS the next
    // input (`pin_input_target = true`), so an input / combinational-of-input
    // target reads its real primed value rather than the (unsound) `true`.
    let mut not_in_any: Vec<z3::ast::Bool> = Vec::with_capacity(target_bits_set.len());
    for &tgt_bits in target_bits_set {
        let mut tgt = Vec::with_capacity(predicates.len());
        for (i, pred) in predicates.iter().enumerate() {
            let tgt_polarity = (tgt_bits >> i) & 1 == 1;
            let Some(c) = build_pred_constraint_uniform(
                view,
                primed,
                nid_map,
                pred,
                true,
                tgt_polarity,
                true,
            ) else {
                return SmtMustVerdict::Unknown;
            };
            tgt.push(c);
        }
        not_in_any.push(conj_bool(&tgt).not());
    }

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);

    for c in &src_constraints {
        solver.assert(c);
    }
    // Links state_curr + inputs → state_next (= δ(s,i)); the primed cache is a
    // function of the SAME state_next consts, so the (s,i)→s′→(s′,i′) chain is
    // connected.
    solver.assert(&view.transition);
    for n in &not_in_any {
        solver.assert(n);
    }

    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

// ─────────────────────────────────────────────────────────────────────
// H.U.1c (2026-06-29) — uniform must / hyper-must, on the uniform image.
// ─────────────────────────────────────────────────────────────────────

/// H.U.1c — uniform variant of [`build_src_tgt_constraints`]: builds the source
/// (current-cycle) + target (next-cycle) per-predicate constraints with the
/// uniform rule ([`build_pred_constraint_uniform`]). `None` if any predicate's
/// register is unresolved (caller → `Unknown`), matching the per-kind helper.
fn build_src_tgt_constraints_uniform<P>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
) -> Option<(Vec<z3::ast::Bool>, Vec<z3::ast::Bool>)>
where
    P: PredicateLike,
{
    let mut src = Vec::with_capacity(predicates.len());
    let mut tgt = Vec::with_capacity(predicates.len());
    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let tgt_polarity = (tgt_bits >> i) & 1 == 1;
        src.push(build_pred_constraint_uniform(
            view,
            primed,
            nid_map,
            pred,
            false,
            src_polarity,
            false,
        )?);
        tgt.push(build_pred_constraint_uniform(
            view,
            primed,
            nid_map,
            pred,
            true,
            tgt_polarity,
            false,
        )?);
    }
    Some((src, tgt))
}

/// H.U.1c — uniform variant of [`smt_per_target_must_check_standard`]. Identical
/// `∀∃` query (`src ∧ ∀[inputs, state_next]. ¬(transition ∧ tgt)`, UNSAT ⇒ Must),
/// built with the uniform rule.
///
/// **Behaviour-identical to the per-kind path on every existing cube dimension**
/// (state / free-input / state-compound): a state target's BV is `state_next`
/// (the same const the per-kind `next_state` returns), a free-input target is
/// `true` (target-free, the H.B design), and a state-compound's leaves are
/// `state_next` — so `build_pred_constraint_uniform` produces the *same* Z3 terms
/// as `build_pred_constraint`. The primed cache + the `∀ bound` are therefore
/// untouched relative to the per-kind path (the `∀ bound` stays `inputs +
/// state_next`; the next-cycle `i'` is referenced by no existing target
/// constraint). The new precision — a combinational-of-state target read from
/// the primed cache — is latent until such a predicate is routed as a cube
/// dimension (H.U.2). The `uniform_must_matches_per_kind` differential pins the
/// equivalence.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub(crate) fn smt_per_target_must_check_uniform<P>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    let Some((src_constraints, tgt_constraints)) =
        build_src_tgt_constraints_uniform(view, primed, src_bits, tgt_bits, predicates, nid_map)
    else {
        return SmtMustVerdict::Unknown;
    };
    let tgt_conj = conj_bool(&tgt_constraints);
    // mununu#512 — substitute the next state rather than quantify over it.
    let (inner_body, bound_bvs, bound_arrs) = must_inner_body_and_bounds(view, &tgt_conj);
    let mut bound_refs: Vec<&dyn z3::ast::Ast> =
        bound_bvs.iter().map(|bv| bv as &dyn z3::ast::Ast).collect();
    bound_refs.extend(bound_arrs.iter().map(|a| a as &dyn z3::ast::Ast));
    let universal = z3::ast::forall_const(&bound_refs, &[], &inner_body);

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);
    for c in &src_constraints {
        solver.assert(c);
    }
    solver.assert(&universal);
    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

/// H.U.1c — uniform variant of [`smt_hyper_must_check`] (GKMTS hyper-must over a
/// target SET). Same disjunction-of-targets `∀∃` encoding, built with the
/// uniform rule; behaviour-identical to the per-kind path on existing cube
/// dimensions (see [`smt_per_target_must_check_uniform`]).
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
pub(crate) fn smt_hyper_must_check_uniform<P>(
    view: &Btor2SmtView,
    primed: &PrimedEnv,
    src_bits: u64,
    target_bits_set: &[u64],
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMustVerdict
where
    P: PredicateLike,
{
    if target_bits_set.is_empty() {
        return SmtMustVerdict::NotMust;
    }
    let Some((src_constraints, _placeholder)) =
        build_src_tgt_constraints_uniform(view, primed, src_bits, 0, predicates, nid_map)
    else {
        return SmtMustVerdict::Unknown;
    };
    let mut tgt_disjuncts: Vec<z3::ast::Bool> = Vec::with_capacity(target_bits_set.len());
    for &tgt_bits in target_bits_set {
        let Some((_, tgt_constraints)) = build_src_tgt_constraints_uniform(
            view, primed, src_bits, tgt_bits, predicates, nid_map,
        ) else {
            return SmtMustVerdict::Unknown;
        };
        tgt_disjuncts.push(conj_bool(&tgt_constraints));
    }
    let tgt_disjuncts_refs: Vec<&z3::ast::Bool> = tgt_disjuncts.iter().collect();
    let any_tgt = z3::ast::Bool::or(&tgt_disjuncts_refs);
    // mununu#512 — substitute the next state rather than quantify over it.
    let (inner_body, bound_bvs, bound_arrs) = must_inner_body_and_bounds(view, &any_tgt);
    let mut bound_refs: Vec<&dyn z3::ast::Ast> =
        bound_bvs.iter().map(|bv| bv as &dyn z3::ast::Ast).collect();
    bound_refs.extend(bound_arrs.iter().map(|a| a as &dyn z3::ast::Ast));
    let universal = z3::ast::forall_const(&bound_refs, &[], &inner_body);

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);
    for c in &src_constraints {
        solver.assert(c);
    }
    solver.assert(&universal);
    match solver.check() {
        z3::SatResult::Unsat => SmtMustVerdict::Must,
        z3::SatResult::Sat => SmtMustVerdict::NotMust,
        z3::SatResult::Unknown => SmtMustVerdict::Unknown,
    }
}

// ─────────────────────────────────────────────────────────────────────
// MIG-3 (S-track migration, 2026-06-13) — sound SMT may-edge query.
// ─────────────────────────────────────────────────────────────────────

/// Verdict of the SMT may-edge query ([`smt_per_target_may_check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtMayVerdict {
    /// A concrete `(state ⊨ src, inputs, next ⊨ tgt)` witness exists
    /// (SAT): `src → tgt` is a sound may-edge. Also returned
    /// **conservatively on Unknown** (timeout) and when a predicate's
    /// register/handle is unresolved — the may relation must
    /// over-approximate, so only PROVEN-impossible edges are excluded.
    May,
    /// Z3 proved no witness exists (UNSAT): `src → tgt` is impossible
    /// and is excluded from the may relation.
    NotMay,
}

/// MIG-3 — the sound SMT may-edge query for one
/// (source-cube, target-cube) pair. This is the existential **dual** of
/// [`smt_per_target_must_check`]: it asserts
/// `transition ∧ src_constraints ∧ tgt_constraints` (target POSITIVE,
/// not negated) and returns [`SmtMayVerdict::May`] iff SAT — i.e. a
/// concrete `(s ⊨ src, inputs, s' ⊨ tgt)` witness exists. This is the
/// may-relation definition `R_may(b, b') ⟺ ∃ s ⊨ b, s' ⊨ b'. (s,s') ∈ R`.
///
/// **Why this is the sound replacement for sampling.** The
/// `predicate_cube_lift` sampling may-edges enumerate only a *subset*
/// of inputs per cube, so they can MISS a real may-edge → an
/// under-approximation of the may relation → unsound for safety (a
/// violation reachable only via an unsampled transition is lost). This
/// SMT query is exact: an edge is excluded ONLY when Z3 proves no
/// witness exists. On Unknown / unresolved predicates it conservatively
/// returns `May` (over-approximation).
///
/// `src_bits` / `tgt_bits` are predicate-polarity bitmasks (bit `i` =
/// predicate `predicates[i]` holds), exactly as in
/// [`smt_per_target_must_check`].
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
//
// Consumed by `predicate_cube_lift`'s `MayEdgeInference::SmtAllPairs`
// policy (MIG-3.2) — the sound all-pairs may relation.
pub fn smt_per_target_may_check<P>(
    view: &Btor2SmtView,
    src_bits: u64,
    tgt_bits: u64,
    predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    timeout_ms: u32,
) -> SmtMayVerdict
where
    P: PredicateLike,
{
    let mut constraints: Vec<z3::ast::Bool> = Vec::new();

    for (i, pred) in predicates.iter().enumerate() {
        let src_polarity = (src_bits >> i) & 1 == 1;
        let tgt_polarity = (tgt_bits >> i) & 1 == 1;
        // B.1 — compound-aware. An unresolved register → conservatively include
        // the edge (over-approximation: never exclude an edge we can't rule out).
        let (Some(src_c), Some(tgt_c)) = (
            build_pred_constraint(view, nid_map, pred, false, src_polarity),
            build_pred_constraint(view, nid_map, pred, true, tgt_polarity),
        ) else {
            return SmtMayVerdict::May;
        };
        constraints.push(src_c);
        constraints.push(tgt_c);
    }

    let solver = z3::Solver::new();
    let mut params = z3::Params::new();
    params.set_u32("timeout", timeout_ms);
    if let Some(rl) = cube_smt_rlimit() {
        params.set_u32("rlimit", rl); // ③b deterministic bound (SOUNDNESS: Unknown → NotMust → weaker must)
    }
    solver.set_params(&params);

    solver.assert(&view.transition);
    for c in &constraints {
        solver.assert(c);
    }

    match solver.check() {
        z3::SatResult::Sat => SmtMayVerdict::May,
        z3::SatResult::Unsat => SmtMayVerdict::NotMay,
        // Conservative: an undecided edge stays in the (over-approximate)
        // may relation.
        z3::SatResult::Unknown => SmtMayVerdict::May,
    }
}

/// H.E.2 / H.F — the per-cube 3-valued label of a **derived predicate**: a
/// combinational-of-input atom (`signal == value`, `signal` a combinational node)
/// OR (H.F) a *relational* whose operands include an input / combinational signal
/// (`cnt_q >= cfg_detect_timer_i`, `trigger_i != trigger_active`). Neither is a
/// sound cube *dimension* (its next-cycle value depends on the demonic next
/// input), so it is LABELLED per cube — KleeneT/F where the cube + the design's
/// combinational logic pin it, KleeneBot where the free input swings it.
///
/// `cube_bits` + `cube_predicates` define the cube C (its dimension predicates,
/// over state + free inputs); `derived` is the predicate to label, evaluated at
/// the **current cycle** via the uniform source lookup ([`term_source_bv`]:
/// state_curr ∪ inputs ∪ combinational signal cache) — so a relational with an
/// input or combinational operand resolves (the legacy `build_pred_constraint`
/// compound lookup reads `curr_state` only and could not).
///
/// Decides, over all concrete states ⊨ C (current cycle):
/// - `UNSAT(C ∧ ¬derived)` → `KleeneT` (derived always holds in C — e.g.
///   `trigger_i != trigger_active` when `trigger_active = ~trigger_i`);
/// - `UNSAT(C ∧ derived)` → `KleeneF` (never holds in C);
/// - else → `KleeneBot` (the cube doesn't pin it — honest; e.g. `cnt_q >= cfg_*`
///   with both operands free).
///
/// **Soundness:** a definite KleeneT/F is a sound label (standard 3-valued
/// preservation); the empty-cube guard prevents a vacuous definite. Timeout or an
/// unresolvable operand → conservative `KleeneBot`.
///
/// **Caller must hold a [`z3::with_z3_config`] scope.**
///
/// Resolve a derived predicate's operand NAME to its current-cycle source BV:
/// state / input via `nid_map` (→ `state_curr` / `inputs`), or a combinational
/// signal via the encoder's `view.signals` symbol table (→ `curr_signal`). The
/// combinational case MUST go through `view.signals` (not `nid_map` +
/// `term_source_bv`): an `output`-line symbol's NID differs from its driving
/// node's NID, and only `view.signals` maps the symbol to the node that owns a
/// `signal_bvs` entry — `nid_map` alone misses it, which would drop a definite
/// combinational label to KleeneBot.
fn derived_source_bv<'a>(
    view: &'a Btor2SmtView,
    nid_map: &HashMap<String, Nid>,
    name: &str,
) -> Option<&'a z3::ast::BV> {
    if let Some(&nid) = nid_map.get(name)
        && let Some(bv) = view.state_curr.get(&nid).or_else(|| view.inputs.get(&nid))
    {
        return Some(bv);
    }
    view.signals
        .iter()
        .find(|s| s.symbol.as_deref() == Some(name))
        .and_then(|s| view.curr_signal(s.nid))
}

pub fn smt_combinational_label<P: PredicateLike>(
    view: &Btor2SmtView,
    cube_bits: u64,
    cube_predicates: &[P],
    nid_map: &HashMap<String, Nid>,
    derived: &P,
    timeout_ms: u32,
) -> crate::clts::Tristate {
    use crate::clts::Tristate;
    // The derived predicate's CURRENT-cycle constraint. `None` (an operand absent
    // from the design) → honest KleeneBot.
    let pred_bool = match derived.expr() {
        Some(e) => {
            let lookup = |reg: &str| -> Option<z3::ast::BV> {
                derived_source_bv(view, nid_map, reg).cloned()
            };
            // SEL — current-cycle array lookup for a derived Select predicate.
            // Array cells resolve by name via `array_name_nid` (absent from `nid_map`).
            let arr_lookup = |arr: &str| -> Option<z3::ast::Array> {
                let nid = *view.array_name_nid.get(arr)?;
                view.state_curr_arr.get(&nid).cloned()
            };
            match e.build_constraint_arr(&lookup, &arr_lookup) {
                Some(b) => b,
                None => return Tristate::KleeneBot,
            }
        }
        None => {
            let Some(bv) = derived_source_bv(view, nid_map, derived.register()) else {
                return Tristate::KleeneBot;
            };
            build_predicate_constraint(bv, derived.value(), true)
        }
    };
    // Cube C = conjunction of the dimension predicates at this cube's polarities,
    // over the current cycle. A predicate whose constraint can't be built is
    // omitted (sound — only widens C; see the doc note).
    let mut cube: Vec<z3::ast::Bool> = Vec::new();
    for (b, pred) in cube_predicates.iter().enumerate() {
        let polarity = (cube_bits >> b) & 1 == 1;
        if let Some(c) = build_pred_constraint(view, nid_map, pred, false, polarity) {
            cube.push(c);
        }
    }
    let eq = pred_bool;

    let check = |extra: &z3::ast::Bool| -> z3::SatResult {
        let solver = z3::Solver::new();
        let mut params = z3::Params::new();
        params.set_u32("timeout", timeout_ms);
        solver.set_params(&params);
        for c in &cube {
            solver.assert(c);
        }
        solver.assert(extra);
        solver.check()
    };

    // SOUNDNESS GUARD (H.E.2, 2026-06-28) — empty/unreachable cube. If the cube's
    // own dimension constraints are UNSAT (no concrete state satisfies them), then
    // BOTH `C ∧ signal == value` and `C ∧ signal != value` are trivially UNSAT, and
    // the `KleeneF` check below would fire SPURIOUSLY — a definite label on an empty
    // cube. That spurious definite is what fabricated the VIOLATED on shipped
    // OpenTitan SVA (the `!trigger_active` antecedents). An empty cube has no
    // states, so its label is meaningless: return KleeneBot (honest "undetermined").
    // A definite KleeneT/F is only emitted for a NON-empty cube below, where it is
    // a sound fact over that cube's concrete states.
    if matches!(check(&z3::ast::Bool::from_bool(true)), z3::SatResult::Unsat) {
        return Tristate::KleeneBot;
    }
    // never == value?  UNSAT(C ∧ signal == value) → KleeneF.
    if matches!(check(&eq), z3::SatResult::Unsat) {
        return Tristate::KleeneF;
    }
    // always == value?  UNSAT(C ∧ signal != value) → KleeneT.
    if matches!(check(&eq.not()), z3::SatResult::Unsat) {
        return Tristate::KleeneT;
    }
    // Both satisfiable (mixed), or undecided → honest KleeneBot.
    Tristate::KleeneBot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::kmts_lift::PredicateSpec;
    use crate::adapter::btor2::parser::parse;
    use crate::adapter::sidecar::predicate_image::btor2_encode::{encode_design, encode_primed};

    // ③b — the deterministic cube must-edge rlimit is opt-in and validates its input:
    // a positive integer bounds the queries; unset / 0 / negative / garbage = no bound.
    #[test]
    fn parse_cube_smt_rlimit_only_accepts_a_positive_integer() {
        assert_eq!(
            parse_cube_smt_rlimit(Some("2000000".into())),
            Some(2_000_000)
        );
        assert_eq!(parse_cube_smt_rlimit(Some("  4096 ".into())), Some(4096));
        assert_eq!(parse_cube_smt_rlimit(None), None); // unset ⇒ no bound (historical)
        assert_eq!(parse_cube_smt_rlimit(Some("0".into())), None); // 0 ⇒ disabled
        assert_eq!(parse_cube_smt_rlimit(Some("-5".into())), None); // negative ⇒ no bound
        assert_eq!(parse_cube_smt_rlimit(Some("lots".into())), None); // garbage ⇒ no bound
    }

    /// R.2.5b session 2 — deterministic transition `reg_a := 0`
    /// (input is dropped). Predicate `p:reg_a==0`. A transition
    /// from cube `{p}` (i.e. reg_a==0) goes deterministically
    /// to cube `{p}` again — Z3 must prove this Sharp.
    const DETERMINISTIC_ZERO_BTOR2: &str = "\
1 sort bitvec 1
2 state 1 reg_a
3 zero 1
4 init 1 2 3
5 next 1 2 3
";

    /// R.2.5b session 2 — input-driven transition `reg_a := in_a`.
    /// Predicate `p:reg_a==0`. From src cube `{p}`, the target
    /// depends on input: input=0 → tgt `{p}`; input=1 → tgt `{¬p}`.
    /// Z3 must conclude NotMust on the src→`{p}` query (input=1
    /// escapes).
    const INPUT_DRIVEN_BTOR2: &str = "\
1 sort bitvec 1
2 input 1 in_a
3 state 1 reg_a
4 zero 1
5 init 1 3 4
6 next 1 3 2
";

    fn run_check<F: FnOnce() -> SmtMustVerdict + Send + Sync>(f: F) -> SmtMustVerdict {
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, f)
    }

    #[test]
    fn smt_must_check_deterministic_zero_proves_must() {
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse deterministic fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode deterministic");
            let nid_map = build_register_nid_map(&view);
            // src cube = {p}: bit 0 set → src_bits = 0b1.
            // tgt cube = {p}: bit 0 set → tgt_bits = 0b1.
            smt_per_target_must_check(&view, 0b1, 0b1, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "deterministic reg_a := 0 with src=tgt={{p}} must prove Sharp; got {verdict:?}"
        );
    }

    #[test]
    fn smt_must_check_input_driven_rejects_must() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse input-driven fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode input-driven");
            let nid_map = build_register_nid_map(&view);
            // src cube = {p}: src_bits = 0b1.
            // tgt cube = {p}: tgt_bits = 0b1.
            // input=1 → reg_a:=1 → ¬p in next-state → escapes tgt.
            smt_per_target_must_check(&view, 0b1, 0b1, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::NotMust,
            "input-driven reg_a := in_a with src=tgt={{p}} must reject Sharp; got {verdict:?}"
        );
    }

    #[test]
    fn smt_must_check_unknown_register_returns_unknown() {
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse fixture");
        // Predicate references a register that doesn't exist
        // in the BTOR2 source.
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "ghost_reg".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode fixture");
            let nid_map = build_register_nid_map(&view);
            smt_per_target_must_check(&view, 0b1, 0b1, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::Unknown,
            "missing register must yield Unknown; got {verdict:?}"
        );
    }

    #[test]
    fn build_register_nid_map_collects_state_signals() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse fixture");
        run_check(|| {
            let view = encode_design(&file).expect("encode fixture");
            let nid_map = build_register_nid_map(&view);
            assert!(
                nid_map.contains_key("reg_a"),
                "reg_a state-cell must appear; got keys: {:?}",
                nid_map.keys().collect::<Vec<_>>()
            );
            // Inputs must NOT appear (they are constants per step,
            // not predicate-targetable state cells).
            assert!(
                !nid_map.contains_key("in_a"),
                "input in_a must NOT appear; got keys: {:?}",
                nid_map.keys().collect::<Vec<_>>()
            );
            SmtMustVerdict::Must
        });
    }

    // ---- Nested-∀i′ hyper-must (combinational-input-atoms.md §6.2) ----

    /// The Proposition 1/5 refutation system: one self-looping state
    /// (`reg_a := 0`, irrelevant) + a free input `in_a`. The predicate
    /// `p ≡ (in_a == 1)` is input-dependent — its value at the *next* cube is
    /// decided by the demonic next input `i′`, exactly the combinational-of-input
    /// target case (`g(s,i) = i`).
    const INPUT_TARGET_BTOR2: &str = "\
1 sort bitvec 1
2 input 1 in_a
3 state 1 reg_a
4 zero 1
5 init 1 3 4
6 next 1 3 4
";

    #[test]
    fn nested_hyper_must_singleton_input_target_rejects_must() {
        // Proposition 5: a SINGLETON target set over an input-dependent target
        // predicate is not a must — the demonic next input i′ escapes it. Here
        // src = {p} (in_a==1), singleton T = [{p}]: the next input i′=0 sends the
        // successor to {¬p}, so the singleton hyper-must fails.
        let file = parse(INPUT_TARGET_BTOR2).expect("parse input-target fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "in_a".into(),
            value: 1,
        }];
        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            smt_nested_hyper_must_check(&view, &primed, 0b1, &[0b1], &predicates, &nid_map, 5_000)
        });
        assert_eq!(
            verdict,
            SmtMustVerdict::NotMust,
            "a singleton input-dependent target is not a hyper-must (Prop 5); got {verdict:?}"
        );
    }

    #[test]
    fn nested_hyper_must_target_set_both_polarities_proves_must() {
        // Proposition 4: the target SET {{¬p},{p}} covers every demonic i′ — for
        // ANY next input the successor lands in one of the two cubes — so the
        // hyper-must holds. This is the sound GKMTS hyper-must the ⊥-label could
        // not express as a definite witness.
        let file = parse(INPUT_TARGET_BTOR2).expect("parse input-target fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "in_a".into(),
            value: 1,
        }];
        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            // T = [{¬p} = 0b0, {p} = 0b1].
            smt_nested_hyper_must_check(
                &view,
                &primed,
                0b1,
                &[0b0, 0b1],
                &predicates,
                &nid_map,
                5_000,
            )
        });
        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "a target set covering both polarities proves the hyper-must (Prop 4); got {verdict:?}"
        );
    }

    #[test]
    fn nested_hyper_must_target_free_shortcut_would_be_unsound() {
        // Direct guard on the §5.1 unsoundness the nested query fixes: the SAME
        // singleton {p} query, if the target input were left FREE (the H.B
        // target-free shortcut), would fabricate a must. `smt_hyper_must_check`
        // (which uses the target-free per-kind builder) reports Must here — the
        // fabricated edge. The nested check (pinned i′) correctly reports NotMust.
        // Their disagreement IS the bug §6.2 closes; this pins it so a future
        // refactor cannot silently route this case through the unsound path.
        let file = parse(INPUT_TARGET_BTOR2).expect("parse input-target fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "in_a".into(),
            value: 1,
        }];
        let (nested, target_free) = z3::with_z3_config(&z3::Config::new(), || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            let nested = smt_nested_hyper_must_check(
                &view,
                &primed,
                0b1,
                &[0b1],
                &predicates,
                &nid_map,
                5_000,
            );
            let target_free =
                smt_hyper_must_check(&view, 0b1, &[0b1], &predicates, &nid_map, 5_000);
            (nested, target_free)
        });
        assert_eq!(
            nested,
            SmtMustVerdict::NotMust,
            "the nested (pinned-i′) check is sound: singleton input target is NotMust; got {nested:?}"
        );
        assert_eq!(
            target_free,
            SmtMustVerdict::Must,
            "the shipped target-free check fabricates the must here (the §5.1 unsoundness the \
             nested query exists to fix); got {target_free:?}"
        );
    }

    #[test]
    fn nested_hyper_must_state_only_singleton_matches_standard() {
        // A pure-STATE target does not depend on i′, so a SINGLETON target set
        // already proves the must — the nested query degrades to the standard
        // must on state predicates (differential vs
        // smt_per_target_must_check_standard on deterministic reg_a := 0).
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse deterministic fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let (nested, standard) = z3::with_z3_config(&z3::Config::new(), || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            let nested = smt_nested_hyper_must_check(
                &view,
                &primed,
                0b1,
                &[0b1],
                &predicates,
                &nid_map,
                5_000,
            );
            let standard =
                smt_per_target_must_check_standard(&view, 0b1, 0b1, &predicates, &nid_map, 5_000);
            (nested, standard)
        });
        assert_eq!(
            nested,
            SmtMustVerdict::Must,
            "a state-only singleton hyper-must proves Must; got {nested:?}"
        );
        assert_eq!(
            nested, standard,
            "nested degrades to standard on a state-only target; nested={nested:?} standard={standard:?}"
        );
    }

    #[test]
    fn nested_hyper_must_empty_target_set_is_not_must() {
        // An empty candidate set can cover no successor — trivially NotMust.
        let file = parse(INPUT_TARGET_BTOR2).expect("parse input-target fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "in_a".into(),
            value: 1,
        }];
        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            smt_nested_hyper_must_check(&view, &primed, 0b1, &[], &predicates, &nid_map, 5_000)
        });
        assert_eq!(
            verdict,
            SmtMustVerdict::NotMust,
            "empty target set is NotMust; got {verdict:?}"
        );
    }

    // ---- H.B — free-input predicate (source-pin / target-free) ----

    #[test]
    fn build_register_nid_map_with_inputs_includes_inputs() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse fixture");
        run_check(|| {
            let view = encode_design(&file).expect("encode fixture");
            let map = build_register_nid_map_with_inputs(&view);
            assert!(map.contains_key("reg_a"), "state still mapped");
            assert!(
                map.contains_key("in_a"),
                "input now mapped; got {:?}",
                map.keys().collect::<Vec<_>>()
            );
            SmtMustVerdict::Must
        });
    }

    #[test]
    fn free_input_pred_source_pins_target_is_free() {
        // H.B: an input predicate `in_a == 1` constrains the CURRENT input on
        // the source cube (so `in_a==1` ∧ `in_a==0` is UNSAT) and is FREE on the
        // target cube (the next-cycle input is not a one-step variable).
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse fixture");
        run_check(|| {
            let view = encode_design(&file).expect("encode fixture");
            let map = build_register_nid_map_with_inputs(&view);
            let pred = PredicateSpec {
                name: "in".into(),
                register: "in_a".into(),
                value: 1,
            };
            let src_true =
                build_pred_constraint(&view, &map, &pred, false, true).expect("source builds");
            let src_false =
                build_pred_constraint(&view, &map, &pred, false, false).expect("source builds");
            let tgt = build_pred_constraint(&view, &map, &pred, true, true).expect("target builds");

            // Source pins the input: in_a==1 ∧ in_a==0 is unsatisfiable.
            let s1 = z3::Solver::new();
            s1.assert(&src_true);
            s1.assert(&src_false);
            assert_eq!(
                s1.check(),
                z3::SatResult::Unsat,
                "source input pred pins the current-cycle input"
            );

            // Target is free: it imposes nothing even with the input pinned to 0.
            let s2 = z3::Solver::new();
            s2.assert(&tgt);
            s2.assert(&src_false);
            assert_eq!(
                s2.check(),
                z3::SatResult::Sat,
                "target input pred is free (next-cycle input unconstrained)"
            );
            SmtMustVerdict::Must
        });
    }

    // ---- B.1 — compound-predicate SMT-constraint path ----
    //
    // A `PredicateLike` whose `expr()` is a compound boolean over several
    // registers, exercising `build_pred_constraint`'s compound branch.
    use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};

    struct CompoundPred {
        /// Vestigial for a compound (the simple `register()`/`value()` path is
        /// never taken when `expr()` is `Some`); a representative for display.
        reg0: String,
        e: PredicateExpr,
    }
    impl PredicateLike for CompoundPred {
        fn register(&self) -> &str {
            &self.reg0
        }
        fn value(&self) -> u64 {
            0
        }
        fn expr(&self) -> Option<&PredicateExpr> {
            Some(&self.e)
        }
    }

    // a := 0, b := 0 (deterministic). idle = (a == 0 && b == 0).
    const COMPOUND_DET_BTOR2: &str = "\
1 sort bitvec 1
2 state 1 a
3 state 1 b
4 zero 1
5 init 1 2 4
6 init 1 3 4
7 next 1 2 4
8 next 1 3 4
";

    // a := 0, b := in_b (b is input-driven). idle = (a == 0 && b == 0).
    const COMPOUND_INPUT_BTOR2: &str = "\
1 sort bitvec 1
2 input 1 in_b
3 state 1 a
4 state 1 b
5 zero 1
6 init 1 3 5
7 init 1 4 5
8 next 1 3 5
9 next 1 4 2
";

    fn idle_compound() -> PredicateExpr {
        // a == 0 && b == 0
        PredicateExpr::And(
            Box::new(PredicateExpr::Cmp {
                register: "a".into(),
                op: CmpOp::Eq,
                value: 0,
            }),
            Box::new(PredicateExpr::Cmp {
                register: "b".into(),
                op: CmpOp::Eq,
                value: 0,
            }),
        )
    }

    #[test]
    fn compound_predicate_deterministic_proves_must() {
        let file = parse(COMPOUND_DET_BTOR2).expect("parse compound-det fixture");
        let preds = vec![CompoundPred {
            reg0: "a".into(),
            e: idle_compound(),
        }];
        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode compound-det");
            let nid_map = build_register_nid_map(&view);
            // src cube {idle} (bit0=1) → tgt cube {idle} (bit0=1).
            smt_per_target_must_check(&view, 0b1, 0b1, &preds, &nid_map, 5_000)
        });
        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "a:=0,b:=0 with idle=(a==0 && b==0), src=tgt={{idle}} must prove Sharp; got {verdict:?}"
        );
    }

    #[test]
    fn compound_predicate_honors_every_conjunct() {
        // b is input-driven, so the `&& b == 0` conjunct must make the must-edge
        // FAIL (input in_b=1 escapes the target). If the encoder ignored b and
        // only checked a==0, it would (wrongly) prove Must — see the contrast
        // assertion below.
        let file = parse(COMPOUND_INPUT_BTOR2).expect("parse compound-input fixture");
        let compound = vec![CompoundPred {
            reg0: "a".into(),
            e: idle_compound(),
        }];
        let compound_verdict = run_check(|| {
            let view = encode_design(&file).expect("encode compound-input");
            let nid_map = build_register_nid_map(&view);
            smt_per_target_must_check(&view, 0b1, 0b1, &compound, &nid_map, 5_000)
        });
        assert_eq!(
            compound_verdict,
            SmtMustVerdict::NotMust,
            "idle=(a==0 && b==0) with b:=in_b must reject Sharp (in_b=1 escapes); got {compound_verdict:?}"
        );

        // Contrast: the simple atom `a == 0` alone IS Must here (a:=0 always),
        // proving the compound's b-conjunct genuinely changed the verdict.
        let a_only = vec![PredicateSpec {
            name: "a_is_zero".into(),
            register: "a".into(),
            value: 0,
        }];
        let a_only_verdict = run_check(|| {
            let view = encode_design(&file).expect("encode compound-input");
            let nid_map = build_register_nid_map(&view);
            smt_per_target_must_check(&view, 0b1, 0b1, &a_only, &nid_map, 5_000)
        });
        assert_eq!(
            a_only_verdict,
            SmtMustVerdict::Must,
            "the simple atom a==0 alone is Must (a:=0); got {a_only_verdict:?}"
        );
    }

    // ---- R.2.5b session-2 follow-up (2026-06-09) ----
    //
    // Tests for the standard ∀∃ must-check + hyper-must helpers.
    //
    // The discriminating contrast vs the MVP ∀∀ check:
    // - ∀∀: deterministic into tgt regardless of input.
    // - ∀∃: for every state in src, SOME input reaches tgt.
    //
    // The INPUT_DRIVEN fixture (reg_a := in_a) is the canonical
    // discriminator: ∀∀ rejects the src=tgt={p} self-loop (input=1
    // escapes); ∀∃ ACCEPTS it (input=0 keeps reg_a==0). So the same
    // fixture that produced NotMust under the MVP produces Must
    // under the standard form — the strict-supremacy invariant.

    /// R.2.5b session-2 follow-up — on the input-driven fixture
    /// where input=0 keeps `reg_a==0` and input=1 sets `reg_a:=1`,
    /// the standard ∀∃ check proves the must-edge src=tgt={p}
    /// (for every state in src, the input=0 witness reaches tgt).
    /// The MVP ∀∀ check rejected this same edge.
    #[test]
    fn smt_standard_must_check_input_driven_proves_must() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse input-driven fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode input-driven");
            let nid_map = build_register_nid_map(&view);
            smt_per_target_must_check_standard(&view, 0b1, 0b1, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "standard ∀∃: input=0 witness reaches src=tgt={{p}} from every state in src \
             ⇒ Must (more permissive than the MVP's ∀∀ form which rejected this)"
        );
    }

    /// R.2.5b session-2 follow-up — on the deterministic-zero
    /// fixture (always sets `reg_a:=0`), the standard ∀∃ check
    /// proves the must-edge (same as the MVP ∀∀ form). Confirms
    /// strict-supremacy: every ∀∀ Must is also a ∀∃ Must.
    #[test]
    fn smt_standard_must_check_deterministic_zero_proves_must() {
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse deterministic fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode deterministic");
            let nid_map = build_register_nid_map(&view);
            smt_per_target_must_check_standard(&view, 0b1, 0b1, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "deterministic reg_a := 0 with src=tgt={{p}} must prove ∀∃-Must too \
             (strict-supremacy invariant: every ∀∀-Must is also ∀∃-Must)"
        );
    }

    /// R.2.5b session-2 follow-up — when the src→tgt transition is
    /// IMPOSSIBLE under all inputs (e.g. src={p}, tgt={¬p}, with
    /// reg_a := 0 — every input keeps p true so tgt is never
    /// reached), the standard ∀∃ check returns NotMust (no input
    /// witness exists for any state in src).
    #[test]
    fn smt_standard_must_check_unreachable_target_returns_not_must() {
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse deterministic fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode deterministic");
            let nid_map = build_register_nid_map(&view);
            // src cube = {p}: src_bits = 0b1.
            // tgt cube = {¬p}: tgt_bits = 0b0.
            // reg_a := 0 always; from src (reg_a==0), next is reg_a==0 ⇒ p stays true.
            // So tgt={¬p} is unreachable ⇒ NotMust.
            smt_per_target_must_check_standard(&view, 0b1, 0b0, &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::NotMust,
            "unreachable target ⇒ NotMust under standard ∀∃ form"
        );
    }

    /// R.2.5b session-2 follow-up — hyper-must on a 2-target set.
    /// On the input-driven fixture, hyper-must over {p, ¬p} covers
    /// both possible next-states (input=0 → p; input=1 → ¬p). For
    /// every state in src, some input reaches some target in the
    /// set ⇒ Must.
    #[test]
    fn smt_hyper_must_covers_full_target_set_proves_must() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse input-driven fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode input-driven");
            let nid_map = build_register_nid_map(&view);
            // src cube = {p}: src_bits = 0b1.
            // target set = [{p}, {¬p}] (bits 0b1 + 0b0).
            // Every input combo from src reaches some target in the set ⇒ Must.
            smt_hyper_must_check(&view, 0b1, &[0b1, 0b0], &predicates, &nid_map, 5_000)
        });

        assert_eq!(
            verdict,
            SmtMustVerdict::Must,
            "hyper-must over the full {{p, ¬p}} target set covers all input-driven \
             paths from src ⇒ Must"
        );
    }

    /// R.2.5b session-2 follow-up — hyper-must on a singleton
    /// (degenerates to the per-target ∀∃ check). Confirms the
    /// hyper-must helper agrees with `smt_per_target_must_check_standard`
    /// on cardinality-1 target sets.
    #[test]
    fn smt_hyper_must_singleton_agrees_with_per_target_standard() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let (singleton_hyper, per_target) = run_two_checks(|| {
            let view = encode_design(&file).expect("encode fixture");
            let nid_map = build_register_nid_map(&view);
            let h = smt_hyper_must_check(&view, 0b1, &[0b1], &predicates, &nid_map, 5_000);
            let p =
                smt_per_target_must_check_standard(&view, 0b1, 0b1, &predicates, &nid_map, 5_000);
            (h, p)
        });

        assert_eq!(
            singleton_hyper, per_target,
            "hyper-must({{p}}) must agree with per-target ∀∃({{p}}) \
             (cardinality-1 reduction); got singleton={singleton_hyper:?}, per-target={per_target:?}"
        );
    }

    /// R.2.5b session-2 follow-up — hyper-must on an empty target
    /// set returns NotMust (no possible witnesses).
    #[test]
    fn smt_hyper_must_empty_set_returns_not_must() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let verdict = run_check(|| {
            let view = encode_design(&file).expect("encode fixture");
            let nid_map = build_register_nid_map(&view);
            smt_hyper_must_check(&view, 0b1, &[], &predicates, &nid_map, 5_000)
        });

        assert_eq!(verdict, SmtMustVerdict::NotMust);
    }

    /// Run a closure inside the Z3 scope returning two verdicts.
    /// Local helper since `run_check` is single-verdict.
    fn run_two_checks<F: FnOnce() -> (SmtMustVerdict, SmtMustVerdict) + Send + Sync>(
        f: F,
    ) -> (SmtMustVerdict, SmtMustVerdict) {
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, f)
    }

    // ── MIG-3 — sound SMT may-edge query tests ──────────────────────

    /// MIG-3 — an input-driven transition `reg_a := in_a` admits BOTH
    /// targets from src cube `{p}` (reg_a==0): input=0 reaches `{p}`,
    /// input=1 reaches `{¬p}`. Both are sound may-edges (∃ witness),
    /// even though the must-check rejects src→`{p}` (input=1 escapes).
    /// This is the may/must distinction the sound may-edge captures and
    /// the sampling could miss.
    #[test]
    fn smt_may_check_input_driven_admits_both_targets() {
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse input-driven fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let cfg = z3::Config::new();
        let (to_p, to_not_p) = z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode input-driven");
            let nid_map = build_register_nid_map(&view);
            // src cube {p} = reg_a==0 (src_bits = 0b1).
            let to_p = smt_per_target_may_check(&view, 0b1, 0b1, &predicates, &nid_map, 5_000);
            let to_not_p = smt_per_target_may_check(&view, 0b1, 0b0, &predicates, &nid_map, 5_000);
            (to_p, to_not_p)
        });

        assert_eq!(
            to_p,
            SmtMayVerdict::May,
            "input=0 reaches reg_a==0, so src{{p}}→tgt{{p}} is a may-edge"
        );
        assert_eq!(
            to_not_p,
            SmtMayVerdict::May,
            "input=1 reaches reg_a==1, so src{{p}}→tgt{{¬p}} is a may-edge"
        );
    }

    /// MIG-3 — a deterministic transition `reg_a := 0` makes the target
    /// `{¬p}` (reg_a==1) UNREACHABLE from any source. The sound
    /// may-edge query EXCLUDES it (UNSAT → NotMay) — the key soundness
    /// property: only proven-impossible edges are excluded, never a
    /// reachable one (which sampling could miss).
    #[test]
    fn smt_may_check_excludes_proven_impossible_target() {
        let file = parse(DETERMINISTIC_ZERO_BTOR2).expect("parse deterministic fixture");
        let predicates = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];

        let cfg = z3::Config::new();
        let (to_p, to_not_p) = z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode deterministic");
            let nid_map = build_register_nid_map(&view);
            let to_p = smt_per_target_may_check(&view, 0b1, 0b1, &predicates, &nid_map, 5_000);
            let to_not_p = smt_per_target_may_check(&view, 0b1, 0b0, &predicates, &nid_map, 5_000);
            (to_p, to_not_p)
        });

        assert_eq!(
            to_p,
            SmtMayVerdict::May,
            "reg_a:=0 reaches reg_a==0, so src{{p}}→tgt{{p}} is a may-edge"
        );
        assert_eq!(
            to_not_p,
            SmtMayVerdict::NotMay,
            "reg_a:=0 can NEVER reach reg_a==1, so src{{p}}→tgt{{¬p}} is excluded"
        );
    }

    /// H.U.1a differential — the uniform may-check
    /// ([`smt_per_target_may_check_uniform`]) must produce the SAME verdict as
    /// the per-kind [`smt_per_target_may_check`] for EVERY cube pair. This is the
    /// guard on the `BtorSts::may_edges` cutover: it certifies the uniform image
    /// is may-preserving across atom kinds, so the production may-edge set (and
    /// every cube verdict built on it) is unchanged.
    fn assert_uniform_may_matches<P: PredicateLike + Sync>(src: &str, preds: &[P]) {
        let file = parse(src).expect("parse");
        let n = 1u64 << preds.len();
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            for sb in 0..n {
                for tb in 0..n {
                    let per_kind = smt_per_target_may_check(&view, sb, tb, preds, &nid_map, 5_000);
                    let uniform = smt_per_target_may_check_uniform(
                        &view, &primed, sb, tb, preds, &nid_map, 5_000,
                    );
                    assert_eq!(
                        per_kind, uniform,
                        "uniform may ≠ per-kind may (src={src:?} sb={sb} tb={tb})"
                    );
                }
            }
        });
    }

    #[test]
    fn uniform_may_matches_per_kind_state_input_compound() {
        // State, deterministic transition.
        assert_uniform_may_matches(
            DETERMINISTIC_ZERO_BTOR2,
            &[PredicateSpec {
                name: "p".into(),
                register: "reg_a".into(),
                value: 0,
            }],
        );
        // State + a FREE INPUT predicate together (the case where the per-kind
        // path uses target-free `true` and the uniform path uses a fresh `i'`).
        assert_uniform_may_matches(
            INPUT_DRIVEN_BTOR2,
            &[
                PredicateSpec {
                    name: "p".into(),
                    register: "reg_a".into(),
                    value: 0,
                },
                PredicateSpec {
                    name: "q".into(),
                    register: "in_a".into(),
                    value: 1,
                },
            ],
        );
        // Compound over state (deterministic).
        assert_uniform_may_matches(
            COMPOUND_DET_BTOR2,
            &[CompoundPred {
                reg0: "a".into(),
                e: idle_compound(),
            }],
        );
        // Compound over an input-driven register.
        assert_uniform_may_matches(
            COMPOUND_INPUT_BTOR2,
            &[CompoundPred {
                reg0: "a".into(),
                e: idle_compound(),
            }],
        );
    }

    /// H.U.1c differential — the uniform must
    /// ([`smt_per_target_must_check_uniform`]) must produce the SAME verdict as
    /// the per-kind [`smt_per_target_must_check_standard`] for EVERY cube pair.
    /// The `BtorSts::must_edges` cutover guard.
    fn assert_uniform_must_matches<P: PredicateLike + Sync>(src: &str, preds: &[P]) {
        let file = parse(src).expect("parse");
        let n = 1u64 << preds.len();
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            for sb in 0..n {
                for tb in 0..n {
                    let per_kind =
                        smt_per_target_must_check_standard(&view, sb, tb, preds, &nid_map, 5_000);
                    let uniform = smt_per_target_must_check_uniform(
                        &view, &primed, sb, tb, preds, &nid_map, 5_000,
                    );
                    assert_eq!(
                        per_kind, uniform,
                        "uniform must ≠ per-kind must (src={src:?} sb={sb} tb={tb})"
                    );
                }
            }
        });
    }

    #[test]
    fn uniform_must_matches_per_kind_state_input_compound() {
        assert_uniform_must_matches(
            DETERMINISTIC_ZERO_BTOR2,
            &[PredicateSpec {
                name: "p".into(),
                register: "reg_a".into(),
                value: 0,
            }],
        );
        assert_uniform_must_matches(
            INPUT_DRIVEN_BTOR2,
            &[
                PredicateSpec {
                    name: "p".into(),
                    register: "reg_a".into(),
                    value: 0,
                },
                PredicateSpec {
                    name: "q".into(),
                    register: "in_a".into(),
                    value: 1,
                },
            ],
        );
        assert_uniform_must_matches(
            COMPOUND_DET_BTOR2,
            &[CompoundPred {
                reg0: "a".into(),
                e: idle_compound(),
            }],
        );
        assert_uniform_must_matches(
            COMPOUND_INPUT_BTOR2,
            &[CompoundPred {
                reg0: "a".into(),
                e: idle_compound(),
            }],
        );
    }

    #[test]
    fn uniform_hyper_must_matches_per_kind() {
        // Hyper-must over several target SETs; uniform == per-kind on a state +
        // input fixture (the `BtorSts::hyper_must_edges` cutover guard).
        let file = parse(INPUT_DRIVEN_BTOR2).expect("parse");
        let preds = vec![PredicateSpec {
            name: "p".into(),
            register: "reg_a".into(),
            value: 0,
        }];
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            let target_sets: &[&[u64]] = &[&[0b0], &[0b1], &[0b0, 0b1]];
            for sb in 0..2u64 {
                for ts in target_sets {
                    let per_kind = smt_hyper_must_check(&view, sb, ts, &preds, &nid_map, 5_000);
                    let uniform = smt_hyper_must_check_uniform(
                        &view, &primed, sb, ts, &preds, &nid_map, 5_000,
                    );
                    assert_eq!(
                        per_kind, uniform,
                        "uniform hyper-must ≠ per-kind (sb={sb} ts={ts:?})"
                    );
                }
            }
        });
    }

    // ---- H.U.1d — combinational signal resolution + cube-dimension handling ----

    // `reg` stuck at 0 (next = reg); `g = not(reg)` is a combinational-of-state
    // op named `g` (so g ≡ 1 forever). A second combinational `h = not(reg)`
    // carries NO own symbol but is named `h_out` on an `output` line.
    const COMB_FIXTURE: &str = "\
1 sort bitvec 1
2 state 1 reg
3 zero 1
4 init 1 2 3
5 next 1 2 2
6 not 1 2 g
7 not 1 2
8 output 7 h_out
";

    #[test]
    fn nid_map_resolves_combinational_op_and_output_symbols() {
        let file = parse(COMB_FIXTURE).expect("parse comb fixture");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode");
            let map = build_register_nid_map_with_inputs(&view);
            // Op-symbol combinational (`g`, nid 6) resolves.
            assert_eq!(
                map.get("g"),
                Some(&6),
                "op-symbol combinational `g` resolves"
            );
            // Output-line-only combinational (`h_out`, the `not` at nid 7) resolves
            // (H.U.1d output-line registration).
            assert_eq!(
                map.get("h_out"),
                Some(&7),
                "output-line combinational `h_out` resolves to its driving node"
            );
            // State still resolves + takes precedence (no regression).
            assert_eq!(map.get("reg"), Some(&2));
        });
    }

    #[test]
    fn uniform_combinational_as_cube_dimension_may_and_must() {
        // `g = not(reg)` with `reg` stuck → g ≡ 1. As a CUBE DIMENSION via the
        // uniform image: {g==1}→{g==0} is NOT a may-edge (g never changes), and
        // {g==1}→{g==1} IS a must-edge (g stays 1) — the new capability the
        // per-kind path lacked (it returned conservative May / Unknown for a
        // combinational register name absent from its nid-map).
        let file = parse(COMB_FIXTURE).expect("parse");
        let preds = vec![PredicateSpec {
            name: "g".into(),
            register: "g".into(),
            value: 1,
        }];
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            let view = encode_design(&file).expect("encode");
            let primed = encode_primed(&file, &view).expect("primed");
            let nid_map = build_register_nid_map_with_inputs(&view);
            // may {g}→{g}: g stays 1 ⇒ May.
            assert_eq!(
                smt_per_target_may_check_uniform(&view, &primed, 0b1, 0b1, &preds, &nid_map, 5_000),
                SmtMayVerdict::May
            );
            // may {g}→{¬g}: g cannot become 0 ⇒ NotMay (precise).
            assert_eq!(
                smt_per_target_may_check_uniform(&view, &primed, 0b1, 0b0, &preds, &nid_map, 5_000),
                SmtMayVerdict::NotMay
            );
            // must {g}→{g}: from every g==1 state, the (only) successor has g==1 ⇒ Must.
            assert_eq!(
                smt_per_target_must_check_uniform(
                    &view, &primed, 0b1, 0b1, &preds, &nid_map, 5_000
                ),
                SmtMustVerdict::Must
            );
        });
    }
}
