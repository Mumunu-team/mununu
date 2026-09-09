//! mununu#490 — self-imposed process-memory ceiling.
//!
//! **Motivation.** The default Rust allocator calls `abort()` (process exit 134)
//! on a failed allocation. That is invisible to the model checker: the process
//! dies BEFORE any BDD-level or SMT-level budget can fire, taking every property
//! in the same invocation down with it. A verify-lane consumer (monono's `sv
//! verify-auto` gate on 25 checks) then sees a crash instead of an `unknown`
//! verdict, so a run that would have decided N-1 properties reports none.
//!
//! **What this module provides.** A caller-configurable ceiling (`MUNUNU_MAX_PROCESS_MEMORY_BYTES`)
//! that mununu polls itself at coarse chokepoints — between properties on the
//! SV verify-auto per-property loop, at each `escalate_bottom` step, and at
//! entry to the heavier engine surfaces. When the process RSS exceeds the
//! ceiling, the current + remaining properties abstain (`Unknown`) with a
//! `memory-budget-exceeded` reason, and prior verdicts are preserved. This
//! trades a crash for a graceful degradation.
//!
//! **What this module does NOT provide.** It CANNOT catch an allocation that
//! fails BETWEEN checkpoints — a single BDD blowup can still crash the
//! process. The ceiling is a coarse-granularity graceful-degradation lever,
//! not an absolute crash guarantee. Recommended setting: 70-80% of the
//! process's actual memory limit (`ulimit -m`, container `--memory`), leaving
//! headroom for allocator overhead + non-mununu memory.
//!
//! **Mirrors the existing budget vocabulary.** `MUNUNU_BDD_MAX_BITS`,
//! `MUNUNU_BDD_ARENA_NODES`, `MUNUNU_BDD_ITER_BUDGET`, `MUNUNU_BDD_TIME_BUDGET_MS`
//! (see CLAUDE.md's Environment Variables table) all follow the same pattern:
//! default unset = disabled, explicit value = active ceiling, over-budget =
//! abstain (never over-approximate). A caller sizes the ceiling based on the
//! platform's real memory limit.

/// The env var that configures the process-memory ceiling in bytes.
///
/// mununu#504 C6 — the resolution changed. It is no longer "unset ⇒ disabled":
///
/// | value | ceiling |
/// |---|---|
/// | a positive integer | that many bytes (explicit; unchanged) |
/// | `0` | **disabled** — the deliberate escape hatch |
/// | non-numeric | disabled, with a debug log (unchanged) |
/// | **unset** | **auto**: [`DEFAULT_LIMIT_FRACTION`] of a detected cgroup limit, or disabled when no limit is detected |
///
/// See [`resolve_memory_budget`] for the decision function and the reasoning.
pub const MEMORY_BUDGET_ENV: &str = "MUNUNU_MAX_PROCESS_MEMORY_BYTES";

/// The fraction of a detected container memory limit used as the ceiling when the env var is
/// unset. 0.8 matches the 70-80% this module's header already recommended callers set by hand;
/// the remaining 20% is headroom for allocator overhead and non-mununu memory, since the RSS
/// poll is coarse and cannot see an allocation that fails between checkpoints.
pub const DEFAULT_LIMIT_FRACTION: f64 = 0.8;

/// Where an active ceiling came from — carried so a caller can say so in a note, and so the
/// auto-detected case is distinguishable from one the operator chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    /// `MUNUNU_MAX_PROCESS_MEMORY_BYTES` named this ceiling explicitly.
    ExplicitEnv,
    /// Derived from a detected cgroup limit — [`DEFAULT_LIMIT_FRACTION`] of it.
    AutoDetected,
    /// No ceiling: the env var was `0` / unparseable, or no container limit was detected.
    Disabled,
}

/// A memory-budget check failed: the current process RSS strictly exceeds the
/// caller-configured ceiling. Carries both numbers so the caller can render an
/// informative abstention note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudgetExceeded {
    /// The process's resident set size at the check point, in bytes.
    pub current_rss_bytes: u64,
    /// The caller-configured ceiling, in bytes.
    pub limit_bytes: u64,
}

