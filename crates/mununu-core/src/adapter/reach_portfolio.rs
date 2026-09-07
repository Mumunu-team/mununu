//! Reachability portfolio — decide BTOR2 `bad`-reachability with every available
//! engine and merge under the **differential-oracle discipline**.
//!
//! The P1 payoff made usable — five sound engines, each deciding a *different*
//! slice of designs:
//! - the **exact** BDD engine
//!   ([`exact_bad_reachable`](crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable))
//!   within its 40-bit cone cap;
//! - the in-house **native** engine (BMC + k-induction on the Z3 seam,
//!   [`native_bmc`](crate::adapter::btor2::native_bmc)) — no bit cap, no
//!   subprocess: mununu's own scalable safety member;
//! - the in-house **SPACER** engine (IC3/PDR + interpolation via Z3's Fixedpoint,
//!   [`native_spacer`](crate::adapter::btor2::native_spacer)) — decides safe
//!   properties whose inductive invariant native k-induction cannot reach by simple
//!   induction. It is run in an ISOLATED child process (a self-exec of the hidden
//!   `btor2 spacer-check` subcommand, gated on `MUNUNU_SELF_EXE`) because z3's
//!   Fixedpoint can flaky-`SIGSEGV` on some CHC encodings; a crash in the child is
//!   read as a sound `Unknown`, never a crash of the whole verify;
//! - **btormc** (BMC + k-induction) on deep counterexamples;
//! - **Pono** (IC3/PDR) on IC3-provable / violated instances.
//!
//! Plus a sixth member — the in-house **interpolation** engine
//! ([`native_interp`], owned McMillan-style forward reachability with an
//! interpolation k-schedule). It synthesises inductive invariants that neither
//! k-induction nor even z3-SPACER reach at their budgets (measured *unique* HWMCC
//! decides `gen12`/`gen14`/`gen39`). In the **parallel** driver it runs
//! *concurrently* with the other five but polls a shared cancellation flag and
//! bails the instant any faster engine reaches a definite verdict — so it costs
//! ~nothing on the common (fast-decided) path yet still spends its full budget on
//! the designs where it is the unique decider. In the **sequential** driver it is
//! gated as a last resort, computed only once the other five have abstained. It
//! needs cvc5 (a subprocess) and abstains when absent.
//!
//! Running them together decides strictly more than any one, and — crucially —
//! every engine's verdict is **sound**, so:
//!
//! - **first definite wins** — any engine's `Reachable` / `Unreachable` decides it;
//! - **two DEFINITE verdicts that DISAGREE raise a soundness alarm** ([`ReachVerdict::Contradiction`])
//!   rather than a guess. Since all engines are sound, a disagreement can only mean
//!   a real bug — exactly what the differential oracle exists to catch;
//! - **a SPACER-only `Reachable` is not trusted without corroboration** — SPACER
//!   decides from a Horn *derivation* over mununu's btor2→CHC encoding (which has a
//!   demonstrated spurious-counterexample bug on a pure-BV design), whereas every
//!   other member exhibits a concrete witness. A sole-decider spacer-reachable is
//!   therefore dropped to a sound `Unknown` rather than emitted (see [`collect`]).
//!
//! Both a **sequential** ([`decide_reach_portfolio`]) and a **parallel**
//! ([`decide_reach_portfolio_parallel`]) driver are provided. They merge
//! identically — the parallel variant overlaps every member (the two subprocess
//! engines, the in-process exact/native/spacer engines, and the cancellable
//! interpolation member) in wall-clock, which matters once a per-engine timeout is
//! in play: a slow member no longer serialises in front of a fast one. Every
//! member carries a **wall-clock timeout** (Pono's IC3 has no native bound and can
//! run unbounded on a hard instance); a member that errors, times out, or is
//! undecided simply abstains — a timeout is a sound [`ReachVerdict::Unknown`],
//! never a wrong verdict.

use crate::adapter::btor2::ast::Btor2File;
use crate::adapter::btor2::emit::emit_btor2;
use crate::adapter::btor2::native_bmc::{self, SafetyVerdict};
use crate::adapter::btor2::native_interp::{self, InterpSafetyVerdict};
use crate::adapter::btor2::native_spacer;
use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
use crate::adapter::btormc::{self, McVerdict};
use crate::adapter::pono;

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
        }
    }
}

/// The exact engine's optional verdict: `Some(true)` reachable, `Some(false)`
/// unreachable, `None` abstained (over-cap / free-init / error).
fn run_exact(content: &str) -> Option<bool> {
    exact_bad_reachable(content).ok()
}

