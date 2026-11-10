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
//! Pure-bitvector designs are encoded under `Theory::BvOnly`. A design with a memory
//! (array-sorted state) is encoded under `Theory::BvUfArray` since N5 (2026-11-06): `read` →
//! `select`, `write` → `store`, a memory's next state an extensional array equality, its `init`
//! applied at frame 0 (constant fill or an array expression). The induction step quantifies
//! over the memory's contents like any other state, so a k-inductive SAFE stays sound.

#![deny(clippy::unwrap_used)] // docs/policies/failure-paths.md Rule 2: abstain, never panic
use std::collections::HashMap;

use crate::adapter::btor2::ast::{Btor2File, Nid, Node, Operand};
use crate::adapter::btor2::cex_value::CexValue;
use crate::adapter::sidecar::predicate_image::btor2_encode::{
    Btor2SmtView, EncodeError, encode_array_inits, encode_design_with_theory,
};
use crate::adapter::sidecar::predicate_image::theory::Theory;
use z3::ast::{Array, Ast, BV, Bool};

/// N5 — encode a design for BMC / k-induction. A design with an array-sorted (memory) state is
/// encoded under `Theory::BvUfArray` (QF_AUFBV: `read` → `select`, `write` → `store`, a memory's
/// next state an extensional array equality); every other design stays `Theory::BvOnly`, so the
/// pure bit-vector path is unchanged. Also returns each memory's `init` value; an `init` the
/// encoder cannot carry is a refusal, never a free frame-0 memory (that would make a
/// counterexample spurious).
fn encode_for_bmc(file: &Btor2File) -> Result<(Btor2SmtView, HashMap<Nid, Array>), EncodeError> {
    let has_memory = file.lines.iter().any(|l| match &l.node {
        Node::State { sort, .. } => {
            crate::adapter::btor2::parser::array_widths(file, *sort).is_some()
        }
        _ => false,
    });
    let theory = if has_memory {
        Theory::BvUfArray
    } else {
        Theory::BvOnly
    };
    let view = encode_design_with_theory(file, theory)?;
    let inits = encode_array_inits(file, &view)?;
    Ok((view, inits))
}

/// Default unrolling / induction depth bound for the portfolio-facing native
/// engine — mirrors [`crate::adapter::btormc::DEFAULT_KMAX`].
pub const DEFAULT_MAX_K: u32 = 40;

/// Default per-check budget of the portfolio-facing native engine, in **nominal milliseconds**
/// (mununu#893). It is not a wall clock: it is converted to a deterministic z3 resource limit
/// ([`UNITS_PER_NOMINAL_MS`] units per nominal millisecond), so the same query gets the same
/// answer on every host and under any load — a slower machine takes longer to reach the same
/// stop, it does not get a different verdict. Before #893 this was a 5 s z3 `timeout`, and a
/// query finishing in 4.9 s on one run and 5.1 s on the next flipped a row between ⊥ and `holds`
/// (HWMCC'20 `zipcpu_dcache-p588`). A check that exceeds the limit returns `Unknown` and the
/// engine **abstains with the cause** ([`BmcError::ResourceLimit`]) — never a wrong verdict.
pub const DEFAULT_QUERY_NOMINAL_MS: u32 = 5_000;

/// z3 resource units per nominal millisecond — the calibration behind [`QueryBudget`]. Measured
/// 2026-11-08 on the development laptop (Intel, z3 4.16) with the module's
/// `probe_rlimit_semantics_and_calibration`: the pigeonhole query spent 4.63 M units in a 2.0 s
/// wall stop and 0.31 M units in 77 ms, i.e. 2.3–3.9 M units per second; 3 000 units per
/// nominal millisecond makes the 5 s default about five seconds there and two to three times
/// that on the slower e2e host — deterministically. The constant is a unit conversion, never a
/// verdict input: two hosts with different speeds reach the same stop.
pub const UNITS_PER_NOMINAL_MS: u32 = 3_000;

/// Overrides the resource limit directly, in z3 units (`0` = no resource limit).
pub const QUERY_RLIMIT_ENV: &str = "MUNUNU_NATIVE_QUERY_RLIMIT";
/// Opt-in per-check WALL clock, milliseconds (`0` / unset = none beyond the run budget). Like
/// `MUNUNU_BDD_TIME_BUDGET_MS`, a clock makes the verdict host-dependent; it is off by default.
pub const QUERY_TIMEOUT_ENV: &str = "MUNUNU_NATIVE_QUERY_TIMEOUT_MS";

/// What one z3 check may spend: a deterministic resource limit and, optionally, a wall clock.
/// The run budget's remainder (`run_budget`, mununu#726) is applied as a wall cap on every check
/// whenever a budget is in effect, so the last check cannot overshoot the deadline — that cap is
/// the user's own wall budget, reported as [`BmcError::BudgetExpired`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryBudget {
    /// z3 `rlimit` per check; `None` = unbounded.
    pub rlimit: Option<u32>,
    /// A per-check wall clock; `None` = none beyond the run budget's remainder.
    pub timeout_ms: Option<u32>,
}

impl QueryBudget {
    /// The budget for a nominal per-check allowance: the resource limit is
    /// `nominal_ms × UNITS_PER_NOMINAL_MS` (saturating) unless [`QUERY_RLIMIT_ENV`] names the
    /// units itself; the wall clock is [`QUERY_TIMEOUT_ENV`] when set. `None` = no nominal
    /// allowance (unbounded, as [`bmc_bad_reachable`] runs).
    pub fn for_nominal(nominal_ms: Option<u32>) -> Self {
        let env_u32 = |name: &'static str| -> Option<u32> {
            crate::adapter::env_var::read(name).and_then(|v| v.trim().parse::<u32>().ok())
        };
        let rlimit = match env_u32(QUERY_RLIMIT_ENV) {
            Some(0) => None,
            Some(units) => Some(units),
            None => nominal_ms.map(|ms| ms.saturating_mul(UNITS_PER_NOMINAL_MS)),
        };
        let timeout_ms = env_u32(QUERY_TIMEOUT_ENV).filter(|ms| *ms > 0);
        QueryBudget { rlimit, timeout_ms }
    }

    /// The wall this check may take right now: the opt-in per-check clock, clamped to the run
    /// budget's remainder; with no clock, the remainder alone when a budget is in effect.
    fn wall_ms_now(&self) -> Option<u32> {
        let budgeted = crate::adapter::run_budget::current()
            .remaining_ms()
            .is_some();
        match (self.timeout_ms, budgeted) {
            (Some(ms), _) => Some(crate::adapter::run_budget::clamp_query_ms(ms)),
            (None, true) => Some(crate::adapter::run_budget::clamp_query_ms(u32::MAX)),
            (None, false) => None,
        }
    }

    /// Set this check's parameters on `solvers`. Called before every check, because the wall cap
    /// shrinks with the run budget.
    fn apply(&self, solvers: &[&z3::Solver]) {
        let mut params = z3::Params::new();
        let mut any = false;
        if let Some(units) = self.rlimit {
            params.set_u32("rlimit", units);
            any = true;
        }
        if let Some(ms) = self.wall_ms_now() {
            params.set_u32("timeout", ms);
            any = true;
        }
        if any {
            for s in solvers {
                s.set_params(&params);
            }
        }
    }
}

/// Why a check came back `Unknown`: the wall clock, the resource limit, or neither (an incomplete
/// theory, a genuine unknown). z3's reason string alone cannot tell the first two apart — a wall
/// stop reads `"timeout"` when the SAT core's timer fires but `"canceled"` when it lands during
/// preprocessing, and a resource stop reads `"canceled"` too — so the cause is read from what was
/// CONFIGURED and what the check CONSUMED: the resource units the check used (z3's `rlimit count`,
/// the delta across the check) reaching the limit is a resource stop; otherwise a configured wall
/// is the wall. An interrupt by this engine's own [`QueryGate`] also reads "canceled"; the gate
/// knows it interrupted, so the ladder asks it first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownCause {
    Timeout,
    ResourceLimit,
    Other,
}