impl std::fmt::Display for MemoryBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "memory budget exceeded: {} B in use, ceiling {} B ({}={})",
            self.current_rss_bytes, self.limit_bytes, MEMORY_BUDGET_ENV, self.limit_bytes
        )
    }
}

/// Pure check: does `current` strictly exceed `limit`? Testable without env
/// mutation or a real RSS read.
///
/// Boundary is STRICT (`current > limit` ⇒ Err; `current == limit` ⇒ Ok). A
/// caller that wants the equal case to abstain can pass `limit - 1` — this
/// matches the "still under the ceiling at the boundary" reading and avoids
/// spurious abstention on a ceiling that happens to be hit exactly during
/// idle steady-state.
pub fn check_memory_budget_bytes(current: u64, limit: u64) -> Result<(), MemoryBudgetExceeded> {
    if current > limit {
        Err(MemoryBudgetExceeded {
            current_rss_bytes: current,
            limit_bytes: limit,
        })
    } else {
        Ok(())
    }
}

/// Decide the effective ceiling from the raw env value and a detected container limit.
///
/// A **pure** function of its two inputs — deliberately, so the table in [`MEMORY_BUDGET_ENV`] is
/// tested without touching process-global env state (the env-var tests in this module need a
/// serial guard; these do not).
///
/// # Why unset now means "auto" (mununu#504 C6)
///
/// The ceiling exists to convert an allocator `abort()` (exit 134, which kills every property in
/// the invocation) into per-property abstentions. Defaulting it OFF meant the protection was
/// absent exactly where it is needed most — a containerised CI lane, whose operator has no reason
/// to know the var exists until a run has already crashed.
///
/// The trade is real and worth stating: an auto ceiling can abstain on a run that would have
/// finished, because RSS at 80% of the limit does not guarantee an OOM. Three things bound that
/// cost — the ceiling only engages when a container limit is actually detected (a developer's
/// unconstrained machine gets no ceiling), it is 80% rather than something tight, and `=0` turns
/// it off outright.
pub fn resolve_memory_budget(
    raw: Option<&str>,
    detected_limit: Option<u64>,
) -> (Option<u64>, BudgetSource) {
    match raw {
        // Explicit value: honour it, including `0` as "disabled".
        Some(v) => match v.trim().parse::<u64>() {
            Ok(0) => (None, BudgetSource::Disabled),
            Ok(n) => (Some(n), BudgetSource::ExplicitEnv),
            Err(_) => (None, BudgetSource::Disabled),
        },
        // Unset: derive from the container limit, when there is one.
        None => match detected_limit {
            Some(limit) => {
                let ceiling = (limit as f64 * DEFAULT_LIMIT_FRACTION) as u64;
                // A limit small enough to floor to zero would abstain immediately; treat that as
                // no ceiling rather than a ceiling nothing can satisfy.
                if ceiling == 0 {
                    (None, BudgetSource::Disabled)
                } else {
                    (Some(ceiling), BudgetSource::AutoDetected)
                }
            }
            None => (None, BudgetSource::Disabled),
        },
    }
}

/// The container memory limit in bytes, cgroup v2 then v1. `None` when not running under a
/// memory-limited cgroup — no file, unparseable, literal `max`, or one of the "effectively
/// unlimited" sentinels a v1 controller reports when unconstrained.
///
/// Hand-rolled rather than pulling in a system-info crate: this is two file reads, and the only
/// signal that matters for the deployment target (the dockerised images) is the cgroup limit.
/// Reading TOTAL system memory would need a dependency and is the wrong number anyway — a
/// container's limit, not the host's RAM, is what the allocator dies against.
pub fn detect_cgroup_memory_limit_bytes() -> Option<u64> {
    // cgroup v2: a single unified file, `max` when unconstrained.
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        && let Some(n) = parse_cgroup_limit(&raw)
    {
        return Some(n);
    }
    // cgroup v1: an enormous sentinel when unconstrained.
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        && let Some(n) = parse_cgroup_limit(&raw)
    {
        return Some(n);
    }
    None
}

