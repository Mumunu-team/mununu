//! mununu#504 — a cooperative wall-clock budget for a verification run.
//!
//! # Why this exists
//!
//! Nothing in `sv verify-auto` was bounded by time at any granularity. A consumer lost a full
//! `make formal` run to a six-hour hang, and a second run to an OS kill that emitted **zero
//! output** — losing verdicts already computed in memory. The engines have size budgets (the BDD
//! bit cap, node budget, iteration budget) and the subprocesses now have wall-clock caps, but the
//! SMT enumeration loops in the lift had neither: only a 5 s *per-query* timeout, with no
//! aggregate bound across `2^|P|` cubes x 17 CEGAR rounds x N properties.
//!
//! # Why cooperative cancel rather than a thread per property
//!
//! The obvious alternative — run each property on a detached thread and `recv_timeout` — was
//! rejected because it **sabotages the memory ceiling**: an abandoned thread keeps its z3 context
//! and partial maps alive and still allocating, and that RSS is exactly what the memory budget
//! reads. Two mechanisms fighting each other is worse than either alone. The measured hang is
//! also genuinely interruptible: every iteration of the enumeration loops returns to Rust.
//!
//! # Why a thread-local rather than threading `&Budget` through
//!
//! The loops that burn the time sit 4-6 frames below the driver, behind `CegarOptions`,
//! `PredicateCubeLiftOptions` and the public `SmtEncode` trait. Explicit threading would touch
//! ~40 signatures and ~60 test literals on exactly the code path that must not regress. Those
//! leaves *already* read process-global state at these points (`cube_smt_rlimit()` reads an env
//! var inside nine separate check functions), so a thread-local is **strictly more scoped** than
//! the idiom already in place — and unlike env, it is per-test-thread, so budget tests need no
//! serialisation and no environment mutation.
//!
//! # Footgun
//!
//! **A spawned thread inherits nothing.** Any `std::thread::spawn` that should honour the budget
//! must capture [`current`] and [`enter`] it inside the closure. `budget_is_not_inherited_by_a_
//! spawned_thread` pins that, so the surprise is documented rather than discovered.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Per-property wall-clock budget. Unset ⇒ unbounded.
pub const PROPERTY_BUDGET_ENV: &str = "MUNUNU_PROPERTY_BUDGET_MS";
/// Whole-run wall-clock budget. Unset ⇒ unbounded.
pub const RUN_BUDGET_ENV: &str = "MUNUNU_VERIFY_BUDGET_MS";
/// The ⊥ re-plan's per-call slice for a portfolio-backed rescue (the reach portfolio on a
/// `bad`-monitor, the fair-cycle l2s rescue, the safety-witness BMC). Unset ⇒ the default below;
/// `0` ⇒ the per-property budget alone applies.
pub const RESCUE_SLICE_ENV: &str = "MUNUNU_RESCUE_SLICE_MS";

/// Who set a time limit: what a stop at that limit can tell the user to change (mununu#1197).
///
/// A message that reports a stop at a limit names the setting that moves the limit, in the names
/// of the surface that set it, or says that no setting does. The setter is recorded where the
/// limit is made: the code that prints the stop sees a number of milliseconds, and a number does
/// not say whether a flag, a request field, a variable or a constant produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetBy {
    /// `--timeout-ms` of the CLI verb that ran. Named only by the CLI.
    TimeoutFlag,
    /// `timeout_ms` of the HTTP request. Named only by the API handlers.
    TimeoutField,
    /// [`RUN_BUDGET_ENV`].
    RunBudgetEnv,
    /// [`PROPERTY_BUDGET_ENV`].
    PropertyBudgetEnv,
    /// [`RESCUE_SLICE_ENV`].
    RescueSliceEnv,
    /// A constant of mununu's own: no setting moves it.
    Mununu,
}