/// `reason` is z3's `reason_unknown`, `units_used` the `rlimit count` delta across the check,
/// `wall_applied` whether a wall clock (the opt-in one or the run budget's remainder) was set on
/// the check.
pub(crate) fn classify_stop(
    reason: Option<&str>,
    units_used: u64,
    rlimit: Option<u32>,
    wall_applied: bool,
) -> UnknownCause {
    let reason = reason.map(str::to_ascii_lowercase);
    if reason.as_deref().is_some_and(|r| r.contains("timeout")) {
        return UnknownCause::Timeout;
    }
    // z3 stops once the count passes the scoped limit; the statistic can trail it by a little,
    // so 95 % of the limit already reads as the limit.
    if let Some(limit) = rlimit
        && units_used.saturating_mul(100) >= u64::from(limit).saturating_mul(95)
    {
        return UnknownCause::ResourceLimit;
    }
    let cancelled = reason
        .as_deref()
        .is_some_and(|r| r.contains("cancel") || r.contains("resource"));
    if cancelled && wall_applied {
        return UnknownCause::Timeout;
    }
    UnknownCause::Other
}

/// z3's cumulative resource counter for the solver's context (`rlimit count`); `0` before the
/// first check. The delta across one check is what that check consumed.
fn rlimit_count(solver: &z3::Solver) -> u64 {
    match solver.get_statistics().value("rlimit count") {
        Some(z3::StatisticsValue::UInt(n)) => u64::from(n),
        Some(z3::StatisticsValue::Double(d)) if d.is_finite() && d > 0.0 => d as u64,
        _ => 0,
    }
}

/// mununu#893 — what an `Unknown` check means for the k-induction ladder at depth `k`: a peer's
/// interrupt (the gate knows) or a genuine z3 unknown abstains at this depth; a budget stop is an
/// ERROR that names its cause — the wall ([`BmcError::BudgetExpired`]) or the deterministic
/// resource limit ([`BmcError::ResourceLimit`]). A `"canceled"` with no resource limit configured
/// is not a resource stop and abstains.
fn ladder_unknown(
    k: usize,
    gate: &QueryGate,
    reason: Option<String>,
    units_used: u64,
    budget: &QueryBudget,
) -> Result<SafetyVerdict, BmcError> {
    let k32 = k as u32;
    if gate.interrupted() {
        return Ok(SafetyVerdict::Unknown { k: k32 });
    }
    let wall_applied = budget.wall_ms_now().is_some();
    match (
        classify_stop(reason.as_deref(), units_used, budget.rlimit, wall_applied),
        budget.rlimit,
    ) {
        (UnknownCause::Timeout, _) => Err(BmcError::BudgetExpired { k: k32 }),
        (UnknownCause::ResourceLimit, Some(units)) => {
            Err(BmcError::ResourceLimit { k: k32, units })
        }
        (UnknownCause::ResourceLimit, None) | (UnknownCause::Other, _) => {
            Ok(SafetyVerdict::Unknown { k: k32 })
        }
    }
}

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
    /// sorted `(symbol, value)` list (only named state cells). A value is carried at its full
    /// width (mununu#723 — a cell wider than 64 bits used to be dropped from the frame).
    pub states: Vec<Vec<(String, CexValue)>>,
    /// Input valuations at each frame, 0 .. k: `inputs[j]` drives the transition out of frame
    /// `j`, and `inputs[k]` is what the `bad` condition read at the last frame.
    pub inputs: Vec<Vec<(String, CexValue)>>,
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
    /// unsupported op, or a memory `init` the encoder cannot carry).
    Encode(EncodeError),
    /// The design declares no `bad` property — nothing to check.
    NoBadProperty,
    /// mununu#726 / #728 — the thread-local wall-clock budget (`run_budget`) expired, or a query
    /// timed out under it, before depth `k` was decided. Distinct from a bounded `NoCexWithin`:
    /// a timed-out frame says NOTHING about whether `bad` is reachable there, so it must never be
    /// read as "no counterexample".
    BudgetExpired { k: u32 },
    /// mununu#893 — the deterministic per-check resource limit ([`QueryBudget::rlimit`], `units`
    /// z3 resource units) ran out at depth `k`. The same query stops at the same point on every
    /// host; a larger `--timeout-ms` (the nominal budget scales with it) or
    /// [`QUERY_RLIMIT_ENV`] gives it more. Like a wall stop, it says NOTHING about the frame.
    ResourceLimit { k: u32, units: u32 },
}

impl std::fmt::Display for BmcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BmcError::Encode(e) => write!(f, "native BMC: encode failed: {e:?}"),
            BmcError::NoBadProperty => write!(f, "native BMC: design has no `bad` property"),
            BmcError::BudgetExpired { k } => {
                write!(f, "native BMC: the wall-clock budget expired at depth {k}")
            }
            BmcError::ResourceLimit { k, units } => write!(
                f,
                "native BMC: the deterministic query budget ({units} z3 resource units, about \
                 {} ms nominal) ran out at depth {k}; a larger --timeout-ms scales it, \
                 {QUERY_RLIMIT_ENV} sets it",
                units / UNITS_PER_NOMINAL_MS.max(1)
            ),
        }
    }
}

/// The property / init / assumption operands extracted from a BTOR2 file:
/// `(bad operands, (state, init-value) pairs, constraint operands)`. Operands keep their
/// SIGN (PR A5 — `bad -N` is BTOR2's bit-not shorthand; reading it as `bad N` checked the
/// opposite property).
type Props = (Vec<Operand>, Vec<(Nid, Operand)>, Vec<Operand>);

fn extract_props(file: &Btor2File) -> Props {
    let mut bad_ops = Vec::new();
    let mut init_pairs = Vec::new();
    let mut constraint_ops = Vec::new();
    for l in &file.lines {
        match &l.node {
            Node::Bad { signal } => bad_ops.push(*signal),
            Node::Init { state, value, .. } => init_pairs.push((*state, *value)),
            Node::Constraint { signal } => constraint_ops.push(*signal),
            _ => {}
        }
    }
    (bad_ops, init_pairs, constraint_ops)
}

/// A design signal's current-cycle BV by OPERAND: resolved through `signal_bvs` (Op/Const
/// nodes), then `state_curr`, then `inputs` — so a property, constraint or `init` value that
/// names a state cell or an input directly is not lost (mununu#741) — with BTOR2's `-N`
/// bit-not applied when the operand is negated (PR A5).
pub(crate) fn operand_bv(view: &Btor2SmtView, op: &Operand) -> Option<BV> {
    let nid = op.nid();
    let bv = view
        .signal_bvs
        .get(&nid)
        .or_else(|| view.state_curr.get(&nid))
        .or_else(|| view.inputs.get(&nid))?;
    Some(if op.is_negated() {
        bv.bvnot()
    } else {
        bv.clone()
    })
}

