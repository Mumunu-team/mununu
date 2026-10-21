//! Native **in-process Boolector** bounded model checking — the owned-standalone
//! portfolio's *fast bit-vector* reachability member.
//!
//! `native_bmc` already gives mununu an owned scalable-safety back-end, but it runs
//! on **Z3 QF_BV**, which is slow on the *deep* bit-level unrollings some real
//! designs need: measured on HWMCC20, the Z3 owned path leaves `krebs.3`
//! (counterexample at depth 75) and `vis_arrays_buf_bug` at `unknown` even with a
//! 180 s/engine budget, while Boolector's dedicated BV engine cracks both (~73 s and
//! ~1 s respectively via `btormc`, Boolector's own model checker). This module gives
//! the *owned* portfolio that same dedicated-BV muscle **without a subprocess** — it
//! links Boolector in-process (the MIT-licensed `boolector` crate + vendored C
//! library) and unrolls the BTOR2 transition relation directly into Boolector BV
//! nodes.
//!
//! It is feature-gated (`--features boolector`) because it C-compiles Boolector; the
//! default `make ci` build stays Boolector-free. Its tests run in the `mununu-dev` /
//! `mununu-sva` Docker images.
//!
//! # What it decides
//!
//! Like [`super::native_bmc`], it only ever claims **Violated** (a reachable `bad`),
//! never **Safe** — so it cannot produce a spurious safety proof:
//!
//! - **SAT at frame `k`** ⇒ [`BmcOutcome::Violated`] `{ depth: k }` — a concrete
//!   `Init → bad` execution of the BTOR2 model, sound w.r.t. the model.
//! - **UNSAT through `k`** ⇒ [`BmcOutcome::NoCexWithin`] — bounded, NOT a proof of
//!   safety.
//!
//! # Soundness
//!
//! Two independent guards keep every verdict sound:
//!
//! 1. **Never Safe.** BMC asserts `bad` and looks for SAT; it can only ever *find* a
//!    counterexample, so [`decide_reachable_boolector`] returns `Some(true)` (a
//!    sound `Violated`) or `None` (bounded / abstain) — **never** `Some(false)`.
//! 2. **Abstain on anything unmodelled.** The encoder returns `Err` — the portfolio
//!    reads that as *abstain*, not a verdict — on any operator it does not model
//!    exactly ([`Op::Rol`]/[`Op::Ror`], array [`Op::Read`]/[`Op::Write`]) or on an
//!    array *sort* declaration. A partial encoding never yields a verdict.
//!
//! The `Violated` direction is cross-checked against the exact BDD engine under a
//! differential oracle in this module's tests (`agrees_with_exact_no_spurious_*`).
//!
//! # Encoding
//!
//! Rather than Z3's substitution, this uses Boolector's structural sharing directly:
//! frame 0's state cells are fresh BV vars (with `init` asserted as equalities, and
//! init-less cells left free — BTOR2's nondeterministic-init semantics); each later
//! frame's state cell is *the expression* its `next` operand evaluated to in the
//! previous frame (no per-transition equality assertion needed). `constraint`s are
//! asserted permanently at every frame.
//!
//! # Bound scheduling (the frame-simplification win)
//!
//! Instead of asking "is `bad` true at *exactly* depth k?" once per depth, the search
//! maintains a running monitor `reached_k = bad_0 ∨ … ∨ bad_k` ("a `bad` is reachable
//! *within* k steps") and checks it as a one-shot assumption. Crucially, one
//! `reached_k` UNSAT proves the *whole* `0..k` range clean, so after a shallow
//! threshold the search **strides** — a single deep solve covers `STRIDE` frames
//! rather than one solve each. Measured on HWMCC `krebs.3` (CEX @ depth 75), the deep
//! per-frame UNSAT proofs (frames 60–74) dominate the runtime, and this striding cuts
//! the wall time to roughly a third by skipping the intermediate ones. On a SAT bound
//! the *exact* CEX depth is read back as the shallowest frame whose `bad` holds in the
//! model, so verdicts stay depth-exact. (Cone-of-influence pruning was measured to not
//! help here — `bad`'s cone covers ~97 % of the logic; and disabling model generation
//! during the search is a net loss — the witness re-solve of the deep deciding formula
//! costs more than it saves.)

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;

