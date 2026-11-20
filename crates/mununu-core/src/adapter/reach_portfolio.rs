//! Reachability portfolio — decide BTOR2 `bad`-reachability with every available
//! engine and merge under the **differential-oracle discipline**.
//!
//! The P1 payoff made usable — sound engines, each deciding a *different* slice of
//! designs:
//! - the **exact** BDD engine
//!   ([`exact_bad_reachable`](crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable))
//!   within its 40-bit cone cap;
//! - the in-house **native** engine (BMC + k-induction on the Z3 seam,
//!   [`native_bmc`](crate::adapter::btor2::native_bmc)) — no bit cap, no
//!   subprocess: mununu's own scalable safety member;
//! - **btormc** (BMC + k-induction) on deep counterexamples;
//! - **Pono** (IC3/PDR) on IC3-provable / violated instances;
//! - **rIC3** (bit-level IC3/PDR) on the inductive-invariant safety class;
//! - **rIC3's dynamic-step BMC** (`ric3-bmc`, `-e bmc --bmc-dyn-step`, mununu#997) on deep
//!   counterexamples whose shallow frames are hard: counterexamples only, never a proof.
//! - the owned **SAT BMC** (`sat-bmc`, [`sat_bmc`](crate::adapter::btor2::sat_bmc), N5.4): the
//!   same dynamic-step idea on in-process CaDiCaL over the AIG model, in both the full and the
//!   owned-only drivers; counterexamples only, never a proof.
//!
//! Plus the in-house **interpolation** engine ([`native_interp`], owned McMillan-style
//! forward reachability with an interpolation k-schedule). It synthesises inductive
//! invariants that k-induction does not reach at its budget (measured *unique* HWMCC
//! decides `gen12`/`gen14`/`gen39`). It runs *concurrently* with the other members
//! but polls a shared cancellation flag and bails the instant any faster engine
//! reaches a definite verdict — so it costs ~nothing on the common (fast-decided)
//! path yet still spends its full budget on the designs where it is the unique
//! decider. It needs cvc5 (a subprocess) and abstains when absent.
//!
//! Running them together decides strictly more than any one, and — crucially —
//! every engine's verdict is **sound**, so:
//!
//! - **first definite wins** — any engine's `Reachable` / `Unreachable` decides it;
//! - **two DEFINITE verdicts that DISAGREE raise a soundness alarm** ([`ReachVerdict::Contradiction`])
//!   rather than a guess. Since all engines are sound, a disagreement can only mean
//!   a real bug — exactly what the differential oracle exists to catch;
//! - **z3-SPACER is no longer a member** (N2, 2026-11-06). Since mununu#740 it had been
//!   corroborate-only in both directions, so it could not decide a row on its own: a lone
//!   `reachable` was a Horn derivation over the btor2→CHC encoding with a demonstrated
//!   spurious counterexample, and libz3 4.13.3's SPACER proved the violated HWMCC'20
//!   `vis_arrays_am2901` safe. rIC3 and the interpolation member carry the IC3-class proofs.
//!
//! One driver, [`decide_reach_portfolio`] (and its explicit-budget form
//! [`decide_reach_portfolio_parallel_with_timeout`]): every member — the subprocess
//! engines (btormc, Pono, rIC3), the in-process exact/native engines and the
//! interpolation member — runs concurrently, and the moment one of
//! them reaches a trusted definite verdict the subprocess members are cancelled
//! after a short grace, so a call's wall is the first decider's time, not the
//! slowest member's budget (S1, engine-performance roadmap). Until S1 the rescue
//! verbs (`verify-liveness`, `check-fsm`, the vacuity probe and non-vacuity gate,
//! verify-auto's ⊥ re-plan) ran a *sequential* driver that executed all six
//! members one after another whatever the first had decided — measured on the i2c
//! liveness monitor: native decided at 93 ms and SPACER then ran another 184 ms;
//! with btormc and Pono installed, their 60 s budgets each. Every member carries a
//! **wall-clock timeout** (Pono's IC3 has no native bound and can run unbounded on
//! a hard instance); a member that errors, times out, is cancelled or is undecided
//! simply abstains — a timeout is a sound [`ReachVerdict::Unknown`], never a wrong
//! verdict.

#![deny(clippy::unwrap_used)] // docs/policies/failure-paths.md Rule 2: abstain, never panic
use crate::adapter::btor2::ast::Btor2File;
use crate::adapter::btor2::emit::emit_btor2;
use crate::adapter::btor2::native_bmc::{self, BmcTrace, SafetyVerdict};
use crate::adapter::btor2::native_interp::{self, InterpSafetyVerdict};
use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
use crate::adapter::btormc::{self, McVerdict};
use crate::adapter::pono;
use crate::adapter::ric3;

/// The merged reachability verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReachVerdict {
    /// At least one sound engine found `bad` reachable (a real counterexample).
    Reachable,
    /// At least one sound engine proved `bad` unreachable (a real safety proof).
    Unreachable,
    /// No engine decided (all abstained / timed out / were undecided).
    #[default]
    Unknown,
    /// Two DEFINITE verdicts disagree — a soundness alarm, never a silent guess.
    Contradiction,
}

// The surface verdict label comes from the canonical
// [`crate::verdict::PropertyVerdict`] (`From<ReachVerdict>`), not a per-enum string,
// so `btor2 verify` reports the same `holds`/`violated`/`unknown` vocabulary as every
// other verify surface. The reachability *detail* stays in `reachable_by` /
// `unreachable_by` + the `Contradiction` alarm.

