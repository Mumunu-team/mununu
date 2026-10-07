//! Native BMC — in-process **bounded model checking** on the Z3
//! transition-relation seam ([`crate::adapter::sidecar::predicate_image::btor2_encode`]).
//!
//! This is the first slice of P1's *in-house* scalable-safety back-end: mununu's
//! OWN scalable safety verdict, no subprocess (no btormc/Pono dependency), running
//! everywhere Z3 links (make-ci / mununu-dev). It unrolls the BTOR2 transition
//! relation `k` steps in Z3 (QF_BV) and asks "is a `bad` reachable within `k`
//! steps?":
//!
//! - **SAT** ⇒ a concrete init→bad execution exists ⇒ [`BmcOutcome::Violated`] at
//!   that depth — a **sound** counterexample w.r.t. the BTOR2 model.
//! - **UNSAT within `k`** ⇒ [`BmcOutcome::NoCexWithin`] — **bounded**, NOT a proof
//!   of safety (that is k-induction / IMC, the follow-up slices).
//!
//! # Why it matters
//!
//! No 40-bit cone cap (Z3 QF_BV, not BDD), so it decides reachable-`bad` on wide
//! datapaths the exact engine abstains on — the scale win the P1 anchors
//! (`synth_pipeline(W)`, HWMCC) demand. It is bit-precise, so a `Violated` verdict
//! is cross-checkable against the exact engine + btormc under the differential
//! oracle.
//!
//! # Soundness
//!
//! BMC only ever claims **Violated** (a reachable `bad`), never **Safe** — so it
//! cannot produce a spurious safety proof. A `Violated` is `Init(s⁰) ∧ ⋀ⱼ T(sʲ,sʲ⁺¹)
//! ∧ ⋀ⱼ C(sʲ) ∧ Bad(sᵏ)` being SAT: a concrete run from an initial state, honouring
//! every `constraint`, reaching `bad`. State cells **without** an `init` line are
//! left free at frame 0 — that is exactly BTOR2's semantics (an init-less state is
//! nondeterministic), so a free-init counterexample is a real run of the model.
//! (The SAFE direction's free-init subtlety that the exact engine guards against
//! does not arise here — BMC never asserts SAFE.)
//!
//! # Scope (slice 1)
//!
//! Pure-bitvector (`Theory::BvOnly`) designs. An array/memory design (HWMCC memory
//! track) currently returns [`BmcError::Encode`]; wiring the `BvUfArray` theory is
//! a follow-up. k-induction (unbounded SAFE proof) and IMC are the next slices.

use std::collections::HashMap;

use crate::adapter::btor2::ast::{Btor2File, Nid, Node};
use crate::adapter::sidecar::predicate_image::btor2_encode::{
    Btor2SmtView, EncodeError, encode_design,
};
use z3::ast::{Ast, BV, Bool};

/// Default unrolling / induction depth bound for the portfolio-facing native
/// engine — mirrors [`crate::adapter::btormc::DEFAULT_KMAX`].
pub const DEFAULT_MAX_K: u32 = 40;

/// Default per-check Z3 wall-clock budget (milliseconds) for the portfolio-facing
/// native engine. A check that exceeds it returns `Unknown`, and the engine
/// **abstains** (never a wrong verdict) — the in-process analogue of the
/// subprocess members' kill-on-timeout.
pub const DEFAULT_TIMEOUT_MS: u32 = 5_000;

/// The outcome of a bounded model-checking run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmcOutcome {
    /// A `bad` is reachable at exactly `depth` steps from an initial state — a
    /// sound counterexample. `depth = 0` means `bad` holds in an initial state.
    Violated { depth: u32 },
    /// No `bad` is reachable within `k` steps. **Bounded** — this is NOT a proof
    /// of safety (a deeper counterexample may exist; k-induction / IMC decide that).
    NoCexWithin { k: u32 },
}

/// A concrete `Init → bad` execution witness extracted from the BMC model
/// ([`bmc_bad_reachable_witness`]). Each frame lists only the NAMED state cells /
/// inputs (a BTOR2 line with a `symbol`), so it maps back onto the original
/// design's registers without projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmcTrace {
    /// State-register valuations, frame 0 (reset) .. frame k (the bad state), each a
    /// sorted `(symbol, value)` list (only named state cells).
    pub states: Vec<Vec<(String, u64)>>,
    /// Input valuations driving each transition, frame 0 .. frame k-1.
    pub inputs: Vec<Vec<(String, u64)>>,
}

/// A complete safety verdict from the k-induction driver
/// ([`decide_bad_safety`]) — BMC's bounded outcome plus a genuine SAFE proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyVerdict {
    /// A `bad` is reachable at `depth` steps — a sound counterexample (BMC base).
    Violated { depth: u32 },
    /// The `bad` is proven UNREACHABLE: the property is `k`-inductive (base holds
    /// through depth `k` and the inductive step at `k` is unsatisfiable). A sound,
    /// unbounded safety proof.
    Safe { k: u32 },
    /// Neither a counterexample nor a `k`-inductive proof within `max_k` —
    /// inconclusive (a larger bound, a strengthening, or IMC may decide it).
    Unknown { k: u32 },
}