use boolector::option::{BtorOption, ModelGen};
use boolector::{BV, Btor, SolverResult};

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Op, Operand, Sort};
use crate::adapter::btor2::cex_value::CexValue;
use crate::adapter::btor2::native_bmc::{BmcOutcome, BmcTrace};

/// Portfolio-facing depth cap — well above [`super::native_bmc::DEFAULT_MAX_K`] so a
/// *deep* counterexample (e.g. HWMCC `krebs.3` at depth 75) is caught, with the wall
/// `deadline` / `cancel` keeping the search bounded.
pub const DEFAULT_MAX_K: u32 = 200;

/// A Boolector BV node bound to the shared solver.
type Bv = BV<Rc<Btor>>;
/// `nid → BV` for one unrolled frame.
type Env = HashMap<Nid, Bv>;

/// Why a Boolector BMC run produced no verdict (the portfolio reads any of these as
/// *abstain*, never a wrong answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoolectorError {
    /// The design declares no `bad` property — nothing to check.
    NoBadProperty,
    /// An operator this encoder does not model exactly (`rol`/`ror`, array
    /// `read`/`write`) — abstain rather than approximate.
    UnsupportedOp(String),
    /// An array/memory *sort* is declared — the BV-only encoder abstains.
    UnsupportedSort,
    /// A node references an operand that is not in scope (a malformed/out-of-order
    /// BTOR2 file) — abstain.
    MissingOperand,
}

impl std::fmt::Display for BoolectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoolectorError::NoBadProperty => write!(f, "native Boolector: design has no `bad`"),
            BoolectorError::UnsupportedOp(op) => {
                write!(
                    f,
                    "native Boolector: unmodelled operator `{op}` — abstaining"
                )
            }
            BoolectorError::UnsupportedSort => {
                write!(f, "native Boolector: array sort — BV-only engine abstains")
            }
            BoolectorError::MissingOperand => {
                write!(
                    f,
                    "native Boolector: dangling operand reference — abstaining"
                )
            }
        }
    }
}

/// The structural maps extracted once from a BTOR2 file.
struct Maps {
    /// `sort-nid → bit width` (array sorts make [`build_maps`] abstain).
    sort_width: HashMap<Nid, u32>,
    /// `bad` signal operands (OR-ed for the violation condition).
    bad_ops: Vec<Operand>,
    /// `(state-nid, init-value operand)` pairs.
    init_pairs: Vec<(Nid, Operand)>,
    /// `constraint` signal operands (asserted at every frame).
    constraint_ops: Vec<Operand>,
    /// `state-nid → next-value operand`.
    state_next: HashMap<Nid, Operand>,
}

fn build_maps(file: &Btor2File) -> Result<Maps, BoolectorError> {
    let mut sort_width = HashMap::new();
    let mut bad_ops = Vec::new();
    let mut init_pairs = Vec::new();
    let mut constraint_ops = Vec::new();
    let mut state_next = HashMap::new();
    for l in &file.lines {
        match &l.node {
            Node::Sort { sort } => match sort {
                Sort::BitVec { width } => {
                    sort_width.insert(l.nid, *width);
                }
                // A single array sort declaration makes the BV-only engine abstain.
                Sort::Array { .. } => return Err(BoolectorError::UnsupportedSort),
            },
            Node::Bad { signal } => bad_ops.push(*signal),
            Node::Init { state, value, .. } => init_pairs.push((*state, *value)),
            Node::Constraint { signal } => constraint_ops.push(*signal),
            Node::Next { state, value, .. } => {
                state_next.insert(*state, *value);
            }
            _ => {}
        }
    }
    if bad_ops.is_empty() {
        return Err(BoolectorError::NoBadProperty);
    }
    Ok(Maps {
        sort_width,
        bad_ops,
        init_pairs,
        constraint_ops,
        state_next,
    })
}

/// Current value of an operand, applying BTOR2's negation (a `-nid` reference means
/// the bitwise complement of the node).
fn resolve(env: &Env, op: &Operand) -> Option<Bv> {
    let bv = env.get(&op.nid())?.clone();
    Some(if op.is_negated() { bv.not() } else { bv })
}