/// The portfolio outcome: the merged verdict plus which engines reached each
/// definite conclusion (empty lists ⇒ that side was undecided by all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReachOutcome {
    pub verdict: ReachVerdict,
    /// Engines that found `bad` reachable.
    pub reachable_by: Vec<&'static str>,
    /// Engines that proved `bad` unreachable.
    pub unreachable_by: Vec<&'static str>,
    /// mununu#734 — members that REFUSED the design, with their verbatim reason: an encode error
    /// (`ConstantOutOfRange`, an array sort under the BV-only theory), an absent binary, a child
    /// that exited without a verdict. A timeout, a cancellation and a clean `unknown` are NOT
    /// refusals. Before this the reason was dropped at the member boundary (`Err` → `None`), so a
    /// portfolio that abstained in 40 ms because every member refused the design read exactly like
    /// one that searched and gave up — on OpenTitan csrng that cost a debug run and a probe on a
    /// dumped model to attribute.
    pub refused: Vec<(&'static str, String)>,
    /// mununu#893 — members that ABSTAINED with a stated cause: a budget stop (the native member's
    /// deterministic resource limit, or a wall clock), in the member's own words. Distinct from a
    /// refusal (a construct the member does not carry — more budget will not help) and from a
    /// clean `unknown` (the member searched to its bound). A consumer reads the three apart: a
    /// refusal is routed around, a budget abstention may be bought back with a larger budget, a
    /// clean unknown is the genuine search abstention.
    pub abstained: Vec<(&'static str, String)>,
    /// mununu#1022 — members that searched to their own bound and found neither a counterexample
    /// nor a proof, with how far they got (native: the depth / k reached; interpolation: its
    /// reason). Not a refusal and not a budget stop: a deeper bound or a different engine may
    /// decide the row. A member cancelled because another one decided first is in no list.
    pub undecided: Vec<(&'static str, String)>,
    /// N5.4 — the counterexample a member that found `bad` reachable carried out of the run,
    /// replayed on the concrete simulator before it got here; only when the run was started
    /// with [`ReachMembers::witness`]. The first in the fixed engine order among the members
    /// that finished with one — which members finish before the cancel is a matter of timing,
    /// so `by` (and the trace) may differ between two runs of one design; each is a replayed
    /// counterexample of the design, not necessarily the shallowest.
    pub trace: Option<CarriedTrace>,
    /// N5.4 — members that found `bad` reachable but whose OWN witness did not replay on the
    /// design (`(member, why)`): evidence against that member's verdict (an encode or emit
    /// defect, or a tool's witness for a different model), or a design the simulator cannot
    /// carry (a cell wider than 128 bits — the reason says which). The verdict is unchanged; a
    /// consumer reads the member's verdict as unconfirmed, and as the whole verdict when that
    /// member is the only one in `reachable_by`. The owned IC3 member replays inside the engine,
    /// so its failure is listed whether or not a witness was asked for (mununu#1143); the
    /// subprocess members replay only when carrying one.
    pub trace_replay_failed: Vec<(&'static str, String)>,
}

/// N5.4 — a counterexample carried out of the portfolio by the member that found it, replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedTrace {
    /// The member whose witness this is (`sat-bmc`, `ic3`, `btormc`, `ric3`, `ric3-bmc`).
    pub by: &'static str,
    /// The replayed trace: frame 0 (reset) .. the frame that makes `bad` true.
    pub trace: BmcTrace,
    /// mununu#1057 — whether the member established that no shorter counterexample exists:
    /// `Some(true)` when it refuted every depth below the trace's (`sat-bmc`'s plain search, or
    /// its dynamic step when the hit sits at its window's base), `Some(false)` when it did not
    /// (a dynamic-step window answered with a deeper path), `None` when the member does not
    /// say (`ic3`, btormc, rIC3: their traces are their own, never claimed shortest).
    pub shortest: Option<bool>,
}

impl CarriedTrace {
    /// The trace's depth: the number of steps from reset to the frame that makes `bad` true.
    pub fn depth(&self) -> u32 {
        u32::try_from(self.trace.states.len().saturating_sub(1)).unwrap_or(u32::MAX)
    }
}

impl ReachOutcome {
    /// Merge from the two verdict sets under the first-definite +
    /// contradiction-alarm rule.
    fn from_sets(reachable_by: Vec<&'static str>, unreachable_by: Vec<&'static str>) -> Self {
        let verdict = match (!reachable_by.is_empty(), !unreachable_by.is_empty()) {
            (true, true) => ReachVerdict::Contradiction,
            (true, false) => ReachVerdict::Reachable,
            (false, true) => ReachVerdict::Unreachable,
            (false, false) => ReachVerdict::Unknown,
        };
        ReachOutcome {
            verdict,
            reachable_by,
            unreachable_by,
            refused: Vec::new(),
            abstained: Vec::new(),
            undecided: Vec::new(),
            trace: None,
            trace_replay_failed: Vec::new(),
        }
    }
}

/// `(member, text)` notes — a refusal, an abstention, a progress line or a replay failure.
type MemberNotes = Vec<(&'static str, String)>;

/// N5.4 — `(member, replayed trace, shortest)` for every member that carried a counterexample
/// out of a run (see [`CarriedTrace`] and its `shortest`).
pub type CarriedTraces = Vec<(&'static str, BmcTrace, Option<bool>)>;

/// N5.4 — where the member threads leave a replayed counterexample ([`CarriedTrace`]) or the
/// reason their own witness did not replay; read once at the join. Shared by both drivers.
#[derive(Debug, Default)]
struct Carried {
    traces: std::sync::Mutex<CarriedTraces>,
    replay_failed: std::sync::Mutex<MemberNotes>,
}

impl Carried {
    /// Record `member`'s replayed trace (with whether the member established it is the
    /// shortest, [`CarriedTrace::shortest`]), or why its witness did not replay. A refusal is
    /// also a `warn!`: it is evidence against that member's verdict, and the report carries it
    /// (`trace_replay_failed`).
    fn carry(&self, member: &'static str, r: Result<BmcTrace, String>, shortest: Option<bool>) {
        // A poisoned lock (a member panicked while recording) loses the entry; the panic itself
        // is the louder signal, and the verdict never depends on it (as `note_refusal`).
        match r {
            Ok(t) => {
                if let Ok(mut g) = self.traces.lock() {
                    g.push((member, t, shortest));
                }
            }
            Err(why) => {
                tracing::warn!(member, %why, "reach portfolio: the member's witness did not replay");
                if let Ok(mut g) = self.replay_failed.lock() {
                    g.push((member, why));
                }
            }
        }
    }

    /// Everything carried so far (a poisoned lock yields nothing; the panic is the louder signal).
    fn take(&self) -> (CarriedTraces, MemberNotes) {
        (
            self.traces.lock().map(|g| g.clone()).unwrap_or_default(),
            self.replay_failed
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default(),
        )
    }
}

/// N5.4 — a subprocess member's BTOR2 witness (its stdout on `sat`), replayed on the design.
fn replayed_subprocess_witness(file: &Btor2File, stdout: &str) -> Result<BmcTrace, String> {
    use crate::adapter::btor2::witness::{parse_btor2_witness, trace_from_witness};
    parse_btor2_witness(stdout).and_then(|w| trace_from_witness(file, &w))
}

/// The members' individual results for one portfolio run, named — the input of [`collect`]. A
/// `None` is an abstention (unknown, timeout, cancelled); a refusal is recorded in `refused` beside
/// the `None`. Adding a member is adding a field, not a positional argument.
#[derive(Debug, Default)]
pub(crate) struct Members {
    pub exact: Option<bool>,
    pub native: Option<bool>,
    pub interp: Option<bool>,
    /// N5.4 — the owned SAT BMC (CaDiCaL, dynamic step): counterexamples only.
    pub sat_bmc: Option<bool>,
    /// N5.1 — the owned IC3 (CaDiCaL): proves and refutes, each verdict checked before it ships;
    /// started by default since its N5.5 gate under `--owned-only` and on hosts without rIC3
    /// ([`ic3_default`]).
    pub ic3: Option<bool>,
    pub btormc: Option<McVerdict>,
    pub pono: Option<McVerdict>,
    /// Plan 4 / PR D2 — rIC3 `-e ic3`, bit-level IC3/PDR over the emitted BTOR2 (mununu#736).
    pub ric3: Option<McVerdict>,
    /// mununu#997 — rIC3 `-e bmc --bmc-dyn-step`: counterexamples only.
    pub ric3_bmc: Option<McVerdict>,
    /// `(member, reason)` for every member that refused the design (see [`ReachOutcome::refused`]).
    pub refused: Vec<(&'static str, String)>,
    /// `(member, cause)` for every member that abstained on a budget (see
    /// [`ReachOutcome::abstained`]).
    pub abstained: Vec<(&'static str, String)>,
    /// `(member, progress)` for every member that searched to its bound (see
    /// [`ReachOutcome::undecided`]).
    pub undecided: Vec<(&'static str, String)>,
    /// N5.4 — `(member, replayed trace, shortest)` for every member that carried its
    /// counterexample out (see [`ReachOutcome::trace`], [`CarriedTrace::shortest`]), in
    /// finishing order.
    pub traces: CarriedTraces,
    /// N5.4 — `(member, why)` for every member whose own witness did not replay (see
    /// [`ReachOutcome::trace_replay_failed`]).
    pub replay_failed: Vec<(&'static str, String)>,
}

/// Record a member's refusal (an `Err`) and abstain; a clean result passes through.
fn settle<T>(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    member: &'static str,
    r: Result<Option<T>, String>,
) -> Option<T> {
    match r {
        Ok(v) => v,
        Err(why) => {
            note_refusal(refused, member, why);
            None
        }
    }
}

/// A subprocess member's (btormc, pono, rIC3, rIC3-BMC) result, settled. An `Err` is a refusal
/// (as [`settle`]); an `Unknown` while no other member has decided is a budget stop, recorded in
/// `abstained` so the portfolio's ⊥ names it (mununu#997, the sibling of #1022's owned-member
/// fix: until then a subprocess member that ran out its budget left no trace). An `Unknown`
/// after another member decided is the cancellation, never recorded.
fn settle_subprocess(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    abstained: &std::sync::Mutex<Vec<(&'static str, String)>>,
    decided: &std::sync::atomic::AtomicBool,
    clock_stop: &std::sync::atomic::AtomicBool,
    member: &'static str,
    timeout: std::time::Duration,
    r: Result<Option<McVerdict>, String>,
) -> Option<McVerdict> {
    use std::sync::atomic::Ordering::Relaxed;
    let v = settle(refused, member, r);
    // mununu#1043 — a member stopped by the wall or the budget raising `decided` is recorded
    // too; only a stop because a peer decided is in no list.
    if v == Some(McVerdict::Unknown) && (!decided.load(Relaxed) || clock_stop.load(Relaxed)) {
        note_refusal(
            abstained,
            member,
            format!(
                "returned no verdict within its {} ms subprocess budget (a timeout, or the tool \
                 ended undecided)",
                timeout.as_millis()
            ),
        );
    }
    v
}

/// mununu#1043 — the cause a member stopped by the portfolio's wall or run budget carries.
fn clock_stopped(progress: &str) -> String {
    format!("{progress}; stopped by the portfolio's wall or run budget before a verdict")
}

/// mununu#1143 — the line a member whose thread ended without a result gets in `refused`: a
/// crash is a mununu defect, named as one, never a silent `None`.
fn panicked_reason(member: &str) -> String {
    crate::adapter::owner::mununu_defect(&format!(
        "{member}: the member's thread ended without a result (it panicked)"
    ))
}

/// mununu#1143 — settle a member's join: its result, or `None` with the panic recorded in
/// `refused` (the exact member's straggler path did this; the scoped joins dropped it).
fn settle_join<T>(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    member: &'static str,
    joined: std::thread::Result<Option<T>>,
) -> Option<T> {
    match joined {
        Ok(v) => v,
        Err(_) => {
            note_refusal(refused, member, panicked_reason(member));
            None
        }
    }
}

/// Record one member's refusal. A poisoned lock (a member panicked while recording) loses the
/// list; the panic itself is the louder signal, and the verdict never depends on this list.
fn note_refusal(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    member: &'static str,
    why: String,
) {
    if let Ok(mut g) = refused.lock() {
        g.push((member, why));
    }
}

/// mununu#893 — why an owned member stopped without a verdict: it REFUSED the design (a
/// construct it does not carry) or it ABSTAINED on a budget (the deterministic resource limit, a
/// wall clock). The two reach the report through different lists because they call for opposite
/// responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberStop {
    Refused(String),
    Abstained(String),
    /// mununu#1022 — searched to its bound, no verdict; the progress in the member's words.
    Undecided(String),
    /// mununu#1043 — stopped because the portfolio's shared stop flag was raised, with the
    /// progress it had made. The flag goes up when a peer decides (then the stop is in no list)
    /// and, in the full driver, when the wall or the run budget fires (then it is a budget
    /// abstention, and the progress is its cause): [`settle_native`] tells the two apart.
    Cancelled(String),
}

/// mununu#1043 — no clock stop: the `--owned-only` driver has no watchdog, so its shared flag
/// is raised only by a peer's verdict.
static NO_CLOCK_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl From<native_bmc::BmcError> for MemberStop {
    fn from(e: native_bmc::BmcError) -> Self {
        match e {
            native_bmc::BmcError::BudgetExpired { .. }
            | native_bmc::BmcError::ResourceLimit { .. } => MemberStop::Abstained(e.to_string()),
            native_bmc::BmcError::Encode(_) | native_bmc::BmcError::NoBadProperty => {
                MemberStop::Refused(e.to_string())
            }
        }
    }
}

/// [`settle`] for a member that tells a refusal from a budget abstention: each cause lands in
/// its own list, and the member abstains either way.
fn settle_native<T>(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    abstained: &std::sync::Mutex<Vec<(&'static str, String)>>,
    undecided: &std::sync::Mutex<Vec<(&'static str, String)>>,
    clock_stop: &std::sync::atomic::AtomicBool,
    member: &'static str,
    r: Result<Option<T>, MemberStop>,
) -> Option<T> {
    match r {
        Ok(v) => v,
        Err(MemberStop::Cancelled(progress)) => {
            // mununu#1043 — the flag the member stopped on was the wall's or the budget's, not a
            // peer's verdict: a budget abstention, with the member's own progress as its cause.
            if clock_stop.load(std::sync::atomic::Ordering::Relaxed) {
                note_refusal(abstained, member, clock_stopped(&progress));
            }
            None
        }
        Err(MemberStop::Undecided(progress)) => {
            note_refusal(undecided, member, progress);
            None
        }
        Err(MemberStop::Refused(why)) => {
            note_refusal(refused, member, why);
            None
        }
        Err(MemberStop::Abstained(cause)) => {
            note_refusal(abstained, member, cause);
            None
        }
    }
}

/// Per-member wall attribution (category 4 of the engine-performance roadmap). One
/// `tracing::debug!` line per member — its name, elapsed milliseconds and verdict — so
/// `RUST_LOG=mununu_core::adapter::reach_portfolio=debug` shows WHICH member set a
/// portfolio call's wall and how long the others ran after the first decision. The
/// instrument behind the scheduling measurements; it never changes a verdict.
fn timed<T: std::fmt::Debug>(member: &'static str, run: impl FnOnce() -> T) -> T {
    let t0 = std::time::Instant::now();
    let v = run();
    tracing::debug!(
        member,
        elapsed_ms = t0.elapsed().as_millis() as u64,
        verdict = ?v,
        "reach portfolio member"
    );
    v
}

/// The exact engine's verdict: `Ok(Some(true))` reachable, `Ok(Some(false))` unreachable,
/// `Err(reason)` refused (over-cap / free-init / an unsupported construct — the engine's own
/// message, kept for the attribution of mununu#734).
fn run_exact(content: &str) -> Result<Option<bool>, String> {
    timed("exact", || {
        exact_bad_reachable(content)
            .map(Some)
            .map_err(|e| format!("exact: {e}"))
    })
}

/// The in-house **native engine** (BMC + k-induction on the Z3 seam) verdict:
/// `Ok(Some(true))` reachable (a counterexample), `Ok(Some(false))` unreachable (a
/// k-inductive safety proof), `Ok(None)` abstained (Unknown / timeout), `Err(reason)` refused
/// (an encode error — the design has a construct the SMT encoder does not carry).
/// Unlike the exact BDD engine it has no 40-bit cone cap, and unlike btormc/Pono
/// it needs no subprocess — mununu's own scalable safety member.
fn run_native(
    file: &Btor2File,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    query_ms: u32,
    wall: Option<std::time::Instant>,
) -> Result<Option<bool>, MemberStop> {
    // mununu#893 — `query_ms` is the per-check allowance in NOMINAL milliseconds: a deterministic
    // resource limit inside the member, never a wall clock; a stop on it comes back as an
    // `Abstained` cause, a construct the encoder does not carry as a `Refused` one.
    timed(
        "native",
        || match native_bmc::decide_bad_safety_cancellable(
            file,
            native_bmc::DEFAULT_MAX_K,
            Some(query_ms),
            cancel,
        ) {
            Ok(SafetyVerdict::Violated { .. }) => Ok(Some(true)),
            Ok(SafetyVerdict::Safe { .. }) => Ok(Some(false)),
            // A cancelled run also ends `Unknown` (at the depth it stopped): that one is in no
            // list, as a cancelled subprocess member is not.
            Ok(SafetyVerdict::Unknown { k })
                if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) =>
            {
                Err(MemberStop::Cancelled(format!(
                    "native BMC / k-induction stopped at depth {k}"
                )))
            }
            Ok(SafetyVerdict::Unknown { k }) => Err(MemberStop::Undecided(native_unknown(k, wall))),
            Err(e) => Err(MemberStop::from(e)),
        },
    )
}

/// N5.4 (Track N) — the deepest frame the owned SAT BMC member checks. The dynamic step jumps
/// geometrically, so the bound costs nothing on a design whose counterexample is shallow; the
/// deepest one it decided on HWMCC'20 is `mul9` at 126 (rIC3's dynamic step found 238).
const SAT_BMC_MAX_DEPTH: u32 = 1_000;

/// N5.4 — the owned **SAT BMC** member: bit-level BMC over the AIG model on in-process CaDiCaL
/// ([`sat_bmc`](crate::adapter::btor2::sat_bmc)) with the dynamic step, the owned counterpart of
/// `ric3-bmc`. Counterexamples only: `Ok(Some(true))` reachable, never `Some(false)` (a bounded
/// search proves nothing). A model the AIG backend cannot build (arrays) is a refusal; a stop at
/// the wall is a budget abstention; a search refuted through [`SAT_BMC_MAX_DEPTH`] is undecided;
/// a stop because another member decided is in no list.
fn run_sat_bmc(
    file: &Btor2File,
    deadline: std::time::Instant,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    carried: Option<&Carried>,
) -> Result<Option<bool>, MemberStop> {
    use crate::adapter::btor2::aig::AigModel;
    use crate::adapter::btor2::sat_bmc::{self, DynamicStep, SatBmcOptions, SatBmcOutcome};
    let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
    let opts = SatBmcOptions {
        max_depth: SAT_BMC_MAX_DEPTH,
        deadline: Some(deadline),
        cancel: Some(std::sync::Arc::clone(&cancel)),
        dynamic_step: Some(DynamicStep::default()),
        ..Default::default()
    };
    // N5.4 — with a witness asked for, the model is kept so the counterexample's BTOR2 witness
    // (the format the tier replays) can be built from it and replayed; the search is the same.
    let run = || -> Result<SatBmcOutcome, String> {
        match carried {
            None => sat_bmc::sat_bmc(file, &opts),
            Some(carried) => {
                let model = AigModel::build(file)?;
                let outcome = sat_bmc::sat_bmc_model(&model, &opts)?;
                if let Some(witness) = sat_bmc::btor2_witness(&model, &outcome) {
                    // mununu#1057 — the member says whether it refuted every shorter depth.
                    let shortest = match &outcome {
                        SatBmcOutcome::Violated {
                            depth,
                            refuted_below,
                            ..
                        } => Some(refuted_below == depth),
                        _ => None,
                    };
                    carried.carry(
                        "sat-bmc",
                        replayed_subprocess_witness(file, &witness),
                        shortest,
                    );
                }
                Ok(outcome)
            }
        }
    };
    timed("sat-bmc", || match run() {
        Ok(SatBmcOutcome::Violated { .. }) => Ok(Some(true)),
        Ok(SatBmcOutcome::Stopped { depth, .. }) if cancelled() => Err(MemberStop::Cancelled(
            format!("SAT BMC stopped; every depth below {depth} refuted"),
        )),
        Ok(SatBmcOutcome::Stopped { depth, reason }) => Err(MemberStop::Abstained(format!(
            "SAT BMC stopped ({reason}); every depth below {depth} refuted"
        ))),
        Ok(SatBmcOutcome::NoCounterexample { depth }) => Err(MemberStop::Undecided(format!(
            "SAT BMC found no counterexample through depth {depth} (its bound)"
        ))),
        Err(why) => Err(MemberStop::Refused(format!("sat-bmc: {why}"))),
    })
}

/// N5.1 — the owned **IC3** member ([`ic3`](crate::adapter::btor2::ic3)): `Ok(Some(false))` is an
/// inductive invariant that passed its direct check in a fresh solver, `Ok(Some(true))` a
/// counterexample concretised step by step and replayed on the concrete simulator. A design the
/// AIG backend cannot model (arrays) is a refusal (`ic3:`); a stop at the wall, or a verdict that
/// failed its own check, is an abstention with the cause; a stop because another member decided
/// is in no list.
fn run_ic3(
    file: &Btor2File,
    deadline: std::time::Instant,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    store: &Carried,
    witness: bool,
) -> Result<Option<bool>, MemberStop> {
    use crate::adapter::btor2::ic3::{self, Ic3Options, Ic3Outcome, Replay};
    let cancelled = || cancel.load(std::sync::atomic::Ordering::Relaxed);
    let opts = Ic3Options {
        deadline: Some(deadline),
        cancel: Some(std::sync::Arc::clone(&cancel)),
        max_frames: None,
    };
    timed("ic3", || match ic3::ic3(file, &opts) {
        Ok(Ic3Outcome::Safe { .. }) => Ok(Some(false)),
        Ok(Ic3Outcome::Violated { depth, replay, .. }) => {
            // N5.4 — the engine replayed the trace already; with a witness asked for it is
            // carried out as it is. A design the simulator cannot carry ships unreplayed and the
            // report says so whether or not a witness was asked for: before mununu#1143 the
            // reason went to the log alone and the verdict read like a replayed one.
            // SOUNDNESS: the verdict is the engine's concretised counterexample, unconfirmed by
            // the simulator; it is listed in `trace_replay_failed` (`sole_decider` when it is
            // the whole verdict), which `btor2 verify` prints on CLI and API — the other verbs
            // drop the list on their way to the report (mununu#1175).
            match replay {
                Replay::Confirmed(trace) if witness => store.carry("ic3", Ok(trace), None),
                Replay::Confirmed(_) => {}
                Replay::Unavailable(why) => {
                    tracing::info!(
                        depth,
                        why,
                        "ic3: the counterexample ships without a simulator replay"
                    );
                    store.carry("ic3", Err(format!("not replayed: {why}")), None);
                }
            }
            Ok(Some(true))
        }
        Ok(Ic3Outcome::Stopped { frames, .. }) if cancelled() => Err(MemberStop::Cancelled(
            format!("IC3 stopped at frame {frames}"),
        )),
        Ok(Ic3Outcome::Stopped { frames, reason }) => Err(MemberStop::Abstained(format!(
            "IC3 stopped at frame {frames} ({reason})"
        ))),
        Err(why) => Err(MemberStop::Refused(format!("ic3: {why}"))),
    })
}

/// mununu#1022 — the native member's progress when it searched to its bound without a verdict.
///
/// mununu#1038 — the bound is a fixed depth, not the budget, so the member can stop with most of
/// the portfolio's wall unused; the time left at the stop is part of the reason. (Raising the
/// bound was measured not to help: uncapped, plain and simple-path k-induction proved neither
/// HWMCC'20 k-liveness monitor of #1038 by depth 37–78 in 30–60 min, while each step grew to
/// minutes. The wall there is IC3-class, not depth.)
fn native_unknown(k: u32, wall: Option<std::time::Instant>) -> String {
    let mut why = format!(
        "BMC found no counterexample through depth {k} and k-induction found no proof up to \
         k = {k} (bound {})",
        native_bmc::DEFAULT_MAX_K
    );
    if let Some(left) = wall.and_then(|w| w.checked_duration_since(std::time::Instant::now())) {
        why.push_str(&format!(
            "; it stopped at its depth bound with {:.1} s of the wall left",
            left.as_secs_f64()
        ));
    }
    why
}

/// Budget for the interpolation member. In the parallel driver it runs concurrently
/// but cancels the moment another engine decides (so the common fast-decided design
/// pays almost nothing); in the sequential driver it runs only after the other five
/// abstain. These caps bound the cvc5 interpolation work when it does run — tuned so
/// the measured unique HWMCC decides (`gen12`/`gen14`/`gen39`) land while the
/// minutes-long interpolation queries (`gen43`-class) abstain at the deadline. The
/// 25 s overall cap sits under the 60 s btormc/Pono member timeouts, so as a parallel
/// member it never extends the portfolio's wall-clock past its existing slowest.
const INTERP_MAX_SUFFIX: u32 = 16;
const INTERP_MAX_ITERS: u32 = 24;
const INTERP_QUERY_TIMEOUT_MS: u32 = 5_000;
const INTERP_OVERALL_TIMEOUT_MS: u64 = 25_000;

/// The in-house **interpolation engine** ([`native_interp`], owned McMillan-style
/// forward reachability with an interpolation k-schedule) verdict: `Some(true)`
/// reachable, `Some(false)` unreachable (an interpolation-synthesised inductive
/// invariant), `None` abstained. It decides a slice of safe designs whose invariant
/// needs *synthesised* predicates that k-induction does not reach at its budget — measured *unique* HWMCC coverage (`gen12`/`gen14`/`gen39`,
/// which the whole rest of the portfolio leaves `Unknown`). cvc5 (a subprocess) is
/// required; when it is absent the engine abstains.
fn interp_search(
    file: &Btor2File,
    cancel: &std::sync::atomic::AtomicBool,
    t: MemberTimeouts,
) -> InterpSafetyVerdict {
    native_interp::verify_safety_interp_cancellable(
        file,
        INTERP_MAX_SUFFIX,
        INTERP_MAX_ITERS,
        t.interp_query_ms,
        t.interp_overall_ms,
        cancel,
    )
}

/// mununu#1022 — the interpolation member's run: a cvc5 that cannot be located is a REFUSAL,
/// found before the search and so recorded on every run whatever a peer did (as every other
/// member locates its tool first); a search that ends undecided is `undecided`, unless the member
/// stopped because a peer had already decided. mununu#1043 — a search stopped on the shared flag
/// when the wall or the run budget raised it (`clock_stop`) is a budget abstention with its reason.
fn interp_member(
    refused: &std::sync::Mutex<Vec<(&'static str, String)>>,
    abstained: &std::sync::Mutex<Vec<(&'static str, String)>>,
    undecided: &std::sync::Mutex<Vec<(&'static str, String)>>,
    cancel: &std::sync::atomic::AtomicBool,
    clock_stop: &std::sync::atomic::AtomicBool,
    available: Result<(), String>,
    search: impl FnOnce() -> InterpSafetyVerdict,
) -> Option<bool> {
    if let Err(why) = available {
        note_refusal(refused, "interp", why);
        return None;
    }
    match search() {
        InterpSafetyVerdict::Unsafe { .. } => Some(true),
        InterpSafetyVerdict::Safe { .. } => Some(false),
        InterpSafetyVerdict::Undecided { reason } => {
            use std::sync::atomic::Ordering::Relaxed;
            if !cancel.load(Relaxed) {
                note_refusal(undecided, "interp", reason);
            } else if clock_stop.load(Relaxed) {
                note_refusal(abstained, "interp", clock_stopped(&reason));
            }
            None
        }
    }
}

/// The **btormc** member's run (subprocess BMC): its verdict, an `Err` when the tool cannot be
/// invoked. N5.4 — btormc prints its witness on `sat`; with `carried` it is replayed and carried.
fn run_btormc(
    file: &Btor2File,
    kmax: u32,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
    carried: Option<&Carried>,
) -> Result<Option<McVerdict>, String> {
    timed("btormc", || {
        btormc::decide_via_btormc_with_output(file, kmax, timeout, Some(cancel))
            .map(|(v, out)| {
                if let (Some(carried), McVerdict::Violated) = (carried, v) {
                    carried.carry("btormc", replayed_subprocess_witness(file, &out), None);
                }
                Some(v)
            })
            .map_err(|e| e.message)
    })
}

/// The **Pono** member's run (subprocess, `ic3bits`).
fn run_pono(
    file: &Btor2File,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Option<McVerdict>, String> {
    timed("pono", || {
        pono::decide_via_pono(file, pono::DEFAULT_ENGINE, timeout, Some(cancel))
            .map(Some)
            .map_err(|e| e.message)
    })
}

/// The **rIC3** member's run (subprocess, `-e <engine>`). N5.4 — with `carried`, `--witness` is
/// asked for and the witness is replayed and carried.
fn run_ric3(
    file: &Btor2File,
    engine: &'static str,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
    carried: Option<&Carried>,
) -> Result<Option<McVerdict>, String> {
    timed("ric3", || {
        ric3::decide_via_ric3_with_output(file, engine, timeout, Some(cancel), carried.is_some())
            .map(|(v, out)| {
                if let (Some(carried), McVerdict::Violated) = (carried, v) {
                    carried.carry("ric3", replayed_subprocess_witness(file, &out), None);
                }
                Some(v)
            })
            .map_err(|e| e.message)
    })
}

/// The **rIC3 dynamic-step BMC** member's run (subprocess). N5.4 — with `carried`, the witness
/// is the sticky model's: replayed on the design up to the first frame that fires the original
/// `bad`, and carried.
fn run_ric3_bmc(
    file: &Btor2File,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
    carried: Option<&Carried>,
) -> Result<Option<McVerdict>, String> {
    timed("ric3-bmc", || {
        ric3::decide_via_ric3_bmc_dyn_step_with_output(
            file,
            timeout,
            Some(cancel),
            carried.is_some(),
        )
        .map(|(v, out)| {
            if let (Some(carried), McVerdict::Violated) = (carried, v) {
                carried.carry("ric3-bmc", ric3::sticky_witness_trace(file, &out), None);
            }
            Some(v)
        })
        .map_err(|e| e.message)
    })
}

/// mununu#726 — the members' time bounds for one portfolio run, derived from the thread-local
/// [`run_budget`](crate::adapter::run_budget) ONCE at entry. With no budget in effect every value
/// is the constant it always was (the floor = today's behaviour, byte-identical). Under a budget
/// the members may use the time that is left: the native BMC / k-induction per-query timeout grows
/// to the remaining time (the fixed 5 s let one to three of eight identical rows decide per run on
/// the full OpenTitan `edn` IP), the interpolation search gets the
/// remainder as its wall, the subprocess members' wall IS the remainder (above 60 s btormc's
/// depth cap lifts automatically, as before).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemberTimeouts {
    pub(crate) native_query_ms: u32,
    pub(crate) interp_query_ms: u32,
    pub(crate) interp_overall_ms: u64,
    pub(crate) subprocess: std::time::Duration,
}

/// The full portfolio's member budgets (mununu#892). With a run budget in effect every member
/// follows the time left, as before. Without one, a `--timeout-ms` RAISED above the subprocess
/// default is read as the member budget for the owned members too — native's per-query bound and
/// interpolation's bounds — the way `--owned-only` already computes it. Until this, the raised
/// wall reached only the subprocess members and btormc's depth cap, and native stayed at its 5 s
/// per-query default: two HWMCC'20 array rows were ⊥ in the full run with ten minutes of
/// subprocess time and decided by native alone with sixty seconds of query time. At the default
/// wall nothing changes.
pub(crate) fn portfolio_member_timeouts(
    remaining_ms: Option<u64>,
    subprocess: std::time::Duration,
) -> MemberTimeouts {
    if remaining_ms.is_none() && subprocess > btormc::DEFAULT_TIMEOUT {
        let raised = u64::try_from(subprocess.as_millis()).unwrap_or(u64::MAX);
        return member_timeouts(Some(raised), subprocess);
    }
    member_timeouts(remaining_ms, subprocess)
}

pub(crate) fn member_timeouts(
    remaining_ms: Option<u64>,
    subprocess: std::time::Duration,
) -> MemberTimeouts {
    match remaining_ms {
        None => MemberTimeouts {
            native_query_ms: native_bmc::DEFAULT_QUERY_NOMINAL_MS,
            interp_query_ms: INTERP_QUERY_TIMEOUT_MS,
            interp_overall_ms: INTERP_OVERALL_TIMEOUT_MS,
            subprocess,
        },
        Some(rem) => {
            let rem = rem.max(1);
            let rem32 = u32::try_from(rem).unwrap_or(u32::MAX);
            MemberTimeouts {
                native_query_ms: rem32,
                interp_query_ms: INTERP_QUERY_TIMEOUT_MS.max(rem32 / 5).min(rem32),
                interp_overall_ms: rem,
                subprocess: std::time::Duration::from_millis(rem),
            }
        }
    }
}

/// Merge the members' verdicts into a [`ReachOutcome`], in the fixed engine order
/// (`exact`, `native`, `interp`, `btormc`, `pono`, `ric3`) so the outcome is
/// deterministic regardless of which driver (sequential / parallel) produced them. The
/// members' refusals ride along untouched (mununu#734).
fn collect(m: Members) -> ReachOutcome {
    let mut reachable_by: Vec<&'static str> = Vec::new();
    let mut unreachable_by: Vec<&'static str> = Vec::new();
    // In-house engines report reachability as a bool (`Some(true)` = reachable).
    for (name, v) in [
        ("exact", m.exact),
        ("native", m.native),
        ("interp", m.interp),
        ("sat-bmc", m.sat_bmc),
        ("ic3", m.ic3),
    ] {
        match v {
            Some(true) => reachable_by.push(name),
            Some(false) => unreachable_by.push(name),
            None => {}
        }
    }
    // Subprocess members report an `McVerdict`.
    for (name, v) in [
        ("btormc", m.btormc),
        ("pono", m.pono),
        ("ric3", m.ric3),
        ("ric3-bmc", m.ric3_bmc),
    ] {
        match v {
            Some(McVerdict::Violated) => reachable_by.push(name),
            Some(McVerdict::Safe) => unreachable_by.push(name),
            Some(McVerdict::Unknown) | None => {}
        }
    }
    let mut out = ReachOutcome::from_sets(reachable_by, unreachable_by);
    // The refusals arrive in FINISHING order (each member on its own thread); report them in the
    // fixed engine order so two runs of one design produce one outcome (the drivers' equality
    // tests compare whole outcomes).
    let rank = member_rank;
    let mut refused = m.refused;
    refused.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out.refused = refused;
    let mut abstained = m.abstained;
    abstained.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out.abstained = abstained;
    let mut undecided = m.undecided;
    undecided.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out.undecided = undecided;
    // N5.4 — the carried trace: the first in the fixed order among the members that brought one.
    out.trace = pick_carried(m.traces);
    let mut replay_failed = m.replay_failed;
    replay_failed.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out.trace_replay_failed = replay_failed;
    out
}

/// A member's position in the fixed engine order (the outcome lists' order, and the pick among
/// several carried traces); an unknown name sorts last.
fn member_rank(member: &str) -> usize {
    [
        "exact", "native", "interp", "sat-bmc", "ic3", "btormc", "pono", "ric3", "ric3-bmc",
    ]
    .iter()
    .position(|m| *m == member)
    .unwrap_or(usize::MAX)
}

/// N5.4 — the carried trace an outcome ships: the first in the fixed engine order.
fn pick_carried(traces: CarriedTraces) -> Option<CarriedTrace> {
    traces
        .into_iter()
        .min_by_key(|(by, _, _)| member_rank(by))
        .map(|(by, trace, shortest)| CarriedTrace {
            by,
            trace,
            shortest,
        })
}

/// Decide `bad`-reachability of `file` across all six engines — the exact BDD
/// engine, the in-house native (BMC + k-induction) and interpolation engines, and
/// the btormc, Pono and rIC3 subprocess members — merged under the
/// differential-oracle discipline, with the subprocess members' default budget
/// ([`btormc::DEFAULT_TIMEOUT`]).
///
/// Each member abstains gracefully: the exact engine on an over-cap / free-init
/// design (`Err`), the in-house engines on `Unknown` / timeout / encode error, a
/// subprocess member when its binary is absent (`Err`), it is inconclusive
/// (`Unknown`), it hits its wall-clock timeout, or it is cancelled because another
/// member decided first. The emitted BTOR2 (from [`emit_btor2`]) is the shared
/// input, so a *reduced / transformed* model is decided consistently across all
/// members.
pub fn decide_reach_portfolio(file: &Btor2File) -> ReachOutcome {
    decide_reach_portfolio_parallel_with_timeout(file, btormc::DEFAULT_TIMEOUT)
}

/// btormc unrolling depth cap for the **raised-budget** path. The default portfolio caps
/// btormc at [`btormc::DEFAULT_KMAX`] (40) to bound its cost, but a deep counterexample
/// needs a deeper unrolling — `krebs.3`'s CEX is at depth 75 — so raising the *time* budget
/// is useless while the *depth* stays 40 (measured: `--timeout-ms 120000` alone still
/// returned `unknown` on `krebs.3`). When the caller raises the budget we therefore also
/// raise the depth cap; btormc unrolls incrementally, so it still stops at the first CEX /
/// proof / the wall timeout, never pre-paying for the higher cap.
const DEEP_BUDGET_KMAX: u32 = 1000;

/// [`decide_reach_portfolio`] with the **subprocess** members' (btormc / Pono)
/// wall budget set to `subprocess_timeout` instead of the 60 s default. When the budget is
/// raised above the default, btormc's depth cap is also lifted to [`DEEP_BUDGET_KMAX`] (a
/// longer budget is pointless if the unrolling stays capped at 40 — see that constant).
/// Measured: with both raised, `krebs.3`'s depth-75 CEX is found in ~73 s (`unknown` →
/// `violated`); `vis_arrays_buf_bug`'s is found in ~1 s either way. The in-process members
/// (exact / native / interp) keep their own budgets. Surfaced via `btor2 verify
/// --timeout-ms`.
///
/// Scoped threads borrow `file` directly — the scope guarantees the borrows end before
/// this returns, so no clone / `Arc` is needed. The merge ([`collect`]) runs on the joined
/// results in the fixed engine order, so the outcome is deterministic whatever the
/// members' finishing order.
pub fn decide_reach_portfolio_parallel_with_timeout(
    file: &Btor2File,
    subprocess_timeout: std::time::Duration,
) -> ReachOutcome {
    decide_reach_portfolio_cancellable(file, subprocess_timeout, None)
}

/// [`decide_reach_portfolio_parallel_with_timeout`] with an EXTERNAL cancellation flag: when the
/// caller sets `cancel`, every cancellable member bails exactly as it does when another member
/// decides (the subprocess members kill their child after the grace, native / interpolation
/// poll), and the outcome is whatever had been decided — usually `Unknown`. For a caller that
/// races this portfolio against something else and must not return while it still runs
/// (mununu#689: the justice parallel policy left it running on a detached thread). The exact
/// member has no cancellation point and runs to its own budget.
pub fn decide_reach_portfolio_cancellable(
    file: &Btor2File,
    subprocess_timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> ReachOutcome {
    decide_reach_portfolio_with_members(file, subprocess_timeout, cancel, ReachMembers::shipped())
}

/// N5.5 — where the owned IC3 member runs as a DECIDER by default, from `MUNUNU_REACH_IC3`: `0` in
/// no member set, `1` in every one (the N5.1 opt-in spelling), unset (or any other value) =
/// [`Ic3Default::Auto`]: under `--owned-only`, and in the full portfolio only on a host where no
/// rIC3 is invokable. The N5.4 trace-carrier search ([`ReachMembers::TRACE_CARRIERS`]) is not a
/// decider and keeps `ic3` whatever the setting, as it did before N5.5.
/// The gate measured both halves: beside an invokable rIC3 the member decided no row the
/// portfolio did not (all-136 arm C: 101 with it, 102 without) and costs a core; with no rIC3 it
/// is the only IC3 and carries the owned-only gain (52 / 68 against 35 / 68 on the M2 set, 0
/// disagreements). Private evidence: `experiments/competitiveness-2026-11/n55-default-on-gate/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ic3Default {
    Off,
    On,
    Auto,
}

pub(crate) fn ic3_default() -> Ic3Default {
    match crate::adapter::env_var::read("MUNUNU_REACH_IC3")
        .as_deref()
        .map(str::trim)
    {
        Some("0") => Ic3Default::Off,
        Some("1") => Ic3Default::On,
        _ => Ic3Default::Auto,
    }
}

/// Whether an rIC3 is invokable on this host, probed once per process with the probe the `ric3`
/// member itself runs ([`crate::adapter::ric3::locate_ric3`]); the member refuses when it fails.
fn ric3_invokable() -> bool {
    static PROBE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PROBE.get_or_init(|| crate::adapter::ric3::locate_ric3().is_ok())
}

/// Which members [`decide_reach_portfolio_with_members`] starts. [`ReachMembers::ALL`] is the
/// shipped portfolio; a caller that knows the query's shape may leave members out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachMembers {
    /// The exact ROBDD engine (in-process, on a detached thread; no cancellation point inside one
    /// BDD apply — mununu#816). Since mununu#996 the join leaves it behind once a trusted verdict
    /// is in (plus [`STRAGGLER_GRACE`]) or the run budget expires, so it no longer holds the
    /// portfolio past the first decider.
    pub exact: bool,
    /// Native BMC / k-induction (in-process z3).
    pub native: bool,
    /// Owned McMillan interpolation (in-process).
    pub interp: bool,
    /// N5.4 — the owned SAT BMC with the dynamic step (in-process CaDiCaL; finds `bad`
    /// reachable, never proves).
    pub sat_bmc: bool,
    /// N5.1 — the owned IC3 (in-process CaDiCaL): proves and refutes, each verdict checked before
    /// it ships. In [`ReachMembers::ALL`] (the roster) since its N5.5 gate; [`ReachMembers::shipped`]
    /// includes it per [`ic3_default`] (beside an invokable rIC3 only when forced on). Not in
    /// [`ReachMembers::PROOF_DIRECTED`] (the k-liveness rungs), which has its own measurement.
    pub ic3: bool,
    /// btormc (subprocess BMC — finds `bad` reachable, never proves).
    pub btormc: bool,
    /// Pono (subprocess, `ic3bits`).
    pub pono: bool,
    /// rIC3 (subprocess).
    pub ric3: bool,
    /// mununu#997 — rIC3's dynamic-step BMC (subprocess; finds `bad` reachable, never proves).
    pub ric3_bmc: bool,
    /// rIC3's `-e` engine; [`ric3::PORTFOLIO_ENGINE`] runs its own multi-configuration portfolio.
    pub ric3_engine: &'static str,
    /// N5.4 — carry the deciding member's counterexample out of the run
    /// ([`ReachOutcome::trace`]): `sat-bmc` and `ic3` replay their own trace, `btormc`, `ric3`
    /// and `ric3-bmc` run with their witness output on and the witness is replayed on the design.
    /// Off in every constant set (the replay is work the default verdict does not need); the
    /// `--witness` / `witness` callers turn it on.
    pub witness: bool,
}

impl ReachMembers {
    /// Every member — the shipped portfolio, byte-for-byte the behaviour of
    /// [`decide_reach_portfolio_cancellable`] before this type existed.
    pub const ALL: Self = Self {
        exact: true,
        native: true,
        interp: true,
        sat_bmc: true,
        ic3: true,
        btormc: true,
        pono: true,
        ric3: true,
        ric3_bmc: true,
        ric3_engine: ric3::DEFAULT_ENGINE,
        witness: false,
    };
    /// N5.4 — the members that can carry a replayed counterexample: the witness fallback's set
    /// ([`crate::adapter::btor2::witness::witness_for_violation`]), with `witness` on. No exact,
    /// native, interpolation or Pono: none carries a trace yet.
    pub const TRACE_CARRIERS: Self = Self {
        exact: false,
        native: false,
        interp: false,
        sat_bmc: true,
        ic3: true,
        btormc: true,
        pono: false,
        ric3: true,
        ric3_bmc: true,
        ric3_engine: ric3::DEFAULT_ENGINE,
        witness: true,
    };
    /// The proof-directed set for a `bad` that is a BOUND on a monitor (Track V's k-liveness
    /// rung): IC3 (rIC3 in its portfolio mode, Pono) and k-induction. No exact (bit-blasting the
    /// whole design for a question a small inductive invariant settles, and uninterruptible —
    /// measured: the full portfolio did not return in 400 s on a monitor rIC3 proved in 14 s),
    /// no btormc (a bound's `reachable` is not a verdict here, only "try a wider counter", and
    /// IC3 finds those traces as fast), no interpolation (its query budget is tuned for the
    /// safety rows).
    pub const PROOF_DIRECTED: Self = Self {
        exact: false,
        native: true,
        interp: false,
        sat_bmc: false,
        ic3: false,
        btormc: false,
        pono: true,
        ric3: true,
        ric3_bmc: false,
        ric3_engine: ric3::PORTFOLIO_ENGINE,
        witness: false,
    };

    /// [`Self::PROOF_DIRECTED`] with rIC3 as ONE `-e ic3` process instead of its own
    /// multi-configuration portfolio. For the k-liveness rung when it RACES the other justice
    /// rungs (`--parallel-rungs`). rIC3's portfolio mode forks about 11 solvers, and on a 4-CPU host
    /// they starved the l2s rung beside it. mununu#986 (16-row probe, 4 CPUs): with this, 4 of the
    /// 7 rows the race had lost came back, and all 5 k-liveness proofs held.
    pub const PROOF_DIRECTED_RACED: Self = Self {
        ric3_engine: ric3::DEFAULT_ENGINE,
        ..Self::PROOF_DIRECTED
    };

    /// Every shipped member that stops when cancelled: [`Self::shipped`] without the exact ROBDD
    /// member, which has no cancellation point (mununu#816). For a caller that RACES this
    /// portfolio and joins it once something else wins (the justice verb's `--parallel-rungs`):
    /// with the exact member in, a cancelled portfolio keeps running until that member's own
    /// budget runs out, and the race returns only then. mununu#986 measured 261 s for a verdict
    /// the exact rung had in 0.4 s. The owned IC3 follows the N5.5 default ([`ic3_default`]) here
    /// as in every shipped set; it stops on cancel.
    pub fn cancellable() -> Self {
        Self::shipped().without("exact")
    }

    /// [`ReachMembers::ALL`] with the owned IC3 member on or off.
    pub const fn with_ic3(self, on: bool) -> Self {
        Self { ic3: on, ..self }
    }

    /// The same members, carrying the deciding member's counterexample out ([`Self::witness`]).
    pub const fn with_witness(self, on: bool) -> Self {
        Self {
            witness: on,
            ..self
        }
    }

    /// Without `member` (by its outcome name); an unknown name changes nothing.
    pub fn without(self, member: &str) -> Self {
        match member {
            "exact" => Self {
                exact: false,
                ..self
            },
            "native" => Self {
                native: false,
                ..self
            },
            "interp" => Self {
                interp: false,
                ..self
            },
            "sat-bmc" => Self {
                sat_bmc: false,
                ..self
            },
            "ic3" => Self { ic3: false, ..self },
            "btormc" => Self {
                btormc: false,
                ..self
            },
            "pono" => Self {
                pono: false,
                ..self
            },
            "ric3" => Self {
                ric3: false,
                ..self
            },
            "ric3-bmc" => Self {
                ric3_bmc: false,
                ..self
            },
            _ => self,
        }
    }

    /// The outcome names of the members this set starts, in the fixed engine order.
    pub fn names(self) -> Vec<&'static str> {
        [
            ("exact", self.exact),
            ("native", self.native),
            ("interp", self.interp),
            ("sat-bmc", self.sat_bmc),
            ("ic3", self.ic3),
            ("btormc", self.btormc),
            ("pono", self.pono),
            ("ric3", self.ric3),
            ("ric3-bmc", self.ric3_bmc),
        ]
        .into_iter()
        .filter_map(|(n, on)| on.then_some(n))
        .collect()
    }

    /// What the shipped drivers start: [`ReachMembers::ALL`] with the owned IC3 decided by
    /// [`ic3_default`] and the rIC3 probe ([`ReachMembers::shipped_for`]).
    pub fn shipped() -> Self {
        Self::shipped_for(ic3_default(), ric3_invokable())
    }

    /// [`ReachMembers::shipped`] with its two inputs explicit — the seam its test pins: `Auto`
    /// includes the owned IC3 exactly when no rIC3 is invokable.
    pub(crate) fn shipped_for(setting: Ic3Default, ric3_invokable: bool) -> Self {
        let ic3 = match setting {
            Ic3Default::Off => false,
            Ic3Default::On => true,
            Ic3Default::Auto => !ric3_invokable,
        };
        Self::ALL.with_ic3(ic3)
    }
}

/// An owned SAT-backed member's runner ([`run_sat_bmc`], [`run_ic3`]): the design, the member's
/// wall deadline, the shared stop flag, and where to leave a replayed counterexample.
type OwnedRunner = fn(
    &Btor2File,
    std::time::Instant,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    Option<&Carried>,
) -> Result<Option<bool>, MemberStop>;

/// The native member's runner ([`run_native`]): the design, the shared stop flag, the per-check
/// allowance in nominal milliseconds, and the portfolio's wall (for the progress it reports).
/// mununu#1143 — the owned IC3's runner: it replays inside the engine, so it takes the carried
/// store whether or not a witness was asked for (the flag says whether to carry the trace).
type Ic3Runner = fn(
    &Btor2File,
    std::time::Instant,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    &Carried,
    bool,
) -> Result<Option<bool>, MemberStop>;

type NativeRunner = fn(
    &Btor2File,
    Option<&std::sync::atomic::AtomicBool>,
    u32,
    Option<std::time::Instant>,
) -> Result<Option<bool>, MemberStop>;

/// The btormc member's runner ([`run_btormc`]): as [`SubprocessRunner`], after the depth cap.
type BtormcRunner = fn(
    &Btor2File,
    u32,
    std::time::Duration,
    &std::sync::atomic::AtomicBool,
    Option<&Carried>,
) -> Result<Option<McVerdict>, String>;

/// The rIC3 member's runner ([`run_ric3`]): as [`SubprocessRunner`], after the `-e` engine.
type Ric3Runner = fn(
    &Btor2File,
    &'static str,
    std::time::Duration,
    &std::sync::atomic::AtomicBool,
    Option<&Carried>,
) -> Result<Option<McVerdict>, String>;

/// A subprocess member's runner that can carry a witness ([`run_ric3_bmc`]): the design, the
/// member's wall, the shared stop flag, and where to leave a replayed counterexample.
type SubprocessRunner = fn(
    &Btor2File,
    std::time::Duration,
    &std::sync::atomic::AtomicBool,
    Option<&Carried>,
) -> Result<Option<McVerdict>, String>;

/// What each member thread of a driver calls for its result: the full driver
/// ([`decide_with_members_using`]) passes [`Runners::SHIPPED`], the owned-only driver
/// ([`decide_owned_only_using`]) [`Runners::OWNED_ONLY`], and each calls the runners of the
/// members it starts. A test
/// replaces members with stubs whose finish order it scripts, so the verdict and every list of
/// the outcome are asserted on every host instead of on the order one host's scheduler produced
/// (`os-dependent-test-expectation` in `docs/defect-classes.md`). Settling a result into the
/// outcome's lists, raising the shared stop flag and the join stay in the driver, so a stub goes
/// through them exactly as an engine does. mununu#996 introduced the seam for the exact member
/// alone.
#[derive(Clone, Copy)]
struct Runners {
    exact: fn(&str) -> Result<Option<bool>, String>,
    native: NativeRunner,
    sat_bmc: OwnedRunner,
    ic3: Ic3Runner,
    interp_available: fn() -> Result<(), String>,
    interp_search:
        fn(&Btor2File, &std::sync::atomic::AtomicBool, MemberTimeouts) -> InterpSafetyVerdict,
    btormc: BtormcRunner,
    pono: fn(
        &Btor2File,
        std::time::Duration,
        &std::sync::atomic::AtomicBool,
    ) -> Result<Option<McVerdict>, String>,
    ric3: Ric3Runner,
    ric3_bmc: SubprocessRunner,
    /// The owned-only driver's deep counterexample search (`cex`): the depth it found `bad` at.
    #[cfg(not(feature = "boolector"))]
    cex: fn(
        &Btor2File,
        std::time::Instant,
        &std::sync::atomic::AtomicBool,
    ) -> Result<Option<u32>, MemberStop>,
}

impl Runners {
    /// The engines themselves.
    const SHIPPED: Self = Self {
        exact: run_exact,
        native: run_native,
        sat_bmc: run_sat_bmc,
        ic3: run_ic3,
        interp_available: native_interp::interp_available,
        interp_search,
        btormc: run_btormc,
        pono: run_pono,
        ric3: run_ric3,
        ric3_bmc: run_ric3_bmc,
        #[cfg(not(feature = "boolector"))]
        cex: run_owned_cex,
    };
    /// The owned-only driver's: the same engines, with its own interpolation schedule.
    const OWNED_ONLY: Self = Self {
        interp_search: owned_interp_search,
        ..Self::SHIPPED
    };
}

/// [`decide_reach_portfolio_cancellable`] restricted to `members`. A member left out is simply
/// not started — it neither refuses nor abstains, so the outcome's lists name only members that
/// ran. The merge, the budget handling and the cancellation are unchanged.
pub fn decide_reach_portfolio_with_members(
    file: &Btor2File,
    subprocess_timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    members: ReachMembers,
) -> ReachOutcome {
    decide_with_members_using(file, subprocess_timeout, cancel, members, &Runners::SHIPPED)
}

/// mununu#1064 — the stated reason a model with no `bad` line is not decided.
pub const NO_BAD_LINE_REASON: &str = "the model has no `bad` line: there is no obligation to \
     decide, so no member ran (a proof of an empty obligation set is not `holds`)";

/// mununu#1064 — a model with no `bad` line carries no obligation. A proof engine handed one
/// answers "unreachable" (rIC3 and k-induction both do), which read as `holds` on a design whose
/// only assertion the lift had dropped. Every portfolio driver calls this first and returns the
/// abstention instead: `Unknown`, with the reason under the pseudo-member `portfolio` in
/// [`ReachOutcome::refused`].
fn no_obligation(file: &Btor2File) -> Option<ReachOutcome> {
    if file.bads().next().is_some() {
        return None;
    }
    Some(ReachOutcome {
        refused: vec![("portfolio", NO_BAD_LINE_REASON.to_string())],
        ..ReachOutcome::default()
    })
}

/// [`decide_reach_portfolio_with_members`] with the members' runners as a parameter: the shipped
/// driver passes [`Runners::SHIPPED`]; a test passes stubs, for instance an exact runner that
/// does not return, which is what one long OxiDD apply looks like from the join (mununu#996).
fn decide_with_members_using(
    file: &Btor2File,
    subprocess_timeout: std::time::Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    members: ReachMembers,
    runners: &Runners,
) -> ReachOutcome {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let runners = *runners;
    if let Some(outcome) = no_obligation(file) {
        return outcome;
    }
    // mununu#726 — read the thread-local budget ONCE: an already-expired budget decides nothing
    // (deterministic: no member is spawned), and the members' bounds follow the time left. The
    // scoped threads re-enter the same budget — a spawned thread inherits none (`run_budget`'s
    // documented footgun), so without this the members would run unbounded.
    let budget = crate::adapter::run_budget::current();
    if budget.expired() {
        return ReachOutcome::default();
    }
    let t = portfolio_member_timeouts(budget.remaining_ms(), subprocess_timeout);
    let subprocess_timeout = t.subprocess;
    let content = emit_btor2(file);
    // Raising the wall budget signals "reach deeper" — so lift btormc's depth cap too.
    let btormc_kmax = if subprocess_timeout > btormc::DEFAULT_TIMEOUT {
        DEEP_BUDGET_KMAX
    } else {
        btormc::DEFAULT_KMAX
    };
    // A TRUSTED definite verdict from any member sets this. The owned interpolation
    // member polls it and abandons its (possibly slow) cvc5 interpolation search early
    // (#1 — run interpolation as a concurrent member, not last-resort, so its unique
    // decides get owned credit, without its pathological query dominating wall-clock on
    // instances another engine already decided), and since S1 (engine-performance
    // roadmap, category 4) the CHILD members — btormc, Pono and rIC3 — kill
    // their subprocess after a short grace instead of running out their
    // 60 s / 15 s budgets: the portfolio's wall is the first decider plus the grace, not
    // the slowest member's timeout. A member still running when the flag is set is not
    // cross-checked, exactly as in the owned-only driver; every member is individually
    // sound, so the decision itself is.
    let decided_flag = std::sync::Arc::new(AtomicBool::new(false));
    let decided: &AtomicBool = &decided_flag;
    let finished = AtomicBool::new(false);
    let finished = &finished;
    let budget = &budget;
    // mununu#734 — every member's refusal lands here, in finishing order; `collect` carries them.
    let refused: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());
    let refused = &refused;
    // mununu#893 — and every budget abstention with its cause, in its own list.
    let abstained: std::sync::Mutex<Vec<(&'static str, String)>> =
        std::sync::Mutex::new(Vec::new());
    let abstained = &abstained;
    // mununu#1022 — and every member that searched to its bound, with its progress.
    let undecided: std::sync::Mutex<Vec<(&'static str, String)>> =
        std::sync::Mutex::new(Vec::new());
    let undecided = &undecided;
    // N5.4 — the carried counterexamples, with a witness asked for; `None` leaves every member
    // on its default path.
    let carried_store = Carried::default();
    let carried: Option<&Carried> = members.witness.then_some(&carried_store);
    // mununu#1143 — the owned IC3 replays inside the engine; it records a replay failure in the
    // store whether or not a witness was asked for.
    let ic3_store: &Carried = &carried_store;
    let witness = members.witness;
    // mununu#996 — the exact member runs on a DETACHED thread and reports over a channel, so the
    // join can leave it behind. It has no cancellation point inside one OxiDD apply (#816 polls
    // the budget only between stages), and when it ran on the calling thread it held the
    // portfolio past every other member: measured on HWMCC'20 `krebs.3.prop1-func-interl`,
    // btormc was VIOLATED at 157 s and the run had not returned at 420 s. The join now waits for
    // it only until a trusted verdict is in (plus the straggler grace), or the run budget
    // expires; an abandoned exact thread keeps running in the background until its next budget
    // poll or its end, as the owned-only driver's detached members already do. A member still
    // running when the join returns is not cross-checked — the same trade as S1's cancelled
    // subprocess members; every member is individually sound, so the decision itself is.
    let exact_rx = members.exact.then(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let (decided, budget) = (std::sync::Arc::clone(&decided_flag), budget.clone());
        std::thread::spawn(move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let r = (runners.exact)(&content);
            if matches!(r, Ok(Some(_))) {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(r);
        });
        rx
    });
    // mununu#1039 — the portfolio's own wall: the members' budget plus the straggler grace (so a
    // subprocess member reports its own timeout first). Until #1039 nothing enforced it when no
    // run budget was set (`btor2 verify --timeout-ms`): the native member's per-check allowance is
    // a z3 rlimit, deterministic but not a wall, and on a loaded host one check ran 70 s past the
    // wall, or past an outer `timeout`, which then killed the run before any JSON was written.
    let wall_deadline = std::time::Instant::now() + subprocess_timeout + STRAGGLER_GRACE;
    let wall_fired = AtomicBool::new(false);
    let wall_fired = &wall_fired;
    // mununu#1043 — set beside `decided` when the wall or the run budget fires, so a member that
    // stops on the flag can tell a clock stop (a budget abstention) from a peer's verdict.
    let clock_stop = AtomicBool::new(false);
    let clock_stop = &clock_stop;
    let started = members;
    let mut members = std::thread::scope(|scope| {
        // Forward an external cancellation, the budget's expiry, or the portfolio's wall into
        // `decided`, which every cancellable member already polls (native's watchdog interrupts
        // the z3 check in flight, mununu#878). Stops when the members are joined.
        scope.spawn(move || {
            while !finished.load(Relaxed) {
                // A peer's verdict already raised `decided`: every member is winding down on it,
                // and a wall or budget firing during that wind-down must not relabel their stops
                // as clock stops (mununu#1067 review).
                if decided.load(Relaxed) {
                    break;
                }
                // An external cancellation is a caller's decision (another rung won): no clock.
                if cancel.is_some_and(|c| c.load(Relaxed)) {
                    decided.store(true, Relaxed);
                    break;
                }
                // `clock_stop` goes up before `decided`, but both are `Relaxed`: a member may stop
                // on `decided` and still read `clock_stop` clear. It then records nothing itself,
                // and the post-join sweep below names it with the clock's cause, so the stop is
                // never silent; only the member's own progress is lost in that window.
                if budget.expired() {
                    clock_stop.store(true, Relaxed);
                    decided.store(true, Relaxed);
                    break;
                }
                if std::time::Instant::now() >= wall_deadline {
                    wall_fired.store(true, Relaxed);
                    clock_stop.store(true, Relaxed);
                    decided.store(true, Relaxed);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
        let btormc_h = members.btormc.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let v = (runners.btormc)(file, btormc_kmax, subprocess_timeout, decided, carried);
                let v = settle_subprocess(
                    refused,
                    abstained,
                    decided,
                    clock_stop,
                    "btormc",
                    subprocess_timeout,
                    v,
                );
                if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        let pono_h = members.pono.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let v = (runners.pono)(file, subprocess_timeout, decided);
                let v = settle_subprocess(
                    refused,
                    abstained,
                    decided,
                    clock_stop,
                    "pono",
                    subprocess_timeout,
                    v,
                );
                if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // Plan 4 / PR D2 (mununu#736) — rIC3's bit-level IC3: the member that proves the
        // inductive-invariant safety class (OpenTitan's duplicated-counter fault-flag chains, which
        // k-induction cannot close from a free step state).
        // Trusted like btormc / pono: it reads the same emitted BTOR2 and answers with a certificate-
        // capable verdict; the HWMCC adjudication ran clean before it was promoted.
        let ric3_h = members.ric3.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let v = (runners.ric3)(
                    file,
                    members.ric3_engine,
                    subprocess_timeout,
                    decided,
                    carried,
                );
                let v = settle_subprocess(
                    refused,
                    abstained,
                    decided,
                    clock_stop,
                    "ric3",
                    subprocess_timeout,
                    v,
                );
                if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // mununu#997 — rIC3's dynamic-step BMC: one deep window query instead of every depth in
        // turn, the only member that reaches HWMCC'20 `mul9`'s depth-238 counterexample. It only
        // ever contributes `Violated` (`decide_via_ric3_bmc_dyn_step` maps an UNSAT to Unknown).
        let ric3_bmc_h = members.ric3_bmc.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let v = (runners.ric3_bmc)(file, subprocess_timeout, decided, carried);
                let v = settle_subprocess(
                    refused,
                    abstained,
                    decided,
                    clock_stop,
                    "ric3-bmc",
                    subprocess_timeout,
                    v,
                );
                if matches!(v, Some(McVerdict::Violated)) {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // N5.4 — the owned SAT BMC, the in-process counterpart of `ric3-bmc`: one deep window
        // query at a time under a doubling conflict budget, walled by the subprocess budget and
        // cancelled by `decided`. It only ever contributes `Violated`.
        let sat_bmc_h = members.sat_bmc.then(|| {
            let cancel = std::sync::Arc::clone(&decided_flag);
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let deadline = std::time::Instant::now() + subprocess_timeout;
                let v = settle_native(
                    refused,
                    abstained,
                    undecided,
                    clock_stop,
                    "sat-bmc",
                    (runners.sat_bmc)(file, deadline, cancel, carried),
                );
                if v == Some(true) {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // N5.1 — the owned IC3: proves and refutes, under the subprocess wall, cancelled by
        // `decided`. Its `Safe` is an invariant re-checked in a fresh solver and its `Violated` a
        // trace replayed on the concrete simulator before either reaches this join.
        let ic3_h = members.ic3.then(|| {
            let cancel = std::sync::Arc::clone(&decided_flag);
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let deadline = std::time::Instant::now() + subprocess_timeout;
                let v = settle_native(
                    refused,
                    abstained,
                    undecided,
                    clock_stop,
                    "ic3",
                    (runners.ic3)(file, deadline, cancel, ic3_store, witness),
                );
                if v.is_some() {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // The native engine is in-process Z3 work — on its own
        // thread so it overlaps the exact engine + the subprocess members rather than
        // serialising.
        let native_h = members.native.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                let v = settle_native(
                    refused,
                    abstained,
                    undecided,
                    clock_stop,
                    "native",
                    (runners.native)(file, Some(decided), t.native_query_ms, Some(wall_deadline)),
                );
                if v.is_some() {
                    decided.store(true, Relaxed);
                }
                v
            })
        });
        // Owned McMillan interpolation — now a FIRST-CLASS concurrent member (was
        // last-resort). Polls `decided` and bails out early once a faster member decides.
        let interp_h = members.interp.then(|| {
            scope.spawn(move || {
                let _g = crate::adapter::run_budget::enter(budget);
                timed("interp", || {
                    interp_member(
                        refused,
                        abstained,
                        undecided,
                        decided,
                        clock_stop,
                        (runners.interp_available)(),
                        || (runners.interp_search)(file, decided, t),
                    )
                })
            })
        });
        // The exact member reports on its channel; wait for it no longer than the others need.
        let exact = match exact_rx.map(|rx| await_straggler(&rx, decided, budget, STRAGGLER_GRACE))
        {
            None => None,
            Some(StragglerWait::Finished(r)) => settle(refused, "exact", r),
            Some(StragglerWait::Abandoned(Abandon::OtherDecided)) => None,
            Some(StragglerWait::Abandoned(Abandon::BudgetExpired)) => {
                note_refusal(
                    abstained,
                    "exact",
                    "abandoned at the run budget's deadline: one BDD operation was still running \
                     (a single OxiDD apply does not poll the budget, mununu#996)"
                        .to_string(),
                );
                None
            }
            Some(StragglerWait::Abandoned(Abandon::Lost)) => {
                note_refusal(refused, "exact", panicked_reason("exact"));
                None
            }
        };
        // A panicked member thread abstains (None) rather than poisoning the merge, and is
        // named in `refused` as the exact member is (mununu#1143); a member not started is None.
        let native = native_h.and_then(|h| settle_join(refused, "native", h.join()));
        let btormc = btormc_h.and_then(|h| settle_join(refused, "btormc", h.join()));
        let pono = pono_h.and_then(|h| settle_join(refused, "pono", h.join()));
        let ric3 = ric3_h.and_then(|h| settle_join(refused, "ric3", h.join()));
        let ric3_bmc = ric3_bmc_h.and_then(|h| settle_join(refused, "ric3-bmc", h.join()));
        let interp = interp_h.and_then(|h| settle_join(refused, "interp", h.join()));
        let sat_bmc = sat_bmc_h.and_then(|h| settle_join(refused, "sat-bmc", h.join()));
        let ic3 = ic3_h.and_then(|h| settle_join(refused, "ic3", h.join()));
        finished.store(true, Relaxed);
        Members {
            exact,
            native,
            interp,
            sat_bmc,
            ic3,
            btormc,
            pono,
            ric3,
            ric3_bmc,
            refused: Vec::new(),
            abstained: Vec::new(),
            undecided: Vec::new(),
            traces: Vec::new(),
            replay_failed: Vec::new(),
        }
    });
    // A poisoned lock (a member panicked while recording) yields no refusal list; the panic is
    // the louder signal and the verdict never depends on the list (mununu#803, LOW).
    members.refused = refused.lock().map(|g| g.clone()).unwrap_or_default();
    members.abstained = abstained.lock().map(|g| g.clone()).unwrap_or_default();
    members.undecided = undecided.lock().map(|g| g.clone()).unwrap_or_default();
    (members.traces, members.replay_failed) = carried_store.take();
    // mununu#1039 — silence is a defect: a member the wall stopped, with no verdict and no stop
    // of its own on record, is a budget abstention with that cause.
    // mununu#1043 — and so is one the run budget stopped (the same sweep; until #1043 a budget
    // stop recorded nothing for a member that had not noted its own cause).
    // A definite verdict means a peer stopped the rest, which stays in no list, even when the
    // clock rose in the same instant (the watchdog's check races the peer's store).
    if clock_stop.load(Relaxed) && !any_definite(&members) {
        let cause = clock_sweep_cause(wall_fired.load(Relaxed), subprocess_timeout.as_millis());
        for name in stopped_by_wall_unrecorded(&started, &members) {
            members.abstained.push((name, cause.clone()));
        }
    }
    collect(members)
}

/// mununu#1039 / #1043 — the cause the post-join sweep records: the wall's, or the run budget's.
fn clock_sweep_cause(wall_fired: bool, wall_ms: u128) -> String {
    if wall_fired {
        format!(
            "stopped at the portfolio's wall deadline ({wall_ms} ms: `--timeout-ms` on the `btor2` \
             verbs, the run budget on `sv verify-auto`) before it reached a verdict"
        )
    } else {
        "stopped when the run budget expired, before it reached a verdict".to_string()
    }
}

/// Whether any member reached a definite verdict (a reachable or unreachable answer).
fn any_definite(m: &Members) -> bool {
    let definite = |v: Option<McVerdict>| matches!(v, Some(McVerdict::Violated | McVerdict::Safe));
    [m.exact, m.native, m.interp, m.sat_bmc, m.ic3]
        .iter()
        .any(Option::is_some)
        || [m.btormc, m.pono, m.ric3, m.ric3_bmc]
            .into_iter()
            .any(definite)
}

/// mununu#1039 — the members that were started, returned no verdict, and appear in none of the
/// outcome's lists: the ones the portfolio's wall (or, since #1043, its run budget) stopped
/// without a recorded cause.
fn stopped_by_wall_unrecorded(started: &ReachMembers, m: &Members) -> Vec<&'static str> {
    // A subprocess member's `Some(Unknown)` is no verdict. `settle_subprocess` records its timeout
    // note on a clock stop (#1043), so the sweep only names one that settled before it saw
    // `clock_stop`.
    let definite = |v: Option<McVerdict>| matches!(v, Some(McVerdict::Violated | McVerdict::Safe));
    let ran: [(&'static str, bool, bool); 9] = [
        ("exact", started.exact, m.exact.is_some()),
        ("native", started.native, m.native.is_some()),
        ("interp", started.interp, m.interp.is_some()),
        ("sat-bmc", started.sat_bmc, m.sat_bmc.is_some()),
        ("ic3", started.ic3, m.ic3.is_some()),
        ("btormc", started.btormc, definite(m.btormc)),
        ("pono", started.pono, definite(m.pono)),
        ("ric3", started.ric3, definite(m.ric3)),
        ("ric3-bmc", started.ric3_bmc, definite(m.ric3_bmc)),
    ];
    let recorded = |name: &str| {
        m.refused
            .iter()
            .chain(&m.abstained)
            .chain(&m.undecided)
            .any(|(n, _)| *n == name)
    };
    ran.into_iter()
        .filter(|&(name, on, has_verdict)| on && !has_verdict && !recorded(name))
        .map(|(name, ..)| name)
        .collect()
}

/// How long the join waits for a member that has not reported once a trusted verdict is in:
/// long enough for a co-completing member to land (and a contradiction to surface), never the
/// straggler's own budget. Shared by both drivers.
const STRAGGLER_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Why the join stopped waiting for a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandon {
    /// Another member's trusted verdict (or the caller's cancellation) was in and the grace ran
    /// out — the member is simply not needed, as a cancelled subprocess member is not.
    OtherDecided,
    /// The run budget expired with the member still running: a budget abstention (mununu#893).
    BudgetExpired,
    /// The member's thread ended without sending (it panicked).
    Lost,
}

#[derive(Debug)]
enum StragglerWait<T> {
    Finished(T),
    Abandoned(Abandon),
}

/// mununu#996 — wait for one member's result on `rx`, but only until `decided` is set (by a
/// trusted verdict, the budget watcher, or the caller's cancellation) and `grace` has passed
/// since, or the member's thread is gone. The pure seam of the join: the shipped driver and the
/// tests drive the same function.
fn await_straggler<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    decided: &std::sync::atomic::AtomicBool,
    budget: &crate::adapter::run_budget::Budget,
    grace: std::time::Duration,
) -> StragglerWait<T> {
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};
    const POLL: Duration = Duration::from_millis(10);
    let mut grace_end: Option<(Instant, Abandon)> = None;
    loop {
        let wait = grace_end.map_or(POLL, |(end, _)| {
            end.saturating_duration_since(Instant::now()).min(POLL)
        });
        match rx.recv_timeout(wait) {
            Ok(v) => return StragglerWait::Finished(v),
            Err(RecvTimeoutError::Disconnected) => return StragglerWait::Abandoned(Abandon::Lost),
            Err(RecvTimeoutError::Timeout) => {}
        }
        match grace_end {
            Some((end, why)) if Instant::now() >= end => return StragglerWait::Abandoned(why),
            Some(_) => {}
            None if decided.load(Relaxed) => {
                let why = if budget.expired() {
                    Abandon::BudgetExpired
                } else {
                    Abandon::OtherDecided
                };
                grace_end = Some((Instant::now() + grace, why));
            }
            None => {}
        }
    }
}

/// Depth cap for the owned-standalone deep counterexample search — well above
/// [`native_bmc::DEFAULT_MAX_K`] so a *deep* violation is caught, while the wall
/// deadline + per-query timeout keep the (eagerly-built) unrolling bounded.
const OWNED_DEEP_CEX_MAX_K: u32 = 128;
/// Per-z3-check timeout inside the owned deep CEX search. Gated to match its only
/// use site: with the `boolector` feature on, the deep-CEX direction runs through
/// the native Boolector BMC instead, so an ungated const is dead code and fails the
/// documented pre-push check (`clippy --workspace --all-features -D warnings`).
/// `make ci` does not pass `--all-features`, which is why CI stayed green.
#[cfg(not(feature = "boolector"))]
const OWNED_CEX_QUERY_MS: u32 = 15_000;

/// The owned-only driver's interpolation search: the full driver's ([`interp_search`]) with a
/// longer iteration schedule; the driver passes the default query bound and its own wall.
fn owned_interp_search(
    file: &Btor2File,
    cancel: &std::sync::atomic::AtomicBool,
    t: MemberTimeouts,
) -> InterpSafetyVerdict {
    native_interp::verify_safety_interp_cancellable(
        file,
        INTERP_MAX_SUFFIX,
        64,
        t.interp_query_ms,
        t.interp_overall_ms,
        cancel,
    )
}

/// The owned-only driver's deep counterexample search (z3 BMC to [`OWNED_DEEP_CEX_MAX_K`]),
/// wall-bounded by `deadline`: the depth of the counterexample, `None` when there is none in
/// reach.
#[cfg(not(feature = "boolector"))]
fn run_owned_cex(
    file: &Btor2File,
    deadline: std::time::Instant,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Option<u32>, MemberStop> {
    native_bmc::bmc_cex_until(
        file,
        OWNED_DEEP_CEX_MAX_K,
        OWNED_CEX_QUERY_MS,
        deadline,
        cancel,
    )
    .map_err(MemberStop::from)
}

/// The **owned-standalone** driver: decide `bad`-reachability with ONLY the
/// mununu-owned engines, run concurrently under a shared `timeout_ms` wall budget,
/// deliberately EXCLUDING the external algorithms (btormc, Pono, rIC3). It
/// answers "what can mununu's *own* algorithms decide on their own budget?" — the
/// ceiling that matters for a no-subprocess deployment and the soundness cross-check.
///
/// Two complementary directions run in parallel (first-definite wins; each sets a
/// shared `decided` flag the others poll and bail on):
/// - **safety proof (the formula):** the exact BDD engine (≤ 40 bits), native
///   k-induction, and native McMillan interpolation;
/// - **counterstrategy (the negation):** a dedicated deep, wall-bounded pure BMC
///   counterexample search ([`native_bmc::bmc_cex_until`], depth ≤
///   [`OWNED_DEEP_CEX_MAX_K`]). Finding a concrete counterexample is sound and usually
///   far cheaper than an equivalent safety proof, so it flips many designs the
///   proof side leaves `Unknown` straight to `Violated`. It is attributed to the
///   `"cex"` engine so a native-safe vs cex-violated split still raises the
///   [`ReachVerdict::Contradiction`] alarm rather than being silently merged.
pub fn decide_reach_owned_only(file: &Btor2File, timeout_ms: u32) -> ReachOutcome {
    decide_reach_owned_only_with_witness(file, timeout_ms, false)
}

/// [`decide_reach_owned_only`] with the owned trace carriers (`sat-bmc`, and `ic3` when opted
/// in) carrying their replayed counterexample out when `witness` ([`ReachOutcome::trace`],
/// N5.4). The `cex` / `boolector` deep search and the exact member carry none.
pub fn decide_reach_owned_only_with_witness(
    file: &Btor2File,
    timeout_ms: u32,
    witness: bool,
) -> ReachOutcome {
    decide_owned_only_using(file, timeout_ms, witness, &Runners::OWNED_ONLY)
}

/// [`decide_reach_owned_only_with_witness`] with the members' runners as a parameter (as
/// [`decide_with_members_using`]): the shipped driver passes [`Runners::OWNED_ONLY`], a test
/// passes stubs.
fn decide_owned_only_using(
    file: &Btor2File,
    timeout_ms: u32,
    witness: bool,
    runners: &Runners,
) -> ReachOutcome {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::{Arc, mpsc};
    let runners = *runners;
    if let Some(outcome) = no_obligation(file) {
        return outcome;
    }
    // N5.4 — where the carrying members leave their trace; read at return.
    let carried_store: Arc<Carried> = Arc::default();
    // mununu#803 — a member's refusal (an `Err`, not an abstention) is kept here as the scoped
    // driver keeps it; the detached threads share the list through an `Arc`.
    let refused: Arc<std::sync::Mutex<Vec<(&'static str, String)>>> = Arc::default();
    // mununu#893 — the budget abstentions, with their cause, beside the refusals.
    let abstained: Arc<std::sync::Mutex<Vec<(&'static str, String)>>> = Arc::default();
    // mununu#1022 — the members that searched to their bound, with their progress.
    let undecided: Arc<std::sync::Mutex<Vec<(&'static str, String)>>> = Arc::default();
    use std::time::{Duration, Instant};

    // mununu#816 — the caller's run budget is a THREAD-LOCAL (`run_budget`'s documented
    // footgun): a detached thread inherits none, so every owned member below ran unbounded by
    // the budget the caller installed — the planner's per-property budget, a rescue slice, the
    // HWMCC harness's per-instance budget — and an already-expired budget still spawned four
    // members. Read it once, decline when it has already expired (what the scoped driver does),
    // clamp the collector's wall to the time left, and re-enter it on every member thread.
    let budget = crate::adapter::run_budget::current();
    if budget.expired() {
        return ReachOutcome::default();
    }
    let timeout_ms = budget.remaining_ms().map_or(timeout_ms, |left| {
        timeout_ms.min(u32::try_from(left).unwrap_or(u32::MAX))
    });

    // Detached threads + a result channel (NOT `thread::scope`) so the collector can
    // RETURN the moment a definite verdict arrives, without joining the slow members.
    // The exact BDD engine in particular is synchronous and NON-interruptible: on a wide
    // in-cap datapath it can grind for minutes, and running it synchronously (as before)
    // blocked the whole owned verdict long past the budget even when Boolector had already
    // decided in seconds. Spawning it and reading results off a channel bounds the wall
    // time by the FIRST decider (or the deadline), not the slowest member. Detached
    // members keep running in the background; a CLI reaps them on process exit, and no
    // caller is ever blocked on them.
    //
    // Early-return means a member still running when we return is not cross-checked, so the
    // inter-engine `Contradiction` alarm here only covers members that finished by return
    // time. Since S1 the full portfolio makes the same trade (its cancellable members stop
    // after the grace); every member is individually sound, so the first definite verdict
    // is itself sound.
    let file = Arc::new(file.clone());
    let content = Arc::new(emit_btor2(&file));
    let decided = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let (tx, rx) = mpsc::channel::<(&'static str, OwnedReport)>();
    // The members this run starts, each sending its name once ([`owned_members`]); a member
    // whose thread panics sends `Died` instead (mununu#1143).
    let members = owned_members();
    // The members' bounds here: this driver's wall is native's per-check allowance and the
    // interpolation search's overall bound; interpolation keeps its default query bound.
    let t = MemberTimeouts {
        native_query_ms: timeout_ms,
        interp_query_ms: INTERP_QUERY_TIMEOUT_MS,
        interp_overall_ms: u64::from(timeout_ms),
        subprocess: Duration::from_millis(u64::from(timeout_ms)),
    };

    // Safety proof — shallow native BMC + k-induction.
    {
        let (file, decided, tx, refused, abstained, undecided) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
            Arc::clone(&abstained),
            Arc::clone(&undecided),
        );
        let budget = budget.clone();
        spawn_owned("native", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            // mununu#893 — `timeout_ms` is the member's NOMINAL per-check allowance (a resource
            // limit inside); a budget stop is an abstention with its cause, an encode error a
            // refusal, a search to the bound `undecided` with the wall it left; a stop on a
            // peer's verdict is in no list (this driver has no clock stop).
            let v = settle_native(
                &refused,
                &abstained,
                &undecided,
                &NO_CLOCK_STOP,
                "native",
                (runners.native)(&file, Some(&decided), t.native_query_ms, Some(deadline)),
            );
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("native", OwnedReport::Verdict(v)));
        });
    }
    // Safety proof — McMillan interpolation.
    {
        let (file, decided, tx, refused, abstained, undecided) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
            Arc::clone(&abstained),
            Arc::clone(&undecided),
        );
        let budget = budget.clone();
        spawn_owned("interp", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let v = timed("interp", || {
                interp_member(
                    &refused,
                    &abstained,
                    &undecided,
                    &decided,
                    &NO_CLOCK_STOP,
                    (runners.interp_available)(),
                    || (runners.interp_search)(&file, &decided, t),
                )
            });
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("interp", OwnedReport::Verdict(v)));
        });
    }
    // Deep BV counterexample search. With the `boolector` feature the in-process Boolector
    // BMC fills this role (faster — measured ~56 s vs the z3 search timing out on
    // `krebs.3`); without it, the z3 deep-CEX search. Only ever contributes `Violated`.
    #[cfg(feature = "boolector")]
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        let budget = budget.clone();
        spawn_owned("boolector", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let v = timed("boolector", || {
                crate::adapter::btor2::native_boolector::decide_reachable_boolector(
                    &file,
                    OWNED_DEEP_CEX_MAX_K,
                    deadline,
                    &decided,
                )
            });
            if v == Some(true) {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("boolector", OwnedReport::Verdict(v)));
        });
    }
    #[cfg(not(feature = "boolector"))]
    {
        let (file, decided, tx, refused, abstained, undecided) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
            Arc::clone(&abstained),
            Arc::clone(&undecided),
        );
        let budget = budget.clone();
        spawn_owned("cex", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            // mununu#955 — a budget stop (the deadline, the resource limit) is an abstention with
            // its cause, an encoder refusal a refusal, as for the native member above.
            let hit = timed("cex", || {
                let r = (runners.cex)(&file, deadline, &decided);
                settle_native(&refused, &abstained, &undecided, &NO_CLOCK_STOP, "cex", r).is_some()
            });
            if hit {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("cex", OwnedReport::Verdict(if hit { Some(true) } else { None })));
        });
    }
    // N5.4 — the owned SAT BMC with the dynamic step, beside the z3 / Boolector deep-CEX member:
    // it reaches the deep counterexamples whose shallow frames stall a per-depth search
    // (HWMCC'20 `mul9`, `krebs.3`, `circular_pointer_w32_d32`, none decided by the other owned
    // members at 300 s, item 6 F0). Only ever contributes `Violated`.
    {
        let (file, decided, tx, refused, abstained, undecided) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
            Arc::clone(&abstained),
            Arc::clone(&undecided),
        );
        let budget = budget.clone();
        let carried = witness.then(|| Arc::clone(&carried_store));
        spawn_owned("sat-bmc", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let r = (runners.sat_bmc)(&file, deadline, Arc::clone(&decided), carried.as_deref());
            let v = settle_native(
                &refused,
                &abstained,
                &undecided,
                &NO_CLOCK_STOP,
                "sat-bmc",
                r,
            );
            if v == Some(true) {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("sat-bmc", OwnedReport::Verdict(v)));
        });
    }
    // N5.1 — the owned IC3 (started per [`ic3_default`] since N5.5): the first owned member that
    // PROVES. Its `Safe` is an invariant re-checked in a fresh solver, its `Violated`
    // a trace replayed on the concrete simulator; either failure abstains with the cause.
    if members.contains(&"ic3") {
        let (file, decided, tx, refused, abstained, undecided) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
            Arc::clone(&abstained),
            Arc::clone(&undecided),
        );
        let budget = budget.clone();
        let store = Arc::clone(&carried_store);
        spawn_owned("ic3", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let r = (runners.ic3)(&file, deadline, Arc::clone(&decided), &store, witness);
            let v = settle_native(&refused, &abstained, &undecided, &NO_CLOCK_STOP, "ic3", r);
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("ic3", OwnedReport::Verdict(v)));
        });
    }
    // Exact BDD — the non-interruptible member; spawned so it can never block the return.
    {
        let (content, decided, tx, refused) = (
            Arc::clone(&content),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
        );
        let budget = budget.clone();
        spawn_owned("exact", tx.clone(), move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            // mununu#873 — a refusal (over-cap cone, free init) is an abstention AND a recorded
            // reason, as in the scoped driver; it used to be dropped here.
            let v = settle(&refused, "exact", (runners.exact)(&content));
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("exact", OwnedReport::Verdict(v)));
        });
    }
    drop(tx); // the collector's `rx` disconnects once every member thread has finished.

    let OwnedReports {
        reachable_by,
        unreachable_by,
        reported,
        dead,
        disconnected,
    } = collect_owned(&rx, members.len(), deadline, STRAGGLER_GRACE);
    // mununu#1143 — a member whose thread panicked said so on the channel (`spawn_owned`), while
    // the others may still be running; before this the collector named it "still running at
    // the owned-only wall deadline", or nothing when a verdict was in.
    for m in &dead {
        note_refusal(&refused, m, panicked_reason(m));
    }
    let heard = reported.len() + dead.len();
    if disconnected && heard < members.len() {
        // The backstop: every sender is gone and a member sent neither a verdict nor its death
        // (a thread that ended without unwinding).
        for m in members
            .iter()
            .copied()
            .filter(|m| !reported.contains(m) && !dead.contains(m))
        {
            note_refusal(&refused, m, panicked_reason(m));
        }
    } else if reachable_by.is_empty() && unreachable_by.is_empty() && heard < members.len() {
        let accounted: Vec<&'static str> = reported.iter().chain(dead.iter()).copied().collect();
        for (m, cause) in unreported_at_deadline(&members, &accounted, timeout_ms) {
            note_refusal(&abstained, m, cause);
        }
    }
    let mut out = ReachOutcome::from_sets(reachable_by, unreachable_by);
    out.refused = refused.lock().map(|g| g.clone()).unwrap_or_default();
    out.abstained = abstained.lock().map(|g| g.clone()).unwrap_or_default();
    out.undecided = undecided.lock().map(|g| g.clone()).unwrap_or_default();
    // N5.4 — what the carrying members had left by now (a member still running carries nothing
    // into this outcome, as it is not cross-checked either).
    let (traces, mut replay_failed) = carried_store.take();
    out.trace = pick_carried(traces);
    replay_failed.sort_by_key(|(a, _)| member_rank(a));
    out.trace_replay_failed = replay_failed;
    out
}

/// mununu#1143 — what an owned-only member thread sends the collector: its verdict, or that
/// it died (the thread panicked) — sent by [`spawn_owned`] on the unwind, so a dead member is
/// reported while every other member is still alive, not inferred from a disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnedReport {
    Verdict(Option<bool>),
    Died,
}

/// mununu#1143 — an owned-only member thread: the body sends its own verdict; a panic inside
/// it sends `Died` for the name instead of leaving the collector to guess from a disconnect.
fn spawn_owned(
    name: &'static str,
    tx: std::sync::mpsc::Sender<(&'static str, OwnedReport)>,
    body: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
            let _ = tx.send((name, OwnedReport::Died));
        }
    });
}