impl SetBy {
    /// Every setter, for the tests that must cover each one.
    pub const ALL: [SetBy; 6] = [
        SetBy::TimeoutFlag,
        SetBy::TimeoutField,
        SetBy::RunBudgetEnv,
        SetBy::PropertyBudgetEnv,
        SetBy::RescueSliceEnv,
        SetBy::Mununu,
    ];

    /// The setting as the user writes it, or `None` when the limit is mununu's own.
    pub fn setting(self) -> Option<&'static str> {
        match self {
            SetBy::TimeoutFlag => Some("`--timeout-ms`"),
            SetBy::TimeoutField => Some("the request's `timeout_ms`"),
            SetBy::RunBudgetEnv => Some("`MUNUNU_VERIFY_BUDGET_MS`"),
            SetBy::PropertyBudgetEnv => Some("`MUNUNU_PROPERTY_BUDGET_MS`"),
            SetBy::RescueSliceEnv => Some("`MUNUNU_RESCUE_SLICE_MS`"),
            SetBy::Mununu => None,
        }
    }

    /// The closing clause of a message that reports a stop at a limit this setter set: the
    /// setting to raise, or that there is none and whose limit it is. No trailing period.
    pub fn remedy(self) -> String {
        match self.setting() {
            Some(setting) => {
                format!("this limit follows {setting}: raise it to give the search longer")
            }
            None => NO_SETTING.to_string(),
        }
    }

    /// Of two setters of the same limit, the one a message can act on: this one unless it is
    /// mununu's own.
    pub fn or(self, other: SetBy) -> SetBy {
        if self == SetBy::Mununu { other } else { self }
    }
}

/// What a stop at one of mununu's own limits says: nothing the user sets moves it.
pub const NO_SETTING: &str = "this limit is mununu's own and no setting changes it (a mununu \
                              limitation: nothing to change in the design)";

/// A deadline plus a cancellation flag. Cloning shares the flag.
#[derive(Clone, Debug)]
pub struct Budget {
    deadline: Option<Instant>,
    cancel: Arc<AtomicBool>,
    /// Who set `deadline`. With no deadline: whose setting would set one.
    set_by: SetBy,
}

impl Default for Budget {
    fn default() -> Self {
        Self::unbounded()
    }
}

impl Budget {
    /// No deadline. The default everywhere until a budget is explicitly configured, so this
    /// module is inert unless someone opts in.
    pub fn unbounded() -> Self {
        Self {
            deadline: None,
            cancel: Arc::new(AtomicBool::new(false)),
            set_by: SetBy::Mununu,
        }
    }

    /// A budget expiring `ms` from now, at a limit of mununu's own. A limit a flag, a request
    /// field or a variable sets is a [`Budget::child`] of the current budget, which records it.
    pub fn with_ms(ms: u64) -> Self {
        Self {
            deadline: Some(Instant::now() + Duration::from_millis(ms)),
            cancel: Arc::new(AtomicBool::new(false)),
            set_by: SetBy::Mununu,
        }
    }

    /// A child budget that can never outlive its parent: the effective deadline is the EARLIER of
    /// the parent's and `now + ms`. A per-property budget therefore cannot overrun the run budget,
    /// however many properties there are. The cancel flag is SHARED, so cancelling the parent
    /// cancels every child.
    ///
    /// `set_by` says who chose `ms` (`None` included: a flag that was not given is still the
    /// setting that would bound this run). The child reports the setter of the deadline that
    /// BINDS: its own when `now + ms` is the earlier one, the parent's otherwise. With no deadline
    /// on either side it reports its own setter, or the parent's when its own is mununu.
    pub fn child(outer: &Budget, ms: Option<u64>, set_by: SetBy) -> Self {
        let own = ms.map(|ms| Instant::now() + Duration::from_millis(ms));
        let (deadline, set_by) = match (outer.deadline, own) {
            (Some(a), Some(b)) if b < a => (Some(b), set_by),
            (Some(a), _) => (Some(a), outer.set_by),
            (None, Some(b)) => (Some(b), set_by),
            (None, None) => (None, set_by.or(outer.set_by)),
        };
        Self {
            deadline,
            cancel: Arc::clone(&outer.cancel),
            set_by,
        }
    }