fn const_bv(btor: &Rc<Btor>, value: &ConstValue, width: u32) -> Bv {
    match value {
        ConstValue::Zero => BV::zero(btor.clone(), width),
        ConstValue::One => BV::one(btor.clone(), width),
        ConstValue::Ones => BV::ones(btor.clone(), width),
        ConstValue::Bin(bits) => BV::from_binary_str(btor.clone(), bits),
        ConstValue::Dec(d) => BV::from_dec_str(btor.clone(), &d.to_string(), width),
        ConstValue::Hex(h) => BV::from_hex_str(btor.clone(), h, width),
    }
}

/// Encode one `Op` node into a Boolector BV. Returns `Err` (⇒ the whole run
/// abstains) on any operator not modelled exactly.
fn encode_op(
    op: Op,
    args: &[Operand],
    immediates: &[u32],
    env: &Env,
) -> Result<Bv, BoolectorError> {
    let a = |i: usize| -> Result<Bv, BoolectorError> {
        let operand = args.get(i).ok_or(BoolectorError::MissingOperand)?;
        resolve(env, operand).ok_or(BoolectorError::MissingOperand)
    };
    let imm = |i: usize| -> Result<u32, BoolectorError> {
        immediates
            .get(i)
            .copied()
            .ok_or(BoolectorError::MissingOperand)
    };
    let bv = match op {
        // Unary.
        Op::Not => a(0)?.not(),
        Op::Inc => a(0)?.inc(),
        Op::Dec => a(0)?.dec(),
        Op::Neg => a(0)?.neg(),
        Op::Redand => a(0)?.redand(),
        Op::Redor => a(0)?.redor(),
        Op::Redxor => a(0)?.redxor(),
        // Bitwise / logical binary.
        Op::And => a(0)?.and(&a(1)?),
        Op::Or => a(0)?.or(&a(1)?),
        Op::Xor => a(0)?.xor(&a(1)?),
        Op::Nand => a(0)?.nand(&a(1)?),
        Op::Nor => a(0)?.nor(&a(1)?),
        Op::Xnor => a(0)?.xnor(&a(1)?),
        Op::Iff => a(0)?.iff(&a(1)?),
        Op::Implies => a(0)?.implies(&a(1)?),
        // Comparisons.
        Op::Eq => a(0)?._eq(&a(1)?),
        Op::Neq => a(0)?._ne(&a(1)?),
        Op::Ugt => a(0)?.ugt(&a(1)?),
        Op::Ugte => a(0)?.ugte(&a(1)?),
        Op::Sgt => a(0)?.sgt(&a(1)?),
        Op::Sgte => a(0)?.sgte(&a(1)?),
        Op::Ult => a(0)?.ult(&a(1)?),
        Op::Ulte => a(0)?.ulte(&a(1)?),
        Op::Slt => a(0)?.slt(&a(1)?),
        Op::Slte => a(0)?.slte(&a(1)?),
        // Arithmetic.
        Op::Add => a(0)?.add(&a(1)?),
        Op::Sub => a(0)?.sub(&a(1)?),
        Op::Mul => a(0)?.mul(&a(1)?),
        Op::Udiv => a(0)?.udiv(&a(1)?),
        Op::Sdiv => a(0)?.sdiv(&a(1)?),
        Op::Urem => a(0)?.urem(&a(1)?),
        Op::Srem => a(0)?.srem(&a(1)?),
        Op::Smod => a(0)?.smod(&a(1)?),
        // Overflow detectors.
        Op::Uaddo => a(0)?.uaddo(&a(1)?),
        Op::Saddo => a(0)?.saddo(&a(1)?),
        Op::Usubo => a(0)?.usubo(&a(1)?),
        Op::Ssubo => a(0)?.ssubo(&a(1)?),
        Op::Umulo => a(0)?.umulo(&a(1)?),
        Op::Smulo => a(0)?.smulo(&a(1)?),
        Op::Sdivo => a(0)?.sdivo(&a(1)?),
        // Shifts (Boolector accepts equal-width operands, which is BTOR2's form).
        Op::Sll => a(0)?.sll(&a(1)?),
        Op::Srl => a(0)?.srl(&a(1)?),
        Op::Sra => a(0)?.sra(&a(1)?),
        // Structural bit ops.
        Op::Concat => a(0)?.concat(&a(1)?),
        Op::Slice => a(0)?.slice(imm(0)?, imm(1)?),
        Op::Uext => a(0)?.uext(imm(0)?),
        Op::Sext => a(0)?.sext(imm(0)?),
        Op::Ite => a(0)?.cond_bv(&a(1)?, &a(2)?),
        // Not modelled exactly — abstain. `rol`/`ror` have BTOR2-vs-Boolector width
        // conventions that differ; `read`/`write` are array ops (also excluded via
        // the array-sort guard, handled here defensively).
        Op::Rol | Op::Ror | Op::Read | Op::Write => {
            return Err(BoolectorError::UnsupportedOp(format!("{op:?}")));
        }
    };
    Ok(bv)
}

