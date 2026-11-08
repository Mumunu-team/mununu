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
//! - **rIC3** (bit-level IC3/PDR) on the inductive-invariant safety class.
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
use crate::adapter::btor2::native_bmc::{self, SafetyVerdict};
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
        }
    }
}

/// The members' individual results for one portfolio run, named — the input of [`collect`]. A
/// `None` is an abstention (unknown, timeout, cancelled); a refusal is recorded in `refused` beside
/// the `None`. Adding a member is adding a field, not a positional argument.
#[derive(Debug, Default)]
pub(crate) struct Members {
    pub exact: Option<bool>,
    pub native: Option<bool>,
    pub interp: Option<bool>,
    pub btormc: Option<McVerdict>,
    pub pono: Option<McVerdict>,
    /// Plan 4 / PR D2 — rIC3 `-e ic3`, bit-level IC3/PDR over the emitted BTOR2 (mununu#736).
    pub ric3: Option<McVerdict>,
    /// `(member, reason)` for every member that refused the design (see [`ReachOutcome::refused`]).
    pub refused: Vec<(&'static str, String)>,
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
) -> Result<Option<bool>, String> {
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
            Ok(SafetyVerdict::Unknown { .. }) => Ok(None),
            Err(e) => Err(e.to_string()),
        },
    )
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
fn run_interp(
    file: &Btor2File,
    cancel: &std::sync::atomic::AtomicBool,
    query_ms: u32,
    overall_ms: u64,
) -> Option<bool> {
    timed(
        "interp",
        || match native_interp::verify_safety_interp_cancellable(
            file,
            INTERP_MAX_SUFFIX,
            INTERP_MAX_ITERS,
            query_ms,
            overall_ms,
            cancel,
        ) {
            InterpSafetyVerdict::Unsafe { .. } => Some(true),
            InterpSafetyVerdict::Safe { .. } => Some(false),
            InterpSafetyVerdict::Undecided { .. } => None,
        },
    )
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
pub struct MemberTimeouts {
    pub native_query_ms: u32,
    pub interp_query_ms: u32,
    pub interp_overall_ms: u64,
    pub subprocess: std::time::Duration,
}

/// The full portfolio's member budgets (mununu#892). With a run budget in effect every member
/// follows the time left, as before. Without one, a `--timeout-ms` RAISED above the subprocess
/// default is read as the member budget for the owned members too — native's per-query bound and
/// interpolation's bounds — the way `--owned-only` already computes it. Until this, the raised
/// wall reached only the subprocess members and btormc's depth cap, and native stayed at its 5 s
/// per-query default: two HWMCC'20 array rows were ⊥ in the full run with ten minutes of
/// subprocess time and decided by native alone with sixty seconds of query time. At the default
/// wall nothing changes.
pub fn portfolio_member_timeouts(
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
            native_query_ms: native_bmc::DEFAULT_TIMEOUT_MS,
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
    ] {
        match v {
            Some(true) => reachable_by.push(name),
            Some(false) => unreachable_by.push(name),
            None => {}
        }
    }
    // Subprocess members report an `McVerdict`.
    for (name, v) in [("btormc", m.btormc), ("pono", m.pono), ("ric3", m.ric3)] {
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
    let rank = |member: &str| {
        ["exact", "native", "interp", "btormc", "pono", "ric3"]
            .iter()
            .position(|m| *m == member)
            .unwrap_or(usize::MAX)
    };
    let mut refused = m.refused;
    refused.sort_by(|(a, _), (b, _)| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out.refused = refused;
    out
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
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
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
    let decided = AtomicBool::new(false);
    let decided = &decided;
    let finished = AtomicBool::new(false);
    let finished = &finished;
    let budget = &budget;
    // mununu#734 — every member's refusal lands here, in finishing order; `collect` carries them.
    let refused: std::sync::Mutex<Vec<(&'static str, String)>> = std::sync::Mutex::new(Vec::new());
    let refused = &refused;
    let mut members = std::thread::scope(|scope| {
        // Forward an external cancellation — or the budget's expiry — into `decided`, which
        // every cancellable member already polls. Stops when the members are joined.
        if cancel.is_some() || budget.remaining_ms().is_some() {
            scope.spawn(move || {
                while !finished.load(Relaxed) {
                    if cancel.is_some_and(|c| c.load(Relaxed)) || budget.expired() {
                        decided.store(true, Relaxed);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            });
        }
        let btormc_h = scope.spawn(move || {
            let _g = crate::adapter::run_budget::enter(budget);
            let v = timed("btormc", || {
                btormc::decide_via_btormc(file, btormc_kmax, subprocess_timeout, Some(decided))
                    .map(Some)
                    .map_err(|e| e.message)
            });
            let v = settle(refused, "btormc", v);
            if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                decided.store(true, Relaxed);
            }
            v
        });
        let pono_h = scope.spawn(move || {
            let _g = crate::adapter::run_budget::enter(budget);
            let v = timed("pono", || {
                pono::decide_via_pono(
                    file,
                    pono::DEFAULT_ENGINE,
                    subprocess_timeout,
                    Some(decided),
                )
                .map(Some)
                .map_err(|e| e.message)
            });
            let v = settle(refused, "pono", v);
            if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                decided.store(true, Relaxed);
            }
            v
        });
        // Plan 4 / PR D2 (mununu#736) — rIC3's bit-level IC3: the member that proves the
        // inductive-invariant safety class (OpenTitan's duplicated-counter fault-flag chains, which
        // k-induction cannot close from a free step state).
        // Trusted like btormc / pono: it reads the same emitted BTOR2 and answers with a certificate-
        // capable verdict; the HWMCC adjudication ran clean before it was promoted.
        let ric3_h = scope.spawn(move || {
            let _g = crate::adapter::run_budget::enter(budget);
            let v = timed("ric3", || {
                ric3::decide_via_ric3(
                    file,
                    ric3::DEFAULT_ENGINE,
                    subprocess_timeout,
                    Some(decided),
                )
                .map(Some)
                .map_err(|e| e.message)
            });
            let v = settle(refused, "ric3", v);
            if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                decided.store(true, Relaxed);
            }
            v
        });
        // The native engine is in-process Z3 work — on its own
        // thread so it overlaps the exact engine + the subprocess members rather than
        // serialising.
        let native_h = scope.spawn(move || {
            let _g = crate::adapter::run_budget::enter(budget);
            let v = settle(
                refused,
                "native",
                run_native(file, Some(decided), t.native_query_ms),
            );
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            v
        });
        // Owned McMillan interpolation — now a FIRST-CLASS concurrent member (was
        // last-resort). Polls `decided` and bails out early once a faster member decides.
        let interp_h = scope.spawn(move || {
            let _g = crate::adapter::run_budget::enter(budget);
            run_interp(file, decided, t.interp_query_ms, t.interp_overall_ms)
        });
        // The exact engine is in-process BDD work — run it on this thread while the
        // other members run on theirs.
        let exact = settle(refused, "exact", run_exact(&content));
        if exact.is_some() {
            decided.store(true, Relaxed);
        }
        // A panicked member thread abstains (None) rather than poisoning the merge.
        let native = native_h.join().unwrap_or(None);
        let btormc = btormc_h.join().unwrap_or(None);
        let pono = pono_h.join().unwrap_or(None);
        let ric3 = ric3_h.join().unwrap_or(None);
        let interp = interp_h.join().unwrap_or(None);
        finished.store(true, Relaxed);
        Members {
            exact,
            native,
            interp,
            btormc,
            pono,
            ric3,
            refused: Vec::new(),
        }
    });
    // A poisoned lock (a member panicked while recording) yields no refusal list; the panic is
    // the louder signal and the verdict never depends on the list (mununu#803, LOW).
    members.refused = refused.lock().map(|g| g.clone()).unwrap_or_default();
    collect(members)
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
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::{Arc, mpsc};
    // mununu#803 — a member's refusal (an `Err`, not an abstention) is kept here as the scoped
    // driver keeps it; the detached threads share the list through an `Arc`.
    let refused: Arc<std::sync::Mutex<Vec<(&'static str, String)>>> = Arc::default();
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
    let (tx, rx) = mpsc::channel::<(&'static str, Option<bool>)>();

    // Safety proof — shallow native BMC + k-induction.
    {
        let (file, decided, tx, refused) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
        );
        let budget = budget.clone();
        std::thread::spawn(move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let v = timed(
                "native",
                || match native_bmc::decide_bad_safety_cancellable(
                    &file,
                    native_bmc::DEFAULT_MAX_K,
                    Some(timeout_ms),
                    Some(&decided),
                ) {
                    Ok(SafetyVerdict::Violated { .. }) => Some(true),
                    Ok(SafetyVerdict::Safe { .. }) => Some(false),
                    Ok(SafetyVerdict::Unknown { .. }) => None,
                    Err(e) => {
                        note_refusal(&refused, "native", e.to_string());
                        None
                    }
                },
            );
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("native", v));
        });
    }
    // Safety proof — McMillan interpolation.
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        let budget = budget.clone();
        std::thread::spawn(move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let v = timed(
                "interp",
                || match native_interp::verify_safety_interp_cancellable(
                    &file,
                    INTERP_MAX_SUFFIX,
                    64,
                    INTERP_QUERY_TIMEOUT_MS,
                    u64::from(timeout_ms),
                    &decided,
                ) {
                    InterpSafetyVerdict::Unsafe { .. } => Some(true),
                    InterpSafetyVerdict::Safe { .. } => Some(false),
                    InterpSafetyVerdict::Undecided { .. } => None,
                },
            );
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("interp", v));
        });
    }
    // Deep BV counterexample search. With the `boolector` feature the in-process Boolector
    // BMC fills this role (faster — measured ~56 s vs the z3 search timing out on
    // `krebs.3`); without it, the z3 deep-CEX search. Only ever contributes `Violated`.
    #[cfg(feature = "boolector")]
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        let budget = budget.clone();
        std::thread::spawn(move || {
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
            let _ = tx.send(("boolector", v));
        });
    }
    #[cfg(not(feature = "boolector"))]
    {
        let (file, decided, tx, refused) = (
            Arc::clone(&file),
            Arc::clone(&decided),
            tx.clone(),
            Arc::clone(&refused),
        );
        let budget = budget.clone();
        std::thread::spawn(move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            let hit = timed("cex", || {
                match native_bmc::bmc_cex_until(
                    &file,
                    OWNED_DEEP_CEX_MAX_K,
                    OWNED_CEX_QUERY_MS,
                    deadline,
                    &decided,
                ) {
                    Ok(hit) => hit.is_some(),
                    Err(why) => {
                        note_refusal(&refused, "cex", why);
                        false
                    }
                }
            });
            if hit {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("cex", if hit { Some(true) } else { None }));
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
        std::thread::spawn(move || {
            let _g = crate::adapter::run_budget::enter(&budget);
            // mununu#873 — a refusal (over-cap cone, free init) is an abstention AND a recorded
            // reason, as in the scoped driver; it used to be dropped here.
            let v = settle(&refused, "exact", run_exact(&content));
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("exact", v));
        });
    }
    drop(tx); // the collector's `rx` disconnects once every member thread has finished.

    // Four members: native, interp, {boolector | z3-cex}, exact.
    const N_MEMBERS: usize = 4;
    let mut reachable_by: Vec<&'static str> = Vec::new();
    let mut unreachable_by: Vec<&'static str> = Vec::new();
    let mut reports = 0usize;
    let mut definite = false;
    // Wait for a definite verdict, or all members to abstain, or the deadline.
    while reports < N_MEMBERS && !definite {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok((name, v)) => {
                reports += 1;
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
            // Timeout (deadline) or all members finished (disconnected) — stop waiting.
            Err(_) => break,
        }
    }
    // A definite verdict is in. Give the OTHER members a SHORT bounded grace to finish so a
    // co-completing CONTRADICTION (the inter-engine safety-net — one engine Reachable, another
    // Unreachable ⇒ a latent single-engine bug) is still surfaced. We do NOT wait for the full
    // deadline: a slow, non-interruptible straggler (the exact BDD engine on a wide datapath)
    // would defeat the owned early-return. The grace catches the fast members (native / interp /
    // boolector), which co-complete within ms; a genuinely-slow engine is simply not waited on.
    // Bounded — the added latency past first-definite is at most `STRAGGLER_GRACE` (and the
    // overall `deadline` still caps it). Was a non-blocking `try_recv` only, which missed a
    // straggler that hadn't landed at the exact instant the first verdict arrived (DET-2).
    if definite {
        const STRAGGLER_GRACE: Duration = Duration::from_millis(250);
        let grace_deadline = (Instant::now() + STRAGGLER_GRACE).min(deadline);
        while reports < N_MEMBERS {
            let now = Instant::now();
            if now >= grace_deadline {
                break;
            }
            match rx.recv_timeout(grace_deadline - now) {
                Ok((name, v)) => {
                    reports += 1;
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
                Err(_) => break, // grace elapsed or all members disconnected
            }
        }
    }
    let mut out = ReachOutcome::from_sets(reachable_by, unreachable_by);
    out.refused = refused.lock().map(|g| g.clone()).unwrap_or_default();
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::adapter::btor2::parser;

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
        assert!(
            out.unreachable_by
                .iter()
                .any(|m| *m == "exact" || *m == "native"),
            "exact or native proves the 1-inductive design safe: {out:?}"
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
        // With no btormc/pono on PATH the two subprocess members abstain (Err),
        // but the exact engine alone decides the small in-cap counter — so the
        // portfolio still returns a definite verdict. (In mununu-sva all three
        // run; here we prove the exact-only fallback + the merge shape.)
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Reachable);
        assert!(out.reachable_by.contains(&"exact"));
        assert!(
            out.unreachable_by.is_empty(),
            "no engine should call the reachable counter unreachable"
        );
    }

    /// mununu#726 — an already-expired thread-local budget decides nothing and spawns nothing:
    /// `Unknown`, no engine credited, back in well under a second. The unbounded control is the
    /// existing `portfolio_decides_reachable_without_subprocess` (same fixture, `Reachable`).
    #[test]
    fn portfolio_abstains_promptly_under_an_expired_budget() {
        use crate::adapter::run_budget::{Budget, enter};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 constd 1 5\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let t0 = std::time::Instant::now();
        let out = {
            let _g = enter(&Budget::with_ms(0));
            decide_reach_portfolio(&file)
        };
        assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
        assert!(
            out.reachable_by.is_empty() && out.unreachable_by.is_empty(),
            "{out:?}"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t0.elapsed()
        );
    }

    /// mununu#816 — the owned driver's detached members run under the caller's budget. A
    /// thread-local is not inherited by a spawned thread, so before this an already-expired
    /// budget still spawned four members that ran unbounded by it. The same fixture decides
    /// `Reachable` with no budget (`owned_only_excludes_external_engines_on_small_counter`), and
    /// on the parent commit it decides here too.
    #[test]
    fn the_owned_driver_abstains_promptly_under_an_expired_budget() {
        use crate::adapter::run_budget::{Budget, enter};
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let t0 = std::time::Instant::now();
        let out = {
            let _g = enter(&Budget::with_ms(0));
            decide_reach_owned_only(&file, 30_000)
        };
        assert_eq!(out.verdict, ReachVerdict::Unknown, "{out:?}");
        assert!(
            out.reachable_by.is_empty() && out.unreachable_by.is_empty(),
            "{out:?}"
        );
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t0.elapsed()
        );
    }

    /// mununu#726 — with no budget the members keep the constants they always had (the floor);
    /// under a budget every bound follows the time left.
    #[test]
    fn member_timeouts_floor_is_todays_constants_and_follows_the_budget() {
        let sixty = std::time::Duration::from_secs(60);
        let floor = member_timeouts(None, sixty);
        assert_eq!(floor.native_query_ms, native_bmc::DEFAULT_TIMEOUT_MS);
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
        assert!(
            out.unreachable_by.contains(&"native"),
            "the native engine must carry the beyond-cap verdict: {out:?}"
        );
        assert!(
            !out.unreachable_by.contains(&"exact"),
            "the exact engine must abstain on the over-cap 300-bit design: {out:?}"
        );
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

    #[test]
    fn default_driver_is_the_explicit_budget_driver_at_the_default_budget() {
        // S1 retired the sequential driver: `decide_reach_portfolio` IS the parallel,
        // cancelling driver at the default subprocess budget. With no subprocess
        // members on PATH both reduce to the exact-only verdict; the point is they
        // agree byte-for-byte.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let default = decide_reach_portfolio(&file);
        let explicit = decide_reach_portfolio_parallel_with_timeout(&file, btormc::DEFAULT_TIMEOUT);
        assert_eq!(
            default, explicit,
            "the default driver is the explicit-budget one"
        );
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
        assert_eq!(
            default, custom,
            "budget override must not change an exact-decided verdict"
        );
        assert_eq!(custom.verdict, ReachVerdict::Reachable);
    }

    #[test]
    #[ignore = "requires btormc + pono (mununu-sva); run with --ignored"]
    fn portfolio_agrees_across_engines_in_sva() {
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
                ["exact", "native", "interp", "boolector", "cex"].contains(name),
                "owned-only listed a non-owned engine {name}: {out:?}"
            );
        }
        for ext in ["btormc", "pono", "ric3"] {
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
    #[ignore = "owned-only HWMCC ceiling sweep; set MUNUNU_HWMCC_FLAT (+ optional MUNUNU_HWMCC_GT), MUNUNU_OWNED_TIMEOUT_MS default 240000"]
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
}