    /// Who set the deadline in effect. With no deadline: whose setting would set one.
    pub fn set_by(&self) -> SetBy {
        self.set_by
    }

    /// Has the budget run out, or been cancelled?
    pub fn expired(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    /// Milliseconds left, saturating at 0. `None` ⇒ unbounded.
    pub fn remaining_ms(&self) -> Option<u64> {
        self.deadline.map(|d| {
            d.saturating_duration_since(Instant::now())
                .as_millis()
                .min(u64::MAX as u128) as u64
        })
    }

    /// Cancel this budget and every budget sharing its flag.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

thread_local! {
    static CURRENT: std::cell::RefCell<Budget> = std::cell::RefCell::new(Budget::unbounded());
}

/// RAII installation of `b` as this thread's current budget. The previous budget is restored on
/// drop, so every early `continue` / `return` / `?` in the caller is safe without bookkeeping.
pub struct BudgetGuard(Budget);

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        let prev = self.0.clone();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

/// Install `b` for this thread until the returned guard drops.
pub fn enter(b: &Budget) -> BudgetGuard {
    let prev = CURRENT.with(|c| c.replace(b.clone()));
    BudgetGuard(prev)
}

/// This thread's current budget.
pub fn current() -> Budget {
    CURRENT.with(|c| c.borrow().clone())
}

/// The leaf poll — one call, no plumbing.
pub fn expired() -> bool {
    CURRENT.with(|c| c.borrow().expired())
}

/// mununu#1147 — the flag alone, whatever the deadline: this thread's budget was cancelled
/// (the HTTP request that started the work is gone). A subprocess helper reads it to kill its
/// child; an expired deadline is the member's own stop, not this.
pub fn cancelled() -> bool {
    CURRENT.with(|c| c.borrow().cancel.load(Ordering::Relaxed))
}

/// What [`run_or_abandon`] came back with.
#[derive(Debug, PartialEq, Eq)]
pub enum Bounded<T> {
    /// The work returned.
    Done(T),
    /// The budget (plus the grace) passed with the work still running. Its thread is left
    /// behind: it keeps running until it next polls its budget or the process ends.
    Abandoned { waited_ms: u64 },
    /// The worker thread could not be started, or it panicked.
    Died,
}

/// mununu#1063 — run `work` under this thread's budget and stop WAITING for it when the budget
/// runs out, for work that cannot be interrupted from inside.
///
/// The budget is cooperative: the leaves poll it. One leaf cannot — a single OxiDD apply (the
/// exact engine's `box_pre` substitution) returns to Rust only when it is done, and on a wide
/// cone one apply outlived a 60 s property budget and a 240 s run budget with nothing able to
/// stop it. For that work the caller's only bound is to stop waiting.
///
/// - With no deadline in effect `work` runs inline, on the calling thread, exactly as before.
/// - Otherwise it runs on a named worker thread that re-enters the caller's budget (a spawned
///   thread inherits none), and the caller waits for the time left plus `grace` — the grace lets
///   work that DOES poll report its own, more specific, stop first.
///
/// This is the thread-per-task shape the module doc argues against as a general mechanism, and
/// the cost named there is real: an abandoned worker keeps its memory and may keep allocating
/// until it polls. It is used only where the alternative is an unbounded run, and the caller
/// says so in the report. mununu#996 made the same trade for the reach portfolio's exact member.
pub fn run_or_abandon<T, F>(
    thread_name: &str,
    stack_bytes: usize,
    grace: Duration,
    work: F,
) -> Bounded<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let budget = current();
    let Some(left_ms) = budget.remaining_ms() else {
        return Bounded::Done(work());
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_budget = budget.clone();
    let spawned = std::thread::Builder::new()
        .name(thread_name.to_string())
        .stack_size(stack_bytes)
        .spawn(move || {
            let _guard = enter(&worker_budget);
            // The receiver is gone when the caller abandoned this worker: nothing to report to.
            let _ = tx.send(work());
        });
    if spawned.is_err() {
        return Bounded::Died;
    }
    let wait = Duration::from_millis(left_ms) + grace;
    match rx.recv_timeout(wait) {
        Ok(v) => Bounded::Done(v),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Bounded::Abandoned {
            waited_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        },
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Bounded::Died,
    }
}

/// Clamp a per-query solver timeout to the time actually left, so the LAST query cannot overshoot
/// the deadline by its own full timeout. Never returns 0 (z3 reads 0 as "no timeout", which would
/// be the exact opposite of what an expired budget wants); an expired budget yields 1 ms, and the
/// caller's `expired()` check is what actually stops the loop.
pub fn clamp_query_ms(default_ms: u32) -> u32 {
    match current().remaining_ms() {
        Some(rem) => default_ms.min(rem.max(1) as u32).max(1),
        None => default_ms,
    }
}

/// Why a run stopped early. Kept separate from `AdapterError` so a caller can report the reason
/// per property while preserving verdicts already computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// A wall-clock budget expired.
    TimeBudget { scope: &'static str },
    /// The process-RSS ceiling was exceeded (mununu#490).
    Memory,
    /// Explicitly cancelled.
    Cancelled,
}

/// Default per-property budget: **15 minutes**.
///
/// MEASURED, not guessed (mununu#504, 2026-09-07, `MUNUNU_PROPERTY_TIMING=1` over the real-design
/// e2e corpus, n = 122 properties):
///
/// | p50 | p90 | p99 | max |
/// |---|---|---|---|
/// | 2.2 s | 14.7 s | 41.3 s | **49.1 s** |
///
/// 15 minutes is ~18x the observed maximum. That is deliberately far more headroom than the
/// "3x p99" rule of thumb, for two reasons:
///
/// 1. **The corpus is smaller than a consumer's real gate.** Tuning a default to our own e2e set
///    would be tuning to the easy case.
/// 2. **The two failure modes are not symmetric.** Too high means a hang takes longer to catch —
///    annoying. Too low means mununu abstains on work that was *succeeding*, and since a budget
///    abstention is `unknown` (never `skipped`, so that a gate still fails), that turns
///    currently-GREEN gates RED across every consumer, for properties that were fine. The
///    asymmetry says: err high.
///
/// A single ⊥ property routed through the full rescue ladder can also legitimately spend minutes
/// (btormc 60 s + pono 60 s + SPACER + native + interpolation, sequentially), which the corpus
/// above did not fully exercise.
const DEFAULT_PROPERTY_BUDGET_MS: u64 = 15 * 60 * 1000;

/// Default whole-run budget: **1 hour**.
///
/// This is the one that addresses the reported incident. the consumer lost a full `make formal` run to
/// a SIX-HOUR hang; a per-property cap alone would not have saved them (42 properties x 15 min is
/// still 10 hours in the worst case), but an hour-long run budget turns that six-hour loss into a
/// report after one hour with the undecided tail marked `unknown`.
const DEFAULT_RUN_BUDGET_MS: u64 = 60 * 60 * 1000;

/// Pure parse of a budget env value against a default: a positive integer → that many ms;
/// **`0` → `None` (explicitly disabled)**; unset or unparseable → the default.
///
/// Note the ladder differs from the pre-default version: unset now means "use the default", not
/// "unbounded". `0` is the escape hatch, matching the convention the other budgets use, and
/// unparseable falls back to the default rather than silently uncapping — the safe direction.
pub fn parse_budget_ms_with_default(v: Option<String>, default_ms: u64) -> Option<u64> {
    match v.as_deref().map(str::trim) {
        Some("0") => None,
        Some(s) => Some(
            s.parse::<u64>()
                .ok()
                .filter(|&ms| ms > 0)
                .unwrap_or(default_ms),
        ),
        None => Some(default_ms),
    }
}

/// Pure parse with NO default — a positive integer or `None`. Retained for callers that want an
/// explicit-only reading.
pub fn parse_budget_ms(v: Option<String>) -> Option<u64> {
    v.as_deref()
        .map(str::trim)
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
}

/// The per-property budget: `MUNUNU_PROPERTY_BUDGET_MS`, else [`DEFAULT_PROPERTY_BUDGET_MS`].
/// `0` disables.
pub fn property_budget_ms() -> Option<u64> {
    parse_budget_ms_with_default(
        crate::adapter::env_var::read(PROPERTY_BUDGET_ENV),
        DEFAULT_PROPERTY_BUDGET_MS,
    )
}

/// The whole-run budget: `MUNUNU_VERIFY_BUDGET_MS`, else [`DEFAULT_RUN_BUDGET_MS`]. `0` disables.
pub fn run_budget_ms() -> Option<u64> {
    parse_budget_ms_with_default(
        crate::adapter::env_var::read(RUN_BUDGET_ENV),
        DEFAULT_RUN_BUDGET_MS,
    )
}

/// Default rescue slice: **60 seconds** — the wall the reach portfolio already ran under
/// (`btormc::DEFAULT_TIMEOUT`), so a consumer with btormc installed sees no change in the rescue's
/// worst case, and about 8× the deciding query measured on the full OpenTitan `edn` IP (mununu#726:
/// the native member's FIXED 5-s per-query timeout let one to three of eight identical rows decide
/// per run). The slice is a CHILD of the per-property budget, which is a child of the run budget,
/// so it can never extend either.
///
/// Why a slice and not the whole per-property budget: ~20 undecidable rows × 15 min would turn a
/// ten-minute IP run into hours. Why not "remaining ÷ rows left": a sibling's ⊥ would then change
/// THIS row's verdict — a budget may decide how long, never what.
const DEFAULT_RESCUE_SLICE_MS: u64 = 60 * 1000;

/// The rescue slice: `MUNUNU_RESCUE_SLICE_MS`, else [`DEFAULT_RESCUE_SLICE_MS`]. `0` disables
/// (the per-property budget alone bounds the rescue).
pub fn rescue_slice_ms() -> Option<u64> {
    parse_budget_ms_with_default(
        crate::adapter::env_var::read(RESCUE_SLICE_ENV),
        DEFAULT_RESCUE_SLICE_MS,
    )
}

/// A solver query timeout that may GROW to the budget's remaining time: `default_ms` when no
/// budget is in effect, else the remaining time (never 0; an expired budget yields 1 ms — the
/// caller's [`expired`] poll is what stops the loop). The counterpart of [`clamp_query_ms`], which
/// only ever shrinks: a rescue member that polls the budget between queries may let one query run
/// to the deadline, which is what turns a budget into decisions (mununu#726).
pub fn query_ms_up_to_remaining(default_ms: u32) -> u32 {
    match current().remaining_ms() {
        Some(rem) => u32::try_from(rem.max(1)).unwrap_or(u32::MAX),
        None => default_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mununu#1063 — work that never polls is left behind when the budget runs out: the caller
    /// gets `Abandoned` at about the deadline, not when the work ends. The worker here stands
    /// for one long OxiDD apply. Before `run_or_abandon` the exact first pass ran such work on
    /// the driver thread, and a 60 s budget did not stop a run that was still going at 300 s.
    #[test]
    fn work_that_never_polls_is_abandoned_at_the_deadline() {
        let _g = enter(&Budget::with_ms(40));
        let t0 = Instant::now();
        let out = run_or_abandon("never-polls", 1 << 20, Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_secs(20));
            7u32
        });
        assert!(matches!(out, Bounded::Abandoned { .. }), "{out:?}");
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the caller waited for the work: {:?}",
            t0.elapsed()
        );
    }