/// mununu#1143 — what the owned-only collector gathered from the member channel.
struct OwnedReports {
    reachable_by: Vec<&'static str>,
    unreachable_by: Vec<&'static str>,
    /// The members that sent, in arrival order (a dead member is in `dead`, not here).
    reported: Vec<&'static str>,
    /// The members whose thread panicked ([`OwnedReport::Died`]).
    dead: Vec<&'static str>,
    /// The channel disconnected before every member reported: every sender is gone, so a
    /// member missing from `reported` ended without sending (its thread panicked) — not a
    /// member still running at the deadline.
    disconnected: bool,
}

/// The owned-only collector: wait for the first definite verdict, then a short grace for a
/// co-completing contradiction; with no verdict by the deadline, the same grace for the members'
/// own budget stops. Pure over the channel, so the seam is testable without an engine
/// (mununu#1143).
fn collect_owned(
    rx: &std::sync::mpsc::Receiver<(&'static str, OwnedReport)>,
    n_members: usize,
    deadline: std::time::Instant,
    grace: std::time::Duration,
) -> OwnedReports {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Instant;
    let mut reachable_by: Vec<&'static str> = Vec::new();
    let mut unreachable_by: Vec<&'static str> = Vec::new();
    let mut dead: Vec<&'static str> = Vec::new();
    let mut reports = 0usize;
    let mut reported: Vec<&'static str> = Vec::new();
    let mut definite = false;
    let mut disconnected = false;
    // Wait for a definite verdict, or all members to abstain, or the deadline.
    while reports < n_members && !definite {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok((name, OwnedReport::Died)) => {
                reports += 1;
                dead.push(name);
            }
            Ok((name, OwnedReport::Verdict(v))) => {
                reports += 1;
                reported.push(name);
                match v {
                    Some(true) => {
                        reachable_by.push(name);
                        definite = true;
                    }
                    Some(false) => {
                        unreachable_by.push(name);
                        definite = true;
                    }
                    None => {}
                }
            }
            // Timeout (deadline) or every sender gone (disconnected) — stop waiting; which one
            // decides how an unreported member is named (mununu#1143).
            Err(e) => {
                disconnected = e == RecvTimeoutError::Disconnected;
                break;
            }
        }
    }
    // A definite verdict is in. Give the OTHER members a SHORT bounded grace to finish so a
    // co-completing CONTRADICTION (the inter-engine safety-net — one engine Reachable, another
    // Unreachable ⇒ a latent single-engine bug) is still surfaced. We do NOT wait for the full
    // deadline: a slow, non-interruptible straggler (the exact BDD engine on a wide datapath)
    // would defeat the owned early-return. The grace catches the fast members (native / interp /
    // boolector), which co-complete within ms; a genuinely-slow engine is simply not waited on.
    // Bounded — the added latency past first-definite is at most `grace` (and the
    // overall `deadline` still caps it). Was a non-blocking `try_recv` only, which missed a
    // straggler that hadn't landed at the exact instant the first verdict arrived (DET-2).
    if definite {
        let grace_deadline = (Instant::now() + grace).min(deadline);
        while reports < n_members {
            let now = Instant::now();
            if now >= grace_deadline {
                break;
            }
            match rx.recv_timeout(grace_deadline - now) {
                Ok((name, OwnedReport::Died)) => {
                    reports += 1;
                    dead.push(name);
                }
                Ok((name, OwnedReport::Verdict(v))) => {
                    reports += 1;
                    reported.push(name);
                    match v {
                        Some(true) => reachable_by.push(name),
                        Some(false) => unreachable_by.push(name),
                        None => {}
                    }
                    // Safety-net fired — a contradiction is now present; report it immediately.
                    if !reachable_by.is_empty() && !unreachable_by.is_empty() {
                        break;
                    }
                }
                Err(e) => {
                    // grace elapsed or every sender gone
                    disconnected = e == RecvTimeoutError::Disconnected;
                    break;
                }
            }
        }
    }
    // mununu#1022 — no verdict by the deadline: give the members the same short grace to land
    // their own budget stops (each re-entered the budget and stops on it), then name every member
    // that still has not reported. Before this an owned-only `unknown` at the wall said nothing
    // about any member.
    if !definite && reports < n_members {
        let grace_deadline = Instant::now() + grace;
        while reports < n_members {
            let now = Instant::now();
            if now >= grace_deadline {
                break;
            }
            match rx.recv_timeout(grace_deadline - now) {
                Ok((name, OwnedReport::Died)) => {
                    reports += 1;
                    dead.push(name);
                }
                Ok((name, OwnedReport::Verdict(v))) => {
                    reports += 1;
                    reported.push(name);
                    match v {
                        Some(true) => reachable_by.push(name),
                        Some(false) => unreachable_by.push(name),
                        None => {}
                    }
                }
                Err(e) => {
                    disconnected = e == RecvTimeoutError::Disconnected;
                    break;
                }
            }
        }
    }
    OwnedReports {
        reachable_by,
        unreachable_by,
        reported,
        dead,
        disconnected,
    }
}