/// Build one unrolled frame's `Env`, plus the NAMED (symbol-carrying) state and
/// input cells for witness extraction. `prev = None` is frame 0 (states fresh); on a
/// later frame each state cell is its `next` operand's value in `prev`.
type Frame = (Env, Vec<(String, Bv)>, Vec<(String, Bv)>);

fn build_frame(
    btor: &Rc<Btor>,
    file: &Btor2File,
    maps: &Maps,
    prev: Option<&Env>,
) -> Result<Frame, BoolectorError> {
    let mut env: Env = HashMap::new();
    let mut named_states: Vec<(String, Bv)> = Vec::new();
    let mut named_inputs: Vec<(String, Bv)> = Vec::new();
    let width_of = |sort: &Nid| -> Result<u32, BoolectorError> {
        maps.sort_width
            .get(sort)
            .copied()
            .ok_or(BoolectorError::MissingOperand)
    };
    for l in &file.lines {
        let nid = l.nid;
        match &l.node {
            Node::Sort { .. } => {}
            Node::Const { sort, value } => {
                let bv = const_bv(btor, value, width_of(sort)?);
                env.insert(nid, bv);
            }
            Node::Input { sort, symbol } => {
                let bv = BV::new(btor.clone(), width_of(sort)?, None);
                if let Some(s) = symbol {
                    named_inputs.push((s.clone(), bv.clone()));
                }
                env.insert(nid, bv);
            }
            Node::State { sort, symbol } => {
                let bv = match prev {
                    // Frame 0: fresh var; `init` is asserted separately (init-less
                    // cells stay free — BTOR2 nondeterministic-init semantics).
                    None => BV::new(btor.clone(), width_of(sort)?, None),
                    // Later frame: the `next` expression from the previous frame; a
                    // state with no `next` line holds its value (frozen).
                    Some(pe) => match maps.state_next.get(&nid) {
                        Some(next_op) => {
                            resolve(pe, next_op).ok_or(BoolectorError::MissingOperand)?
                        }
                        None => pe
                            .get(&nid)
                            .cloned()
                            .ok_or(BoolectorError::MissingOperand)?,
                    },
                };
                if let Some(s) = symbol {
                    named_states.push((s.clone(), bv.clone()));
                }
                env.insert(nid, bv);
            }
            Node::Op { op, args, .. } => {
                let bv = encode_op(*op, args, &l.immediates, &env)?;
                env.insert(nid, bv);
            }
            // Structural lines — resolved by the driver, not bound in the env.
            _ => {}
        }
    }
    Ok((env, named_states, named_inputs))
}

/// The `bad` condition at a frame: OR of every `bad` operand (each a 1-bit signal).
fn bad_condition(env: &Env, bad_ops: &[Operand]) -> Result<Bv, BoolectorError> {
    let mut acc: Option<Bv> = None;
    for op in bad_ops {
        let bv = resolve(env, op).ok_or(BoolectorError::MissingOperand)?;
        acc = Some(match acc {
            None => bv,
            Some(prev) => prev.or(&bv),
        });
    }
    acc.ok_or(BoolectorError::NoBadProperty)
}