/// The in-house **native engine** (BMC + k-induction on the Z3 seam) verdict:
/// `Some(true)` reachable (a counterexample), `Some(false)` unreachable (a
/// k-inductive safety proof), `None` abstained (Unknown / timeout / encode error).
/// Unlike the exact BDD engine it has no 40-bit cone cap, and unlike btormc/Pono
/// it needs no subprocess — mununu's own scalable safety member.
fn run_native(file: &Btor2File) -> Option<bool> {
    match native_bmc::decide_bad_safety(
        file,
        native_bmc::DEFAULT_MAX_K,
        Some(native_bmc::DEFAULT_TIMEOUT_MS),
    ) {
        Ok(SafetyVerdict::Violated { .. }) => Some(true),
        Ok(SafetyVerdict::Safe { .. }) => Some(false),
        Ok(SafetyVerdict::Unknown { .. }) | Err(_) => None,
    }
}

/// The in-house **SPACER engine** (IC3/PDR + interpolation via Z3's Fixedpoint)
/// verdict: `Some(true)` reachable, `Some(false)` unreachable (an inductive-invariant
/// safety proof), `None` abstained (Unknown / timeout / encode error). SPACER's
/// invariant discovery decides *safe* designs whose invariant native k-induction
/// cannot reach by simple induction below a large depth — also in-process, no
/// subprocess.
fn run_spacer(file: &Btor2File) -> Option<bool> {
    // z3's Fixedpoint (SPACER) has a demonstrated FLAKY SIGSEGV on some CHC encodings
    // (e.g. `vis_arrays_bufferAlloc`) — a z3-internal abort, uncatchable in-process and
    // independent of concurrency (it segfaults even run alone). When the CLI / server
    // exports `MUNUNU_SELF_EXE`, run SPACER in an ISOLATED child process (a self-exec of
    // the hidden `btor2 spacer-check` subcommand) so a segfault becomes a non-success
    // child exit we read as ABSTAIN rather than a crash of the whole verify. Without that
    // env var (unit tests, or an embedding without the CLI binary) fall back to the
    // in-process engine — those paths never exercise a crash-triggering design.
    match std::env::var_os("MUNUNU_SELF_EXE") {
        Some(exe) => run_spacer_isolated(
            std::path::Path::new(&exe),
            file,
            native_spacer::DEFAULT_TIMEOUT_MS,
        ),
        None => run_spacer_in_process(file, native_spacer::DEFAULT_TIMEOUT_MS),
    }
}

/// In-process SPACER — the direct engine call. Used as the fallback when no isolated
/// self-exec target is available (tests / non-CLI embeddings).
fn run_spacer_in_process(file: &Btor2File, timeout_ms: u32) -> Option<bool> {
    match native_spacer::decide_bad_safety_spacer(file, Some(timeout_ms)) {
        Ok(SafetyVerdict::Violated { .. }) => Some(true),
        Ok(SafetyVerdict::Safe { .. }) => Some(false),
        Ok(SafetyVerdict::Unknown { .. }) | Err(_) => None,
    }
}