/// Build `Nid → symbol` maps for the NAMED state cells and inputs — the cells the
/// witness trace ([`BmcTrace`]) reports. An anonymous state/input (no `symbol`) is
/// not surfaced in the trace.
fn symbol_maps(file: &Btor2File) -> (HashMap<Nid, String>, HashMap<Nid, String>) {
    // A flattened design's registers mostly carry no symbol of their own: yosys names them through
    // a width-matched `uext … 0 <name>` alias (`u_rx.state_q`), which `collect_symbols` forwards to
    // the state (mununu#710 — a trace over unnamed registers shows only the shadows and inputs).
    let names = crate::adapter::btor2::parser::collect_symbols(file);
    let mut state_syms = HashMap::new();
    let mut input_syms = HashMap::new();
    for l in &file.lines {
        match &l.node {
            Node::State { .. } => {
                if let Some(s) = names.get(&l.nid) {
                    state_syms.insert(l.nid, s.clone());
                }
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
    bad_ops: &'a [Operand],
    constraint_ops: &'a [Operand],
    /// `frame_state[j][nid]` — state cell `nid`'s value at frame `j`.
    frame_state: Vec<HashMap<Nid, BV>>,
    /// `frame_input[j][nid]` — input `nid`'s value at frame `j`.
    frame_input: Vec<HashMap<Nid, BV>>,
    /// N5 — `frame_arr[j][nid]` — memory `nid`'s contents at frame `j` (empty for a pure
    /// bit-vector design).
    frame_arr: Vec<HashMap<Nid, Array>>,
    /// N5 — each memory's `init` value over the current-step variables ([`encode_array_inits`]).
    array_inits: HashMap<Nid, Array>,
    /// The 1-bit constant `1`, reused for `bad`/`constraint` truth (`op == 1`).
    one1: BV,
}

impl<'a> Unroller<'a> {
    /// Build fresh frame variables for depths `0..=n`.
    fn build(
        view: &'a Btor2SmtView,
        array_inits: HashMap<Nid, Array>,
        bad_ops: &'a [Operand],
        constraint_ops: &'a [Operand],
        n: usize,
    ) -> Self {
        let fresh = |src: &HashMap<Nid, BV>, prefix: &str| -> HashMap<Nid, BV> {
            src.iter()
                .map(|(nid, bv)| (*nid, BV::fresh_const(prefix, bv.get_size())))
                .collect()
        };
        // A memory's frame variable takes the declared array's own sorts; the encoder built it
        // from the BTOR2 `sort array`, so both are present.
        let fresh_arr = || -> HashMap<Nid, Array> {
            view.state_curr_arr
                .iter()
                .filter_map(|(nid, arr)| {
                    let sort = arr.get_sort();
                    let (dom, rng) = (sort.array_domain()?, sort.array_range()?);
                    Some((*nid, Array::fresh_const("bmc_m", &dom, &rng)))
                })
                .collect()
        };
        Unroller {
            view,
            bad_ops,
            constraint_ops,
            frame_state: (0..=n).map(|_| fresh(&view.state_curr, "bmc_s")).collect(),
            frame_input: (0..=n).map(|_| fresh(&view.inputs, "bmc_i")).collect(),
            frame_arr: (0..=n).map(|_| fresh_arr()).collect(),
            array_inits,
            one1: BV::from_u64(1, 1),
        }
    }

    /// Instantiate a current-cycle term at frame `j` (state cells, inputs and memories), and,
    /// with `next`, the next-cycle cells at frame `j + 1`. The frame variables are fresh, so the
    /// bit-vector and the array substitutions (one sort per `substitute` call) compose into the
    /// simultaneous one.
    fn at<T: Ast>(&self, term: &T, j: usize, next: bool) -> T {
        let mut bv = self.curr_subs(j);
        let mut arr: Vec<(&Array, &Array)> = self
            .view
            .state_curr_arr
            .iter()
            .filter_map(|(nid, a)| Some((a, self.frame_arr[j].get(nid)?)))
            .collect();
        if next {
            for (nid, b) in &self.view.state_next {
                bv.push((b, &self.frame_state[j + 1][nid]));
            }
            arr.extend(
                self.view
                    .state_next_arr
                    .iter()
                    .filter_map(|(nid, a)| Some((a, self.frame_arr[j + 1].get(nid)?))),
            );
        }
        let t = term.substitute(&bv);
        if arr.is_empty() {
            t
        } else {
            t.substitute(&arr)
        }
    }

    /// Current-cycle BV of a `bad` / `constraint` / `init`-value operand — see [`operand_bv`].
    fn curr_bv(&self, op: &Operand) -> Option<BV> {
        operand_bv(self.view, op)
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
        self.at(&self.view.transition, j, true)
    }

    /// `bad` at frame `j`: OR over all `bad` operands of `(operand == 1)`.
    fn bad_at(&self, j: usize) -> Bool {
        let disj: Vec<Bool> = self
            .bad_ops
            .iter()
            .filter_map(|op| {
                self.curr_bv(op)
                    .map(|bv| self.at(&bv, j, false).eq(&self.one1))
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
        self.constraint_ops
            .iter()
            .filter_map(|op| {
                self.curr_bv(op)
                    .map(|bv| self.at(&bv, j, false).eq(&self.one1))
            })
            .collect()
    }

    /// Assert the design's `init` values on frame 0 into `solver` (init-less state
    /// cells stay free — BTOR2's nondeterministic-init semantics). The value is read AT
    /// FRAME 0 (mununu#741): an `init` whose value is an input, another state, or an
    /// expression over them (BTOR2 permits it — a shared nondeterministic initial value) pins
    /// the cell to the frame-0 variable; on the base view it was either dropped (input /
    /// state, not in `signal_bvs`) or tied to a constant unrelated to the frame — both leave
    /// the cell free, an over-approximation of the initial states and a spurious
    /// counterexample.
    fn assert_init(&self, solver: &z3::Solver, init_pairs: &[(Nid, Operand)]) {
        for (state, value) in init_pairs {
            if let (Some(s0), Some(vbv)) = (self.frame_state[0].get(state), self.curr_bv(value)) {
                solver.assert(s0.eq(self.at(&vbv, 0, false)));
            }
        }
        // N5 — a memory with an `init` starts at that value; one without stays free (BTOR2's
        // nondeterministic init). An `init` the encoder could not carry never reaches here: it
        // refused the design.
        for (state, init) in &self.array_inits {
            if let Some(m0) = self.frame_arr[0].get(state) {
                solver.assert(m0.eq(self.at(init, 0, false)));
            }
        }
    }

    /// Extract the concrete `Init → bad` witness from a SAT `model` whose `bad`
    /// fired at frame `k`: per-frame state and input valuations (frames `0..=k`) for the
    /// NAMED cells, each a sorted `(symbol, value)` list. The input at frame `k` is part of
    /// the witness (mununu#710): a combinational `bad` (any `|->` monitor) reads it.
    /// `model_completion` is on, so a free (init-less / unconstrained-input) cell
    /// gets a concrete value — the trace is a full assignment of a real model run.
    fn extract_trace(
        &self,
        model: &z3::Model,
        k: usize,
        state_syms: &HashMap<Nid, String>,
        input_syms: &HashMap<Nid, String>,
    ) -> BmcTrace {
        let eval_frame = |frame: &HashMap<Nid, BV>,
                          syms: &HashMap<Nid, String>|
         -> Vec<(String, CexValue)> {
            let mut out: Vec<(String, CexValue)> = frame
                    .iter()
                    .filter_map(|(nid, bv)| {
                        let sym = syms.get(nid)?;
                        let val = model.eval(bv, true)?;
                        let bits = bv.get_size();
                        // mununu#723 — `as_u64` is only the fast path: a cell wider than 64
                        // bits (OpenTitan's 109-bit `tl_i`) is read from the numeral z3 prints
                        // instead of being dropped from the frame.
                        let value = match val.as_u64() {
                            Some(v) => CexValue::from_u64(v, bits),
                            None => match CexValue::from_smt_numeral(&val.to_string(), bits) {
                                Some(v) => v,
                                None => {
                                    tracing::warn!(cell = %sym, bits, "witness: unreadable model value");
                                    return None;
                                }
                            },
                        };
                        Some((sym.clone(), value))
                    })
                    .collect();
            out.sort();
            out
        };
        let states = (0..=k)
            .map(|j| eval_frame(&self.frame_state[j], state_syms))
            .collect();
        let inputs = (0..=k)
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
        let (view, array_inits) = encode_for_bmc(file).map_err(BmcError::Encode)?;
        let u = Unroller::build(&view, array_inits, &bad_ops, &constraint_ops, n);

        // A single init-constrained path: check `bad` at each frame, extend if clean.
        let solver = z3::Solver::new();
        u.assert_init(&solver, &init_pairs);
        for c in u.constraints_at(0) {
            solver.assert(&c);
        }
        // mununu#726 — under a thread-local budget every frame's query is bounded by the time
        // left (unbounded stays unbounded: the floor is today's behaviour), and the loop polls
        // the budget per depth. A query that times out is an ERROR, not "no cex at this depth".
        // mununu#893 — no nominal allowance here (this entry is the unbounded BMC); the
        // environment may still name a resource limit, and a stop on it is named as such.
        let budget = QueryBudget::for_nominal(None);
        for k in 0..=n {
            if crate::adapter::run_budget::expired() {
                return Err(BmcError::BudgetExpired { k: k as u32 });
            }
            budget.apply(&[&solver]);
            solver.push();
            solver.assert(u.bad_at(k));
            let before = rlimit_count(&solver);
            let res = solver.check();
            match res {
                z3::SatResult::Sat => {
                    // Extract the concrete witness from the model BEFORE popping it.
                    let trace = solver
                        .get_model()
                        .map(|m| u.extract_trace(&m, k, &state_syms, &input_syms));
                    solver.pop(1);
                    return Ok((BmcOutcome::Violated { depth: k as u32 }, trace));
                }
                z3::SatResult::Unsat => {}
                z3::SatResult::Unknown => {
                    let cause = classify_stop(
                        solver.get_reason_unknown().as_deref(),
                        rlimit_count(&solver).saturating_sub(before),
                        budget.rlimit,
                        budget.wall_ms_now().is_some(),
                    );
                    solver.pop(1);
                    return Err(match (cause, budget.rlimit) {
                        (UnknownCause::ResourceLimit, Some(units)) => {
                            BmcError::ResourceLimit { k: k as u32, units }
                        }
                        _ => BmcError::BudgetExpired { k: k as u32 },
                    });
                }
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
/// `nominal_ms` is the per-check allowance in nominal milliseconds (mununu#893): it becomes a
/// deterministic z3 resource limit ([`QueryBudget::for_nominal`]), not a wall clock, so a
/// check that exceeds it stops at the same point on every host and the engine **abstains with
/// the cause** (`Err(`[`BmcError::ResourceLimit`]`)`) — never a wrong verdict, never a silent
/// `Unknown`. `None` = unbounded (QF_BV is decidable, so it still terminates). A wall stop
/// (the run budget's remainder, or the opt-in [`QUERY_TIMEOUT_ENV`]) is
/// `Err(`[`BmcError::BudgetExpired`]`)`; [`SafetyVerdict::Unknown`] is reserved for the depth
/// bound, a cancellation by a peer, and a genuine z3 unknown.
///
/// **Soundness of the SAT/UNSAT/UNKNOWN handling.** `Violated` is returned ONLY on
/// a definite base `Sat`; `Safe` ONLY on a definite step `Unsat`. Any `Unknown`
/// (a budget stop, or Z3 giving up) abstains — crucially, a step `Unknown` must NOT be
/// read as "not satisfiable ⇒ inductive" (that would be a spurious `Safe`).
pub fn decide_bad_safety(
    file: &Btor2File,
    max_k: u32,
    nominal_ms: Option<u32>,
) -> Result<SafetyVerdict, BmcError> {
    decide_bad_safety_cancellable(file, max_k, nominal_ms, None)
}

/// [`decide_bad_safety`] with the reach portfolio's cooperative **cancel** flag (S1 of the
/// engine-performance roadmap): checked before each depth, like [`bmc_cex_until`], so once a
/// peer member has decided the engine abstains at the first depth boundary past the
/// [`crate::adapter::MEMBER_CANCEL_GRACE`] instead of running the remaining
/// `(max_k - k) × 2` queries at up to `nominal_ms` each. The grace keeps a run that is about
/// to finish in the portfolio's attribution; a cancelled run is [`SafetyVerdict::Unknown`]
/// at the depth it stopped — never a wrong verdict. The latency of a cancellation is bounded
/// by the grace plus the query in flight (the [`QueryGate`] interrupts z3 itself).
pub fn decide_bad_safety_cancellable(
    file: &Btor2File,
    max_k: u32,
    nominal_ms: Option<u32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<SafetyVerdict, BmcError> {
    let (bad_ops, init_pairs, constraint_ops) = extract_props(file);
    if bad_ops.is_empty() {
        return Err(BmcError::NoBadProperty);
    }
    let n = max_k as usize;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let (view, array_inits) = encode_for_bmc(file).map_err(BmcError::Encode)?;
        let u = Unroller::build(&view, array_inits, &bad_ops, &constraint_ops, n);

        // N0.5 (2026-11-03) — a peer's decision is honoured INSIDE a query, not only at the
        // next depth boundary. Until now `cancel` was read between depths, so a k-induction
        // query already running kept its row waiting for the full per-query timeout: in the
        // 2026-10-28 attribution run native k-induction ran 40–100 s past rIC3's decision
        // (edn ep0: rIC3 45 s, native 148 s, row 148 s) — a row's wall is its slowest member.
        // A scoped watchdog thread polls the flag and interrupts the z3 context the two
        // solvers run on, but ONLY while a `check()` is in flight: an interrupt on an idle
        // context would poison the next query of whoever reuses this thread's context. The
        // thread is joined before the closure returns (`done`), so nothing outlives the
        // solvers it may interrupt.
        //
        // mununu#878 — an interrupted check does NOT reliably return `Unknown`: under load a
        // cut query came back with a definite answer, and native reported a `reachable` the
        // exact oracle refutes (10 of 240 runs; 0 of 240 with the interrupt removed). So the
        // ladder never reads a result once an interrupt has been issued ([`QueryGate`]), and
        // the interrupt is issued only inside the gate's critical section, so it cannot land
        // between one check ending and the next beginning.
        let ctx = z3::Context::thread_local();
        let handle = ctx.handle();
        let gate = QueryGate::default();
        with_cancel_watchdog(
            cancel,
            &gate,
            || handle.interrupt(),
            || decide_bad_safety_ladder(&u, &init_pairs, n, max_k, nominal_ms, cancel, &gate),
        )
    })
}

/// N0.5 / mununu#878 — run `body` with a scoped watchdog that, once `cancel` has been set for the
/// member grace, calls `interrupt` while the gate reports a check in flight. The watchdog stops
/// when `body` ends, however it ends.
///
/// mununu#951 — "however it ends" is the point. The stop flag used to be set after `body`
/// returned normally; a panic in `body` left the watchdog polling forever, `thread::scope` waited
/// on it, and the row hung instead of abstaining. The flag is now set by a guard that drops on
/// unwind too, inside the scope, before the scope joins its threads.
fn with_cancel_watchdog<T>(
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gate: &QueryGate,
    interrupt: impl Fn() + Sync,
    body: impl FnOnce() -> T,
) -> T {
    struct StopOnDrop<'a>(&'a std::sync::atomic::AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        if let Some(c) = cancel {
            scope.spawn(|| {
                use std::sync::atomic::Ordering::Relaxed;
                // The member grace applies here as at the depth boundary: a query that is
                // about to finish still lands; one still running past the grace is cut.
                let mut seen: Option<std::time::Instant> = None;
                while !done.load(Relaxed) {
                    if c.load(Relaxed)
                        && seen.get_or_insert_with(std::time::Instant::now).elapsed()
                            >= crate::adapter::MEMBER_CANCEL_GRACE
                    {
                        gate.interrupt_if_in_query(&interrupt);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
        }
        let _stop = StopOnDrop(&done);
        body()
    })
}

/// The handshake between the depth ladder and its cancel watchdog (mununu#878).
///
/// One lock covers "a `check()` is in flight" and "an interrupt was issued", so the watchdog
/// interrupts only while a check is in flight, and a check that may have been touched by an
/// interrupt is never read. The `interrupted` flag is sticky: an interrupt that lands while the
/// next check is being set up still discards that check, since z3's cancel can outlive it.
#[derive(Default)]
struct QueryGate {
    state: std::sync::Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    in_query: bool,
    interrupted: bool,
}

impl QueryGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        // A poisoned lock only means a panicking holder; the two flags are still meaningful.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether this gate has issued an interrupt — the one `"canceled"` z3 reason that is NOT a
    /// resource-limit stop (mununu#893 asks the gate before reading z3's reason).
    fn interrupted(&self) -> bool {
        self.lock().interrupted
    }

    /// Run one solver check inside the gate. `Unknown` once an interrupt has been issued, before
    /// or during the check: an interrupted check's answer is not trusted, whatever it says.
    fn check(&self, run: impl FnOnce() -> z3::SatResult) -> z3::SatResult {
        {
            let mut s = self.lock();
            if s.interrupted {
                return z3::SatResult::Unknown;
            }
            s.in_query = true;
        }
        let res = run();
        let mut s = self.lock();
        s.in_query = false;
        if s.interrupted {
            z3::SatResult::Unknown
        } else {
            res
        }
    }

    /// The watchdog's side: interrupt only while a check is in flight, and record it first.
    fn interrupt_if_in_query(&self, interrupt: impl FnOnce()) {
        let mut s = self.lock();
        if s.in_query {
            s.interrupted = true;
            interrupt();
        }
    }
}

/// The BMC + k-induction depth ladder of [`decide_bad_safety_cancellable`], on the thread's
/// current z3 context; every `check()` runs inside `gate` so the caller's watchdog
/// can interrupt exactly those.
fn decide_bad_safety_ladder(
    u: &Unroller,
    init_pairs: &[(Nid, Operand)],
    n: usize,
    max_k: u32,
    nominal_ms: Option<u32>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gate: &QueryGate,
) -> Result<SafetyVerdict, BmcError> {
    {
        // Base: an init-constrained path (as in `bmc_bad_reachable`).
        let base = z3::Solver::new();
        // Step: a FREE path (no init) accumulating `¬bad ∧ transition` per frame;
        // if `bad` at the current frame is then unsatisfiable, the property is
        // k-inductive.
        let step = z3::Solver::new();
        // mununu#893 — the per-check budget is a deterministic resource limit (plus the run
        // budget's wall remainder); applied before every check below, never only once.
        let budget = QueryBudget::for_nominal(nominal_ms);
        u.assert_init(&base, init_pairs);
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
            // mununu#726 — the thread-local budget stops the ladder at a depth boundary (no
            // grace: an expired budget is not "about to finish"), and the per-query timeout is
            // re-clamped every depth so the LAST query cannot overshoot the deadline. With no
            // budget in effect this is exactly the fixed `timeout_ms` of before.
            if crate::adapter::run_budget::expired() {
                return Ok(SafetyVerdict::Unknown { k: k as u32 });
            }
            budget.apply(&[&base, &step]);
            // Base — reachable counterexample? Violated ONLY on a definite Sat.
            base.push();
            base.assert(u.bad_at(k));
            let base_before = rlimit_count(&base);
            let base_res = gate.check(|| base.check());
            // z3's reason and count belong to the check just made: read them before the pop.
            let base_reason = (base_res == z3::SatResult::Unknown)
                .then(|| base.get_reason_unknown())
                .flatten();
            let base_used = rlimit_count(&base).saturating_sub(base_before);
            base.pop(1);
            match base_res {
                z3::SatResult::Sat => return Ok(SafetyVerdict::Violated { depth: k as u32 }),
                z3::SatResult::Unknown => {
                    return ladder_unknown(k, gate, base_reason, base_used, &budget);
                }
                z3::SatResult::Unsat => {}
            }
            // Step — k-inductive? Safe ONLY on a definite Unsat; an Unknown abstains
            // (must NOT be read as "inductive"). Base has held through depth k above.
            step.push();
            step.assert(u.bad_at(k));
            let step_before = rlimit_count(&step);
            let step_res = gate.check(|| step.check());
            let step_reason = (step_res == z3::SatResult::Unknown)
                .then(|| step.get_reason_unknown())
                .flatten();
            let step_used = rlimit_count(&step).saturating_sub(step_before);
            step.pop(1);
            match step_res {
                z3::SatResult::Unsat => return Ok(SafetyVerdict::Safe { k: k as u32 }),
                z3::SatResult::Unknown => {
                    return ladder_unknown(k, gate, step_reason, step_used, &budget);
                }
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
    }
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
/// may therefore be set well above [`DEFAULT_MAX_K`]. `Err` names a refused design (the encoder
/// declined); `Ok(None)` is an honest miss (the bound, the deadline, a cancellation, a query timeout).
/// Bounded by the wall `deadline`
/// and `cancel` (both checked before each depth) and by `per_query_ms` per z3 check.
pub fn bmc_cex_until(
    file: &Btor2File,
    max_k: u32,
    per_query_ms: u32,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Option<u32>, String> {
    let (bad_ops, init_pairs, constraint_ops) = extract_props(file);
    if bad_ops.is_empty() {
        return Ok(None);
    }
    let n = max_k as usize;
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        // mununu#803 — an encoder refusal is a reason the caller records, not a silent miss.
        let (view, array_inits) =
            encode_for_bmc(file).map_err(|e| format!("native BMC: encode failed: {e:?}"))?;
        let u = Unroller::build(&view, array_inits, &bad_ops, &constraint_ops, n);
        let base = z3::Solver::new();
        // mununu#893 — a deterministic resource limit per check (the nominal allowance), plus
        // the run budget's wall remainder; the `deadline` below is this search's own wall.
        let budget = QueryBudget::for_nominal(Some(per_query_ms));
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
                return Ok(None);
            }
            budget.apply(&[&base]);
            base.push();
            base.assert(u.bad_at(k));
            let res = base.check();
            base.pop(1);
            match res {
                // A concrete init→bad path at depth `k` — sound `Violated`.
                z3::SatResult::Sat => return Ok(Some(k as u32)),
                // This depth's query hit `per_query_ms`; deeper frames only get larger,
                // so abstain rather than spin.
                z3::SatResult::Unknown => return Ok(None),
                z3::SatResult::Unsat => {}
            }
            if k < n {
                base.assert(u.transition_at(k));
                for c in u.constraints_at(k + 1) {
                    base.assert(&c);
                }
            }
        }
        Ok(None)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::adapter::btor2::parser;

    /// A 4×4-bit memory written from the inputs (`we`, `addr`, `data`) and read at address 3;
    /// `bad` = that cell holds 15. Zero-initialised, so reachable at depth 1 (write at frame 0,
    /// visible at frame 1).
    const MEM_WRITE_READ: &str = "1 sort bitvec 1\n2 sort bitvec 2\n3 sort bitvec 4\n\
        4 sort array 2 3\n5 state 4 mem\n6 zero 3\n7 init 4 5 6\n8 input 1 we\n\
        9 input 2 addr\n10 input 3 data\n11 write 4 5 9 10\n12 ite 4 8 11 5\n\
        13 next 4 5 12\n14 const 2 11\n15 read 3 5 14\n16 ones 3\n17 eq 1 15 16\n18 bad 17\n";

    /// A 4×4-bit memory that is never written; `bad` = cell 0 is non-zero. With `init` = 0 it is
    /// SAFE (1-inductive); without an `init` the memory is free at frame 0 and `bad` is reachable
    /// at depth 0.
    fn mem_constant(with_init: bool) -> String {
        format!(
            "1 sort bitvec 1\n2 sort bitvec 2\n3 sort bitvec 4\n4 sort array 2 3\n\
             5 state 4 mem\n6 zero 3\n{}8 zero 2\n9 read 3 5 8\n10 neq 1 9 6\n\
             11 bad 10\n12 next 4 5 5\n",
            if with_init { "7 init 4 5 6\n" } else { "" }
        )
    }

    /// N5 — a memory design used to be refused (`BmcError::Encode`); native BMC now decides it
    /// under QF_AUFBV and finds the shallowest counterexample.
    #[test]
    fn bmc_decides_a_write_then_read_on_a_memory() {
        let file = parser::parse(MEM_WRITE_READ).expect("parse");
        assert_eq!(
            bmc_bad_reachable(&file, 4).expect("encodes under BvUfArray"),
            BmcOutcome::Violated { depth: 1 }
        );
        assert!(matches!(
            decide_bad_safety(&file, 4, Some(5_000)).expect("decides"),
            SafetyVerdict::Violated { .. }
        ));
    }

    /// N5 — the memory's `init` is applied at frame 0. Were it dropped, the memory would start
    /// free and `bad` would fire at depth 0: a spurious counterexample on a safe design.
    #[test]
    fn a_memory_init_is_applied_so_a_safe_memory_is_proved_safe() {
        let file = parser::parse(&mem_constant(true)).expect("parse");
        assert_eq!(
            bmc_bad_reachable(&file, 4).expect("encodes"),
            BmcOutcome::NoCexWithin { k: 4 }
        );
        assert!(matches!(
            decide_bad_safety(&file, 4, Some(5_000)).expect("decides"),
            SafetyVerdict::Safe { .. }
        ));
    }

    /// The negative control: without an `init` the memory is nondeterministic at frame 0
    /// (BTOR2 semantics), so the same `bad` is reachable at depth 0.
    #[test]
    fn a_memory_without_init_starts_free() {
        let file = parser::parse(&mem_constant(false)).expect("parse");
        assert_eq!(
            bmc_bad_reachable(&file, 4).expect("encodes"),
            BmcOutcome::Violated { depth: 0 }
        );
    }

    /// mununu#878 — a check the watchdog interrupted is discarded, whatever z3 answered.
    #[test]
    fn an_interrupted_check_is_read_as_unknown_even_when_it_answers() {
        let gate = QueryGate::default();
        let mut fired = false;
        let res = gate.check(|| {
            gate.interrupt_if_in_query(|| fired = true);
            z3::SatResult::Sat
        });
        assert!(fired, "the watchdog interrupts a check in flight");
        assert_eq!(res, z3::SatResult::Unknown);
    }

    /// mununu#878 — once interrupted, a later check is not run: z3's cancel may outlive the query.
    #[test]
    fn after_an_interrupt_no_further_check_is_trusted() {
        let gate = QueryGate::default();
        let _ = gate.check(|| {
            gate.interrupt_if_in_query(|| ());
            z3::SatResult::Unsat
        });
        let mut ran = false;
        let res = gate.check(|| {
            ran = true;
            z3::SatResult::Unsat
        });
        assert!(!ran, "a check after an interrupt must not run");
        assert_eq!(res, z3::SatResult::Unknown);
    }

    /// mununu#878 — between checks the watchdog does nothing, so an idle context is never cut and
    /// the answers of untouched checks stand.
    #[test]
    fn the_watchdog_never_interrupts_between_checks() {
        let gate = QueryGate::default();
        assert_eq!(gate.check(|| z3::SatResult::Sat), z3::SatResult::Sat);
        let mut fired = false;
        gate.interrupt_if_in_query(|| fired = true);
        assert!(!fired, "no check is in flight");
        assert_eq!(gate.check(|| z3::SatResult::Unsat), z3::SatResult::Unsat);
    }

    /// The pigeonhole principle as a BTOR2 `bad` over 1-bit inputs: `pigeons` pigeons, `holes`
    /// holes, every pigeon somewhere, no hole holding two. Unsatisfiable, trivial to bit-blast,
    /// exponential for resolution — z3's SAT core searches for tens of seconds at 11/10.
    fn pigeonhole_btor2(pigeons: usize, holes: usize) -> String {
        let mut src = String::from("1 sort bitvec 1\n");
        let mut nid = 2i64;
        let mut x = vec![vec![0i64; holes]; pigeons];
        for (p, row) in x.iter_mut().enumerate() {
            for (h, cell) in row.iter_mut().enumerate() {
                src.push_str(&format!("{nid} input 1 x_{p}_{h}\n"));
                *cell = nid;
                nid += 1;
            }
        }
        let mut conj: Vec<i64> = Vec::new();
        for row in &x {
            let mut acc = row[0];
            for &cell in &row[1..] {
                src.push_str(&format!("{nid} or 1 {acc} {cell}\n"));
                acc = nid;
                nid += 1;
            }
            conj.push(acc);
        }
        for h in 0..holes {
            let col: Vec<i64> = x.iter().map(|row| row[h]).collect();
            for p in 0..pigeons {
                for q in (p + 1)..pigeons {
                    src.push_str(&format!("{nid} and 1 {} {}\n", col[p], col[q]));
                    let both = nid;
                    nid += 1;
                    src.push_str(&format!("{nid} not 1 {both}\n"));
                    conj.push(nid);
                    nid += 1;
                }
            }
        }
        let mut acc = conj[0];
        for &c in &conj[1..] {
            src.push_str(&format!("{nid} and 1 {acc} {c}\n"));
            acc = nid;
            nid += 1;
        }
        src.push_str(&format!("{nid} bad {acc}\n"));
        src
    }

    /// mununu#893 — a budget stop is an ERROR that names its cause, never a silent `Unknown`;
    /// the resource-limit stop counts work, not time, so two runs stop at the same point with
    /// the same words. The opt-in wall clock is a different stop with a different name.
    #[test]
    fn a_resource_limit_stop_names_itself_and_is_the_same_twice() {
        let file = parser::parse(&pigeonhole_btor2(11, 10)).expect("parses");
        let _rl = crate::adapter::env_var::unset_for_test(QUERY_RLIMIT_ENV);
        let _to = crate::adapter::env_var::unset_for_test(QUERY_TIMEOUT_ENV);
        // 10 nominal ms = 30 000 units: far below the pigeonhole's cost on any host.
        let (a, b) = (
            decide_bad_safety(&file, 0, Some(10)),
            decide_bad_safety(&file, 0, Some(10)),
        );
        for r in [&a, &b] {
            match r {
                Err(BmcError::ResourceLimit { k: 0, units }) => {
                    assert_eq!(*units, 10 * UNITS_PER_NOMINAL_MS);
                }
                other => panic!("a resource-limit stop must be named: {other:?}"),
            }
        }
        let (a, b) = (a.unwrap_err().to_string(), b.unwrap_err().to_string());
        assert_eq!(a, b, "the same stop twice");
        assert!(
            a.contains("deterministic") && a.contains("30000") && a.contains("--timeout-ms"),
            "the message names the budget and the lever: {a}"
        );
    }

    /// The opt-in wall clock, with the resource limit switched off, is a wall stop and says so.
    #[test]
    fn an_opt_in_wall_stop_is_named_as_the_wall() {
        let file = parser::parse(&pigeonhole_btor2(11, 10)).expect("parses");
        let _rl = crate::adapter::env_var::override_for_test(QUERY_RLIMIT_ENV, "0");
        let _to = crate::adapter::env_var::override_for_test(QUERY_TIMEOUT_ENV, "50");
        match decide_bad_safety(&file, 0, Some(60_000)) {
            Err(BmcError::BudgetExpired { k: 0 }) => {}
            other => panic!("a wall stop must be named as the wall: {other:?}"),
        }
    }

    /// The resource limit is per CHECK, not cumulative over the ladder: an 8-bit counter whose
    /// `bad` (255) is beyond the bound and never k-inductive runs all 41 depths (82 checks) of
    /// trivial queries under a limit a single check fits in and reaches the depth bound. A
    /// cumulative limit would stop it as a resource-limit error long before.
    #[test]
    fn the_resource_limit_is_per_check_not_cumulative_over_the_ladder() {
        const CNT8: &str = "1 sort bitvec 8\n2 zero 1\n3 state 1 cnt\n4 init 1 3 2\n5 one 1\n\
                            6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                            10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(CNT8).expect("parses");
        let _rl = crate::adapter::env_var::override_for_test(QUERY_RLIMIT_ENV, "100000");
        let _to = crate::adapter::env_var::unset_for_test(QUERY_TIMEOUT_ENV);
        let v =
            decide_bad_safety(&file, 40, Some(1)).expect("no budget stop under a per-check limit");
        assert_eq!(
            v,
            SafetyVerdict::Unknown { k: 40 },
            "the depth bound, not a stop"
        );
    }

    #[test]
    fn z3_reasons_classify_a_wall_stop_a_resource_stop_and_a_genuine_unknown() {
        // "timeout" is the wall whatever else is configured.
        assert_eq!(
            classify_stop(Some("timeout"), 0, Some(1_000), true),
            UnknownCause::Timeout
        );
        // "canceled" with the units at the limit is the resource stop, wall or no wall …
        assert_eq!(
            classify_stop(Some("canceled"), 1_000, Some(1_000), true),
            UnknownCause::ResourceLimit
        );
        assert_eq!(
            classify_stop(Some("canceled"), 960, Some(1_000), false),
            UnknownCause::ResourceLimit,
            "the statistic may trail the limit a little"
        );
        // … and "canceled" well under the limit with a wall configured is the wall (z3 says
        // "canceled", not "timeout", when the clock lands during preprocessing).
        assert_eq!(
            classify_stop(Some("canceled"), 10, Some(1_000), true),
            UnknownCause::Timeout
        );
        assert_eq!(
            classify_stop(Some("canceled"), 10, None, true),
            UnknownCause::Timeout
        );
        // "canceled" with neither budget configured is somebody else's interrupt: unknown.
        assert_eq!(
            classify_stop(Some("canceled"), 10, None, false),
            UnknownCause::Other
        );
        assert_eq!(
            classify_stop(
                Some("(incomplete (theory arithmetic))"),
                10,
                Some(1_000),
                true
            ),
            UnknownCause::Other
        );
        assert_eq!(classify_stop(None, 0, None, false), UnknownCause::Other);
    }

    /// The nominal allowance converts to units; the environment may name the units itself or
    /// switch the limit off; the wall clock is opt-in.
    #[test]
    fn the_nominal_budget_converts_to_units_unless_the_environment_names_them() {
        {
            let _rl = crate::adapter::env_var::unset_for_test(QUERY_RLIMIT_ENV);
            let _to = crate::adapter::env_var::unset_for_test(QUERY_TIMEOUT_ENV);
            assert_eq!(
                QueryBudget::for_nominal(Some(5_000)),
                QueryBudget {
                    rlimit: Some(15_000_000),
                    timeout_ms: None,
                }
            );
            assert_eq!(QueryBudget::for_nominal(None).rlimit, None);
            assert_eq!(
                QueryBudget::for_nominal(Some(u32::MAX)).rlimit,
                Some(u32::MAX),
                "saturates"
            );
        }
        {
            let _rl = crate::adapter::env_var::override_for_test(QUERY_RLIMIT_ENV, "4242");
            let _to = crate::adapter::env_var::override_for_test(QUERY_TIMEOUT_ENV, "250");
            assert_eq!(
                QueryBudget::for_nominal(Some(5_000)),
                QueryBudget {
                    rlimit: Some(4242),
                    timeout_ms: Some(250),
                }
            );
        }
        {
            let _rl = crate::adapter::env_var::override_for_test(QUERY_RLIMIT_ENV, "0");
            let _to = crate::adapter::env_var::override_for_test(QUERY_TIMEOUT_ENV, "0");
            assert_eq!(
                QueryBudget::for_nominal(Some(5_000)),
                QueryBudget {
                    rlimit: None,
                    timeout_ms: None,
                }
            );
        }
    }

    /// mununu#893 — the three facts the deterministic query budget rests on, printed, not
    /// asserted (they are host and z3-version facts): whether a solver-level `rlimit` is accepted
    /// and counted PER CHECK rather than cumulatively across the ladder's checks; what
    /// `reason_unknown` says for an rlimit stop versus a wall-clock timeout; and how many resource
    /// units this host spends per second on a hard query — the calibration behind
    /// `UNITS_PER_NOMINAL_MS`. Run by hand:
    /// `cargo test -p mununu-core --lib probe_rlimit -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_rlimit_semantics_and_calibration() {
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            // (1) per-check or cumulative? 300 trivial checks on ONE solver under a tiny rlimit.
            let s = z3::Solver::new();
            let mut p = z3::Params::new();
            p.set_u32("rlimit", 500);
            s.set_params(&p);
            let x = z3::ast::BV::new_const("x", 8);
            let mut unknowns = 0usize;
            for i in 0..300u64 {
                s.push();
                s.assert(x.eq(z3::ast::BV::from_u64(i % 256, 8)));
                if s.check() == z3::SatResult::Unknown {
                    unknowns += 1;
                }
                s.pop(1);
            }
            eprintln!(
                "per-check probe: {unknowns} unknown of 300 trivial checks at rlimit 500 \
                 (0 ⇒ the limit is per check)"
            );
            // (2) + (3): the reasons and the units/second on the pigeonhole query.
            let file = parser::parse(&pigeonhole_btor2(11, 10)).expect("parses");
            let (bad_ops, init_pairs, constraint_ops) = extract_props(&file);
            let (view, array_inits) = encode_for_bmc(&file).expect("encodes");
            let u = Unroller::build(&view, array_inits, &bad_ops, &constraint_ops, 0);
            for (key, val) in [("timeout", 2_000u32), ("rlimit", 300_000u32)] {
                let s = z3::Solver::new();
                let mut p = z3::Params::new();
                p.set_u32(key, val);
                s.set_params(&p);
                u.assert_init(&s, &init_pairs);
                for c in u.constraints_at(0) {
                    s.assert(&c);
                }
                s.assert(u.bad_at(0));
                let t0 = std::time::Instant::now();
                let res = s.check();
                let took = t0.elapsed();
                eprintln!(
                    "{key}={val}: {res:?} in {took:?}; reason_unknown={:?}; rlimit count={:?}",
                    s.get_reason_unknown(),
                    s.get_statistics().value("rlimit count")
                );
            }
        });
    }

    /// N0.5 — a peer's decision interrupts the query in flight. The `bad` is the pigeonhole
    /// principle with 11 pigeons and 10 holes over 1-bit inputs — trivial to bit-blast and
    /// exponential for resolution, so z3's SAT core searches for tens of seconds before the
    /// `unsat`; a probe with a 1.5 s per-query timeout establishes that on this host first
    /// (factoring-style instances were tried: z3 settles a 64-bit prime's factorisation in
    /// about a second; a 1536-bit multiplier is slow but its blasting is not a cancellation
    /// point). When the probe times out, the cancelled run must come back within the 300 ms
    /// the peer takes plus the member grace, abstaining; before the watchdog it waited the
    /// whole 60 s. When the probe decides (a faster z3), the interrupt path is not exercised
    /// here and the test says so rather than asserting on a fast machine.
    #[test]
    fn a_peer_decision_interrupts_the_query_in_flight() {
        let src = pigeonhole_btor2(11, 10);
        let file = parser::parse(&src).expect("parses");
        let probe_t0 = std::time::Instant::now();
        // mununu#893 — the probe's budget is a resource limit now: a hard query stops with a
        // named `ResourceLimit` (the signal that the interrupt path can be exercised); a decided
        // probe means a z3 fast enough to settle it within the allowance.
        let probe = decide_bad_safety(&file, 0, Some(1_500));
        eprintln!("probe: {probe:?} in {:?}", probe_t0.elapsed());
        match probe {
            Err(BmcError::ResourceLimit { .. }) => {}
            Ok(v) => {
                eprintln!(
                    "the pigeonhole query decided within the 1.5 s nominal allowance ({v:?}); the \
                     interrupt path is not exercised on this host"
                );
                return;
            }
            Err(e) => panic!("the probe must encode and stop on its budget: {e}"),
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let started = std::time::Instant::now();
        let verdict = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            });
            decide_bad_safety_cancellable(&file, 4, Some(60_000), Some(&cancel)).expect("encodes")
        });
        let elapsed = started.elapsed();
        eprintln!("cancelled run: {verdict:?} in {elapsed:?}");
        assert!(
            matches!(verdict, SafetyVerdict::Unknown { .. }),
            "an interrupted query must abstain, got {verdict:?}"
        );
        assert!(
            elapsed
                < std::time::Duration::from_millis(300)
                    + crate::adapter::MEMBER_CANCEL_GRACE
                    + std::time::Duration::from_millis(700),
            "the in-flight query was not interrupted within the grace: {elapsed:?}"
        );
    }

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
        assert_eq!(
            bmc_cex_until(&reach, 64, 5_000, far, &never).unwrap(),
            Some(1)
        );
        let wide = parser::parse(WIDE).expect("parse"); // depth 5, 300-bit (beyond the auto-cap ceiling)
        assert_eq!(
            bmc_cex_until(&wide, 64, 5_000, far, &never).unwrap(),
            Some(5)
        );
        // Bounded-safe design → no CEX within the depth budget → None (never a wrong verdict).
        let safe = parser::parse(SAFE).expect("parse");
        assert_eq!(bmc_cex_until(&safe, 32, 5_000, far, &never).unwrap(), None);
        // Cancellation / passed deadline are honored before any work.
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            bmc_cex_until(&reach, 64, 5_000, far, &cancelled).unwrap(),
            None
        );
        let past = far - std::time::Duration::from_secs(120);
        assert_eq!(
            bmc_cex_until(&reach, 64, 5_000, past, &never).unwrap(),
            None
        );
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
        let states = crate::adapter::btor2::cex_value::u64_frames(&trace.states);
        assert_eq!(
            states,
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
        assert_eq!(states.last().unwrap(), &vec![("cnt".to_string(), 3)]);
        assert_eq!(
            trace.states[0][0].1.bits(),
            2,
            "the value carries the cell's width"
        );
        // One input frame per state (the last one is what `bad` read); no named inputs ⇒ empty.
        assert_eq!(trace.inputs.len(), 4);
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

    /// mununu#726 — the ladder polls the thread-local budget: with an already-expired budget it
    /// abstains at depth 0 in well under a second instead of running `max_k` depths. Control: the
    /// same call with no budget decides `REACH` as before.
    #[test]
    fn decide_bad_safety_stops_on_budget_expiry_before_any_depth() {
        use crate::adapter::run_budget::{Budget, enter};
        let file = parser::parse(WIDE).unwrap();
        let t0 = std::time::Instant::now();
        let v = {
            let _g = enter(&Budget::with_ms(0));
            decide_bad_safety_cancellable(&file, 400, Some(5_000), None).unwrap()
        };
        assert_eq!(
            v,
            SafetyVerdict::Unknown { k: 0 },
            "an expired budget abstains at depth 0"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t0.elapsed()
        );
        let reach = parser::parse(REACH).unwrap();
        assert_eq!(
            decide_bad_safety(&reach, 8, Some(5_000)).unwrap(),
            SafetyVerdict::Violated { depth: 1 },
            "unbounded: the floor is today's behaviour"
        );
    }

    /// mununu#726 — a witness search under an expired budget is a budget ERROR, never a bounded
    /// `NoCexWithin` (which a caller would read as "spurious counterexample"). Unbounded, the same
    /// call on the bounded-safe design is `NoCexWithin` exactly as before.
    #[test]
    fn witness_search_reports_budget_expiry_not_no_cex() {
        use crate::adapter::run_budget::{Budget, enter};
        let file = parser::parse(SAFE).unwrap();
        let r = {
            let _g = enter(&Budget::with_ms(0));
            bmc_bad_reachable_witness(&file, 10)
        };
        assert!(
            matches!(r, Err(BmcError::BudgetExpired { k: 0 })),
            "expired budget → BudgetExpired at depth 0, got {r:?}"
        );
        let (o, t) = bmc_bad_reachable_witness(&file, 10).unwrap();
        assert_eq!(o, BmcOutcome::NoCexWithin { k: 10 });
        assert!(t.is_none());
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

    // ---- mununu#741 / PR A5 — init values and operand signs the z3 encoders used to lose ----

    // mununu#741 — two states initialised from the SAME input `v`: `s == t` forever, so
    // `bad = (s != t)` is unreachable (rIC3 UNSAT, the exact engine unreachable). An `init`
    // whose value is an input is not an Op/Const node, so `signal_bvs` cannot resolve it —
    // dropping the pair left the cell FREE: an over-approximation of the initial states and
    // a spurious depth-0 counterexample.
    const INIT_FROM_INPUT: &str = "1 sort bitvec 1\n2 input 1 v\n3 state 1 s\n4 init 1 3 2\n\
                                   5 next 1 3 3\n6 state 1 t\n7 init 1 6 2\n8 next 1 6 6\n\
                                   9 neq 1 3 6\n10 bad 9\n";
    // `t` initialised from `¬v` (an Op OVER the input), `bad = (s == t)`: the Op resolves, but
    // its value must be read at FRAME 0 — on the base view the input is a constant unrelated
    // to the frame-0 input `s` was pinned to.
    const INIT_FROM_INPUT_OP: &str = "1 sort bitvec 1\n2 input 1 v\n3 state 1 s\n4 init 1 3 2\n\
                                      5 next 1 3 3\n6 not 1 2\n7 state 1 t\n8 init 1 7 6\n\
                                      9 next 1 7 7\n10 eq 1 3 7\n11 bad 10\n";
    // `t` initialised from the STATE `s` (BTOR2 permits it — a shared nondeterministic
    // initial value): `s == t` forever.
    const INIT_FROM_STATE: &str = "1 sort bitvec 1\n2 state 1 s\n3 next 1 2 2\n4 state 1 t\n\
                                   5 init 1 4 2\n6 next 1 4 4\n7 neq 1 2 4\n8 bad 7\n";

    #[test]
    fn an_init_from_an_input_or_a_state_pins_the_cell() {
        use std::sync::atomic::AtomicBool;
        let far = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let never = AtomicBool::new(false);
        for (name, src) in [
            ("input", INIT_FROM_INPUT),
            ("op-over-input", INIT_FROM_INPUT_OP),
            ("state", INIT_FROM_STATE),
        ] {
            assert_eq!(
                bmc(src, 5),
                BmcOutcome::NoCexWithin { k: 5 },
                "bmc: init from {name}"
            );
            assert!(
                matches!(safety(src, 10), SafetyVerdict::Safe { .. }),
                "k-induction: init from {name}"
            );
            let file = parser::parse(src).expect("parse");
            assert_eq!(
                bmc_cex_until(&file, 32, 5_000, far, &never).unwrap(),
                None,
                "deep cex: init from {name}"
            );
        }
    }

    // PR A5 — a NEGATED operand (`-N`, BTOR2's bit-not shorthand) on `bad`, `constraint` and
    // an `init` value. Reading the operand by its absolute id checked the OPPOSITE property:
    // `bad -2` with `s` stuck at 1 (`bad = ¬s`, unreachable) became `bad = s`.
    const NEGATED_BAD: &str = "1 sort bitvec 1\n2 state 1 s\n3 one 1\n4 init 1 2 3\n\
                               5 next 1 2 2\n6 bad -2\n";
    // `constraint ¬in` pins the input to 0; `next s = in` keeps `s` at 0; `bad = s` unreachable.
    const NEGATED_CONSTRAINT: &str = "1 sort bitvec 1\n2 input 1 in\n3 state 1 s\n4 zero 1\n\
                                      5 init 1 3 4\n6 next 1 3 2\n7 constraint -2\n8 bad 3\n";
    // `init s = ¬0 = 1`, `bad = s`: violated at depth 0 — reading `init 0` made it SAFE.
    const NEGATED_INIT: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 s\n4 init 1 3 -2\n\
                                5 next 1 3 3\n6 bad 3\n";

    #[test]
    fn a_negated_operand_keeps_its_sign() {
        assert_eq!(bmc(NEGATED_BAD, 5), BmcOutcome::NoCexWithin { k: 5 });
        assert!(matches!(
            safety(NEGATED_BAD, 10),
            SafetyVerdict::Safe { .. }
        ));
        assert_eq!(bmc(NEGATED_CONSTRAINT, 5), BmcOutcome::NoCexWithin { k: 5 });
        assert!(matches!(
            safety(NEGATED_CONSTRAINT, 10),
            SafetyVerdict::Safe { .. }
        ));
        assert_eq!(bmc(NEGATED_INIT, 5), BmcOutcome::Violated { depth: 0 });
        assert_eq!(
            safety(NEGATED_INIT, 10),
            SafetyVerdict::Violated { depth: 0 }
        );
    }

    // mununu#723 — a cell wider than 64 bits appears in the witness at its full width. Before,
    // `extract_trace` did `as_u64()?` inside a `filter_map`: the 109-bit `tl_i` that carries
    // the register write behind OpenTitan edn's fatal alert was the one input missing from the
    // trace, and nothing said so. A 65-bit input `bad` compares against 2^64 + 5, and a
    // 100-bit state initialised to 2^99: both must be listed, with their value and width.
    #[test]
    fn a_witness_lists_cells_wider_than_64_bits() {
        const WIDE: &str = "1 sort bitvec 65\n2 input 1 wide_in\n3 sort bitvec 1\n\
                            4 constd 1 18446744073709551621\n5 eq 3 2 4\n6 bad 5\n\
                            7 sort bitvec 100\n8 consth 7 8000000000000000000000000\n\
                            9 state 7 wide_st\n10 init 7 9 8\n11 next 7 9 9\n";
        let file = parser::parse(WIDE).expect("parse btor2");
        let (outcome, trace) = bmc_bad_reachable_witness(&file, 2).expect("bmc runs");
        assert_eq!(outcome, BmcOutcome::Violated { depth: 0 });
        let trace = trace.expect("witness");
        let input = trace.inputs[0]
            .iter()
            .find(|(k, _)| k == "wide_in")
            .map(|(_, v)| v.clone())
            .expect("the 65-bit input is listed");
        assert_eq!(input.bits(), 65);
        assert_eq!(input.as_u64(), None);
        assert_eq!(input.to_hex(), "0x10000000000000005");
        let state = trace.states[0]
            .iter()
            .find(|(k, _)| k == "wide_st")
            .map(|(_, v)| v.clone())
            .expect("the 100-bit state is listed");
        assert_eq!(state.bits(), 100);
        assert_eq!(state.to_hex(), "0x8000000000000000000000000");
    }

    /// mununu#951 — a panic inside the watched body ends the watchdog, so the scope returns (the
    /// panic propagates) instead of hanging the row forever.
    #[test]
    fn a_panic_in_the_ladder_does_not_hang_its_watchdog() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let gate = QueryGate::default();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_cancel_watchdog(
                    Some(&cancel),
                    &gate,
                    || {},
                    || -> () { panic!("a ladder bug") },
                )
            }));
            let _ = tx.send(r.is_err());
        });
        let panicked = rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the watched scope must return; a hang here is mununu#951");
        assert!(
            panicked,
            "the panic propagates to the caller, which abstains on it"
        );
    }

    #[test]
    fn the_watchdog_returns_the_bodys_value() {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let gate = QueryGate::default();
        assert_eq!(with_cancel_watchdog(Some(&cancel), &gate, || {}, || 7), 7);
        assert_eq!(with_cancel_watchdog(None, &gate, || {}, || 8), 8);
    }
}