/// mununu#1022 — the owned-only members that had not reported when the collector gave up at its
/// deadline, each with the cause the report carries.
fn unreported_at_deadline(
    members: &[&'static str],
    reported: &[&'static str],
    timeout_ms: u32,
) -> Vec<(&'static str, String)> {
    members
        .iter()
        .copied()
        .filter(|m| !reported.contains(m))
        .map(|m| {
            (
                m,
                format!(
                    "still running at the owned-only wall deadline ({timeout_ms} ms); no result \
                     was collected"
                ),
            )
        })
        .collect()
}

/// The owned-only driver's members, in its reporting order: the names its threads send;
/// [`owned_members`] appends the opt-in `ic3`.
#[cfg(feature = "boolector")]
const OWNED_MEMBERS: [&str; 5] = ["native", "interp", "boolector", "sat-bmc", "exact"];
#[cfg(not(feature = "boolector"))]
const OWNED_MEMBERS: [&str; 5] = ["native", "interp", "cex", "sat-bmc", "exact"];

/// [`OWNED_MEMBERS`], plus `ic3` unless `MUNUNU_REACH_IC3=0` ([`ic3_default`]): under
/// `--owned-only` no rIC3 runs, so the owned IC3 is on in `Auto` as well as `On`.
fn owned_members() -> Vec<&'static str> {
    owned_members_for(ic3_default() != Ic3Default::Off)
}

fn owned_members_for(ic3: bool) -> Vec<&'static str> {
    let mut m = OWNED_MEMBERS.to_vec();
    if ic3 {
        m.push("ic3");
    }
    m
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::adapter::btor2::cex_value::CexValue;
    use crate::adapter::btor2::parser;

    /// A one-register design with no `bad` line: nothing to decide.
    const NO_BAD: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                          5 input 1 d\n6 next 1 3 5\n";

    /// mununu#1064 — a proof engine handed a model with no `bad` answers "unreachable"; that is
    /// a proof of nothing and must not surface as `holds`. Both drivers abstain with the reason.
    #[test]
    fn a_model_with_no_bad_line_is_unknown_with_a_reason_never_holds() {
        let file = parser::parse(NO_BAD).expect("parse");
        assert!(file.bads().next().is_none());
        for (driver, out) in [
            ("full", decide_reach_portfolio(&file)),
            ("owned-only", decide_reach_owned_only(&file, 5_000)),
        ] {
            assert_eq!(out.verdict, ReachVerdict::Unknown, "{driver}: {out:?}");
            assert!(out.unreachable_by.is_empty(), "{driver}: {out:?}");
            assert_eq!(
                out.refused,
                vec![("portfolio", NO_BAD_LINE_REASON.to_string())],
                "{driver}"
            );
        }
    }

    /// mununu#986 — the member sets a RACED rung runs. The parallel justice policy joins the
    /// losers, so it needs every member to stop on cancel (no exact). Its k-liveness rung runs rIC3
    /// as ONE `-e ic3` process. Each set is the shipped set with exactly that one field changed
    /// (the l2s rung's set is computed from [`ReachMembers::shipped`], so the N5.5 owned-IC3
    /// default carries into the race).
    #[test]
    fn the_raced_member_sets_differ_from_the_shipped_ones_in_one_field() {
        assert_eq!(
            ReachMembers::cancellable(),
            ReachMembers {
                exact: false,
                ..ReachMembers::shipped()
            }
        );
        const { assert!(ReachMembers::ALL.exact, "the shipped portfolio keeps exact") };
        assert!(
            ReachMembers::shipped().exact,
            "the shipped portfolio keeps exact"
        );
        assert_eq!(
            ReachMembers::PROOF_DIRECTED_RACED,
            ReachMembers {
                ric3_engine: ric3::DEFAULT_ENGINE,
                ..ReachMembers::PROOF_DIRECTED
            }
        );
        assert_eq!(
            ReachMembers::PROOF_DIRECTED.ric3_engine,
            ric3::PORTFOLIO_ENGINE
        );
        const { assert!(!ReachMembers::PROOF_DIRECTED_RACED.exact) };
    }

    /// mununu#893 — a budget stop and a refusal reach different lists, and `collect` carries
    /// both in the fixed engine order; a stop of either kind is still an abstention (`None`).
    #[test]
    fn a_budget_stop_is_an_abstention_with_a_cause_not_a_refusal() {
        let refused = std::sync::Mutex::new(Vec::new());
        let abstained = std::sync::Mutex::new(Vec::new());
        let undecided = std::sync::Mutex::new(Vec::new());
        let stop = native_bmc::BmcError::ResourceLimit {
            k: 3,
            units: 15_000_000,
        };
        let v: Option<bool> = settle_native(
            &refused,
            &abstained,
            &undecided,
            &NO_CLOCK_STOP,
            "native",
            Err(MemberStop::from(stop)),
        );
        assert_eq!(v, None);
        assert!(
            refused.lock().unwrap().is_empty(),
            "a budget stop is not a refusal"
        );
        let abst = abstained.lock().unwrap().clone();
        assert_eq!(abst.len(), 1);
        assert_eq!(abst[0].0, "native");
        assert!(
            abst[0].1.contains("deterministic query budget") && abst[0].1.contains("depth 3"),
            "{}",
            abst[0].1
        );
        let v: Option<bool> = settle_native(
            &refused,
            &abstained,
            &undecided,
            &NO_CLOCK_STOP,
            "native",
            Err(MemberStop::from(native_bmc::BmcError::NoBadProperty)),
        );
        assert_eq!(v, None);
        assert_eq!(
            refused.lock().unwrap().len(),
            1,
            "an encode-class error is a refusal"
        );
        assert_eq!(abstained.lock().unwrap().len(), 1);
        // The wall budget is an abstention too, named as the wall.
        assert_eq!(
            MemberStop::from(native_bmc::BmcError::BudgetExpired { k: 1 }),
            MemberStop::Abstained("native BMC: the wall-clock budget expired at depth 1".into())
        );
        let out = collect(Members {
            abstained: vec![("interp", "x".into()), ("native", "y".into())],
            ..Members::default()
        });
        assert_eq!(out.verdict, ReachVerdict::Unknown);
        assert_eq!(
            out.abstained,
            vec![("native", "y".to_string()), ("interp", "x".to_string())],
            "fixed engine order, whatever the finishing order"
        );
    }

    // Merge-logic unit tests (no subprocess): the first-definite + contradiction
    // rule is pure and testable without any external binary.
    #[test]
    fn merge_first_definite_and_contradiction() {
        assert_eq!(
            ReachOutcome::from_sets(vec!["exact"], vec![]).verdict,
            ReachVerdict::Reachable
        );
        assert_eq!(
            ReachOutcome::from_sets(vec![], vec!["btormc"]).verdict,
            ReachVerdict::Unreachable
        );
        assert_eq!(
            ReachOutcome::from_sets(vec![], vec![]).verdict,
            ReachVerdict::Unknown
        );
        // Two sound engines disagreeing ⇒ a soundness alarm, never a silent pick.
        let c = ReachOutcome::from_sets(vec!["btormc"], vec!["exact"]);
        assert_eq!(c.verdict, ReachVerdict::Contradiction);
        assert_eq!(c.reachable_by, vec!["btormc"]);
        assert_eq!(c.unreachable_by, vec!["exact"]);
    }

    #[test]
    fn interp_member_never_overrides_a_definite_verdict() {
        // The interpolation member is gated: the sequential driver only computes it
        // when the base portfolio (exact / native / btormc / pono) is still
        // Unknown, and the merge is first-definite-wins. A design that native
        // k-induction already proves safe must come back Unreachable with the interp
        // member never running or flipping it.
        const SAFE_1IND: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                                 5 next 1 3 2\n6 bad 3\n";
        let file = parser::parse(SAFE_1IND).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Unreachable, "outcome: {out:?}");
        // Which sound in-process member gets there first is a race: on this 1-bit design
        // `exact` usually decides within milliseconds, and under a loaded parallel
        // `cargo test` the portfolio cancels native before it reports. So the test asks
        // that a sound in-process member decided, not which one, and that nothing
        // (the interpolation member included) reports the design reachable.
        // Since N5.5 the set also holds `ic3` on a host without rIC3, and under a loaded
        // `cargo test` it has proved this design before both (2026-11-17), so the test asks
        // that a shipped in-process member decided, whichever it was.
        let shipped = ReachMembers::shipped().names();
        assert!(
            !out.unreachable_by.is_empty()
                && out.unreachable_by.iter().all(|m| shipped.contains(m)),
            "a shipped member proves the 1-inductive design safe: {out:?}"
        );
        assert!(
            out.reachable_by.is_empty(),
            "no member may flip a definite base: {out:?}"
        );
        // At the merge level a definite base is likewise untouched by an interp abstain.
        assert_eq!(
            collect(Members {
                exact: Some(true),
                ..Default::default()
            })
            .verdict,
            ReachVerdict::Reachable
        );
    }

    /// mununu#734 — a member's refusal (an encode error, an absent binary, a child without a
    /// verdict) rides through the merge beside the abstention it caused, so an instant
    /// `Unknown` can name its cause; a timeout or a clean `unknown` leaves the list empty.
    /// N5.4 — among several carried traces `collect` ships the first in the fixed engine order,
    /// and the replay failures are sorted the same way.
    #[test]
    fn collect_ships_the_first_carried_trace_in_engine_order() {
        let t = |n: usize| BmcTrace {
            states: vec![vec![]; n],
            inputs: vec![vec![]; n],
        };
        let out = collect(Members {
            sat_bmc: Some(true),
            ric3_bmc: Some(McVerdict::Violated),
            traces: vec![("ric3-bmc", t(9), None), ("sat-bmc", t(3), Some(false))],
            replay_failed: vec![("ric3", "x".into()), ("ic3", "y".into())],
            ..Default::default()
        });
        assert_eq!(out.verdict, ReachVerdict::Reachable);
        let carried = out.trace.expect("a carried trace");
        assert_eq!((carried.by, carried.depth()), ("sat-bmc", 2));
        // mununu#1057 — what the member said about its depth travels with the trace.
        assert_eq!(carried.shortest, Some(false));
        assert_eq!(
            out.trace_replay_failed,
            vec![("ic3", "y".to_string()), ("ric3", "x".to_string())]
        );
        assert_eq!(collect(Members::default()).trace, None);
    }

    /// A 3-bit counter from 0 by 1: `c = 7` first holds at depth 7.
    const COUNTER_TO_SEVEN: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                                    6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                                    10 eq 9 3 8\n11 bad 10\n";

    /// N5.4 — each owned carrier, run ALONE (so the pick cannot race), carries its replayed
    /// counterexample out when `witness` is on and nothing when it is off.
    #[test]
    fn each_owned_carrier_carries_its_replayed_trace_only_when_asked() {
        let file = parser::parse(COUNTER_TO_SEVEN).unwrap();
        let off = ReachMembers {
            exact: false,
            native: false,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ..ReachMembers::ALL
        };
        for (name, members) in [
            (
                "sat-bmc",
                ReachMembers {
                    sat_bmc: true,
                    ..off
                },
            ),
            ("ic3", ReachMembers { ic3: true, ..off }),
        ] {
            let wall = std::time::Duration::from_secs(60);
            let with =
                decide_reach_portfolio_with_members(&file, wall, None, members.with_witness(true));
            assert_eq!(with.verdict, ReachVerdict::Reachable, "{name}: {with:?}");
            let carried = with
                .trace
                .clone()
                .unwrap_or_else(|| panic!("{name} carried no trace: {with:?}"));
            assert_eq!(carried.by, name);
            assert_eq!(carried.depth(), 7, "{name}");
            assert_eq!(
                carried.trace.states[7],
                vec![("c".to_string(), CexValue::from_u64(7, 3))]
            );
            assert!(with.trace_replay_failed.is_empty(), "{name}: {with:?}");
            let without = decide_reach_portfolio_with_members(&file, wall, None, members);
            assert_eq!(without.verdict, ReachVerdict::Reachable, "{name}");
            assert_eq!(without.trace, None, "{name}: a trace was carried unasked");
        }
    }

    /// N5.4 — the owned-only driver carries too. Its carriers are `sat-bmc` and, since the N5.5
    /// default, `ic3`; whichever decides first carries, and the trace is a depth-7 one either way.
    #[test]
    fn the_owned_only_driver_carries_a_trace_when_asked() {
        let file = parser::parse(COUNTER_TO_SEVEN).unwrap();
        let out = decide_reach_owned_only_with_witness(&file, 60_000, true);
        assert_eq!(out.verdict, ReachVerdict::Reachable, "{out:?}");
        let carriers = ["sat-bmc", "ic3"];
        // The trace is a carrier's own unless a non-carrying member decided first and the
        // carriers were cancelled.
        if let Some(carried) = &out.trace {
            assert!(carriers.contains(&carried.by), "{out:?}");
            assert!(out.reachable_by.contains(&carried.by), "{out:?}");
            assert_eq!(carried.depth(), 7);
        } else {
            assert!(
                !out.reachable_by.iter().any(|m| carriers.contains(m)),
                "a carrier decided but carried nothing: {out:?}"
            );
        }
        assert_eq!(decide_reach_owned_only(&file, 60_000).trace, None);
    }

    /// N5.4 — the wire rows of the replay failures mark the member whose verdict is the whole
    /// `violated`.
    #[cfg(feature = "api")]
    #[test]
    fn a_replay_failure_marks_the_sole_decider() {
        use crate::api::models::TraceReplayFailureView;
        let mut out = ReachOutcome::from_sets(vec!["ric3-bmc"], vec![]);
        out.trace_replay_failed = vec![("ric3-bmc", "state index 9 out of range".to_string())];
        let rows = TraceReplayFailureView::list(&out);
        assert_eq!((rows.len(), rows[0].sole_decider), (1, true));
        assert_eq!(rows[0].member, "ric3-bmc");
        out.reachable_by.push("sat-bmc");
        assert!(!TraceReplayFailureView::list(&out)[0].sole_decider);
    }

    #[test]
    fn collect_carries_member_refusals() {
        let out = collect(Members {
            refused: vec![
                (
                    "native",
                    "native BMC: encode failed: ConstantOutOfRange".to_string(),
                ),
                ("pono", "pono binary not found".to_string()),
            ],
            ..Default::default()
        });
        assert_eq!(out.verdict, ReachVerdict::Unknown);
        assert_eq!(out.refused.len(), 2);
        assert_eq!(out.refused[0].0, "native");
        assert!(out.refused[1].1.contains("not found"));
        // a decider beside a refusal: the verdict stands, the refusal is still reported
        let decided = collect(Members {
            native: Some(false),
            refused: vec![("exact", "exact: cone over the cap".to_string())],
            ..Default::default()
        });
        assert_eq!(decided.verdict, ReachVerdict::Unreachable);
        assert_eq!(decided.refused.len(), 1);
        assert_eq!(
            ReachOutcome::from_sets(vec![], vec![]).refused,
            Vec::<(&str, String)>::new()
        );
    }

    #[test]
    fn portfolio_decides_reachable_without_subprocess() {
        // With no btormc/pono/rIC3 on PATH the subprocess members abstain (Err) and the
        // in-process members decide the small in-cap counter — so the portfolio still returns
        // a definite verdict. WHICH in-process member reports first is a race the host decides
        // (since N5.5 `ic3` is in the shipped set on a host without rIC3, and it beat `exact`
        // on the macOS runner, 2026-11-17), so the shipped driver is held to the verdict and
        // the merge shape, and the exact-only fallback is pinned with the exact member alone.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Reachable, "{out:?}");
        let shipped = ReachMembers::shipped().names();
        assert!(
            !out.reachable_by.is_empty() && out.reachable_by.iter().all(|m| shipped.contains(m)),
            "every decider is a shipped member: {out:?}"
        );
        assert!(
            out.unreachable_by.is_empty(),
            "no engine should call the reachable counter unreachable"
        );
        let exact_only = ReachMembers {
            exact: true,
            native: false,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let alone = decide_reach_portfolio_with_members(
            &file,
            std::time::Duration::from_secs(60),
            None,
            exact_only,
        );
        assert_eq!(alone.verdict, ReachVerdict::Reachable, "{alone:?}");
        assert_eq!(alone.reachable_by, vec!["exact"], "{alone:?}");
    }

    /// mununu#996 — the join's seam. A member that never reports (one long OxiDD apply) is left
    /// behind once a trusted verdict is in and the grace has passed; it is not waited on.
    #[test]
    fn a_member_that_never_reports_is_abandoned_once_another_has_decided() {
        use crate::adapter::run_budget::Budget;
        use std::sync::atomic::AtomicBool;
        let (_tx, rx) = std::sync::mpsc::channel::<Result<Option<bool>, String>>();
        let decided = AtomicBool::new(true);
        // `_tx` is alive and never sends: the call returns only by abandoning the member.
        let w = await_straggler(&rx, &decided, &Budget::unbounded(), STRAGGLER_GRACE);
        assert!(
            matches!(w, StragglerWait::Abandoned(Abandon::OtherDecided)),
            "{w:?}"
        );
    }

    /// Negative control for the seam: with nothing decided the join waits, and a result that
    /// lands after the wait began is taken — an undecided portfolio still hears the exact member.
    #[test]
    fn an_undecided_join_waits_for_the_member_and_takes_its_result() {
        use crate::adapter::run_budget::Budget;
        use std::sync::atomic::AtomicBool;
        let (tx, rx) = std::sync::mpsc::channel::<Result<Option<bool>, String>>();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            let _ = tx.send(Ok(Some(true)));
        });
        let decided = AtomicBool::new(false);
        let w = await_straggler(&rx, &decided, &Budget::unbounded(), STRAGGLER_GRACE);
        assert!(
            matches!(w, StragglerWait::Finished(Ok(Some(true)))),
            "{w:?}"
        );
    }

    /// A member left behind at the budget's deadline is a budget abstention, and a member whose
    /// thread died without sending is lost, not silently undecided.
    #[test]
    fn an_abandoned_member_says_why_it_was_left_behind() {
        use crate::adapter::run_budget::Budget;
        use std::sync::atomic::AtomicBool;
        let (_tx, rx) = std::sync::mpsc::channel::<Result<Option<bool>, String>>();
        let decided = AtomicBool::new(true);
        let w = await_straggler(&rx, &decided, &Budget::with_ms(0), STRAGGLER_GRACE);
        assert!(
            matches!(w, StragglerWait::Abandoned(Abandon::BudgetExpired)),
            "{w:?}"
        );
        let (tx, rx) = std::sync::mpsc::channel::<Result<Option<bool>, String>>();
        drop(tx);
        let w = await_straggler(
            &rx,
            &AtomicBool::new(false),
            &Budget::unbounded(),
            STRAGGLER_GRACE,
        );
        assert!(
            matches!(w, StragglerWait::Abandoned(Abandon::Lost)),
            "{w:?}"
        );
    }

    /// mununu#996 at the driver: the exact member stands for the `krebs.3` apply — it does not
    /// return — while the native member finds the counter's `bad` at depth 7. The portfolio
    /// returns native's verdict instead of joining the exact member: the stub never returns, so
    /// an outcome at all is one the join did not wait for (no wall-clock bound to pass or miss).
    #[test]
    fn the_portfolio_returns_a_decided_verdict_without_joining_a_stuck_exact_member() {
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let members = ReachMembers {
            exact: true,
            native: true,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let out = decide_with_members_using(
            &file,
            std::time::Duration::from_secs(60),
            None,
            members,
            &Runners {
                exact: scripted::exact_never_returns,
                ..Runners::SHIPPED
            },
        );
        assert_eq!(out.verdict, ReachVerdict::Reachable, "{out:?}");
        assert_eq!(out.reachable_by, vec!["native"], "{out:?}");
    }

    /// mununu#1039 at the driver: with no run budget (`btor2 verify --timeout-ms`), a member that
    /// runs past the portfolio's wall no longer holds the run. The stuck member stands for the
    /// native z3 check that ran 70 s past a 300 s wall on HWMCC'20 `mul9`. The portfolio returns
    /// at the wall plus the grace, and the stopped member is named with its cause. The stub never
    /// returns, so only the wall can end the run: an outcome at all is the wall's.
    #[test]
    fn the_portfolio_returns_at_its_wall_and_names_the_member_it_stopped() {
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let members = ReachMembers {
            exact: true,
            native: false,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let out = decide_with_members_using(
            &file,
            std::time::Duration::from_millis(200),
            None,
            members,
            &Runners {
                exact: scripted::exact_never_returns,
                ..Runners::SHIPPED
            },
        );
        assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
        assert!(
            out.abstained
                .iter()
                .any(|(m, why)| *m == "exact" && why.contains("wall deadline (200 ms")),
            "the stopped member is not named: {out:?}"
        );
    }

    /// The post-join sweep names exactly the started members with no verdict and no recorded stop.
    #[test]
    fn the_wall_sweep_names_only_silent_started_members() {
        let m = Members {
            exact: None,
            native: None,
            interp: Some(false),
            sat_bmc: None,
            ic3: None,
            btormc: Some(McVerdict::Unknown),
            pono: Some(McVerdict::Safe),
            ric3: None,
            ric3_bmc: None,
            refused: vec![("sat-bmc", "x".into())],
            abstained: Vec::new(),
            undecided: Vec::new(),
            traces: Vec::new(),
            replay_failed: Vec::new(),
        };
        let started = ReachMembers {
            ric3_bmc: false,
            ..ReachMembers::ALL
        };
        // N5.5 — `ic3` is in the roster, so a started, silent `ic3` is named like the others.
        assert_eq!(
            stopped_by_wall_unrecorded(&started, &m),
            vec!["exact", "native", "ic3", "btormc", "ric3"]
        );
    }

    /// mununu#1067 review — the clock sweep runs only when no member decided: a definite verdict
    /// means a peer stopped the rest, and a subprocess `Unknown` is no verdict.
    #[test]
    fn the_clock_sweep_is_skipped_once_a_member_decided() {
        let unknown = || Members {
            btormc: Some(McVerdict::Unknown),
            ..Members::default()
        };
        assert!(
            !any_definite(&unknown()),
            "an Unknown subprocess is no verdict"
        );
        let owned = Members {
            ic3: Some(true),
            ..unknown()
        };
        assert!(any_definite(&owned));
        let subprocess = Members {
            ric3: Some(McVerdict::Safe),
            ..unknown()
        };
        assert!(any_definite(&subprocess));
    }

    /// mununu#1043 — the sweep names which clock stopped the member.
    #[test]
    fn the_clock_sweep_names_the_wall_or_the_budget() {
        assert_eq!(
            clock_sweep_cause(true, 60_000),
            "stopped at the portfolio's wall deadline (60000 ms: `--timeout-ms` on the `btor2` \
             verbs, the run budget on `sv verify-auto`) before it reached a verdict"
        );
        assert_eq!(
            clock_sweep_cause(false, 60_000),
            "stopped when the run budget expired, before it reached a verdict"
        );
    }

    /// mununu#1022 — an 8-bit counter stepping by 2 from 0 never reaches 255, but 127 bad-free
    /// odd predecessors stand behind it, so k-induction cannot close at the native bound (40).
    const EVEN_COUNTER: &str = "1 sort bitvec 8\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n\
                                5 constd 1 2\n6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n\
                                9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";

    /// The native member that searches to its bound says how far it got, and a cancelled one is
    /// in no list.
    #[test]
    fn a_native_member_that_searched_to_its_bound_reports_its_progress() {
        let file = parser::parse(EVEN_COUNTER).expect("parse");
        match run_native(&file, None, 5_000, None) {
            Err(MemberStop::Undecided(p)) => {
                assert!(p.contains("depth 40") && p.contains("k = 40"), "{p}");
            }
            other => panic!("expected an undecided native member, got {other:?}"),
        }
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(matches!(
            run_native(&file, Some(&cancelled), 5_000, None),
            Err(MemberStop::Cancelled(p)) if p.starts_with("native BMC / k-induction stopped at depth")
        ));
    }

    /// At the driver: with only the native member running, its progress reaches the outcome's
    /// `undecided` list, not `abstained` or `refused`, with the wall it left unused (#1038).
    #[test]
    fn the_portfolio_carries_an_undecided_members_progress() {
        let file = parser::parse(EVEN_COUNTER).expect("parse");
        let members = ReachMembers {
            exact: false,
            native: true,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let out = decide_reach_portfolio_with_members(
            &file,
            std::time::Duration::from_secs(60),
            None,
            members,
        );
        assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
        assert_eq!(out.undecided.len(), 1, "{out:?}");
        assert_eq!(out.undecided[0].0, "native");
        // mununu#1038 — the stop is at the depth bound, long before the 60 s wall: the reason
        // says how much of the wall it left unused.
        assert!(out.undecided[0].1.contains("s of the wall left"), "{out:?}");
        assert!(
            out.refused.is_empty() && out.abstained.is_empty(),
            "{out:?}"
        );
    }

    /// mununu#1022 — interpolation that could not run is a refusal recorded whatever a peer did,
    /// before any search (so two runs of one design report the same lists — CI's
    /// `parallel_with_timeout_delegates_and_agrees` raced on it); a search that ran out is
    /// `undecided` unless a peer had decided.
    #[test]
    fn an_interp_member_that_could_not_run_is_a_refusal_not_an_undecided_search() {
        use std::sync::atomic::AtomicBool;
        let (refused, abstained, undecided) = (
            std::sync::Mutex::new(Vec::new()),
            std::sync::Mutex::new(Vec::new()),
            std::sync::Mutex::new(Vec::new()),
        );
        let ran = std::cell::Cell::new(false);
        let v = interp_member(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(true),
            &NO_CLOCK_STOP,
            Err("cvc5 missing".to_string()),
            || {
                ran.set(true);
                InterpSafetyVerdict::Safe { iterations: 1 }
            },
        );
        assert_eq!(v, None);
        assert!(!ran.get(), "no search without cvc5");
        let undecided_search = || InterpSafetyVerdict::Undecided {
            reason: "no fixpoint".to_string(),
        };
        interp_member(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(false),
            &NO_CLOCK_STOP,
            Ok(()),
            undecided_search,
        );
        interp_member(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(true),
            &NO_CLOCK_STOP,
            Ok(()),
            undecided_search,
        );
        assert_eq!(
            *refused.lock().unwrap(),
            vec![("interp", "cvc5 missing".to_string())]
        );
        assert_eq!(
            *undecided.lock().unwrap(),
            vec![("interp", "no fixpoint".to_string())]
        );
        assert!(
            abstained.lock().unwrap().is_empty(),
            "a peer's stop is in no list"
        );
        // mununu#1043 — the same stop, with the wall or the budget behind the flag: an abstention
        // carrying the search's own reason.
        interp_member(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(true),
            &AtomicBool::new(true),
            Ok(()),
            undecided_search,
        );
        let abst = abstained.lock().unwrap().clone();
        assert_eq!(abst.len(), 1);
        assert!(
            abst[0]
                .1
                .starts_with("no fixpoint; stopped by the portfolio's wall"),
            "{abst:?}"
        );
    }

    /// mununu#1043 — a member stopped on the shared flag: in no list when a peer raised it, a
    /// budget abstention with the member's own progress when the wall or the budget did (the
    /// issue's case: `IC3 stopped at frame 2` was dropped, or replaced by the #1039 sweep's
    /// generic "no result was collected"). The subprocess members' settle takes the same flag.
    #[test]
    fn a_member_stopped_by_the_clock_keeps_its_progress_and_a_peer_stop_is_in_no_list() {
        use std::sync::atomic::AtomicBool;
        let lists = || {
            (
                std::sync::Mutex::new(Vec::new()),
                std::sync::Mutex::new(Vec::new()),
                std::sync::Mutex::new(Vec::new()),
            )
        };
        let stop = || -> Result<Option<bool>, MemberStop> {
            Err(MemberStop::Cancelled("IC3 stopped at frame 2".into()))
        };
        let (refused, abstained, undecided) = lists();
        let v = settle_native(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(false),
            "ic3",
            stop(),
        );
        assert_eq!(v, None);
        assert!(
            refused.lock().unwrap().is_empty()
                && abstained.lock().unwrap().is_empty()
                && undecided.lock().unwrap().is_empty(),
            "a peer's verdict stopped it: in no list"
        );
        let v = settle_native(
            &refused,
            &abstained,
            &undecided,
            &AtomicBool::new(true),
            "ic3",
            stop(),
        );
        assert_eq!(v, None);
        assert_eq!(
            *abstained.lock().unwrap(),
            vec![(
                "ic3",
                "IC3 stopped at frame 2; stopped by the portfolio's wall or run budget before a verdict"
                    .to_string()
            )]
        );
        assert!(refused.lock().unwrap().is_empty() && undecided.lock().unwrap().is_empty());
        // The subprocess members: an `Unknown` after `decided` went up is recorded only on a clock
        // stop.
        let t = std::time::Duration::from_millis(300_000);
        for (clock, want) in [(false, 0), (true, 1)] {
            let (refused, abstained, _) = lists();
            settle_subprocess(
                &refused,
                &abstained,
                &AtomicBool::new(true),
                &AtomicBool::new(clock),
                "btormc",
                t,
                Ok(Some(McVerdict::Unknown)),
            );
            assert_eq!(
                abstained.lock().unwrap().len(),
                want,
                "clock stop = {clock}"
            );
        }
    }

    /// The owned-only collector names every member that had not reported at its deadline.
    #[test]
    fn the_owned_driver_names_the_members_still_running_at_its_deadline() {
        let left = unreported_at_deadline(&OWNED_MEMBERS, &["native", "interp"], 300_000);
        let names: Vec<_> = left.iter().map(|(m, _)| *m).collect();
        assert_eq!(names, vec![OWNED_MEMBERS[2], "sat-bmc", "exact"]);
        assert!(left[2].1.contains("300000 ms"), "{}", left[2].1);
        assert!(unreported_at_deadline(&OWNED_MEMBERS, &OWNED_MEMBERS, 1).is_empty());
        // N5.1 — the opt-in member is waited for only when it was started.
        let with = owned_members_for(true);
        assert_eq!(with.last(), Some(&"ic3"));
        assert_eq!(owned_members_for(false), OWNED_MEMBERS.to_vec());
        let left = unreported_at_deadline(&with, &OWNED_MEMBERS, 7);
        assert_eq!(
            left.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
            vec!["ic3"]
        );
    }

    /// mununu#1143 — the owned-only collector reads a member's death notice while the others
    /// are still alive, tells a disconnect from the deadline as the backstop, and still reads a
    /// co-completing verdict within the grace. The seam the driver consumes; the driver itself
    /// runs real engines, so a panic is injected through `spawn_owned` below instead.
    #[test]
    fn x1143_the_owned_collector_tells_a_dead_member_from_one_still_running() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let grace = Duration::from_secs(5);
        // A member that panicked while another is still running: its death notice arrives, the
        // other's sender stays alive past the deadline.
        let (tx, rx) = mpsc::channel::<(&'static str, OwnedReport)>();
        let _still_running = tx.clone();
        tx.send(("native", OwnedReport::Verdict(None))).expect("send");
        tx.send(("interp", OwnedReport::Died)).expect("send");
        drop(tx);
        let r = collect_owned(&rx, 3, Instant::now() + Duration::from_millis(50), grace);
        assert_eq!(r.reported, vec!["native"]);
        assert_eq!(r.dead, vec!["interp"], "the death notice, not a guess");
        assert!(!r.disconnected, "the third member is still running");
        // The backstop: a sender dropped without any report (a thread that did not unwind).
        let (tx, rx) = mpsc::channel::<(&'static str, OwnedReport)>();
        let tx_dead = tx.clone();
        tx.send(("native", OwnedReport::Verdict(None))).expect("send");
        drop(tx);
        drop(tx_dead);
        let r = collect_owned(&rx, 2, Instant::now() + Duration::from_secs(5), grace);
        assert!(r.disconnected, "every sender gone before every member reported");
        assert_eq!(r.reported, vec!["native"]);
        assert!(r.dead.is_empty());
        // A definite verdict, then a disagreeing straggler inside the grace: both are read, and
        // the contradiction returns at once (the third member is unreported, not dead).
        let (tx, rx) = mpsc::channel::<(&'static str, OwnedReport)>();
        tx.send(("native", OwnedReport::Verdict(Some(true)))).expect("send");
        tx.send(("exact", OwnedReport::Verdict(Some(false)))).expect("send");
        drop(tx);
        let r = collect_owned(&rx, 3, Instant::now() + Duration::from_secs(5), grace);
        assert_eq!(
            (r.reachable_by, r.unreachable_by),
            (vec!["native"], vec!["exact"])
        );
        assert_eq!(r.reported, vec!["native", "exact"]);
        assert!(!r.disconnected);
    }

    /// mununu#1143 — a member thread that panics sends its death notice through `spawn_owned`
    /// (the owned-only driver), and a scoped member's panicked join is `None` AND a named
    /// refusal with its owner (the full driver).
    #[test]
    fn x1143_a_panicked_member_is_a_named_refusal_in_both_drivers() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<(&'static str, OwnedReport)>();
        spawn_owned("pono", tx.clone(), || panic!("boom"));
        spawn_owned("ric3", tx, || {});
        let mut got: Vec<(&str, OwnedReport)> = rx.iter().collect();
        got.sort();
        assert_eq!(got, vec![("pono", OwnedReport::Died)], "a body that sends nothing and does not panic is silent; a panic is a death notice");
        let refused = std::sync::Mutex::new(Vec::new());
        let joined = std::thread::spawn(|| -> Option<bool> { panic!("boom") }).join();
        assert_eq!(settle_join(&refused, "pono", joined), None);
        let ok = std::thread::spawn(|| Some(false)).join();
        assert_eq!(settle_join(&refused, "ric3", ok), Some(false));
        let got = refused.lock().expect("lock").clone();
        assert_eq!(got, vec![("pono", panicked_reason("pono"))]);
        assert!(got[0].1.contains("it panicked") && got[0].1.contains("mununu defect"), "{}", got[0].1);
    }

    /// mununu#1143 — REPRODUCER through the public driver: an IC3-only portfolio on a design
    /// with a 130-bit cell, no witness asked for, lists the unreplayed counterexample in
    /// `trace_replay_failed`. Fails on the parent (empty: the reason went to the log).
    #[test]
    fn x1143_the_portfolio_lists_an_unreplayed_ic3_violation_without_a_witness() {
        const WIDE: &str = "1 sort bitvec 130\n2 sort bitvec 1\n3 state 1 c\n4 next 1 3 3\n\
                            5 constd 1 5\n6 eq 2 3 5\n7 bad 6\n";
        let file = parser::parse(WIDE).expect("parse");
        let ic3_only = ReachMembers {
            exact: false,
            native: false,
            interp: false,
            sat_bmc: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ic3: true,
            witness: false,
            ..ReachMembers::ALL
        };
        let out = decide_reach_portfolio_with_members(
            &file,
            std::time::Duration::from_secs(60),
            None,
            ic3_only,
        );
        assert_eq!(out.verdict, ReachVerdict::Reachable, "{out:?}");
        assert_eq!(out.trace_replay_failed.len(), 1, "{out:?}");
        assert_eq!(out.trace_replay_failed[0].0, "ic3");
        assert!(out.trace_replay_failed[0].1.contains("128"), "{out:?}");
    }

    /// mununu#1143 — an IC3 `Violated` on a design the simulator cannot carry is listed as
    /// unreplayed whether or not a witness was asked for; with one, the trace itself is not
    /// carried (there is none).
    #[test]
    fn x1143_an_unreplayed_ic3_violation_is_listed_without_a_witness() {
        const WIDE: &str = "1 sort bitvec 130\n2 sort bitvec 1\n3 state 1 c\n4 next 1 3 3\n\
                            5 constd 1 5\n6 eq 2 3 5\n7 bad 6\n";
        let file = parser::parse(WIDE).expect("parse");
        let later = std::time::Instant::now() + std::time::Duration::from_secs(60);
        for witness in [false, true] {
            let store = Carried::default();
            let go = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            assert_eq!(run_ic3(&file, later, go, &store, witness), Ok(Some(true)));
            let (traces, failed) = store.take();
            assert!(
                traces.is_empty(),
                "witness={witness}: nothing replayed, nothing carried"
            );
            assert_eq!(failed.len(), 1, "witness={witness}: {failed:?}");
            assert_eq!(failed[0].0, "ic3");
            assert!(
                failed[0].1.contains("128"),
                "witness={witness}: {}",
                failed[0].1
            );
        }
    }

    /// mununu#726 — an already-expired thread-local budget decides nothing and spawns nothing:
    /// `Unknown`, no engine credited. That no member starts is asserted on the runners
    /// (`scripted::an_expired_budget_starts_no_member`), not on how fast the call came back. The
    /// unbounded control is `portfolio_decides_reachable_without_subprocess` (same fixture,
    /// `Reachable`).
    #[test]
    fn portfolio_abstains_promptly_under_an_expired_budget() {
        use crate::adapter::run_budget::{Budget, enter};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 constd 1 5\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = {
            let _g = enter(&Budget::with_ms(0));
            decide_reach_portfolio(&file)
        };
        assert_eq!(out, ReachOutcome::default());
    }

    /// mununu#816 — the owned driver's detached members run under the caller's budget. A
    /// thread-local is not inherited by a spawned thread, so before this an already-expired
    /// budget still spawned four members that ran unbounded by it. The same fixture decides
    /// `Reachable` with no budget (`owned_only_excludes_external_engines_on_small_counter`), and
    /// on the parent commit it decides here too. That no member is asked is
    /// `scripted::owned::an_expired_budget_starts_no_member`.
    #[test]
    fn the_owned_driver_abstains_promptly_under_an_expired_budget() {
        use crate::adapter::run_budget::{Budget, enter};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = {
            let _g = enter(&Budget::with_ms(0));
            decide_reach_owned_only(&file, 30_000)
        };
        assert_eq!(out, ReachOutcome::default());
    }

    /// mununu#726 — with no budget the members keep the constants they always had (the floor);
    /// under a budget every bound follows the time left.
    /// mununu#892 — in the full reach portfolio, a `--timeout-ms` raised above the subprocess
    /// default reaches the owned members too. Before, it set the subprocess wall and btormc's
    /// depth cap only; native kept its 5 s per-query default, and two HWMCC'20 array rows native
    /// decides in under forty seconds of query time stayed ⊥ in a full run given ten minutes.
    /// (Moved from `tests/native_budget_892.rs`, mununu#966: the seam is crate-internal.)
    #[test]
    fn a_raised_timeout_becomes_natives_query_budget_in_the_full_portfolio() {
        let t = portfolio_member_timeouts(None, std::time::Duration::from_secs(600));
        assert_eq!(t.native_query_ms, 600_000);
        assert_eq!(t.interp_overall_ms, 600_000);
        assert_eq!(t.subprocess, std::time::Duration::from_secs(600));
    }

    /// mununu#892 — at the default wall, and below it, every member budget is as before.
    #[test]
    fn the_default_wall_leaves_every_budget_as_before() {
        let t = portfolio_member_timeouts(None, btormc::DEFAULT_TIMEOUT);
        assert_eq!(t.native_query_ms, 5_000, "native's per-query default");
        assert_eq!(t.subprocess, btormc::DEFAULT_TIMEOUT);
        let shorter = portfolio_member_timeouts(None, std::time::Duration::from_secs(10));
        assert_eq!(
            shorter.native_query_ms, 5_000,
            "a wall below the default raises nothing"
        );
        assert_eq!(shorter.subprocess, std::time::Duration::from_secs(10));
    }

    /// mununu#892 — a run budget in effect still bounds every member.
    #[test]
    fn a_run_budget_in_effect_still_wins() {
        let t = portfolio_member_timeouts(Some(2_000), std::time::Duration::from_secs(600));
        assert_eq!(
            t.native_query_ms, 2_000,
            "the time left bounds every member"
        );
        assert_eq!(t.subprocess, std::time::Duration::from_millis(2_000));
    }

    #[test]
    fn member_timeouts_floor_is_todays_constants_and_follows_the_budget() {
        let sixty = std::time::Duration::from_secs(60);
        let floor = member_timeouts(None, sixty);
        assert_eq!(floor.native_query_ms, native_bmc::DEFAULT_QUERY_NOMINAL_MS);
        assert_eq!(floor.interp_query_ms, INTERP_QUERY_TIMEOUT_MS);
        assert_eq!(floor.interp_overall_ms, INTERP_OVERALL_TIMEOUT_MS);
        assert_eq!(floor.subprocess, sixty);
        let b = member_timeouts(Some(60_000), sixty);
        assert_eq!(b.native_query_ms, 60_000);
        assert_eq!(b.interp_query_ms, 12_000);
        assert_eq!(b.interp_overall_ms, 60_000);
        assert_eq!(b.subprocess, sixty);
        let short = member_timeouts(Some(2_000), sixty);
        assert!(short.native_query_ms <= 2_000 && short.interp_query_ms <= 2_000);
        assert_eq!(short.subprocess, std::time::Duration::from_millis(2_000));
        let big = member_timeouts(Some(u64::MAX), sixty);
        assert_eq!(big.native_query_ms, u32::MAX, "no overflow");
    }

    #[test]
    fn native_engine_decides_beyond_the_exact_cap_in_portfolio() {
        // An 300-bit register that stays 0, `bad = (big != 0)`: SAFE, but the 300-bit
        // cone is over the exact engine's auto-cap ceiling (192). With no btormc/pono on
        // PATH and the exact engine abstaining, the portfolio would previously be
        // Unknown; the in-house native engine (k-induction) now proves it
        // Unreachable — the scale win, in-process, no subprocess.
        const WIDE_SAFE: &str = "1 sort bitvec 300\n2 zero 1\n3 state 1 big\n4 init 1 3 2\n\
                                 5 next 1 3 3\n6 sort bitvec 1\n7 neq 6 3 2\n8 bad 7\n";
        let file = parser::parse(WIDE_SAFE).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Unreachable, "outcome: {out:?}");
        // Which proof member reports first is a race the host decides (since N5.5 `ic3` is in
        // the set on a host without rIC3 and has beaten `native` under a loaded `cargo test`,
        // 2026-11-17); the shipped driver is held to the verdict, the members' names and the
        // exact engine's abstention, and the native claim is pinned with that member alone.
        let shipped = ReachMembers::shipped().names();
        assert!(
            !out.unreachable_by.is_empty()
                && out.unreachable_by.iter().all(|m| shipped.contains(m)),
            "a shipped in-process member carries the beyond-cap verdict: {out:?}"
        );
        assert!(
            !out.unreachable_by.contains(&"exact"),
            "the exact engine must abstain on the over-cap 300-bit design: {out:?}"
        );
        let native_only = ReachMembers {
            exact: false,
            native: true,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let alone = decide_reach_portfolio_with_members(
            &file,
            std::time::Duration::from_secs(60),
            None,
            native_only,
        );
        assert_eq!(alone.verdict, ReachVerdict::Unreachable, "{alone:?}");
        assert_eq!(alone.unreachable_by, vec!["native"], "{alone:?}");
    }

    #[test]
    fn a_design_only_an_ic3_member_proves_is_unknown_without_one() {
        // An 300-bit counter that STALLS at 0 (init 0, next = (c==0)?0:c+1), bad = (c==MAX).
        // Reachable set is {0} ⇒ SAFE, but:
        //   - the exact BDD engine abstains (300-bit > its auto-cap ceiling of 192);
        //   - native k-induction abstains — the property is not provable by *simple*
        //     k-induction: an unreachable free path MAX-k → … → MAX stays ¬bad until
        //     the last step at every depth, so it never closes below depth 2^80-1.
        // Only an invariant-discovering member (`c == 0`) decides it: rIC3 or Pono's IC3, or
        // the interpolation member with cvc5. With none on PATH (`make ci`) the portfolio
        // answers `Unknown`. That was also the case before N2 removed z3-SPACER, which had been
        // corroborate-only since mununu#740 (`h_RCU` on the HWMCC'20 sweep is this class). In
        // `mununu-sva` rIC3 proves this design.
        const WIDE_STALL: &str = "1 sort bitvec 300\n2 zero 1\n3 one 1\n4 state 1 c\n5 init 1 4 2\n\
             6 sort bitvec 1\n7 eq 6 4 2\n8 add 1 4 3\n9 ite 1 7 2 8\n\
             10 next 1 4 9\n11 ones 1\n12 eq 6 4 11\n13 bad 12\n";
        let file = parser::parse(WIDE_STALL).expect("parse");
        let out = decide_reach_portfolio(&file);
        // Whoever is on PATH (cvc5 lets the interpolation member prove it; rIC3 / btormc / pono
        // in the image) carries the proof; without such a member the verdict is `Unknown`.
        match out.verdict {
            ReachVerdict::Unreachable => assert!(
                out.unreachable_by
                    .iter()
                    .any(|m| ["interp", "ric3", "pono", "btormc"].contains(m)),
                "a proof is carried by an invariant-discovering member: {out:?}"
            ),
            ReachVerdict::Unknown => {}
            other => panic!("unexpected {other:?}: {out:?}"),
        }
        assert!(
            !out.unreachable_by.contains(&"native"),
            "native k-induction must abstain on the non-simple-inductive design: {out:?}"
        );
        assert!(
            !out.unreachable_by.contains(&"exact"),
            "the exact engine must abstain on the over-cap 300-bit design: {out:?}"
        );
    }

    /// Two runs of the shipped portfolio on the same design agree in everything but the
    /// finish order: the in-process members (`exact`, `native`, `sat-bmc`, and `ic3` on a host
    /// without rIC3 since N5.5) race, and which of them has finished when the first verdict
    /// cancels the rest is timing-dependent. The verdict, the refusals, the abstentions and the
    /// trace are not, and every decider is a shipped member.
    fn agree_up_to_finish_order(a: &ReachOutcome, b: &ReachOutcome) {
        assert_eq!(a.verdict, b.verdict, "{a:?}\n{b:?}");
        assert_eq!(a.refused, b.refused);
        assert_eq!(a.abstained, b.abstained);
        assert_eq!(a.undecided, b.undecided);
        assert_eq!(a.trace, b.trace);
        assert_eq!(a.trace_replay_failed, b.trace_replay_failed);
        let shipped = ReachMembers::shipped().names();
        for out in [a, b] {
            assert!(
                !out.reachable_by.is_empty() || !out.unreachable_by.is_empty(),
                "{out:?}"
            );
            for m in out.reachable_by.iter().chain(&out.unreachable_by) {
                assert!(shipped.contains(m), "{m} is not a shipped member: {out:?}");
            }
        }
    }

    #[test]
    fn default_driver_is_the_explicit_budget_driver_at_the_default_budget() {
        // S1 retired the sequential driver: `decide_reach_portfolio` IS the parallel,
        // cancelling driver at the default subprocess budget. With no subprocess
        // members on PATH both reduce to the owned verdict; the point is they agree
        // (up to the finish order, which the race decides).
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let default = decide_reach_portfolio(&file);
        let explicit = decide_reach_portfolio_parallel_with_timeout(&file, btormc::DEFAULT_TIMEOUT);
        agree_up_to_finish_order(&default, &explicit);
        assert_eq!(default.verdict, ReachVerdict::Reachable);
    }

    #[test]
    fn parallel_with_timeout_delegates_and_agrees() {
        // The `--timeout-ms` variant only changes the subprocess budget; on a design
        // the exact engine decides (no subprocess needed) it returns the identical
        // outcome regardless of the budget passed.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let default = decide_reach_portfolio(&file);
        let custom = decide_reach_portfolio_parallel_with_timeout(
            &file,
            std::time::Duration::from_secs(120),
        );
        agree_up_to_finish_order(&default, &custom);
        assert_eq!(custom.verdict, ReachVerdict::Reachable);
    }

    #[test]
    #[ignore = "requires the mununu-sva image (btormc + pono); run by the e2e sweep"]
    fn e2e_portfolio_agrees_across_engines_in_sva() {
        // In mununu-sva all three engines run. On a small reachable counter they
        // must AGREE (all Reachable) — no contradiction alarm — demonstrating the
        // cross-check with the subprocess members live.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Reachable);
        assert!(
            out.reachable_by.len() >= 2,
            "at least the exact engine + one subprocess member should agree; got {:?}",
            out
        );
    }

    /// mununu#997 — the dynamic-step BMC member, alone, in the image: it finds the reachable
    /// counter's `bad`, and on a design whose `bad` is unreachable it never claims a proof.
    #[test]
    #[ignore = "requires the mununu-sva image (rIC3); run by the e2e sweep"]
    fn e2e_the_dynamic_step_bmc_member_finds_counterexamples_and_never_proves() {
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                            5 next 1 3 2\n6 bad 3\n";
        let only_bmc = ReachMembers {
            exact: false,
            native: false,
            interp: false,
            sat_bmc: false,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: true,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        // On a design this small depth 0 takes microseconds, so the dynamic step's first jump
        // lands near depth 416 666 and unrolling it took 5–26 s in the image (rIC3 1.5.2, four
        // runs): the reachable case gets room; the safe case only has to not prove.
        let run = |src: &str, secs: u64| {
            decide_reach_portfolio_with_members(
                &parser::parse(src).expect("parse"),
                std::time::Duration::from_secs(secs),
                None,
                only_bmc,
            )
        };
        let reach = run(COUNTER, 120);
        assert_eq!(reach.verdict, ReachVerdict::Reachable, "{reach:?}");
        assert_eq!(reach.reachable_by, vec!["ric3-bmc"], "{reach:?}");
        let safe = run(SAFE, 5);
        assert!(
            safe.unreachable_by.is_empty(),
            "the BMC member proved nothing: {safe:?}"
        );
        assert!(safe.refused.is_empty(), "{safe:?}");
        // The silence fixed alongside: a member that ran out its budget says so.
        assert_eq!(
            safe.abstained.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
            vec!["ric3-bmc"],
            "{safe:?}"
        );
    }

    /// N5.4 — the owned SAT BMC member's four exits, each in its own list: a counterexample is
    /// `Some(true)`, a refuted bound is `undecided` (never a proof), a cancelled run is in no list,
    /// an array design is a refusal.
    #[test]
    fn the_sat_bmc_member_finds_counterexamples_never_proves_and_names_every_stop() {
        use std::sync::{Arc, atomic::AtomicBool};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                            5 next 1 3 2\n6 bad 3\n";
        const ARRAY: &str = "1 sort bitvec 1\n2 sort array 1 1\n3 state 2 m\n4 zero 1\n\
                             5 read 1 3 4\n6 bad 5\n";
        let later = || std::time::Instant::now() + std::time::Duration::from_secs(60);
        let go = || Arc::new(AtomicBool::new(false));
        let parse = |s: &str| parser::parse(s).expect("parse");
        assert_eq!(
            run_sat_bmc(&parse(COUNTER), later(), go(), None),
            Ok(Some(true))
        );
        match run_sat_bmc(&parse(SAFE), later(), go(), None) {
            Err(MemberStop::Undecided(p)) => assert!(p.contains("depth 1000"), "{p}"),
            other => panic!("a bounded search must not prove: {other:?}"),
        }
        assert!(
            matches!(
                run_sat_bmc(&parse(SAFE), later(), Arc::new(AtomicBool::new(true)), None),
                Err(MemberStop::Cancelled(p)) if p.starts_with("SAT BMC stopped")
            ),
            "a member stopped on the shared flag says how far it got (mununu#1043)"
        );
        assert!(
            matches!(
                run_sat_bmc(&parse(ARRAY), later(), go(), None),
                Err(MemberStop::Refused(why)) if why.starts_with("sat-bmc:")
            ),
            "an array design is refused by name"
        );
    }

    /// N5.4 at the driver: the SAT BMC member alone decides the counter and is credited by name;
    /// on the safe design the outcome is `Unknown` with its progress in `undecided`.
    #[test]
    fn the_portfolio_credits_the_sat_bmc_member_and_carries_its_bound() {
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                            5 next 1 3 2\n6 bad 3\n";
        let only = ReachMembers {
            exact: false,
            native: false,
            interp: false,
            sat_bmc: true,
            ic3: false,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let run = |src: &str| {
            decide_reach_portfolio_with_members(
                &parser::parse(src).expect("parse"),
                std::time::Duration::from_secs(60),
                None,
                only,
            )
        };
        let reach = run(COUNTER);
        assert_eq!(reach.verdict, ReachVerdict::Reachable, "{reach:?}");
        assert_eq!(reach.reachable_by, vec!["sat-bmc"], "{reach:?}");
        let safe = run(SAFE);
        assert_eq!(safe.verdict, ReachVerdict::Unknown, "{safe:?}");
        assert_eq!(
            safe.undecided.iter().map(|(m, _)| *m).collect::<Vec<_>>(),
            vec!["sat-bmc"],
            "{safe:?}"
        );
        assert!(
            safe.refused.is_empty() && safe.abstained.is_empty(),
            "{safe:?}"
        );
        const { assert!(ReachMembers::ALL.sat_bmc && !ReachMembers::PROOF_DIRECTED.sat_bmc) };
    }

    /// N5.1 — the owned IC3 member's exits, each in its own list: a proof is `Some(false)`, a
    /// counterexample `Some(true)`, a cancelled run is in no list, a stop at the wall is an
    /// abstention naming it, an array design a refusal.
    #[test]
    fn the_ic3_member_proves_refutes_and_names_every_stop() {
        use std::sync::{Arc, atomic::AtomicBool};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                            5 next 1 3 2\n6 bad 3\n";
        const ARRAY: &str = "1 sort bitvec 1\n2 sort array 1 1\n3 state 2 m\n4 zero 1\n\
                             5 read 1 3 4\n6 bad 5\n";
        let later = || std::time::Instant::now() + std::time::Duration::from_secs(60);
        let go = || Arc::new(AtomicBool::new(false));
        let parse = |s: &str| parser::parse(s).expect("parse");
        assert_eq!(
            run_ic3(&parse(COUNTER), later(), go(), &Carried::default(), false),
            Ok(Some(true))
        );
        assert_eq!(
            run_ic3(&parse(SAFE), later(), go(), &Carried::default(), false),
            Ok(Some(false))
        );
        assert!(
            matches!(
                run_ic3(&parse(SAFE), later(), Arc::new(AtomicBool::new(true)), &Carried::default(), false),
                Err(MemberStop::Cancelled(p)) if p.starts_with("IC3 stopped at frame")
            ),
            "a member stopped on the shared flag says how far it got (mununu#1043)"
        );
        match run_ic3(
            &parse(SAFE),
            std::time::Instant::now(),
            go(),
            &Carried::default(),
            false,
        ) {
            Err(MemberStop::Abstained(cause)) => assert!(cause.contains("deadline"), "{cause}"),
            other => panic!("a run past its deadline abstains with the cause: {other:?}"),
        }
        assert!(
            matches!(
                run_ic3(&parse(ARRAY), later(), go(), &Carried::default(), false),
                Err(MemberStop::Refused(why)) if why.starts_with("ic3:")
            ),
            "an array design is refused by name"
        );
    }

    /// N5.1 at the driver: the IC3 member alone decides both the counter (reachable) and the safe
    /// design (unreachable) and is credited by name; it is off in every constant set.
    #[test]
    fn the_portfolio_credits_the_ic3_member_on_both_verdicts() {
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        const SAFE: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                            5 next 1 3 2\n6 bad 3\n";
        let only = ReachMembers {
            exact: false,
            native: false,
            interp: false,
            sat_bmc: false,
            ic3: true,
            btormc: false,
            pono: false,
            ric3: false,
            ric3_bmc: false,
            ric3_engine: ric3::DEFAULT_ENGINE,
            witness: false,
        };
        let run = |src: &str| {
            decide_reach_portfolio_with_members(
                &parser::parse(src).expect("parse"),
                std::time::Duration::from_secs(60),
                None,
                only,
            )
        };
        let reach = run(COUNTER);
        assert_eq!(reach.verdict, ReachVerdict::Reachable, "{reach:?}");
        assert_eq!(reach.reachable_by, vec!["ic3"], "{reach:?}");
        let safe = run(SAFE);
        assert_eq!(safe.verdict, ReachVerdict::Unreachable, "{safe:?}");
        assert_eq!(safe.unreachable_by, vec!["ic3"], "{safe:?}");
        assert!(
            safe.refused.is_empty() && safe.abstained.is_empty() && safe.undecided.is_empty(),
            "{safe:?}"
        );
        const { assert!(ReachMembers::ALL.ic3 && !ReachMembers::PROOF_DIRECTED.ic3) };
        assert!(ReachMembers::ALL.with_ic3(true).ic3);
        assert!(!ReachMembers::ALL.with_ic3(false).ic3);
    }

    /// N5.5 — the default is scoped by the gate's two measurements: on beside no rIC3 (the
    /// owned-only gain), off beside an invokable one (arm C: no row); `1` / `0` force either way,
    /// and `--owned-only` runs it unless `0`. Nothing but the `ic3` flag moves.
    #[test]
    fn the_owned_ic3_default_follows_the_setting_and_the_ric3_probe() {
        use Ic3Default::{Auto, Off, On};
        for (setting, ric3, expect) in [
            (Auto, false, true),
            (Auto, true, false),
            (On, true, true),
            (On, false, true),
            (Off, false, false),
            (Off, true, false),
        ] {
            let m = ReachMembers::shipped_for(setting, ric3);
            assert_eq!(m.ic3, expect, "{setting:?} ric3={ric3}");
            assert_eq!(
                m,
                ReachMembers::ALL.with_ic3(expect),
                "{setting:?} ric3={ric3}"
            );
        }
        let g = crate::adapter::env_var::override_for_test("MUNUNU_REACH_IC3", "0");
        assert_eq!(ic3_default(), Off);
        assert_eq!(owned_members(), OWNED_MEMBERS.to_vec());
        drop(g);
        let g = crate::adapter::env_var::override_for_test("MUNUNU_REACH_IC3", " 1 ");
        assert_eq!(ic3_default(), On);
        assert_eq!(owned_members().last(), Some(&"ic3"));
        drop(g);
        let g = crate::adapter::env_var::unset_for_test("MUNUNU_REACH_IC3");
        assert_eq!(ic3_default(), Auto);
        assert_eq!(owned_members().last(), Some(&"ic3"));
        drop(g);
        let g = crate::adapter::env_var::override_for_test("MUNUNU_REACH_IC3", "yes");
        assert_eq!(ic3_default(), Auto, "any other value reads as unset");
        drop(g);
    }

    #[test]
    fn owned_only_excludes_external_engines_on_small_counter() {
        // The owned-standalone driver decides the small reachable counter via the
        // exact engine and never lists an external member.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = decide_reach_owned_only(&file, 30_000);
        assert_eq!(out.verdict, ReachVerdict::Reachable, "outcome: {out:?}");
        // An owned member decides it. Which one wins is timing-dependent under the
        // early-return collector, so assert only that the decider(s) are owned engines —
        // never an external member.
        assert!(!out.reachable_by.is_empty(), "{out:?}");
        for name in out.reachable_by.iter().chain(out.unreachable_by.iter()) {
            assert!(
                [
                    "exact",
                    "native",
                    "interp",
                    "boolector",
                    "cex",
                    "sat-bmc",
                    "ic3"
                ]
                .contains(name),
                "owned-only listed a non-owned engine {name}: {out:?}"
            );
        }
        for ext in ["btormc", "pono", "ric3", "ric3-bmc"] {
            assert!(
                !out.reachable_by.contains(&ext) && !out.unreachable_by.contains(&ext),
                "owned-only must never list {ext}: {out:?}"
            );
        }
    }

    /// The **owned-standalone ceiling** sweep: for every `*.btor2` in
    /// `MUNUNU_HWMCC_FLAT`, run [`decide_reach_owned_only`] with `MUNUNU_OWNED_TIMEOUT_MS`
    /// (default 240 000) and report how many the owned engines decide *without any
    /// external tool*. Cross-checks each definite verdict against an optional
    /// `MUNUNU_HWMCC_GT` (`basename safe|unsafe` per line) and fails on any mismatch —
    /// the soundness net. `#[ignore]`d (reads an external corpus, minutes-to-hours).
    #[test]
    #[ignore = "manual: owned-only HWMCC ceiling sweep; set MUNUNU_HWMCC_FLAT (+ optional MUNUNU_HWMCC_GT), MUNUNU_OWNED_TIMEOUT_MS default 240000"]
    fn owned_only_hwmcc_ceiling() {
        let Ok(flat) = std::env::var("MUNUNU_HWMCC_FLAT") else {
            eprintln!("SKIPPED: external-corpus sweep — set MUNUNU_HWMCC_FLAT to run it.");
            return;
        };
        let to_ms: u32 = std::env::var("MUNUNU_OWNED_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(240_000);
        let gt: std::collections::HashMap<String, String> = std::env::var("MUNUNU_HWMCC_GT")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let mut it = l.split_whitespace();
                        Some((it.next()?.to_string(), it.next()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut files: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&flat)
            .expect("read MUNUNU_HWMCC_FLAT dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "btor2"))
            .map(|p| {
                (
                    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX),
                    p,
                )
            })
            .collect();
        // Small-file-first: the owned-decidable designs skew small, so the decide
        // count stabilises early and the large-undecided tail just confirms itself.
        files.sort();
        let files: Vec<std::path::PathBuf> = files.into_iter().map(|(_, p)| p).collect();
        let (mut decided, mut safe, mut unsafe_n) = (0u32, 0u32, 0u32);
        let mut violations: Vec<String> = Vec::new();
        for p in &files {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let content = std::fs::read_to_string(p).unwrap();
            let file = match crate::adapter::btor2::parser::parse(&content) {
                Ok(f) => f,
                Err(_) => {
                    eprintln!("  ----  parse-error       {name}");
                    continue;
                }
            };
            let t0 = std::time::Instant::now();
            let out = decide_reach_owned_only(&file, to_ms);
            let secs = t0.elapsed().as_secs();
            let (label, eng) = match out.verdict {
                ReachVerdict::Reachable => ("unsafe", out.reachable_by.join("+")),
                ReachVerdict::Unreachable => ("safe", out.unreachable_by.join("+")),
                ReachVerdict::Unknown => ("unknown", String::new()),
                ReachVerdict::Contradiction => ("CONTRADICTION", String::new()),
            };
            if label == "safe" || label == "unsafe" {
                decided += 1;
                if label == "safe" {
                    safe += 1;
                } else {
                    unsafe_n += 1;
                }
                let gtv = gt.get(&name).map(String::as_str).unwrap_or("?");
                let sound = gtv == "?" || gtv == label;
                if !sound {
                    violations.push(format!("{name}: gt={gtv} owned={label}"));
                }
                eprintln!(
                    "{} {secs:>4}s  {label:<7} via {eng:<8} gt={gtv:<7} {name}",
                    if sound { "✓" } else { "✗✗✗" }
                );
            } else {
                eprintln!("  {secs:>4}s  {label:<7}          {name}");
            }
        }
        eprintln!(
            "\nOWNED-ONLY CEILING @ {to_ms}ms: {decided}/{} decided ({safe} safe, {unsafe_n} unsafe); soundness violations {}",
            files.len(),
            violations.len()
        );
        assert!(
            violations.is_empty(),
            "SOUNDNESS VIOLATIONS: {violations:?}"
        );
    }

    // mununu#803 — the owned driver keeps a member's refusal beside the verdict, as the scoped
    // driver does: the native encoder declines a memory initialised from an array-sorted input (it
    // encodes memories since N5, not that `init`), and `refused` says so.
    #[test]
    fn the_owned_driver_records_the_native_members_refusal() {
        const ARRAY_STATE: &str = "1 sort bitvec 2\n2 sort bitvec 4\n3 sort array 1 2\n4 state 3 mem\n5 next 3 4 4\n6 sort bitvec 1\n7 input 6 i\n8 bad 7\n9 input 3 seed\n10 init 3 4 9\n";
        let file = crate::adapter::btor2::parser::parse(ARRAY_STATE).expect("parse");
        let out = decide_reach_owned_only(&file, 3_000);
        let members: Vec<&str> = out.refused.iter().map(|(m, _)| *m).collect();
        assert!(
            members.contains(&"native"),
            "the native member's refusal is recorded; refused = {:?}, verdict = {:?}",
            out.refused,
            out.verdict
        );
        let (_, why) = out
            .refused
            .iter()
            .find(|(m, _)| *m == "native")
            .expect("native");
        assert!(why.contains("encode"), "{why}");
    }

    /// N5.4 — the row that motivated the carry (HWMCC'20 `circular_pointer_top_w16_d64_e0`: only
    /// `ric3-bmc` decides it, at ~168 s on the box; the re-derivation reached depth 16 in the
    /// 300 s budget, mununu#1045). With `witness` on, the portfolio ships `ric3-bmc`'s own
    /// witness, replayed on the original file through the sticky-model prefix replay.
    #[test]
    #[ignore = "needs rIC3 and MUNUNU_HWMCC_FLAT (the HWMCC'20 flat bv directory); mununu-sva"]
    fn e2e_circular_pointer_w16_d64_carries_the_ric3_bmc_trace() {
        let Ok(flat) = std::env::var("MUNUNU_HWMCC_FLAT") else {
            eprintln!("MUNUNU_HWMCC_FLAT unset — skipped");
            return;
        };
        let text = std::fs::read_to_string(format!("{flat}/circular_pointer_top_w16_d64_e0.btor2"))
            .unwrap();
        let file = parser::parse(&text).unwrap();
        let members = ReachMembers {
            ric3_bmc: true,
            witness: true,
            ..ReachMembers::TRACE_CARRIERS
                .without("sat-bmc")
                .without("ic3")
                .without("btormc")
                .without("ric3")
        };
        let started = std::time::Instant::now();
        let out = decide_reach_portfolio_with_members(
            &file,
            std::time::Duration::from_secs(420),
            None,
            members,
        );
        let elapsed = started.elapsed();
        assert_eq!(out.verdict, ReachVerdict::Reachable, "{out:?}");
        assert!(
            out.trace_replay_failed.is_empty(),
            "{:?}",
            out.trace_replay_failed
        );
        let carried = out.trace.expect("ric3-bmc carried its replayed witness");
        assert_eq!(carried.by, "ric3-bmc");
        eprintln!(
            "circular_pointer w16_d64: depth {} in {:.1} s",
            carried.depth(),
            elapsed.as_secs_f64()
        );
        assert!(carried.depth() >= 1);
    }

    /// Scripted members: the every-host half of the race tests (`os-dependent-test-expectation`
    /// in `docs/defect-classes.md`). A stub either reports at once or blocks until the shared
    /// stop flag is up, which is what "still running when a peer decided, or when the clock
    /// fired" is from the driver's side. Which members have reported when the first verdict
    /// lands is then the test's choice and not the scheduler's, and the whole outcome is
    /// compared. Settling, the stop flag, the join and the merge are the shipped driver's. The
    /// detector that races the real engines stays: `portfolio_decides_reachable_without_subprocess`.
    mod scripted {
        use super::*;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        use std::time::{Duration, Instant};

        /// A wall no scripted run reaches: every script ends on its own.
        const NO_WALL: Duration = Duration::from_secs(600);

        fn counter() -> Btor2File {
            parser::parse(COUNTER_TO_SEVEN).unwrap()
        }

        fn until_stopped(stop: &AtomicBool) {
            while !stop.load(Relaxed) {
                std::thread::yield_now();
            }
        }

        /// The exact member inside one long BDD apply: it never returns. The thread stays parked
        /// (the shipped driver leaves a real one running the same way); the test process ends it.
        pub(super) fn exact_never_returns(_: &str) -> Result<Option<bool>, String> {
            loop {
                std::thread::park();
            }
        }

        fn native_stopped(
            _: &Btor2File,
            stop: Option<&AtomicBool>,
            _: u32,
            _: Option<Instant>,
        ) -> Result<Option<bool>, MemberStop> {
            until_stopped(stop.expect("the driver passes its stop flag"));
            Err(MemberStop::Cancelled("native stopped at depth 3".into()))
        }

        fn owned_stopped(
            _: &Btor2File,
            _: Instant,
            stop: Arc<AtomicBool>,
            _: Option<&Carried>,
        ) -> Result<Option<bool>, MemberStop> {
            until_stopped(&stop);
            Err(MemberStop::Cancelled("stopped at frame 2".into()))
        }

        fn ic3_stopped(
            f: &Btor2File,
            d: Instant,
            stop: Arc<AtomicBool>,
            _: &Carried,
            _: bool,
        ) -> Result<Option<bool>, MemberStop> {
            owned_stopped(f, d, stop, None)
        }

        fn interp_stopped(
            _: &Btor2File,
            stop: &AtomicBool,
            _: MemberTimeouts,
        ) -> InterpSafetyVerdict {
            until_stopped(stop);
            InterpSafetyVerdict::Undecided {
                reason: "the search was cancelled".into(),
            }
        }

        /// A killed child: `Unknown` once the stop flag is up.
        fn subprocess_stopped(
            _: &Btor2File,
            _: Duration,
            stop: &AtomicBool,
            _: Option<&Carried>,
        ) -> Result<Option<McVerdict>, String> {
            until_stopped(stop);
            Ok(Some(McVerdict::Unknown))
        }

        /// Every member still running: each reports as a stopped engine does once the stop flag
        /// is up, and the exact member never returns.
        const ALL_RUNNING: Runners = Runners {
            exact: exact_never_returns,
            native: native_stopped,
            sat_bmc: owned_stopped,
            ic3: ic3_stopped,
            interp_available: || Ok(()),
            interp_search: interp_stopped,
            btormc: |f, _, t, stop, c| subprocess_stopped(f, t, stop, c),
            pono: |f, t, stop| subprocess_stopped(f, t, stop, None),
            ric3: |f, _, t, stop, c| subprocess_stopped(f, t, stop, c),
            ric3_bmc: subprocess_stopped,
            #[cfg(not(feature = "boolector"))]
            cex: |_, _, stop| {
                until_stopped(stop);
                Ok(None)
            },
        };

        fn run(runners: &Runners) -> ReachOutcome {
            decide_with_members_using(&counter(), NO_WALL, None, ReachMembers::ALL, runners)
        }

        fn names(notes: &[(&'static str, String)]) -> Vec<&'static str> {
            notes.iter().map(|(m, _)| *m).collect()
        }

        /// Whichever member decides first is credited alone, and every member its verdict
        /// stopped is in no list: a cancelled owned member, a cancelled interpolation search, a
        /// killed child and the abandoned exact member alike. (The interpolation member is not
        /// a row: its verdict does not raise the stop flag, so its peers run on.)
        #[test]
        fn the_first_verdict_is_credited_alone_and_a_member_it_stopped_is_in_no_list() {
            let reach = |m| ReachOutcome::from_sets(vec![m], vec![]);
            let safe = |m| ReachOutcome::from_sets(vec![], vec![m]);
            let cases: [(Runners, ReachOutcome); 9] = [
                (
                    Runners {
                        exact: |_| Ok(Some(true)),
                        ..ALL_RUNNING
                    },
                    reach("exact"),
                ),
                (
                    Runners {
                        native: |_, _, _, _| Ok(Some(false)),
                        ..ALL_RUNNING
                    },
                    safe("native"),
                ),
                (
                    Runners {
                        sat_bmc: |_, _, _, _| Ok(Some(true)),
                        ..ALL_RUNNING
                    },
                    reach("sat-bmc"),
                ),
                (
                    Runners {
                        ic3: |_, _, _, _, _| Ok(Some(false)),
                        ..ALL_RUNNING
                    },
                    safe("ic3"),
                ),
                (
                    Runners {
                        ic3: |_, _, _, _, _| Ok(Some(true)),
                        ..ALL_RUNNING
                    },
                    reach("ic3"),
                ),
                (
                    Runners {
                        btormc: |_, _, _, _, _| Ok(Some(McVerdict::Violated)),
                        ..ALL_RUNNING
                    },
                    reach("btormc"),
                ),
                (
                    Runners {
                        pono: |_, _, _| Ok(Some(McVerdict::Safe)),
                        ..ALL_RUNNING
                    },
                    safe("pono"),
                ),
                (
                    Runners {
                        ric3: |_, _, _, _, _| Ok(Some(McVerdict::Safe)),
                        ..ALL_RUNNING
                    },
                    safe("ric3"),
                ),
                (
                    Runners {
                        ric3_bmc: |_, _, _, _| Ok(Some(McVerdict::Violated)),
                        ..ALL_RUNNING
                    },
                    reach("ric3-bmc"),
                ),
            ];
            for (runners, want) in &cases {
                assert_eq!(&run(runners), want);
            }
        }

        /// A member that reports its own verdict after the first one is in (the scoped join
        /// waits for it) is credited beside the first, in the fixed engine order; when it
        /// disagrees, the outcome is the contradiction alarm naming both sides.
        #[test]
        fn a_verdict_that_lands_after_the_first_is_credited_and_a_disagreement_is_the_alarm() {
            let agreeing = Runners {
                native: |_, _, _, _| Ok(Some(true)),
                ic3: |_, _, stop, _| {
                    until_stopped(&stop);
                    Ok(Some(true))
                },
                pono: |_, _, stop| {
                    until_stopped(stop);
                    Ok(Some(McVerdict::Violated))
                },
                ..ALL_RUNNING
            };
            assert_eq!(
                run(&agreeing),
                ReachOutcome::from_sets(vec!["native", "ic3", "pono"], vec![])
            );
            let disagreeing = Runners {
                native: |_, _, _, _| Ok(Some(true)),
                ric3: |_, _, _, stop, _| {
                    until_stopped(stop);
                    Ok(Some(McVerdict::Safe))
                },
                ..ALL_RUNNING
            };
            let out = run(&disagreeing);
            assert_eq!(out.verdict, ReachVerdict::Contradiction);
            assert_eq!(out, ReachOutcome::from_sets(vec!["native"], vec!["ric3"]));
        }

        /// With no verdict, every member that ended on its own reaches the outcome under its
        /// own stop: a refusal, a budget abstention and a search to the bound each in their
        /// list, in the fixed engine order. Nothing raises the stop flag in this script.
        #[test]
        fn with_no_verdict_each_member_is_listed_under_its_own_stop() {
            let stops = Runners {
                exact: |_| Err("exact: over the cap".into()),
                native: |_, _, _, _| Err(MemberStop::Undecided("searched to depth 40".into())),
                sat_bmc: |_, _, _, _| Err(MemberStop::Abstained("conflict budget spent".into())),
                ic3: |_, _, _, _, _| Err(MemberStop::Refused("ic3: arrays".into())),
                interp_available: || Err("no cvc5".into()),
                interp_search: |_, _, _| unreachable!("no search without cvc5"),
                btormc: |_, _, _, _, _| Ok(Some(McVerdict::Unknown)),
                pono: |_, _, _| Err("pono: not found".into()),
                ric3: |_, _, _, _, _| Ok(Some(McVerdict::Unknown)),
                ric3_bmc: |_, _, _, _| Err("ric3: not found".into()),
                #[cfg(not(feature = "boolector"))]
                cex: |_, _, _| Ok(None),
            };
            let out = run(&stops);
            assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
            let note = |m, why: &str| (m, why.to_string());
            assert_eq!(
                out.refused,
                vec![
                    note("exact", "exact: over the cap"),
                    note("interp", "no cvc5"),
                    note("ic3", "ic3: arrays"),
                    note("pono", "pono: not found"),
                    note("ric3-bmc", "ric3: not found"),
                ]
            );
            assert_eq!(out.undecided, vec![note("native", "searched to depth 40")]);
            // A child that ended undecided with nothing decided is a budget stop naming its budget.
            assert_eq!(names(&out.abstained), vec!["sat-bmc", "btormc", "ric3"]);
            assert_eq!(out.abstained[0].1, "conflict budget spent");
            let budget = NO_WALL.as_millis().to_string();
            assert!(
                out.abstained[1..]
                    .iter()
                    .all(|(_, why)| why.contains(&budget)),
                "{out:?}"
            );
            assert_eq!((out.trace, out.trace_replay_failed), (None, Vec::new()));
        }

        /// mununu#1039 — the portfolio's wall stops every member still running, and each is a
        /// budget abstention: none is silent, none is a refusal or a search to its bound.
        #[test]
        fn the_wall_makes_every_running_member_a_budget_abstention() {
            let out = decide_with_members_using(
                &counter(),
                Duration::from_millis(50),
                None,
                ReachMembers::ALL,
                &ALL_RUNNING,
            );
            assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
            assert_eq!(names(&out.abstained), ReachMembers::ALL.names(), "{out:?}");
            assert!(
                out.refused.is_empty() && out.undecided.is_empty(),
                "{out:?}"
            );
        }

        /// mununu#1043 — the run budget stops them the same way, and no cause blames the wall.
        /// The budget is spent from inside a member (each member thread re-enters it), so no
        /// clock decides when.
        #[test]
        fn the_run_budget_makes_every_running_member_a_budget_abstention() {
            use crate::adapter::run_budget::{Budget, current, enter};
            let spent = Runners {
                native: |f, stop, query_ms, wall| {
                    current().cancel();
                    native_stopped(f, stop, query_ms, wall)
                },
                ..ALL_RUNNING
            };
            let budget = Budget::unbounded();
            let out = {
                let _g = enter(&budget);
                run(&spent)
            };
            assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
            assert_eq!(names(&out.abstained), ReachMembers::ALL.names(), "{out:?}");
            assert!(
                out.refused.is_empty() && out.undecided.is_empty(),
                "{out:?}"
            );
            let wall = clock_sweep_cause(true, NO_WALL.as_millis());
            assert!(out.abstained.iter().all(|(_, why)| *why != wall), "{out:?}");
        }

        /// The caller's cancellation (another rung won) stops every member and names none: it
        /// is neither a verdict nor a clock.
        #[test]
        fn a_callers_cancellation_names_no_member() {
            let cancel = AtomicBool::new(true);
            let out = decide_with_members_using(
                &counter(),
                NO_WALL,
                Some(&cancel),
                ReachMembers::ALL,
                &ALL_RUNNING,
            );
            assert_eq!(out, ReachOutcome::default());
        }

        /// Every member would decide at once.
        const ALL_DECIDE: Runners = Runners {
            exact: |_| Ok(Some(true)),
            native: |_, _, _, _| Ok(Some(true)),
            sat_bmc: |_, _, _, _| Ok(Some(true)),
            ic3: |_, _, _, _, _| Ok(Some(true)),
            interp_available: || Ok(()),
            interp_search: |_, _, _| InterpSafetyVerdict::Unsafe { depth: 1 },
            btormc: |_, _, _, _, _| Ok(Some(McVerdict::Violated)),
            pono: |_, _, _| Ok(Some(McVerdict::Violated)),
            ric3: |_, _, _, _, _| Ok(Some(McVerdict::Violated)),
            ric3_bmc: |_, _, _, _| Ok(Some(McVerdict::Violated)),
            #[cfg(not(feature = "boolector"))]
            cex: |_, _, _| Ok(Some(1)),
        };

        /// mununu#726 — under an already-expired budget no member is asked: every runner here
        /// would decide at once, and the outcome credits none. The control is the same runners
        /// with no budget.
        #[test]
        fn an_expired_budget_starts_no_member() {
            use crate::adapter::run_budget::{Budget, enter};
            let out = {
                let _g = enter(&Budget::with_ms(0));
                run(&ALL_DECIDE)
            };
            assert_eq!(out, ReachOutcome::default());
            assert_eq!(run(&ALL_DECIDE).verdict, ReachVerdict::Reachable);
        }

        /// The owned-only driver under scripted members. Its detectors, which race the real
        /// engines, are `owned_only_excludes_external_engines_on_small_counter` and
        /// `the_owned_only_driver_carries_a_trace_when_asked`. (Not under the `boolector`
        /// feature: that member has no runner and would decide the fixture itself.)
        #[cfg(not(feature = "boolector"))]
        mod owned {
            use super::*;

            /// A deadline no scripted run reaches.
            const NO_DEADLINE: u32 = 600_000;

            /// A member that never returns. The collector's deadline raises no flag, so a stub
            /// waiting for one would spin on after the test; this one stays parked.
            fn stuck() -> ! {
                loop {
                    std::thread::park();
                }
            }

            /// Every owned-only member still running, whatever happens.
            const ALL_STUCK: Runners = Runners {
                exact: |_| stuck(),
                native: |_, _, _, _| stuck(),
                sat_bmc: |_, _, _, _| stuck(),
                ic3: |_, _, _, _, _| stuck(),
                interp_search: |_, _, _| stuck(),
                cex: |_, _, _| stuck(),
                ..ALL_RUNNING
            };

            fn run(runners: &Runners) -> ReachOutcome {
                decide_owned_only_using(&counter(), NO_DEADLINE, false, runners)
            }

            /// The driver starts `ic3` unless `MUNUNU_REACH_IC3=0`; its rows follow the setting.
            fn ic3_starts() -> bool {
                owned_members().contains(&"ic3")
            }

            /// This driver's lists are in finishing order; compared sorted.
            fn sorted(mut notes: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
                notes.sort();
                notes
            }

            #[test]
            fn the_first_verdict_is_credited_alone_and_a_member_it_stopped_is_in_no_list() {
                let reach = |m| ReachOutcome::from_sets(vec![m], vec![]);
                let safe = |m| ReachOutcome::from_sets(vec![], vec![m]);
                let mut cases = vec![
                    (
                        Runners {
                            exact: |_| Ok(Some(true)),
                            ..ALL_RUNNING
                        },
                        reach("exact"),
                    ),
                    (
                        Runners {
                            native: |_, _, _, _| Ok(Some(false)),
                            ..ALL_RUNNING
                        },
                        safe("native"),
                    ),
                    (
                        Runners {
                            interp_search: |_, _, _| InterpSafetyVerdict::Safe { iterations: 1 },
                            ..ALL_RUNNING
                        },
                        safe("interp"),
                    ),
                    (
                        Runners {
                            cex: |_, _, _| Ok(Some(3)),
                            ..ALL_RUNNING
                        },
                        reach("cex"),
                    ),
                    (
                        Runners {
                            sat_bmc: |_, _, _, _| Ok(Some(true)),
                            ..ALL_RUNNING
                        },
                        reach("sat-bmc"),
                    ),
                ];
                if ic3_starts() {
                    cases.push((
                        Runners {
                            ic3: |_, _, _, _, _| Ok(Some(false)),
                            ..ALL_RUNNING
                        },
                        safe("ic3"),
                    ));
                }
                for (runners, want) in &cases {
                    assert_eq!(&run(runners), want);
                }
            }

            /// With no verdict and every member ended on its own, each is listed under its own
            /// stop, and none is named as still running.
            #[test]
            fn with_no_verdict_each_member_is_listed_under_its_own_stop() {
                let stops = Runners {
                    exact: |_| Err("exact: over the cap".into()),
                    native: |_, _, _, _| Err(MemberStop::Undecided("searched to depth 40".into())),
                    sat_bmc: |_, _, _, _| Err(MemberStop::Refused("sat-bmc: arrays".into())),
                    ic3: |_, _, _, _, _| Err(MemberStop::Abstained("frame budget spent".into())),
                    interp_available: || Err("no cvc5".into()),
                    cex: |_, _, _| Err(MemberStop::Abstained("query budget spent".into())),
                    ..ALL_RUNNING
                };
                let out = run(&stops);
                let note = |m, why: &str| (m, why.to_string());
                assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
                assert_eq!(
                    sorted(out.refused),
                    vec![
                        note("exact", "exact: over the cap"),
                        note("interp", "no cvc5"),
                        note("sat-bmc", "sat-bmc: arrays"),
                    ]
                );
                assert_eq!(out.undecided, vec![note("native", "searched to depth 40")]);
                let mut abstained = vec![note("cex", "query budget spent")];
                if ic3_starts() {
                    abstained.push(note("ic3", "frame budget spent"));
                }
                assert_eq!(sorted(out.abstained), abstained);
            }

            /// mununu#1022 — at the deadline, every member that has not reported is named, with
            /// the cause the collector gives (`unreported_at_deadline`).
            #[test]
            fn at_the_deadline_every_member_still_running_is_named() {
                let out = decide_owned_only_using(&counter(), 50, false, &ALL_STUCK);
                assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
                assert!(
                    out.refused.is_empty() && out.undecided.is_empty(),
                    "{out:?}"
                );
                assert_eq!(
                    sorted(out.abstained),
                    sorted(unreported_at_deadline(&owned_members(), &[], 50))
                );
            }

            /// N5.4 — a carrier that decides alone (the real engine; every other member still
            /// running) carries its own depth-7 trace when asked, and nothing unasked.
            #[test]
            fn a_carrier_that_decides_carries_its_trace_only_when_asked() {
                let carriers = [
                    (
                        "sat-bmc",
                        Runners {
                            sat_bmc: run_sat_bmc,
                            ..ALL_STUCK
                        },
                    ),
                    (
                        "ic3",
                        Runners {
                            ic3: run_ic3,
                            ..ALL_STUCK
                        },
                    ),
                ];
                for (name, runners) in &carriers {
                    if *name == "ic3" && !ic3_starts() {
                        continue;
                    }
                    let with = decide_owned_only_using(&counter(), NO_DEADLINE, true, runners);
                    assert_eq!(with.reachable_by, vec![*name], "{with:?}");
                    let carried = with.trace.as_ref().expect("the decider carried its trace");
                    assert_eq!((carried.by, carried.depth()), (*name, 7), "{with:?}");
                    assert!(with.trace_replay_failed.is_empty(), "{with:?}");
                    assert_eq!(
                        decide_owned_only_using(&counter(), NO_DEADLINE, false, runners),
                        ReachOutcome::from_sets(vec![*name], vec![])
                    );
                }
            }

            /// mununu#816 — under an already-expired budget no member is asked (as the full
            /// driver's test of the same name).
            #[test]
            fn an_expired_budget_starts_no_member() {
                use crate::adapter::run_budget::{Budget, enter};
                let out = {
                    let _g = enter(&Budget::with_ms(0));
                    run(&ALL_DECIDE)
                };
                assert_eq!(out, ReachOutcome::default());
                assert_eq!(run(&ALL_DECIDE).verdict, ReachVerdict::Reachable);
            }
        }
    }
}