/// Why a BMC run could not produce a verdict.
#[derive(Debug, Clone)]
pub enum BmcError {
    /// The design could not be encoded to a Z3 QF_BV transition relation (an
    /// unsupported op, or an array/memory sort — the `BvUfArray` follow-up).
    Encode(EncodeError),
    /// The design declares no `bad` property — nothing to check.
    NoBadProperty,
}

impl std::fmt::Display for BmcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BmcError::Encode(e) => write!(f, "native BMC: encode failed: {e:?}"),
            BmcError::NoBadProperty => write!(f, "native BMC: design has no `bad` property"),
        }
    }
}

/// The property / init / assumption node references extracted from a BTOR2 file:
/// `(bad operands, (state, init-value) pairs, constraint operands)`.
type Props = (Vec<Nid>, Vec<(Nid, Nid)>, Vec<Nid>);

fn extract_props(file: &Btor2File) -> Props {
    let mut bad_ops = Vec::new();
    let mut init_pairs = Vec::new();
    let mut constraint_ops = Vec::new();
    for l in &file.lines {
        match &l.node {
            Node::Bad { signal } => bad_ops.push(signal.nid()),
            Node::Init { state, value, .. } => init_pairs.push((*state, value.nid())),
            Node::Constraint { signal } => constraint_ops.push(signal.nid()),
            _ => {}
        }
    }
    (bad_ops, init_pairs, constraint_ops)
}

/// Build `Nid → symbol` maps for the NAMED state cells and inputs — the cells the
/// witness trace ([`BmcTrace`]) reports. An anonymous state/input (no `symbol`) is
/// not surfaced in the trace.
fn symbol_maps(file: &Btor2File) -> (HashMap<Nid, String>, HashMap<Nid, String>) {
    let mut state_syms = HashMap::new();
    let mut input_syms = HashMap::new();
    for l in &file.lines {
        match &l.node {
            Node::State {
                symbol: Some(s), ..
            } => {
                state_syms.insert(l.nid, s.clone());
            }
            Node::Input {
                symbol: Some(s), ..
            } => {
                input_syms.insert(l.nid, s.clone());
            }
            _ => {}
        }
    }
    (state_syms, input_syms)
}

/// The shared unrolling machinery: fresh per-frame state/input Z3 variables and
/// the frame-instantiated `bad` / `transition` / `constraint` terms. Built once
/// per run inside a [`z3::with_z3_config`] scope; BMC and k-induction both drive it.
struct Unroller<'a> {
    view: &'a Btor2SmtView,
    bad_ops: &'a [Nid],
    constraint_ops: &'a [Nid],
    /// `frame_state[j][nid]` — state cell `nid`'s value at frame `j`.
    frame_state: Vec<HashMap<Nid, BV>>,
    /// `frame_input[j][nid]` — input `nid`'s value at frame `j`.
    frame_input: Vec<HashMap<Nid, BV>>,
    /// The 1-bit constant `1`, reused for `bad`/`constraint` truth (`op == 1`).
    one1: BV,
}

impl<'a> Unroller<'a> {
    /// Build fresh frame variables for depths `0..=n`.
    fn build(
        view: &'a Btor2SmtView,
        bad_ops: &'a [Nid],
        constraint_ops: &'a [Nid],
        n: usize,
    ) -> Self {
        let fresh = |src: &HashMap<Nid, BV>, prefix: &str| -> HashMap<Nid, BV> {
            src.iter()
                .map(|(nid, bv)| (*nid, BV::fresh_const(prefix, bv.get_size())))
                .collect()
        };
        Unroller {
            view,
            bad_ops,
            constraint_ops,
            frame_state: (0..=n).map(|_| fresh(&view.state_curr, "bmc_s")).collect(),
            frame_input: (0..=n).map(|_| fresh(&view.inputs, "bmc_i")).collect(),
            one1: BV::from_u64(1, 1),
        }
    }

    /// Current-cycle BV of a `bad`/`constraint` operand, falling back through
    /// `state_curr`/`inputs` so a property referencing a state cell or input
    /// directly (`bad = q`) is not dropped.
    fn curr_bv(&self, nid: &Nid) -> Option<&BV> {
        self.view
            .signal_bvs
            .get(nid)
            .or_else(|| self.view.state_curr.get(nid))
            .or_else(|| self.view.inputs.get(nid))
    }

    /// `(state_curr → frame j) ∪ (inputs → frame j)` — instantiates a current-cycle
    /// term at frame `j`.
    fn curr_subs(&self, j: usize) -> Vec<(&BV, &BV)> {
        let mut pairs: Vec<(&BV, &BV)> = Vec::new();
        for (nid, bv) in &self.view.state_curr {
            pairs.push((bv, &self.frame_state[j][nid]));
        }
        for (nid, bv) in &self.view.inputs {
            pairs.push((bv, &self.frame_input[j][nid]));
        }
        pairs
    }