/// Read a NAMED cell list's model values from the current (SAT) Boolector model at their full
/// width (mununu#723 — a cell wider than 64 bits was dropped); sorted for stability.
fn eval_named(frame: &[(String, Bv)]) -> Vec<(String, CexValue)> {
    let mut out: Vec<(String, CexValue)> = frame
        .iter()
        .map(|(sym, bv)| {
            let sol = bv.get_a_solution();
            let bits = bv.get_width();
            let value = match sol.as_u64() {
                Some(v) => CexValue::from_u64(v, bits),
                None => CexValue::from_binary_str(sol.as_01x_str(), bits),
            };
            (sym.clone(), value)
        })
        .collect();
    out.sort();
    out
}

/// Bounded model check via in-process Boolector: is a `bad` reachable within `max_k`
/// steps? On a `Violated` verdict, also extracts the concrete `Init → bad` witness.
///
/// Bounded by the wall `deadline` and shared `cancel` flag (both polled before each
/// depth), so a peer engine deciding first, or the budget expiring, ends the search
/// cleanly (returning the bounded `NoCexWithin`, never a wrong verdict).
pub fn bmc_bad_reachable_boolector(
    file: &Btor2File,
    max_k: u32,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(BmcOutcome, Option<BmcTrace>), BoolectorError> {
    let maps = build_maps(file)?;
    // `MUNUNU_BOOLECTOR_TRACE` — eprintln each `sat()`'s wall time (per-bound diagnostics).
    let trace = std::env::var_os("MUNUNU_BOOLECTOR_TRACE").is_some();
    let btor = Rc::new(Btor::new());
    btor.set_opt(BtorOption::Incremental(true));
    // Model generation stays ON: it costs nothing on the UNSAT bound checks (a model is
    // only built on SAT), and it is needed to extract the exact CEX depth + witness from
    // the deciding model. (Measured: turning it off during the search and re-solving for
    // the witness is a *net loss* — the re-solve of the deep deciding formula dominates.)
    btor.set_opt(BtorOption::ModelGen(ModelGen::All));

    // Frame 0 — fresh states, then assert `init` and the frame-0 constraints.
    let (env0, ns0, ni0) = build_frame(&btor, file, &maps, None)?;
    for (state_nid, val_op) in &maps.init_pairs {
        if let (Some(sbv), Some(vbv)) = (env0.get(state_nid), resolve(&env0, val_op)) {
            sbv._eq(&vbv).assert();
        }
    }
    for c in &maps.constraint_ops {
        if let Some(cbv) = resolve(&env0, c) {
            cbv.assert();
        }
    }

    let mut frames: Vec<Env> = vec![env0];
    let mut named_states: Vec<Vec<(String, Bv)>> = vec![ns0];
    let mut named_inputs: Vec<Vec<(String, Bv)>> = vec![ni0];
    // Per-frame `bad` signal (1-bit), kept so the exact CEX depth can be read back from
    // a SAT model, and `reached_k = bad_0 ∨ … ∨ bad_k` (the running "a `bad` is reachable
    // within k steps" monitor).
    let mut bad_per_frame: Vec<Bv> = vec![bad_condition(&frames[0], &maps.bad_ops)?];
    let mut reached: Bv = bad_per_frame[0].clone();

    // Bound-check schedule. Measured on HWMCC `krebs.3` (CEX @ depth 75): the deep
    // per-frame UNSAT proofs (frames 60–74) cost ~180 s of the ~205 s total, and
    // checking `bad` at *every* depth re-proves each. A single `reached_k` UNSAT proves
    // the WHOLE 0..k range clean, so after a threshold we STRIDE — one deep solve covers
    // `STRIDE` frames instead of one each. Shallow depths (≤ threshold) are still checked
    // every frame (cheap, and keeps the reported CEX depth exact for shallow properties).
    // `max_k` is always a check point so the last frame is never skipped.
    const EXACT_THRESHOLD: usize = 32;
    const STRIDE: usize = 8;

    let n = max_k as usize;
    let mut last_check: Option<usize> = None;
    for k in 0..=n {
        if cancel.load(Relaxed) || Instant::now() >= deadline {
            // Budget / peer-decided: honest bounded outcome, no verdict claimed.
            return Ok((
                BmcOutcome::NoCexWithin {
                    k: k.saturating_sub(1) as u32,
                },
                None,
            ));
        }
        // Lazily extend the unrolling to frame k, asserting its constraints and folding
        // its `bad` into the running `reached` monitor.
        while frames.len() <= k {
            let j = frames.len() - 1;
            let (envk, nsk, nik) = build_frame(&btor, file, &maps, Some(&frames[j]))?;
            for c in &maps.constraint_ops {
                if let Some(cbv) = resolve(&envk, c) {
                    cbv.assert();
                }
            }
            let bad_k = bad_condition(&envk, &maps.bad_ops)?;
            reached = reached.or(&bad_k);
            bad_per_frame.push(bad_k);
            frames.push(envk);
            named_states.push(nsk);
            named_inputs.push(nik);
        }
        // Only *solve* at a scheduled bound — every frame up to the threshold, then every
        // STRIDE frames, plus always the last frame. In between we just keep unrolling
        // (cheap — node construction, no solve).
        let is_check =
            k <= EXACT_THRESHOLD || k == n || last_check.is_none_or(|lc| k - lc >= STRIDE);
        if !is_check {
            continue;
        }
        last_check = Some(k);
        // `reached_k` as a one-shot assumption: is a `bad` reachable within k steps?
        reached.assume();
        let t0 = trace.then(Instant::now);
        let res = btor.sat();
        if let Some(t0) = t0 {
            eprintln!("[boolector] reached≤{k}: {res:?} in {:.3?}", t0.elapsed());
        }
        match res {
            SolverResult::Sat => {
                // A `bad` is reachable within k steps. The exact depth is the SHALLOWEST
                // frame whose `bad` holds in this model — sound (a real init→bad run) and,
                // since every bound below the previous check was proven clean, correct.
                let depth = (0..=k)
                    .find(|&j| bad_per_frame[j].get_a_solution().as_bool() == Some(true))
                    .unwrap_or(k);
                let states = (0..=depth).map(|j| eval_named(&named_states[j])).collect();
                let inputs = (0..depth).map(|j| eval_named(&named_inputs[j])).collect();
                return Ok((
                    BmcOutcome::Violated {
                        depth: depth as u32,
                    },
                    Some(BmcTrace { states, inputs }),
                ));
            }
            SolverResult::Unsat => {}
            // Solver gave up on this bound (deeper only grows) — abstain.
            SolverResult::Unknown => return Ok((BmcOutcome::NoCexWithin { k: k as u32 }, None)),
        }
    }
    Ok((BmcOutcome::NoCexWithin { k: max_k }, None))
}

/// Portfolio-facing wrapper: `Some(true)` on a sound `Violated`, else `None`
/// (bounded / abstain). **Never `Some(false)`** — BMC cannot prove safety, so this
/// member can only ever contribute a reachable verdict.
pub fn decide_reachable_boolector(
    file: &Btor2File,
    max_k: u32,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<bool> {
    match bmc_bad_reachable_boolector(file, max_k, deadline, cancel) {
        Ok((BmcOutcome::Violated { .. }, _)) => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;
    use std::time::Duration;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn run(content: &str, max_k: u32) -> (BmcOutcome, Option<BmcTrace>) {
        let file = parser::parse(content).expect("parse btor2");
        let never = AtomicBool::new(false);
        bmc_bad_reachable_boolector(&file, max_k, far(), &never).expect("boolector bmc runs")
    }

    // `q` init 0, `next q = 1`, `bad = q`: violated at frame 1 (shallowest).
    const REACH: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                         6 next 1 4 3\n7 bad 4\n";
    // `q` init 0, `next q = 0`: never violated (bounded safe).
    const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 2\n\
                        6 bad 3\n";
    // 300-bit counter: init 0, `next = big + 1`, `bad = (big == 5)`. Reaches 5 at
    // depth 5 — a cone OVER the exact engine's auto-cap ceiling (192), so a BV engine
    // is the only one that decides it.
    const WIDE: &str = "1 sort bitvec 300\n2 zero 1\n3 one 1\n4 state 1 big\n5 init 1 4 2\n\
                        6 add 1 4 3\n7 next 1 4 6\n8 constd 1 5\n9 sort bitvec 1\n\
                        10 eq 9 4 8\n11 bad 10\n";

    #[test]
    fn finds_shallowest_cex() {
        assert_eq!(run(REACH, 5).0, BmcOutcome::Violated { depth: 1 });
    }

    #[test]
    fn bad_in_initial_state_is_depth_zero() {
        let d0 = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 3\n\
                  6 next 1 4 3\n7 bad 4\n";
        assert_eq!(run(d0, 5).0, BmcOutcome::Violated { depth: 0 });
    }

    #[test]
    fn no_cex_on_bounded_safe_design() {
        assert_eq!(run(SAFE, 10).0, BmcOutcome::NoCexWithin { k: 10 });
    }

    #[test]
    fn decides_beyond_the_exact_40_bit_cap() {
        assert_eq!(run(WIDE, 10).0, BmcOutcome::Violated { depth: 5 });
        // With too small a bound it is honestly bounded, never a wrong SAFE.
        assert_eq!(run(WIDE, 3).0, BmcOutcome::NoCexWithin { k: 3 });
    }

    #[test]
    fn strided_search_recovers_exact_depth_past_threshold() {
        // 8-bit counter, `bad = (cnt == 37)`. The CEX depth 37 is PAST the exact-check
        // threshold (32), so it is found by a STRIDED `reached_k` check (whose bound
        // overshoots to 40) — yet the reported depth must be the EXACT 37, read back
        // from the model, not the check bound. This is the core correctness guard for
        // the striding optimisation.
        const C37: &str = "1 sort bitvec 8\n2 zero 1\n3 one 1\n4 state 1 cnt\n5 init 1 4 2\n\
                           6 add 1 4 3\n7 next 1 4 6\n8 constd 1 37\n9 sort bitvec 1\n\
                           10 eq 9 4 8\n11 bad 10\n";
        let (outcome, trace) = run(C37, 60);
        assert_eq!(
            outcome,
            BmcOutcome::Violated { depth: 37 },
            "strided search must report the exact CEX depth (37), not the check bound (40)"
        );
        let trace = trace.expect("witness");
        assert_eq!(trace.states.len(), 38, "frames 0..=37");
        assert_eq!(
            crate::adapter::btor2::cex_value::u64_frames(&trace.states)
                .last()
                .unwrap(),
            &vec![("cnt".to_string(), 37)]
        );
    }

    #[test]
    fn no_bad_property_abstains() {
        let no_bad = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 2\n";
        let file = parser::parse(no_bad).expect("parse");
        let never = AtomicBool::new(false);
        assert_eq!(
            bmc_bad_reachable_boolector(&file, 5, far(), &never),
            Err(BoolectorError::NoBadProperty)
        );
    }

    #[test]
    fn array_sort_abstains_soundly() {
        // A design declaring an array sort must abstain (Err), never a verdict.
        let arr = "1 sort bitvec 1\n2 sort bitvec 3\n3 sort array 2 1\n\
                   4 state 1 q\n5 zero 1\n6 init 1 4 5\n7 next 1 4 5\n8 bad 4\n";
        let file = parser::parse(arr).expect("parse");
        let never = AtomicBool::new(false);
        assert_eq!(
            bmc_bad_reachable_boolector(&file, 5, far(), &never),
            Err(BoolectorError::UnsupportedSort)
        );
    }

    #[test]
    fn witness_is_the_concrete_counting_path() {
        // 2-bit up-counter: `cnt` init 0, `next = cnt + 1`, `bad = (cnt == 3)`.
        // `cnt` is fully determined by init + transition, so the ONLY model run is
        // the concrete 0→1→2→3 count — the extracted witness must be exactly that.
        const COUNTER: &str = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 cnt\n5 init 1 4 2\n\
                               6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                               10 eq 9 4 8\n11 bad 10\n";
        let (outcome, trace) = run(COUNTER, 5);
        assert_eq!(outcome, BmcOutcome::Violated { depth: 3 });
        let trace = trace.expect("a Violated verdict carries the witness");
        assert_eq!(
            crate::adapter::btor2::cex_value::u64_frames(&trace.states),
            vec![
                vec![("cnt".to_string(), 0)],
                vec![("cnt".to_string(), 1)],
                vec![("cnt".to_string(), 2)],
                vec![("cnt".to_string(), 3)],
            ],
            "the extracted trace must be the concrete 0→3 counting path"
        );
        assert_eq!(trace.inputs.len(), 3);
        assert!(trace.inputs.iter().all(Vec::is_empty));
    }

    #[test]
    fn constraint_blocks_a_counterexample() {
        // 2-bit `q`: 0→1→2→3→0, `bad = (q == 3)` reachable at depth 3.
        let base = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                    6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                    10 eq 9 4 8\n11 bad 10\n";
        assert_eq!(run(base, 5).0, BmcOutcome::Violated { depth: 3 });
        // With `constraint = !(q == 3)` the only violating state is excluded.
        let constrained = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                           6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                           10 eq 9 4 8\n12 not 9 10\n13 constraint 12\n11 bad 10\n";
        assert_eq!(run(constrained, 5).0, BmcOutcome::NoCexWithin { k: 5 });
    }

    #[test]
    fn deep_cex_search_respects_cancel_and_deadline() {
        let file = parser::parse(REACH).expect("parse");
        // A pre-set cancel flag ends the search immediately (no verdict claimed).
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            bmc_bad_reachable_boolector(&file, 64, far(), &cancelled)
                .expect("runs")
                .0,
            BmcOutcome::NoCexWithin { k: 0 }
        );
        // A passed deadline likewise abstains before any solving.
        let never = AtomicBool::new(false);
        let past = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            bmc_bad_reachable_boolector(&file, 64, past, &never)
                .expect("runs")
                .0,
            BmcOutcome::NoCexWithin { k: 0 }
        );
    }

    #[test]
    fn agrees_with_exact_engine_no_spurious_violated() {
        // Differential oracle: Boolector's `Violated` must NEVER contradict the
        // exact BDD engine (Bruns–Godefroid sound). On in-cap designs the exact
        // engine decides both directions.
        use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
        for (content, name) in [(REACH, "reach"), (SAFE, "safe")] {
            let outcome = run(content, 20).0;
            let exact_reachable = exact_bad_reachable(content).expect("exact decides in-cap");
            if exact_reachable {
                assert!(
                    matches!(outcome, BmcOutcome::Violated { .. }),
                    "{name}: exact says reachable but Boolector returned {outcome:?}"
                );
            } else {
                assert!(
                    !matches!(outcome, BmcOutcome::Violated { .. }),
                    "{name}: SOUNDNESS — exact says unreachable but Boolector returned {outcome:?}"
                );
            }
        }
    }

    #[test]
    fn decide_wrapper_never_reports_safe() {
        // The portfolio-facing wrapper returns Some(true) on a CEX, None otherwise —
        // and NEVER Some(false) (BMC cannot prove safety).
        let never = AtomicBool::new(false);
        let reach = parser::parse(REACH).expect("parse");
        let safe = parser::parse(SAFE).expect("parse");
        assert_eq!(
            decide_reachable_boolector(&reach, 20, far(), &never),
            Some(true)
        );
        assert_eq!(decide_reachable_boolector(&safe, 20, far(), &never), None);
    }

    #[test]
    fn agrees_with_native_z3_bmc() {
        // Cross-engine differential: Boolector and the native Z3 BMC must reach the
        // SAME bounded outcome on the shared fixtures — two independent BV back-ends
        // agreeing is the strongest owned-portfolio soundness signal.
        use crate::adapter::btor2::native_bmc::bmc_bad_reachable;
        for (content, k) in [(REACH, 20u32), (SAFE, 20), (WIDE, 10)] {
            let file = parser::parse(content).expect("parse");
            let z3 = bmc_bad_reachable(&file, k).expect("z3 bmc runs");
            let bl = run(content, k).0;
            assert_eq!(z3, bl, "z3 vs boolector disagreed on a fixture");
        }
    }
}