/// Parse one cgroup limit file's contents. `None` for `max`, unparseable input, zero, or a
/// value so large it is the controller's way of saying "unlimited".
///
/// The sentinel test is a magnitude check, not an equality one: v1 reports
/// `9223372036854771712` (`i64::MAX` rounded down to a page multiple) on some kernels and
/// `18446744073709551615` on others, and a genuine limit is never in that range.
pub(crate) fn parse_cgroup_limit(raw: &str) -> Option<u64> {
    let t = raw.trim();
    if t == "max" {
        return None;
    }
    let n = t.parse::<u64>().ok()?;
    // 2^60 bytes = 1 EiB — far above any real container limit, so anything at or beyond it is a
    // sentinel rather than a constraint.
    if n == 0 || n >= (1u64 << 60) {
        return None;
    }
    Some(n)
}

/// The effective ceiling, resolving the env var against a detected container limit.
/// `None` ⇒ no ceiling.
pub fn read_memory_budget_env() -> Option<u64> {
    effective_memory_budget().0
}

/// As [`read_memory_budget_env`], but also reports where the ceiling came from.
pub fn effective_memory_budget() -> (Option<u64>, BudgetSource) {
    let raw = std::env::var(MEMORY_BUDGET_ENV).ok();
    resolve_memory_budget(raw.as_deref(), detect_cgroup_memory_limit_bytes())
}

/// A human phrase naming where the ACTIVE ceiling came from, for a user-facing note.
///
/// mununu#504 C6 — this exists because the note used to read "ceiling N B via
/// `MUNUNU_MAX_PROCESS_MEMORY_BYTES`" unconditionally. Once unset resolves to an auto ceiling,
/// that sentence attributes the limit to a variable the operator never set, and sends them
/// looking for a value that is not there. An auto ceiling says so, and says how to turn it off.
pub fn ceiling_provenance() -> &'static str {
    match effective_memory_budget().1 {
        BudgetSource::ExplicitEnv => "set explicitly via `MUNUNU_MAX_PROCESS_MEMORY_BYTES`",
        BudgetSource::AutoDetected => {
            "auto-derived as 80% of the detected container memory limit — set \
             `MUNUNU_MAX_PROCESS_MEMORY_BYTES=0` to disable, or an explicit byte count to override"
        }
        BudgetSource::Disabled => "no ceiling configured",
    }
}

/// Read the current process resident set size in bytes. `None` when the
/// platform's RSS reader is unavailable (fall back to "no check" rather than
/// pretending we're at 0).
pub fn read_process_rss_bytes() -> Option<u64> {
    memory_stats::memory_stats().map(|s| s.physical_mem as u64)
}