    /// The transition relation for frame `j → j+1`.
    fn transition_at(&self, j: usize) -> Bool {
        let mut pairs = self.curr_subs(j);
        for (nid, bv) in &self.view.state_next {
            pairs.push((bv, &self.frame_state[j + 1][nid]));
        }
        self.view.transition.substitute(&pairs)
    }

    /// `bad` at frame `j`: OR over all `bad` operands of `(operand == 1)`.
    fn bad_at(&self, j: usize) -> Bool {
        let pairs = self.curr_subs(j);
        let disj: Vec<Bool> = self
            .bad_ops
            .iter()
            .filter_map(|op| {
                self.curr_bv(op)
                    .map(|bv| bv.substitute(&pairs).eq(&self.one1))
            })
            .collect();
        let refs: Vec<&Bool> = disj.iter().collect();
        if refs.is_empty() {
            Bool::from_bool(false)
        } else {
            Bool::or(&refs)
        }
    }

    /// Each `constraint` at frame `j`: `(operand == 1)`.
    fn constraints_at(&self, j: usize) -> Vec<Bool> {
        let pairs = self.curr_subs(j);
        self.constraint_ops
            .iter()
            .filter_map(|op| {
                self.curr_bv(op)
                    .map(|bv| bv.substitute(&pairs).eq(&self.one1))
            })
            .collect()
    }

    /// Assert the design's `init` values on frame 0 into `solver` (init-less state
    /// cells stay free — BTOR2's nondeterministic-init semantics).
    fn assert_init(&self, solver: &z3::Solver, init_pairs: &[(Nid, Nid)]) {
        for (state, value_nid) in init_pairs {
            if let (Some(s0), Some(vbv)) = (
                self.frame_state[0].get(state),
                self.view.signal_bvs.get(value_nid),
            ) {
                solver.assert(s0.eq(vbv));
            }
        }
    }

    /// Extract the concrete `Init → bad` witness from a SAT `model` whose `bad`
    /// fired at frame `k`: per-frame state (frames `0..=k`) and input (frames
    /// `0..k`) valuations for the NAMED cells, each a sorted `(symbol, value)` list.
    /// `model_completion` is on, so a free (init-less / unconstrained-input) cell
    /// gets a concrete value — the trace is a full assignment of a real model run.
    fn extract_trace(
        &self,
        model: &z3::Model,
        k: usize,
        state_syms: &HashMap<Nid, String>,
        input_syms: &HashMap<Nid, String>,
    ) -> BmcTrace {
        let eval_frame =
            |frame: &HashMap<Nid, BV>, syms: &HashMap<Nid, String>| -> Vec<(String, u64)> {
                let mut out: Vec<(String, u64)> = frame
                    .iter()
                    .filter_map(|(nid, bv)| {
                        let sym = syms.get(nid)?;
                        let val = model.eval(bv, true)?.as_u64()?;
                        Some((sym.clone(), val))
                    })
                    .collect();
                out.sort();
                out
            };
        let states = (0..=k)
            .map(|j| eval_frame(&self.frame_state[j], state_syms))
            .collect();
        let inputs = (0..k)
            .map(|j| eval_frame(&self.frame_input[j], input_syms))
            .collect();
        BmcTrace { states, inputs }
    }
}

/// Bounded model check: is a `bad` reachable within `max_k` steps of the BTOR2
/// transition relation? See [`BmcOutcome`] for the verdict semantics.
///
/// Runs entirely in-process on Z3 (no subprocess). `max_k` bounds the unrolling
/// depth; the returned `Violated { depth }` is the SHALLOWEST counterexample
/// (frames are checked in increasing order), which is the most useful witness.
pub fn bmc_bad_reachable(file: &Btor2File, max_k: u32) -> Result<BmcOutcome, BmcError> {
    bmc_bad_reachable_witness(file, max_k).map(|(outcome, _trace)| outcome)
}