    /// The other three outcomes: work inside the budget is `Done`; with no deadline it runs
    /// inline on the calling thread (same thread id — the unbudgeted path is unchanged); a
    /// worker that panics is `Died`, not a hang and not a caller panic. The worker re-enters the
    /// caller's budget, which a spawned thread does not inherit.
    #[test]
    fn run_or_abandon_returns_the_work_inline_or_from_the_worker_and_survives_a_panic() {
        let here = std::thread::current().id();
        let inline = run_or_abandon("inline", 1 << 20, Duration::ZERO, move || {
            std::thread::current().id() == here
        });
        assert_eq!(
            inline,
            Bounded::Done(true),
            "no deadline ⇒ the calling thread"
        );

        let _g = enter(&Budget::with_ms(60_000));
        let worker = run_or_abandon("worker", 1 << 20, Duration::ZERO, move || {
            (
                std::thread::current().id() == here,
                current().remaining_ms().is_some(),
            )
        });
        assert_eq!(
            worker,
            Bounded::Done((false, true)),
            "a deadline ⇒ a worker thread that carries the caller's budget"
        );

        let died: Bounded<u32> =
            run_or_abandon("panics", 1 << 20, Duration::ZERO, || panic!("worker panic"));
        assert_eq!(died, Bounded::Died);
    }