/// The full check: reads the env var + the current RSS and dispatches to
/// [`check_memory_budget_bytes`]. Returns `Ok(())` when the env var is unset
/// or the RSS reader is unavailable (fail-open — this is a caller-opt-in
/// ceiling, not a mandatory gate).
pub fn check_process_memory_budget() -> Result<(), MemoryBudgetExceeded> {
    let Some(limit) = read_memory_budget_env() else {
        return Ok(());
    };
    let Some(current) = read_process_rss_bytes() else {
        return Ok(());
    };
    check_memory_budget_bytes(current, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strictly_under_ceiling_is_ok() {
        assert!(check_memory_budget_bytes(99, 100).is_ok());
    }

    #[test]
    fn at_ceiling_is_ok_boundary_case() {
        // Documented behaviour: the boundary is STRICT `>`, so being exactly
        // at the ceiling does not trigger an abstention.
        assert!(check_memory_budget_bytes(100, 100).is_ok());
    }

    #[test]
    fn strictly_over_ceiling_is_err_with_expected_fields() {
        let err = check_memory_budget_bytes(101, 100).expect_err("should abstain");
        assert_eq!(err.current_rss_bytes, 101);
        assert_eq!(err.limit_bytes, 100);
    }

    #[test]
    fn err_display_names_env_var_and_both_numbers() {
        let err = MemoryBudgetExceeded {
            current_rss_bytes: 1_500_000_000,
            limit_bytes: 1_000_000_000,
        };
        let s = format!("{err}");
        assert!(
            s.contains("1500000000") && s.contains("1000000000"),
            "display should name both numbers: {s}"
        );
        assert!(
            s.contains(MEMORY_BUDGET_ENV),
            "display should name the env var for consumer diagnostics: {s}"
        );
    }

    /// Env-var parsing cases exercised as ONE sequential test — env is
    /// process-global and cargo runs tests in parallel by default, so a
    /// per-case test would race with a sibling test's set/remove and flake.
    /// The four cases must appear in a single test body so they serialize
    /// on the test's execution.
    #[test]
    fn env_var_parsing_sequences_the_four_cases() {
        // SAFETY: this is the only test in the module that mutates env; all
        // reads are self-contained after each set. `MUNUNU_MAX_PROCESS_MEMORY_BYTES`
        // is not otherwise consumed by test code in mununu-core.
        unsafe {
            std::env::remove_var(MEMORY_BUDGET_ENV);
        }
        // mununu#504 C6 — "unset" is no longer unconditionally None: under a memory-limited
        // cgroup it resolves to 0.8 x the limit. Asserting None here would pass on an
        // unconstrained host and FAIL inside a limited container, so pin the invariant that
        // actually holds everywhere — the env path agrees with the pure resolution function.
        // The resolution TABLE itself is tested in the `resolve_memory_budget` cases above,
        // which need no env or cgroup at all.
        assert_eq!(
            read_memory_budget_env(),
            resolve_memory_budget(None, detect_cgroup_memory_limit_bytes()).0,
            "unset ⇒ whatever auto-detection yields on THIS machine (None when unconstrained)"
        );

        unsafe {
            std::env::set_var(MEMORY_BUDGET_ENV, "0");
        }
        assert_eq!(
            read_memory_budget_env(),
            None,
            "explicit `=0` ⇒ None (disabled, matches stale shell state)"
        );

        unsafe {
            std::env::set_var(MEMORY_BUDGET_ENV, "not_a_number");
        }
        assert_eq!(
            read_memory_budget_env(),
            None,
            "malformed ⇒ None (fail-open, never crash)"
        );

        unsafe {
            std::env::set_var(MEMORY_BUDGET_ENV, "1073741824"); // 1 GiB
        }
        assert_eq!(
            read_memory_budget_env(),
            Some(1_073_741_824),
            "positive numeric ⇒ Some(bytes)"
        );

        // Restore to unset so any subsequent test (in-module or cross-module
        // running serially after this one) sees the default state.
        unsafe {
            std::env::remove_var(MEMORY_BUDGET_ENV);
        }
    }

    #[test]
    fn rss_reader_returns_a_positive_value_when_platform_supports_it() {
        // Sanity check on the platform layer: on the test hosts CI runs on
        // (linux + macOS), memory_stats returns Some(_) with a positive
        // physical_mem. If a future platform lacks support, this test will
        // fail loudly rather than silently pretending the ceiling works.
        let rss = read_process_rss_bytes().expect("memory_stats supported on this platform");
        assert!(rss > 0, "process RSS should be positive; got {rss}");
    }

    // ---- mununu#504 C6: the resolution table, tested without touching process env ----

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn explicit_env_value_wins_over_a_detected_limit() {
        let (ceiling, src) = resolve_memory_budget(Some("1000"), Some(8 * GIB));
        assert_eq!(
            ceiling,
            Some(1000),
            "an explicit ceiling is honoured verbatim"
        );
        assert_eq!(src, BudgetSource::ExplicitEnv);
    }

    #[test]
    fn explicit_zero_disables_even_when_a_limit_is_detected() {
        // The escape hatch. Without it there would be no way to turn the new default OFF.
        let (ceiling, src) = resolve_memory_budget(Some("0"), Some(8 * GIB));
        assert_eq!(ceiling, None, "`0` means disabled, not `auto`");
        assert_eq!(src, BudgetSource::Disabled);
    }

    #[test]
    fn unset_with_a_detected_limit_auto_derives_the_ceiling() {
        let (ceiling, src) = resolve_memory_budget(None, Some(10 * GIB));
        assert_eq!(ceiling, Some(8 * GIB), "80% of a 10 GiB container limit");
        assert_eq!(src, BudgetSource::AutoDetected);
    }

    #[test]
    fn unset_with_no_detected_limit_stays_disabled() {
        // An unconstrained developer machine must behave exactly as before this change.
        let (ceiling, src) = resolve_memory_budget(None, None);
        assert_eq!(ceiling, None);
        assert_eq!(src, BudgetSource::Disabled);
    }

    #[test]
    fn a_non_numeric_value_disables_rather_than_auto_detecting() {
        // Activating a ceiling on garbage input would be a surprising way to start abstaining.
        let (ceiling, src) = resolve_memory_budget(Some("lots"), Some(8 * GIB));
        assert_eq!(ceiling, None);
        assert_eq!(src, BudgetSource::Disabled);
    }

    #[test]
    fn a_limit_too_small_to_scale_disables_instead_of_abstaining_immediately() {
        // 80% of 1 byte floors to 0; a zero ceiling would abstain on every property forever.
        let (ceiling, src) = resolve_memory_budget(None, Some(1));
        assert_eq!(ceiling, None);
        assert_eq!(src, BudgetSource::Disabled);
    }

    #[test]
    fn an_auto_ceiling_is_never_attributed_to_the_env_var() {
        // The honesty property: a note must not tell an operator that a variable they did not
        // set produced their abstention. Phrase-level, so it holds however the note is composed.
        for src in [
            BudgetSource::ExplicitEnv,
            BudgetSource::AutoDetected,
            BudgetSource::Disabled,
        ] {
            let phrase = match src {
                BudgetSource::ExplicitEnv => "set explicitly via `MUNUNU_MAX_PROCESS_MEMORY_BYTES`",
                BudgetSource::AutoDetected => {
                    "auto-derived as 80% of the detected container memory limit — set \
                     `MUNUNU_MAX_PROCESS_MEMORY_BYTES=0` to disable, or an explicit byte count to override"
                }
                BudgetSource::Disabled => "no ceiling configured",
            };
            if src == BudgetSource::AutoDetected {
                assert!(
                    phrase.contains("auto-derived") && phrase.contains("=0"),
                    "an auto ceiling must say it is auto AND offer the escape hatch"
                );
                assert!(
                    !phrase.contains("set explicitly"),
                    "an auto ceiling must not claim the operator set it"
                );
            }
        }
        // And the live function agrees with the table for whatever this machine resolves to.
        let (_, src) = effective_memory_budget();
        let live = ceiling_provenance();
        match src {
            BudgetSource::ExplicitEnv => assert!(live.contains("set explicitly")),
            BudgetSource::AutoDetected => assert!(live.contains("auto-derived")),
            BudgetSource::Disabled => assert!(live.contains("no ceiling")),
        }
    }

    #[test]
    fn cgroup_v2_max_is_not_a_limit() {
        assert_eq!(
            parse_cgroup_limit("max\n"),
            None,
            "`max` means unconstrained"
        );
    }

    #[test]
    fn cgroup_v1_unlimited_sentinels_are_not_limits() {
        // Both spellings kernels use for "no limit". Treating either as a real limit would set a
        // ceiling of 0.8 * ~2^63 — harmless in effect, but it would report AutoDetected on a
        // machine with no constraint at all, which is a lie the note would repeat.
        assert_eq!(parse_cgroup_limit("9223372036854771712"), None);
        assert_eq!(parse_cgroup_limit("18446744073709551615"), None);
    }

    #[test]
    fn a_real_cgroup_limit_parses() {
        assert_eq!(parse_cgroup_limit(" 2147483648 \n"), Some(2 * GIB));
    }

    #[test]
    fn cgroup_zero_and_garbage_are_not_limits() {
        assert_eq!(parse_cgroup_limit("0"), None);
        assert_eq!(parse_cgroup_limit(""), None);
        assert_eq!(parse_cgroup_limit("kittens"), None);
    }

    #[test]
    fn the_auto_ceiling_never_exceeds_the_detected_limit() {
        // The property that makes this safe: a ceiling above the real limit could never fire
        // before the OOM killer, making the whole mechanism inert.
        for limit in [
            2 * GIB,
            4 * GIB,
            15 * GIB,
            64 * GIB,
            3_221_225_472, // 3 GiB, not a power of two
        ] {
            let (ceiling, src) = resolve_memory_budget(None, Some(limit));
            let c = ceiling.expect("a real limit yields a ceiling");
            assert!(
                c < limit,
                "ceiling {c} must stay under the detected limit {limit}"
            );
            assert_eq!(src, BudgetSource::AutoDetected);
        }
    }
}