/// As [`bmc_bad_reachable`], but on a `Violated` verdict also extracts the concrete
/// `Init → bad` execution witness ([`BmcTrace`]) from the Z3 model.
///
/// The search loop is identical to [`bmc_bad_reachable`]'s; the only difference is
/// that on SAT at frame `k` — before the `pop` that discards the model — the model
/// is queried for every named per-frame state/input variable, giving the concrete
/// counting/driving path that reaches `bad`. `NoCexWithin` carries no trace
/// (`None`); a `Violated` carries `Some(trace)` unless the solver returned no model
/// (also `None`, defensively — a SAT check always has a model here).
pub fn bmc_bad_reachable_witness(
    file: &Btor2File,
    max_k: u32,
) -> Result<(BmcOutcome, Option<BmcTrace>), BmcError> {
    let (bad_ops, init_pairs, constraint_ops) = extract_props(file);
    if bad_ops.is_empty() {
        return Err(BmcError::NoBadProperty);
    }
    let (state_syms, input_syms) = symbol_maps(file);
    let n = max_k as usize;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = encode_design(file).map_err(BmcError::Encode)?;
        let u = Unroller::build(&view, &bad_ops, &constraint_ops, n);

        // A single init-constrained path: check `bad` at each frame, extend if clean.
        let solver = z3::Solver::new();
        u.assert_init(&solver, &init_pairs);
        for c in u.constraints_at(0) {
            solver.assert(&c);
        }
        for k in 0..=n {
            solver.push();
            solver.assert(u.bad_at(k));
            let sat = matches!(solver.check(), z3::SatResult::Sat);
            if sat {
                // Extract the concrete witness from the model BEFORE popping it.
                let trace = solver
                    .get_model()
                    .map(|m| u.extract_trace(&m, k, &state_syms, &input_syms));
                solver.pop(1);
                return Ok((BmcOutcome::Violated { depth: k as u32 }, trace));
            }
            solver.pop(1);
            if k < n {
                solver.assert(u.transition_at(k));
                for c in u.constraints_at(k + 1) {
                    solver.assert(&c);
                }
            }
        }
        Ok((BmcOutcome::NoCexWithin { k: max_k }, None))
    })
}

/// Decide `bad`-reachability by **k-induction**: BMC for counterexamples + an
/// inductive-step proof for safety. Runs entirely in-process on Z3.
///
/// For increasing `k = 0..=max_k`, on the shared [`Unroller`]:
/// - **Base** — is `bad` reachable at frame `k` from an initial state? SAT ⇒
///   [`SafetyVerdict::Violated`] (the shallowest counterexample).
/// - **Step** — is the property `k`-inductive? Over UNCONSTRAINED frames (no
///   init), with `¬bad` assumed at frames `0..k` and the transitions chained, is
///   `bad` at frame `k` unsatisfiable? UNSAT ⇒ [`SafetyVerdict::Safe`] (base has
///   been verified through depth `k`, so k-induction gives a sound safety proof).
///
/// If neither fires by `max_k`, [`SafetyVerdict::Unknown`]. This is plain
/// k-induction — sound in both directions, but not guaranteed complete (a design
/// whose shortest inductive certificate needs a *simple-path* restriction, or IMC,
/// may return `Unknown`). The base and step run on independent solvers over the
/// SAME frame variables (shared Z3 consts, separate assertion stacks), so the
/// step's free initial state does not clash with the base's pinned init.
///
/// `timeout_ms` sets a per-check Z3 wall-clock budget: a check that exceeds it
/// returns `Unknown`, and the engine **abstains** ([`SafetyVerdict::Unknown`]) —
/// never a wrong verdict. `None` = unbounded (QF_BV is decidable, so it still
/// terminates). This is what lets the portfolio bound the in-process member the
/// way it kills a timed-out subprocess.
///
/// **Soundness of the SAT/UNSAT/UNKNOWN handling.** `Violated` is returned ONLY on
/// a definite base `Sat`; `Safe` ONLY on a definite step `Unsat`. Any `Unknown`
/// (a timeout, or Z3 giving up) abstains — crucially, a step `Unknown` must NOT be
/// read as "not satisfiable ⇒ inductive" (that would be a spurious `Safe`).
pub fn decide_bad_safety(
    file: &Btor2File,
    max_k: u32,
    timeout_ms: Option<u32>,
) -> Result<SafetyVerdict, BmcError> {
    decide_bad_safety_cancellable(file, max_k, timeout_ms, None)
}