    #[test]
    fn unbounded_never_expires_and_has_no_remaining() {
        let b = Budget::unbounded();
        assert!(!b.expired());
        assert_eq!(b.remaining_ms(), None);
    }

    #[test]
    fn a_zero_budget_is_immediately_expired() {
        assert!(Budget::with_ms(0).expired());
    }

    #[test]
    fn cancel_expires_even_an_unbounded_budget() {
        let b = Budget::unbounded();
        assert!(!b.expired());
        b.cancel();
        assert!(b.expired(), "cancellation is independent of the deadline");
    }

    #[test]
    /// A per-property budget must never outlive the run budget, however generous it is.
    fn child_clamps_to_the_earlier_deadline() {
        let outer = Budget::with_ms(0); // already expired
        let child = Budget::child(&outer, Some(60_000), SetBy::Mununu);
        assert!(
            child.expired(),
            "a long child budget cannot extend an exhausted run budget"
        );

        let generous = Budget::with_ms(60_000);
        let short = Budget::child(&generous, Some(0), SetBy::Mununu);
        assert!(short.expired(), "the child's own deadline still applies");

        let inherit = Budget::child(&generous, None, SetBy::Mununu);
        assert!(!inherit.expired());
        assert!(
            inherit.remaining_ms().is_some(),
            "inherits the parent deadline"
        );
    }