/// SPACER in an isolated child (`<exe> btor2 spacer-check --timeout-ms <t>`, BTOR2 on
/// stdin, `safe`/`unsafe`/`unknown` on stdout). A z3-Fixedpoint SIGSEGV in the child is
/// a non-success exit ⇒ `None` (abstain); a timeout ⇒ `None`; a clean verdict maps like
/// the in-process engine. This contains the crash to a throwaway process — the verify
/// itself never dies.
fn run_spacer_isolated(exe: &std::path::Path, file: &Btor2File, timeout_ms: u32) -> Option<bool> {
    let content = emit_btor2(file);
    let mut cmd = std::process::Command::new(exe);
    // `--quiet` keeps the child's tracing off stdout; we still scan for the verdict
    // line defensively (the child may print a startup log line before it).
    cmd.args(["--quiet", "btor2", "spacer-check", "--timeout-ms"])
        .arg(timeout_ms.to_string());
    // Wall headroom over the child's own z3 timeout so we read its verdict rather than
    // killing it; a hung or segfaulting child is bounded either way.
    let wall = std::time::Duration::from_millis(u64::from(timeout_ms) + 5_000);
    let (status, stdout, _stderr) =
        crate::adapter::run_with_timeout(&mut cmd, Some(content.as_bytes()), wall).ok()??;
    if !status.success() {
        return None; // SIGSEGV / error exit — the isolation payoff: abstain, don't crash.
    }
    // The verdict is the last `safe`/`unsafe`/`unknown` line the child printed.
    for line in stdout.lines().rev() {
        match line.trim() {
            "safe" => return Some(false),
            "unsafe" => return Some(true),
            "unknown" => return None,
            _ => {}
        }
    }
    None
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
/// needs *synthesised* predicates that neither k-induction nor even z3-SPACER's PDR
/// reach at their budgets — measured *unique* HWMCC coverage (`gen12`/`gen14`/`gen39`,
/// which the whole rest of the portfolio leaves `Unknown`). cvc5 (a subprocess) is
/// required; when it is absent the engine abstains.
fn run_interp(file: &Btor2File, cancel: &std::sync::atomic::AtomicBool) -> Option<bool> {
    match native_interp::verify_safety_interp_cancellable(
        file,
        INTERP_MAX_SUFFIX,
        INTERP_MAX_ITERS,
        INTERP_QUERY_TIMEOUT_MS,
        INTERP_OVERALL_TIMEOUT_MS,
        cancel,
    ) {
        InterpSafetyVerdict::Unsafe { .. } => Some(true),
        InterpSafetyVerdict::Safe { .. } => Some(false),
        InterpSafetyVerdict::Undecided { .. } => None,
    }
}

/// Merge the members' verdicts into a [`ReachOutcome`], in the fixed engine order
/// (`exact`, `native`, `spacer`, `interp`, `btormc`, `pono`) so the outcome is
/// deterministic regardless of which driver (sequential / parallel) produced them.
fn collect(
    exact: Option<bool>,
    native: Option<bool>,
    spacer: Option<bool>,
    interp: Option<bool>,
    btormc_v: Option<McVerdict>,
    pono_v: Option<McVerdict>,
) -> ReachOutcome {
    let mut reachable_by: Vec<&'static str> = Vec::new();
    let mut unreachable_by: Vec<&'static str> = Vec::new();
    // In-house engines report reachability as a bool (`Some(true)` = reachable).
    for (name, v) in [
        ("exact", exact),
        ("native", native),
        ("spacer", spacer),
        ("interp", interp),
    ] {
        match v {
            Some(true) => reachable_by.push(name),
            Some(false) => unreachable_by.push(name),
            None => {}
        }
    }
    // Subprocess members report an `McVerdict`.
    for (name, v) in [("btormc", btormc_v), ("pono", pono_v)] {
        match v {
            Some(McVerdict::Violated) => reachable_by.push(name),
            Some(McVerdict::Safe) => unreachable_by.push(name),
            Some(McVerdict::Unknown) | None => {}
        }
    }
    // SOUNDNESS GUARD — uncorroborated SPACER counterexample.
    //
    // Every other member proves *reachable* with a concrete witness: exact-BDD is
    // exact, native BMC / btormc / Pono return a bounded trace, and native interp
    // (McMillan) escalates through BMC. SPACER alone reports `reachable` from a Horn
    // *derivation* over mununu's btor2→CHC rule encoding, and that encoding has at
    // least one demonstrated spurious-CEX bug on a pure-BV design
    // (`vcegar_arrays_itc99_b12_p2`: SPACER says reachable, ground truth is safe, and
    // no BMC engine can corroborate the trace). The inter-engine `Contradiction`
    // alarm did not catch it because SPACER was the *sole* decider — nothing else
    // ran to disagree.
    //
    // So: when SPACER is the only engine claiming reachable AND nothing contradicts
    // it, we cannot trust the derivation — drop the claim and abstain (`Unknown`)
    // rather than emit a spurious `Reachable`. A spacer-reachable that any
    // concrete-witness engine corroborates, or a spacer-vs-safe disagreement (which
    // stays a `Contradiction` alarm), is left untouched. See
    // `measurements/hwmcc-owned-engine-gaps.md` (Category D) for the root-cause
    // encoding fix that would let SPACER emit `Reachable` on its own again.
    if reachable_by == ["spacer"] && unreachable_by.is_empty() {
        reachable_by.clear();
    }
    ReachOutcome::from_sets(reachable_by, unreachable_by)
}

/// Decide `bad`-reachability of `file` across all five engines — the exact BDD
/// engine, the in-house native (BMC + k-induction) and SPACER engines, and the
/// btormc and Pono subprocess members — merged under the differential-oracle
/// discipline.
///
/// Each member abstains gracefully: the exact engine on an over-cap / free-init
/// design (`Err`), the in-house engines on `Unknown` / timeout / encode error, a
/// subprocess member when its binary is absent (`Err`), it is inconclusive
/// (`Unknown`), or it hits its wall-clock timeout. The emitted BTOR2 (from
/// [`emit_btor2`]) is the shared input, so a *reduced / transformed* model is decided
/// consistently across all members.
///
/// This is the **sequential** driver — members run one after another. Use
/// [`decide_reach_portfolio_parallel`] to overlap them in wall-clock.
pub fn decide_reach_portfolio(file: &Btor2File) -> ReachOutcome {
    let content = emit_btor2(file);
    // Exact BDD engine — sound both ways (REACHABLE is always sound; an
    // UNREACHABLE verdict is refused on free-init state), within the bit cap.
    let exact = run_exact(&content);
    // Native engine — in-house BMC + k-induction on the Z3 seam, no bit cap.
    let native = run_native(file);
    // SPACER (via Z3's Fixedpoint) — external algorithm, in-process invariant discovery.
    let spacer = run_spacer(file);
    // btormc — BMC (CEX) + k-induction (proof).
    let btormc_v =
        btormc::decide_via_btormc(file, btormc::DEFAULT_KMAX, btormc::DEFAULT_TIMEOUT).ok();
    // Pono — IC3/PDR (proof + shallow CEX).
    let pono_v = pono::decide_via_pono(file, pono::DEFAULT_ENGINE, pono::DEFAULT_TIMEOUT).ok();
    // Owned interpolation member: in the SEQUENTIAL driver it runs after the others, so
    // keep it last-resort (only when nothing else decided) to keep its cost off the
    // common path — the flag is set iff a definite verdict already exists.
    let interp = if collect(exact, native, spacer, None, btormc_v, pono_v).verdict
        == ReachVerdict::Unknown
    {
        run_interp(file, &std::sync::atomic::AtomicBool::new(false))
    } else {
        None
    };
    collect(exact, native, spacer, interp, btormc_v, pono_v)
}

/// The **parallel** driver: run the exact engine (in-process) and the two
/// subprocess members concurrently, then merge identically to
/// [`decide_reach_portfolio`]. Scoped threads borrow `file` directly — the scope
/// guarantees the borrows end before this returns, so no clone / `Arc` is needed.
///
/// The merge is unchanged, so the merged *verdict* is identical to the sequential
/// driver (all members are sound, so they cannot disagree on a definite answer);
/// only the wall-clock differs (≈ the slowest single member instead of the sum).
/// This matters once per-engine timeouts are in play — a member that burns its full
/// budget no longer serialises in front of a fast one. One benign detail difference:
/// because the interpolation member runs concurrently here (rather than only as a
/// sequential last resort), a design it decides in time will additionally list
/// `interp` in `reachable_by` / `unreachable_by`, giving the owned engine its credit.
pub fn decide_reach_portfolio_parallel(file: &Btor2File) -> ReachOutcome {
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

/// [`decide_reach_portfolio_parallel`] with the **subprocess** members' (btormc / Pono)
/// wall budget set to `subprocess_timeout` instead of the 60 s default. When the budget is
/// raised above the default, btormc's depth cap is also lifted to [`DEEP_BUDGET_KMAX`] (a
/// longer budget is pointless if the unrolling stays capped at 40 — see that constant).
/// Measured: with both raised, `krebs.3`'s depth-75 CEX is found in ~73 s (`unknown` →
/// `violated`); `vis_arrays_buf_bug`'s is found in ~1 s either way. The in-process members
/// (exact / native / SPACER / interp) keep their own budgets. Surfaced via `btor2 verify
/// --timeout-ms`.
pub fn decide_reach_portfolio_parallel_with_timeout(
    file: &Btor2File,
    subprocess_timeout: std::time::Duration,
) -> ReachOutcome {
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let content = emit_btor2(file);
    // Raising the wall budget signals "reach deeper" — so lift btormc's depth cap too.
    let btormc_kmax = if subprocess_timeout > btormc::DEFAULT_TIMEOUT {
        DEEP_BUDGET_KMAX
    } else {
        btormc::DEFAULT_KMAX
    };
    // A definite verdict from ANY faster member sets this; the owned interpolation
    // member polls it and abandons its (possibly slow) cvc5 interpolation search early
    // (#1 — run interpolation as a concurrent member, not last-resort, so its unique
    // decides get owned credit, without its pathological query dominating wall-clock on
    // instances another engine already decided).
    let decided = AtomicBool::new(false);
    let decided = &decided;
    std::thread::scope(|scope| {
        let btormc_h = scope.spawn(|| {
            let v = btormc::decide_via_btormc(file, btormc_kmax, subprocess_timeout).ok();
            if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                decided.store(true, Relaxed);
            }
            v
        });
        let pono_h = scope.spawn(|| {
            let v = pono::decide_via_pono(file, pono::DEFAULT_ENGINE, subprocess_timeout).ok();
            if matches!(v, Some(McVerdict::Violated) | Some(McVerdict::Safe)) {
                decided.store(true, Relaxed);
            }
            v
        });
        // The native and SPACER engines are in-process Z3 work — each on its own
        // thread so it overlaps the exact engine + the subprocess members rather than
        // serialising.
        let native_h = scope.spawn(|| {
            let v = run_native(file);
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            v
        });
        let spacer_h = scope.spawn(|| {
            let v = run_spacer(file);
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            v
        });
        // Owned McMillan interpolation — now a FIRST-CLASS concurrent member (was
        // last-resort). Polls `decided` and bails out early once a faster member decides.
        let interp_h = scope.spawn(|| run_interp(file, decided));
        // The exact engine is in-process BDD work — run it on this thread while the
        // other members run on theirs.
        let exact = run_exact(&content);
        if exact.is_some() {
            decided.store(true, Relaxed);
        }
        // A panicked member thread abstains (None) rather than poisoning the merge.
        let native = native_h.join().unwrap_or(None);
        let spacer = spacer_h.join().unwrap_or(None);
        let btormc_v = btormc_h.join().unwrap_or(None);
        let pono_v = pono_h.join().unwrap_or(None);
        let interp = interp_h.join().unwrap_or(None);
        collect(exact, native, spacer, interp, btormc_v, pono_v)
    })
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
/// deliberately EXCLUDING the external algorithms (Z3 SPACER, btormc, Pono). It
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
    use std::time::{Duration, Instant};

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
    // Trade vs the full parallel portfolio: early-return means a member still running when
    // we return is not cross-checked, so the inter-engine `Contradiction` alarm here only
    // covers members that finished by return time (the full `decide_reach_portfolio_parallel`
    // keeps the complete cross-check). Every member is individually sound, so the first
    // definite verdict is itself sound.
    let file = Arc::new(file.clone());
    let content = Arc::new(emit_btor2(&file));
    let decided = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let (tx, rx) = mpsc::channel::<(&'static str, Option<bool>)>();

    // Safety proof — shallow native BMC + k-induction.
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        std::thread::spawn(move || {
            let v = match native_bmc::decide_bad_safety(
                &file,
                native_bmc::DEFAULT_MAX_K,
                Some(timeout_ms),
            ) {
                Ok(SafetyVerdict::Violated { .. }) => Some(true),
                Ok(SafetyVerdict::Safe { .. }) => Some(false),
                Ok(SafetyVerdict::Unknown { .. }) | Err(_) => None,
            };
            if v.is_some() {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("native", v));
        });
    }
    // Safety proof — McMillan interpolation.
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        std::thread::spawn(move || {
            let v = match native_interp::verify_safety_interp_cancellable(
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
            };
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
        std::thread::spawn(move || {
            let v = crate::adapter::btor2::native_boolector::decide_reachable_boolector(
                &file,
                OWNED_DEEP_CEX_MAX_K,
                deadline,
                &decided,
            );
            if v == Some(true) {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("boolector", v));
        });
    }
    #[cfg(not(feature = "boolector"))]
    {
        let (file, decided, tx) = (Arc::clone(&file), Arc::clone(&decided), tx.clone());
        std::thread::spawn(move || {
            let hit = native_bmc::bmc_cex_until(
                &file,
                OWNED_DEEP_CEX_MAX_K,
                OWNED_CEX_QUERY_MS,
                deadline,
                &decided,
            )
            .is_some();
            if hit {
                decided.store(true, Relaxed);
            }
            let _ = tx.send(("cex", if hit { Some(true) } else { None }));
        });
    }
    // Exact BDD — the non-interruptible member; spawned so it can never block the return.
    {
        let (content, decided, tx) = (Arc::clone(&content), Arc::clone(&decided), tx.clone());
        std::thread::spawn(move || {
            let v = run_exact(&content);
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
    ReachOutcome::from_sets(reachable_by, unreachable_by)
}

#[cfg(test)]
mod tests {
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
        // when the base portfolio (exact / native / spacer / btormc / pono) is still
        // Unknown, and the merge is first-definite-wins. A design that native
        // k-induction already proves safe must come back Unreachable with the interp
        // member never running or flipping it.
        const SAFE_1IND: &str = "1 sort bitvec 1\n2 zero 1\n3 state 1 q\n4 init 1 3 2\n\
                                 5 next 1 3 2\n6 bad 3\n";
        let file = parser::parse(SAFE_1IND).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Unreachable, "outcome: {out:?}");
        assert!(
            out.unreachable_by.contains(&"native"),
            "native k-induction proves the 1-inductive design safe: {out:?}"
        );
        // At the merge level a definite base is likewise untouched by an interp abstain.
        assert_eq!(
            collect(Some(true), None, None, None, None, None).verdict,
            ReachVerdict::Reachable
        );
    }

    #[test]
    fn uncorroborated_spacer_counterexample_abstains() {
        // SOUNDNESS GUARD (`vcegar_arrays_itc99_b12_p2`): a SPACER-only `reachable`
        // with no concrete-witness corroboration is a suspect derivation over the
        // btor2→CHC encoding, so the merge drops it and abstains rather than emit a
        // spurious Reachable.
        let out = collect(None, None, Some(true), None, None, None);
        assert_eq!(
            out.verdict,
            ReachVerdict::Unknown,
            "sole-decider spacer-reachable must abstain: {out:?}"
        );
        assert!(
            out.reachable_by.is_empty(),
            "the uncorroborated spacer claim is dropped: {out:?}"
        );
        // Corroborated by any concrete-witness engine ⇒ trusted (stays Reachable).
        assert_eq!(
            collect(None, Some(true), Some(true), None, None, None).verdict,
            ReachVerdict::Reachable,
            "native BMC corroborates spacer ⇒ trust the counterexample"
        );
        // A spacer-vs-safe disagreement is a real soundness alarm, NOT silently
        // resolved — the guard only fires when spacer is the *sole* decider.
        assert_eq!(
            collect(Some(false), None, Some(true), None, None, None).verdict,
            ReachVerdict::Contradiction,
            "exact-safe vs spacer-reachable must still raise the alarm"
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
    fn spacer_decides_beyond_native_k_induction_in_portfolio() {
        // An 300-bit counter that STALLS at 0 (init 0, next = (c==0)?0:c+1), bad = (c==MAX).
        // Reachable set is {0} ⇒ SAFE, but:
        //   - the exact BDD engine abstains (300-bit > its auto-cap ceiling of 192);
        //   - native k-induction abstains — the property is not provable by *simple*
        //     k-induction: an unreachable free path MAX-k → … → MAX stays ¬bad until
        //     the last step at every depth, so it never closes below depth 2^80-1.
        // Only SPACER's invariant discovery (`c == 0`) decides it, so the portfolio
        // verdict is carried uniquely by "spacer". (btormc/pono absent in make-ci.)
        const WIDE_STALL: &str = "1 sort bitvec 300\n2 zero 1\n3 one 1\n4 state 1 c\n5 init 1 4 2\n\
             6 sort bitvec 1\n7 eq 6 4 2\n8 add 1 4 3\n9 ite 1 7 2 8\n\
             10 next 1 4 9\n11 ones 1\n12 eq 6 4 11\n13 bad 12\n";
        let file = parser::parse(WIDE_STALL).expect("parse");
        let out = decide_reach_portfolio(&file);
        assert_eq!(out.verdict, ReachVerdict::Unreachable, "outcome: {out:?}");
        assert!(
            out.unreachable_by.contains(&"spacer"),
            "SPACER must carry the beyond-k-induction verdict: {out:?}"
        );
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
    fn parallel_driver_agrees_with_sequential() {
        // The parallel driver must return the *identical* outcome to the
        // sequential one — it only overlaps members in wall-clock, never changes
        // the merge. With no subprocess members on PATH both reduce to the
        // exact-only verdict; the point is they agree byte-for-byte.
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n\
                               6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                               10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let seq = decide_reach_portfolio(&file);
        let par = decide_reach_portfolio_parallel(&file);
        assert_eq!(seq, par, "parallel and sequential drivers must agree");
        assert_eq!(par.verdict, ReachVerdict::Reachable);
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
        let default = decide_reach_portfolio_parallel(&file);
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
        for ext in ["spacer", "btormc", "pono"] {
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
}