/// [`decide_bad_safety`] with the reach portfolio's cooperative **cancel** flag (S1 of the
/// engine-performance roadmap): checked before each depth, like [`bmc_cex_until`], so once a
/// peer member has decided the engine abstains at the first depth boundary past the
/// [`crate::adapter::MEMBER_CANCEL_GRACE`] instead of running the remaining
/// `(max_k - k) × 2` queries at up to `timeout_ms` each. The grace keeps a run that is about
/// to finish in the portfolio's attribution; a cancelled run is [`SafetyVerdict::Unknown`]
/// at the depth it stopped — never a wrong verdict. The latency of a cancellation is bounded
/// by the grace plus the query in flight (≤ `timeout_ms`).
pub fn decide_bad_safety_cancellable(
    file: &Btor2File,
    max_k: u32,
    timeout_ms: Option<u32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<SafetyVerdict, BmcError> {
    let (bad_ops, init_pairs, constraint_ops) = extract_props(file);
    if bad_ops.is_empty() {
        return Err(BmcError::NoBadProperty);
    }
    let n = max_k as usize;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = encode_design(file).map_err(BmcError::Encode)?;
        let u = Unroller::build(&view, &bad_ops, &constraint_ops, n);

        // Base: an init-constrained path (as in `bmc_bad_reachable`).
        let base = z3::Solver::new();
        // Step: a FREE path (no init) accumulating `¬bad ∧ transition` per frame;
        // if `bad` at the current frame is then unsatisfiable, the property is
        // k-inductive.
        let step = z3::Solver::new();
        if let Some(ms) = timeout_ms {
            let mut params = z3::Params::new();
            params.set_u32("timeout", ms);
            base.set_params(&params);
            step.set_params(&params);
        }
        u.assert_init(&base, &init_pairs);
        for c in u.constraints_at(0) {
            base.assert(&c);
            step.assert(&c);
        }

        let mut cancel_seen: Option<std::time::Instant> = None;
        for k in 0..=n {
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                && cancel_seen
                    .get_or_insert_with(std::time::Instant::now)
                    .elapsed()
                    >= crate::adapter::MEMBER_CANCEL_GRACE
            {
                return Ok(SafetyVerdict::Unknown { k: k as u32 });
            }
            // Base — reachable counterexample? Violated ONLY on a definite Sat.
            base.push();
            base.assert(u.bad_at(k));
            let base_res = base.check();
            base.pop(1);
            match base_res {
                z3::SatResult::Sat => return Ok(SafetyVerdict::Violated { depth: k as u32 }),
                z3::SatResult::Unknown => return Ok(SafetyVerdict::Unknown { k: k as u32 }),
                z3::SatResult::Unsat => {}
            }
            // Step — k-inductive? Safe ONLY on a definite Unsat; an Unknown abstains
            // (must NOT be read as "inductive"). Base has held through depth k above.
            step.push();
            step.assert(u.bad_at(k));
            let step_res = step.check();
            step.pop(1);
            match step_res {
                z3::SatResult::Unsat => return Ok(SafetyVerdict::Safe { k: k as u32 }),
                z3::SatResult::Unknown => return Ok(SafetyVerdict::Unknown { k: k as u32 }),
                z3::SatResult::Sat => {}
            }
            if k < n {
                // Extend both paths to frame k+1.
                base.assert(u.transition_at(k));
                step.assert(u.bad_at(k).not()); // assume ¬bad on the inductive path
                step.assert(u.transition_at(k));
                for c in u.constraints_at(k + 1) {
                    base.assert(&c);
                    step.assert(&c);
                }
            }
        }
        Ok(SafetyVerdict::Unknown { k: max_k })
    })
}