    #[test]
    fn cancelling_the_parent_cancels_the_child() {
        let outer = Budget::unbounded();
        let child = Budget::child(&outer, Some(60_000), SetBy::Mununu);
        outer.cancel();
        assert!(child.expired(), "the cancel flag is shared");
    }

    /// mununu#1197 — a budget reports who set the deadline that BINDS, for every pair of setters:
    /// the child's own when it is the earlier one, the parent's when the parent's is.
    #[test]
    fn x1197_a_child_reports_the_setter_of_the_deadline_that_binds() {
        for outer_by in SetBy::ALL {
            for own_by in SetBy::ALL {
                let far = Budget::child(&Budget::unbounded(), Some(3_600_000), outer_by);
                assert_eq!(far.set_by(), outer_by, "a first deadline is its setter's");
                let shorter = Budget::child(&far, Some(1_000), own_by);
                assert_eq!(
                    shorter.set_by(),
                    own_by,
                    "{outer_by:?} then a shorter {own_by:?}"
                );
                let longer = Budget::child(&shorter, Some(3_600_000), outer_by);
                assert_eq!(
                    longer.set_by(),
                    own_by,
                    "a longer child keeps the binding setter"
                );
                let none = Budget::child(&far, None, own_by);
                assert_eq!(
                    none.set_by(),
                    outer_by,
                    "no own deadline: the parent's binds"
                );
                assert_eq!(none.remaining_ms().is_some(), far.remaining_ms().is_some());
            }
        }
    }

