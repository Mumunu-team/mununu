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

/// A deadline plus a cancellation flag. Cloning shares the flag.
#[derive(Clone, Debug)]
pub struct Budget {
    deadline: Option<Instant>,
    cancel: Arc<AtomicBool>,
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
        }
    }

    /// A budget expiring `ms` from now.
    pub fn with_ms(ms: u64) -> Self {
        Self {
            deadline: Some(Instant::now() + Duration::from_millis(ms)),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `None` ⇒ unbounded, `Some(ms)` ⇒ bounded. Convenience for an env-derived option.
    pub fn with_ms_opt(ms: Option<u64>) -> Self {
        match ms {
            Some(ms) => Self::with_ms(ms),
            None => Self::unbounded(),
        }
    }

    /// A child budget that can never outlive its parent: the effective deadline is the EARLIER of
    /// the parent's and `now + ms`. A per-property budget therefore cannot overrun the run budget,
    /// however many properties there are. The cancel flag is SHARED, so cancelling the parent
    /// cancels every child.
    pub fn child(outer: &Budget, ms: Option<u64>) -> Self {
        let own = ms.map(|ms| Instant::now() + Duration::from_millis(ms));
        let deadline = match (outer.deadline, own) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        Self {
            deadline,
            cancel: Arc::clone(&outer.cancel),
        }
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
/// This is the one that addresses the reported incident. monono lost a full `make formal` run to
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
        std::env::var(PROPERTY_BUDGET_ENV).ok(),
        DEFAULT_PROPERTY_BUDGET_MS,
    )
}

/// The whole-run budget: `MUNUNU_VERIFY_BUDGET_MS`, else [`DEFAULT_RUN_BUDGET_MS`]. `0` disables.
pub fn run_budget_ms() -> Option<u64> {
    parse_budget_ms_with_default(std::env::var(RUN_BUDGET_ENV).ok(), DEFAULT_RUN_BUDGET_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let child = Budget::child(&outer, Some(60_000));
        assert!(
            child.expired(),
            "a long child budget cannot extend an exhausted run budget"
        );

        let generous = Budget::with_ms(60_000);
        let short = Budget::child(&generous, Some(0));
        assert!(short.expired(), "the child's own deadline still applies");

        let inherit = Budget::child(&generous, None);
        assert!(!inherit.expired());
        assert!(
            inherit.remaining_ms().is_some(),
            "inherits the parent deadline"
        );
    }

    #[test]
    fn cancelling_the_parent_cancels_the_child() {
        let outer = Budget::unbounded();
        let child = Budget::child(&outer, Some(60_000));
        outer.cancel();
        assert!(child.expired(), "the cancel flag is shared");
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
        assert!(
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