/// Time-boxed **pure counterexample search** — the "counterstrategy" half of the
/// owned portfolio, run alongside (not inside) the safety proof.
///
/// Unrolls the transition relation depth-by-depth on a single init-constrained solver,
/// looking ONLY for a reachable `bad` (no k-induction step queries to slow it down or
/// abort it). Returns `Some(depth)` on a concrete, sound `Violated`; `None` if no
/// counterexample is found within budget.
///
/// The point of splitting this out: finding a concrete counterexample is *sound* and
/// is usually far cheaper than synthesising an equivalent safety invariant, so a deep,
/// time-boxed CEX search flips many designs that the shallow ([`DEFAULT_MAX_K`]) or
/// proof-oriented engines leave `Unknown` straight to a definite `Violated`. `max_k`
/// may therefore be set well above [`DEFAULT_MAX_K`]. Bounded by the wall `deadline`
/// and `cancel` (both checked before each depth) and by `per_query_ms` per z3 check.
pub fn bmc_cex_until(
    file: &Btor2File,
    max_k: u32,
    per_query_ms: u32,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<u32> {
    let (bad_ops, init_pairs, constraint_ops) = extract_props(file);
    if bad_ops.is_empty() {
        return None;
    }
    let n = max_k as usize;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = encode_design(file).ok()?;
        let u = Unroller::build(&view, &bad_ops, &constraint_ops, n);
        let base = z3::Solver::new();
        let mut params = z3::Params::new();
        params.set_u32("timeout", per_query_ms);
        base.set_params(&params);
        u.assert_init(&base, &init_pairs);
        for c in u.constraints_at(0) {
            base.assert(&c);
        }
        for k in 0..=n {
            // Wall-clock / cancellation bound: abandon the search (return `None`, never
            // a wrong verdict) when the deadline passes or a peer engine decides first.
            if cancel.load(std::sync::atomic::Ordering::Relaxed)
                || std::time::Instant::now() >= deadline
            {
                return None;
            }
            base.push();
            base.assert(u.bad_at(k));
            let res = base.check();
            base.pop(1);
            match res {
                // A concrete init→bad path at depth `k` — sound `Violated`.
                z3::SatResult::Sat => return Some(k as u32),
                // This depth's query hit `per_query_ms`; deeper frames only get larger,
                // so abstain rather than spin.
                z3::SatResult::Unknown => return None,
                z3::SatResult::Unsat => {}
            }
            if k < n {
                base.assert(u.transition_at(k));
                for c in u.constraints_at(k + 1) {
                    base.assert(&c);
                }
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    fn bmc(content: &str, max_k: u32) -> BmcOutcome {
        let file = parser::parse(content).expect("parse btor2");
        bmc_bad_reachable(&file, max_k).expect("bmc runs")
    }

    // `q` init 0, `next q = 1`, `bad = q`. `bad` is false at frame 0 (q=0), true
    // at frame 1 (q=1) — the shallowest CEX is depth 1.
    const REACH: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                         6 next 1 4 3\n7 bad 4\n";
    // `q` init 0, `next q = 0` (stays 0), `bad = q`. Never violated (bounded safe).
    const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 2\n\
                        6 bad 3\n";
    // 300-bit counter: init 0, `next = big + 1`, `bad = (big == 5)`. Reaches 5 at
    // depth 5. The 300-bit cone is OVER the exact engine's auto-cap ceiling (192), so
    // only a SAT-based engine (this) decides it — the beyond-cap scale win.
    const WIDE: &str = "1 sort bitvec 300\n2 zero 1\n3 one 1\n4 state 1 big\n5 init 1 4 2\n\
                        6 add 1 4 3\n7 next 1 4 6\n8 constd 1 5\n9 sort bitvec 1\n\
                        10 eq 9 4 8\n11 bad 10\n";

    #[test]
    fn finds_shallowest_cex() {
        assert_eq!(bmc(REACH, 5), BmcOutcome::Violated { depth: 1 });
    }

    #[test]
    fn bmc_cex_until_finds_cex_and_abstains_soundly() {
        use std::sync::atomic::AtomicBool;
        let far = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let never = AtomicBool::new(false);
        // Reaches bad at depth 1 / 5 — the deep CEX-only search returns the depth.
        let reach = parser::parse(REACH).expect("parse");
        assert_eq!(bmc_cex_until(&reach, 64, 5_000, far, &never), Some(1));
        let wide = parser::parse(WIDE).expect("parse"); // depth 5, 300-bit (beyond the auto-cap ceiling)
        assert_eq!(bmc_cex_until(&wide, 64, 5_000, far, &never), Some(5));
        // Bounded-safe design → no CEX within the depth budget → None (never a wrong verdict).
        let safe = parser::parse(SAFE).expect("parse");
        assert_eq!(bmc_cex_until(&safe, 32, 5_000, far, &never), None);
        // Cancellation / passed deadline are honored before any work.
        let cancelled = AtomicBool::new(true);
        assert_eq!(bmc_cex_until(&reach, 64, 5_000, far, &cancelled), None);
        let past = far - std::time::Duration::from_secs(120);
        assert_eq!(bmc_cex_until(&reach, 64, 5_000, past, &never), None);
    }

    /// S1 (engine-performance roadmap) — the safety engine's cancel flag is checked before
    /// each depth, with the member grace: a run that finishes inside the grace still lands
    /// (the flag is set from the start here and the depth-1 counterexample is still found,
    /// identical to the uncancelled run), while a long unrolling abstains at the first depth
    /// past the grace with `Unknown`, never a verdict and never its full `max_k` of queries.
    #[test]
    fn decide_bad_safety_cancel_lands_inside_the_grace_and_abstains_past_it() {
        use std::sync::atomic::AtomicBool;
        let cancelled = AtomicBool::new(true);
        let reach = parser::parse(REACH).expect("parse");
        assert_eq!(
            decide_bad_safety_cancellable(&reach, 5, None, Some(&cancelled)).expect("k-ind ok"),
            decide_bad_safety(&reach, 5, None).expect("k-ind ok")
        );
        assert!(matches!(
            decide_bad_safety(&reach, 5, None).expect("k-ind ok"),
            SafetyVerdict::Violated { depth: 1 }
        ));
        // A 64-bit free counter whose `bad` (all ones) is 2^64 - 1 steps away and not
        // k-inductive at any small k: 401 depths of base + step queries is well past the
        // grace, so the cancelled run stops early with the depth it reached.
        const LONG: &str = "1 sort bitvec 64\n2 zero 1\n3 state 1 cnt\n4 init 1 3 2\n5 one 1\n\
                            6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                            10 eq 9 3 8\n11 bad 10\n";
        let long = parser::parse(LONG).expect("parse");
        let t0 = std::time::Instant::now();
        let v =
            decide_bad_safety_cancellable(&long, 400, None, Some(&cancelled)).expect("k-ind ok");
        let took = t0.elapsed();
        assert!(
            matches!(v, SafetyVerdict::Unknown { k } if k < 400),
            "a cancelled long run abstains before its depth cap: {v:?}"
        );
        assert!(
            took >= crate::adapter::MEMBER_CANCEL_GRACE
                && took < std::time::Duration::from_secs(60),
            "stops at the first depth past the grace, took {took:?}"
        );
    }

    #[test]
    fn witness_is_the_concrete_counting_path() {
        // 2-bit up-counter: `cnt` init 0, `next = cnt + 1`, `bad = (cnt == 3)`.
        // `bad` is reachable at depth 3 and `cnt` is fully determined by init +
        // transition (no free input drives it), so the ONLY model run is the
        // concrete 0→1→2→3 count — the extracted witness must be exactly that path,
        // proving the trace is the real execution and not a fabricated one.
        const COUNTER: &str = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 cnt\n5 init 1 4 2\n\
                               6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                               10 eq 9 4 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse btor2");
        let (outcome, trace) = bmc_bad_reachable_witness(&file, 5).expect("bmc runs");
        assert_eq!(outcome, BmcOutcome::Violated { depth: 3 });
        let trace = trace.expect("a Violated verdict carries the witness trace");
        assert_eq!(
            trace.states,
            vec![
                vec![("cnt".to_string(), 0)],
                vec![("cnt".to_string(), 1)],
                vec![("cnt".to_string(), 2)],
                vec![("cnt".to_string(), 3)],
            ],
            "the extracted trace must be the concrete 0→3 counting path"
        );
        // The last state satisfies the `bad` condition (cnt == 3) — the trace really
        // ends in a violating state.
        assert_eq!(trace.states.last().unwrap(), &vec![("cnt".to_string(), 3)]);
        // 3 transitions ⇒ 3 input frames; this design has no named inputs ⇒ empty.
        assert_eq!(trace.inputs.len(), 3);
        assert!(trace.inputs.iter().all(Vec::is_empty));
    }

    #[test]
    fn no_witness_when_bounded_safe() {
        // A bounded-safe design produces `NoCexWithin` and therefore no trace.
        let file = parser::parse(SAFE).expect("parse btor2");
        let (outcome, trace) = bmc_bad_reachable_witness(&file, 10).expect("bmc runs");
        assert_eq!(outcome, BmcOutcome::NoCexWithin { k: 10 });
        assert!(
            trace.is_none(),
            "a bounded-safe run carries no counterexample"
        );
    }

    #[test]
    fn bad_in_initial_state_is_depth_zero() {
        // `q` init 1, `bad = q` — violated already at frame 0.
        let d0 = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 3\n\
                  6 next 1 4 3\n7 bad 4\n";
        assert_eq!(bmc(d0, 5), BmcOutcome::Violated { depth: 0 });
    }

    #[test]
    fn no_cex_on_bounded_safe_design() {
        // BMC never claims SAFE — only "no CEX within k".
        assert_eq!(bmc(SAFE, 10), BmcOutcome::NoCexWithin { k: 10 });
    }

    #[test]
    fn decides_beyond_the_exact_40_bit_cap() {
        // An 300-bit reachability the exact BDD engine abstains on (over the 192-bit auto-cap
        // ceiling); native BMC finds the depth-5 counterexample bit-precisely.
        assert_eq!(bmc(WIDE, 10), BmcOutcome::Violated { depth: 5 });
        // And with too small a bound it is honestly bounded, never a wrong SAFE.
        assert_eq!(bmc(WIDE, 3), BmcOutcome::NoCexWithin { k: 3 });
    }

    #[test]
    fn agrees_with_exact_engine_no_spurious_violated() {
        // Differential-oracle cross-check: BMC's `Violated` must NEVER contradict
        // the exact engine (Bruns–Godefroid sound). On in-cap designs the exact
        // engine decides both directions; BMC must (a) find a CEX where exact says
        // reachable, and (b) NEVER report Violated where exact says unreachable —
        // a spurious Violated would be a soundness bug.
        use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
        for (content, name) in [(REACH, "reach"), (SAFE, "safe")] {
            let file = parser::parse(content).expect("parse");
            let bmc = bmc_bad_reachable(&file, 20).expect("bmc runs");
            let exact_reachable = exact_bad_reachable(content).expect("exact decides in-cap");
            if exact_reachable {
                assert!(
                    matches!(bmc, BmcOutcome::Violated { .. }),
                    "{name}: exact says reachable but BMC returned {bmc:?}"
                );
            } else {
                assert!(
                    !matches!(bmc, BmcOutcome::Violated { .. }),
                    "{name}: SOUNDNESS — exact says unreachable but BMC returned {bmc:?}"
                );
            }
        }
    }

    #[test]
    fn no_bad_property_is_an_error() {
        let no_bad = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n5 next 1 3 2\n";
        let file = parser::parse(no_bad).expect("parse");
        assert!(matches!(
            bmc_bad_reachable(&file, 5),
            Err(BmcError::NoBadProperty)
        ));
    }

    #[test]
    fn constraint_can_block_a_counterexample() {
        // `q` init 0, `next = q + 1` (2-bit: 0→1→2→3→0), `bad = (q == 3)`. Without
        // a constraint, bad is reachable at depth 3. Add `constraint = (q != 3)` —
        // wait, that would make bad+constraint jointly UNSAT at the bad frame, so
        // no CEX: the assumption forbids the only violating state.
        // (sort id 1 = bitvec 2, the datapath; sort id 9 = bitvec 1, the bool.)
        let base = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                    6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                    10 eq 9 4 8\n11 bad 10\n";
        assert_eq!(bmc(base, 5), BmcOutcome::Violated { depth: 3 });
        // Same design + `constraint = !(q == 3)` (the negation of bad): the only
        // violating state is excluded, so no reachable bad honours the assumption.
        let constrained = "1 sort bitvec 2\n2 zero 1\n3 one 1\n4 state 1 q\n5 init 1 4 2\n\
                           6 add 1 4 3\n7 next 1 4 6\n8 constd 1 3\n9 sort bitvec 1\n\
                           10 eq 9 4 8\n12 not 9 10\n13 constraint 12\n11 bad 10\n";
        assert_eq!(bmc(constrained, 5), BmcOutcome::NoCexWithin { k: 5 });
    }

    // ---- k-induction (decide_bad_safety) — the SAFE-proving slice ----

    fn safety(content: &str, max_k: u32) -> SafetyVerdict {
        let file = parser::parse(content).expect("parse btor2");
        decide_bad_safety(&file, max_k, None).expect("k-induction runs")
    }

    #[test]
    fn k_induction_proves_a_1_inductive_invariant_safe() {
        // `q` stays 0, `bad = q`. `q == 0` is not 0-inductive (a free state can be
        // 1) but IS 1-inductive (the transition pins next q to 0) — proven SAFE.
        assert_eq!(safety(SAFE, 10), SafetyVerdict::Safe { k: 1 });
    }

    #[test]
    fn k_induction_finds_the_counterexample_first() {
        // The base case finds the depth-1 CEX before the step could prove safety.
        assert_eq!(safety(REACH, 10), SafetyVerdict::Violated { depth: 1 });
    }

    #[test]
    fn k_induction_proves_safe_beyond_the_exact_40_bit_cap() {
        // An 300-bit register that stays 0, `bad = (big != 0)`. The exact BDD engine
        // abstains (over the 192-bit auto-cap ceiling); native k-induction proves it SAFE
        // (1-inductive) bit-precisely — an in-house UNBOUNDED safety proof past the cap.
        let wide_safe = "1 sort bitvec 300\n2 zero 1\n3 state 1 big\n4 init 1 3 2\n\
                         5 next 1 3 3\n6 sort bitvec 1\n7 neq 6 3 2\n8 bad 7\n";
        assert_eq!(safety(wide_safe, 10), SafetyVerdict::Safe { k: 1 });
    }

    #[test]
    fn k_induction_agrees_with_the_exact_engine() {
        // Differential-oracle cross-check: a DEFINITE k-induction verdict must
        // match the exact engine's reachability — never a spurious Safe/Violated.
        use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
        for (content, name) in [(REACH, "reach"), (SAFE, "safe")] {
            let exact_reachable = exact_bad_reachable(content).expect("exact decides in-cap");
            match safety(content, 20) {
                SafetyVerdict::Violated { .. } => assert!(
                    exact_reachable,
                    "{name}: SOUNDNESS — k-induction Violated but exact says unreachable"
                ),
                SafetyVerdict::Safe { .. } => assert!(
                    !exact_reachable,
                    "{name}: SOUNDNESS — k-induction Safe but exact says reachable"
                ),
                SafetyVerdict::Unknown { .. } => {
                    panic!("{name}: expected a definite k-induction verdict")
                }
            }
        }
    }

    #[test]
    fn scale_curve_native_decides_past_the_exact_40_bit_cap() {
        // The roadmap's `synth_pipeline(W)` signal, distilled to a scale CURVE: a
        // W-bit register that stays 0 (`AG(big == 0)`, encoded `bad = big != 0`),
        // swept over widths. The native k-induction engine proves it SAFE at EVERY
        // width; the exact BDD engine ABSTAINS once the cone exceeds its cap (the
        // auto-cap ceiling, 192). That is the in-house scale win charted, not asserted by hand.
        use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
        let wide_safe = |w: u32| -> String {
            format!(
                "1 sort bitvec {w}\n2 zero 1\n3 state 1 big\n4 init 1 3 2\n\
                 5 next 1 3 3\n6 sort bitvec 1\n7 neq 6 3 2\n8 bad 7\n"
            )
        };
        // Native proves SAFE at every width — including far past the BDD cap.
        for w in [8u32, 20, 40, 64, 128, 256, 512] {
            let file = parser::parse(&wide_safe(w)).expect("parse");
            assert_eq!(
                decide_bad_safety(&file, 5, None).expect("native runs"),
                SafetyVerdict::Safe { k: 1 },
                "native must prove W={w} SAFE"
            );
        }
        // The exact engine abstains once the cone is over-cap (> 192 bits, the auto-cap
        // ceiling raised in P2.5-A), where the native engine still decides — the scale delta.
        // (At W ≤ 192 the auto-cap now admits the cone and exact DECIDES — covered by
        // `symbolic_bitblast::auto_cap_admits_modestly_wide_concrete_cone`.)
        for w in [300u32, 512] {
            assert!(
                exact_bad_reachable(&wide_safe(w)).is_err(),
                "the exact engine must abstain (over-cap) at W={w}"
            );
        }
    }
}