    /// With no deadline anywhere a budget still says whose setting WOULD bound the run: the
    /// innermost one a user can change. That is what a stop at a member's default limit names.
    #[test]
    fn x1197_an_unbounded_budget_keeps_the_setting_that_would_bound_it() {
        assert_eq!(Budget::unbounded().set_by(), SetBy::Mununu);
        assert_eq!(Budget::with_ms(5).set_by(), SetBy::Mununu);
        for outer_by in SetBy::ALL {
            for own_by in SetBy::ALL {
                let outer = Budget::child(&Budget::unbounded(), None, outer_by);
                assert!(outer.remaining_ms().is_none());
                assert_eq!(outer.set_by(), outer_by);
                let inner = Budget::child(&outer, None, own_by);
                assert!(inner.remaining_ms().is_none());
                assert_eq!(inner.set_by(), own_by.or(outer_by));
            }
        }
        assert_eq!(SetBy::Mununu.or(SetBy::RunBudgetEnv), SetBy::RunBudgetEnv);
        assert_eq!(
            SetBy::TimeoutFlag.or(SetBy::RunBudgetEnv),
            SetBy::TimeoutFlag
        );
    }

    /// Every setter either names one setting, distinct from every other setter's, or is mununu's
    /// own and names none. The match in `setting` is exhaustive, so a new setter cannot be added
    /// without deciding which; this pins that the decision is visible in the remedy.
    #[test]
    fn x1197_every_setter_names_its_own_setting_or_says_there_is_none() {
        let settings: Vec<&str> = SetBy::ALL.iter().filter_map(|s| s.setting()).collect();
        for by in SetBy::ALL {
            let remedy = by.remedy();
            assert!(!remedy.ends_with('.'), "{remedy}");
            match by.setting() {
                Some(own) => {
                    assert!(remedy.contains(own), "{by:?}: {remedy}");
                    assert_eq!(settings.iter().filter(|s| **s == own).count(), 1, "{own}");
                    assert_ne!(remedy, NO_SETTING);
                }
                None => {
                    assert_eq!(by, SetBy::Mununu);
                    assert_eq!(remedy, NO_SETTING);
                }
            }
            for other in settings.iter().filter(|s| Some(**s) != by.setting()) {
                assert!(!remedy.contains(other), "{by:?} names {other}: {remedy}");
            }
        }
        // The three variables are the ones this module reads.
        for (by, env) in [
            (SetBy::RunBudgetEnv, RUN_BUDGET_ENV),
            (SetBy::PropertyBudgetEnv, PROPERTY_BUDGET_ENV),
            (SetBy::RescueSliceEnv, RESCUE_SLICE_ENV),
        ] {
            assert_eq!(by.setting(), Some(format!("`{env}`").as_str()));
        }
    }

    #[test]
    fn enter_installs_and_drop_restores() {
        assert!(!expired());
        {
            let _g = enter(&Budget::with_ms(0));
            assert!(expired(), "installed for the scope");
        }
        assert!(!expired(), "restored on drop");
    }

    #[test]
    fn nested_enter_restores_the_outer_budget() {
        let _outer = enter(&Budget::unbounded());
        {
            let _inner = enter(&Budget::with_ms(0));
            assert!(expired());
        }
        assert!(!expired(), "the OUTER budget is restored, not the default");
    }

    #[test]
    /// The documented footgun, pinned: a spawned thread starts unbounded. Any spawn site that
    /// should honour the budget must capture `current()` and `enter()` it inside the closure.
    fn budget_is_not_inherited_by_a_spawned_thread() {
        let _g = enter(&Budget::with_ms(0));
        assert!(expired());
        let inherited = std::thread::spawn(expired).join().expect("thread ok");
        assert!(!inherited, "a spawned thread does NOT inherit the budget");

        // ...and the documented workaround works.
        let b = current();
        let honoured = std::thread::spawn(move || {
            let _g = enter(&b);
            expired()
        })
        .join()
        .expect("thread ok");
        assert!(
            honoured,
            "capturing `current()` and re-entering carries it over"
        );
    }

    #[test]
    fn query_ms_up_to_remaining_grows_to_the_budget_and_floors_at_one() {
        assert_eq!(
            query_ms_up_to_remaining(5_000),
            5_000,
            "unbounded: the default"
        );
        let _g = enter(&Budget::with_ms(60_000));
        let q = query_ms_up_to_remaining(5_000);
        assert!(
            q > 50_000 && q <= 60_000,
            "grows to the remaining time, got {q}"
        );
        drop(_g);
        let _g = enter(&Budget::with_ms(0));
        assert_eq!(query_ms_up_to_remaining(5_000), 1, "expired: 1 ms, never 0");
    }

    #[test]
    fn rescue_slice_default_is_the_portfolio_wall_and_zero_disables() {
        assert_eq!(
            parse_budget_ms_with_default(None, DEFAULT_RESCUE_SLICE_MS),
            Some(60_000)
        );
        assert_eq!(
            parse_budget_ms_with_default(Some("0".into()), DEFAULT_RESCUE_SLICE_MS),
            None
        );
        assert_eq!(
            parse_budget_ms_with_default(Some("300000".into()), DEFAULT_RESCUE_SLICE_MS),
            Some(300_000)
        );
    }

    #[test]
    fn clamp_query_ms_never_returns_zero() {
        let _g = enter(&Budget::with_ms(0));
        assert_eq!(
            clamp_query_ms(5000),
            1,
            "0 would mean `no timeout` to z3 — the opposite of an expired budget"
        );
    }

    #[test]
    fn clamp_query_ms_passes_through_when_unbounded() {
        let _g = enter(&Budget::unbounded());
        assert_eq!(clamp_query_ms(5000), 5000);
    }

    #[test]
    /// mununu#504 C5 — the DEFAULTED ladder. Unset now means "use the default", not "unbounded";
    /// `0` is the escape hatch; garbage falls back to the default rather than silently uncapping.
    fn parse_budget_with_default_ladder() {
        assert_eq!(
            parse_budget_ms_with_default(None, 900_000),
            Some(900_000),
            "unset ⇒ the DEFAULT (this is the C5 behaviour change)"
        );
        assert_eq!(
            parse_budget_ms_with_default(Some("0".into()), 900_000),
            None,
            "0 is the explicit escape hatch"
        );
        assert_eq!(
            parse_budget_ms_with_default(Some(" 250 ".into()), 900_000),
            Some(250),
            "an explicit value wins, trimmed"
        );
        assert_eq!(
            parse_budget_ms_with_default(Some("garbage".into()), 900_000),
            Some(900_000),
            "unparseable falls back to the default, NOT to unbounded — the safe direction"
        );
    }

    #[test]
    /// The shipped defaults, pinned so a change is deliberate and shows up in review.
    fn shipped_defaults_are_the_measured_ones() {
        assert_eq!(DEFAULT_PROPERTY_BUDGET_MS, 900_000, "15 min/property");
        assert_eq!(DEFAULT_RUN_BUDGET_MS, 3_600_000, "1 h/run");
        // `const` block: both operands are constants, so this is a COMPILE-TIME check rather
        // than a runtime one — stronger, and it satisfies clippy's `assertions_on_constants`
        // (which CI enforces via `-D warnings`).
        const _: () = assert!(
            DEFAULT_PROPERTY_BUDGET_MS * 2 < DEFAULT_RUN_BUDGET_MS,
            "the run budget must comfortably exceed a single property's, else one slow property \
             consumes the whole run"
        );
    }

    #[test]
    fn parse_budget_ladder() {
        assert_eq!(parse_budget_ms(None), None, "unset ⇒ unbounded");
        assert_eq!(parse_budget_ms(Some("0".into())), None, "0 ⇒ unbounded");
        assert_eq!(parse_budget_ms(Some("  250 ".into())), Some(250), "trimmed");
        assert_eq!(parse_budget_ms(Some("nonsense".into())), None);
    }
}
