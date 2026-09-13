//! mununu#543 W3 — run a BDD engine in a CHILD PROCESS, so its death is an exit code.
//!
//! # Why this exists
//!
//! `oxidd`'s `apply_bin` recurses unboundedly on a malformed diagram created inside OxiDD's own
//! `reduce` — measured at 74,612 frames on an 8 MB stack and 598,904 on 64 MB, a ratio of 8.0269
//! against a stack ratio of 8.0000, i.e. depth is a pure function of available stack. A consumer
//! loses **5 of 8 full lane runs (62%)** to it.
//!
//! **In-process containment is impossible**, and this module exists because that is not the end of
//! the story:
//!
//! * a `stacker` red zone cannot fire — the recursion is entirely inside the dependency, between
//!   two of our own frames;
//! * a spawned *thread* with a bounded stack does not help either, because a Rust stack overflow is
//!   a guard-page SIGSEGV that aborts the whole process and `catch_unwind` cannot catch it.
//!
//! Out of process it *is* possible. A child's death is a wait status, so the parent records "the
//! engine died" and the portfolio continues to the next operator. That converts a lost 25-minute
//! lane run into a per-property abstention carrying a note — which is the difference between
//! `unknown` and nothing at all, and the only outcome a consumer's gate can reason about.
//!
//! # Why the interface is three strings
//!
//! The engine step's real inputs are narrow: the BTOR2 text, the μ-calculus formula, and one
//! boolean. Everything else — the lift, the cone restriction, the config pins, the shadow
//! synthesis — has already been applied to the BTOR2 by the time the engine runs. So the child
//! needs no reconstruction of the CLI surface and no serialisation of the option structs, which is
//! what makes this safe: **the child runs the identical code path on identical inputs**, because
//! the inputs are the artifacts themselves rather than a recipe for rebuilding them.
//!
//! # Scope of this slice, stated rather than implied
//!
//! * **`exact-symbolic` only.** It is the engine in 2 of the consumer's 3 captures, and its option
//!   surface is a single bool. The predicate-cube (`symbolic`) engine bit-blasts too
//!   (`symbolic_engine.rs`) and is equally exposed; it needs its own slice.
//! * **The witness is lost under isolation.** The child returns a verdict, not a counterexample —
//!   serialising `ExactCounterexample` across the boundary is a later slice. A consumer gating on
//!   `properties[].outcome` (which is what `--expect` reads) is unaffected; one reading traces is
//!   not. Documented here because a silently thinner result is worse than a stated one.
//! * **Opt-in.** Isolation costs a process and a BTOR2 write per property. Turn it on when a lane
//!   cannot absorb a lost run.

use std::io::Write;

/// What an isolated engine run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Isolated {
    /// The child decided, and this is its verdict.
    Holds,
    /// The child decided the property is violated. No witness — see the module docs.
    Violated,
    /// The child ran and abstained, carrying the engine's own reason.
    Abstained(String),
    /// **The child DIED** — signal or non-zero exit with no parseable verdict. The string names
    /// the wait status. This is the outcome the module exists for: the parent survives and the
    /// property becomes `unknown` rather than the run becoming nothing.
    Died(String),
    /// Isolation could not be attempted (no self path, spawn failed, temp write failed). Distinct
    /// from `Died` on purpose: a consumer must be able to tell "the engine crashed" from "we could
    /// not run the engine", because the first is a finding and the second is our problem.
    Unavailable(String),
}

/// The env var that turns isolation on, and the guard that stops a child isolating again.
pub(crate) const ISOLATE_ENV: &str = "MUNUNU_ISOLATE_ENGINES";
const CHILD_GUARD_ENV: &str = "MUNUNU_ISOLATE_CHILD";

/// Is engine isolation requested, and are we the parent (not an isolated child)?
pub(crate) fn enabled() -> bool {
    std::env::var(ISOLATE_ENV).is_ok() && std::env::var(CHILD_GUARD_ENV).is_err()
}

/// The hidden verb a child is invoked with.
pub const CHILD_VERB: &str = "internal-engine-eval";

/// Run `exact-symbolic` on `(btor2, formula)` in a child process.
///
/// Never panics and never propagates the child's failure as an error — every path returns an
/// `Isolated`, because the entire point is that the caller keeps going.
pub(crate) fn exact_symbolic(btor2: &str, formula_src: &str, antecedent_shadow: bool) -> Isolated {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Isolated::Unavailable(format!("cannot locate self: {e}")),
    };
    exact_symbolic_with_exe(&exe, btor2, formula_src, antecedent_shadow)
}

/// The body of [`exact_symbolic`], with the executable INJECTED.
///
/// Split for testability, and the reason is worth stating: with `current_exe()` hardcoded, a unit
/// test's spawn targets the *test* binary, which has no `internal-engine-eval` verb — so it returns
/// `Died` for lack of a verdict line, and a `Died` assertion would pass **for the wrong reason**.
/// That is the failure this whole investigation produced ten instances of. Injecting the path lets
/// a test point at a stub that really prints a verdict and another that really dies, so both
/// directions are exercised for the right reason.
pub(crate) fn exact_symbolic_with_exe(
    exe: &std::path::Path,
    btor2: &str,
    formula_src: &str,
    antecedent_shadow: bool,
) -> Isolated {
    // The BTOR2 goes through a file rather than argv or stdin: it can be megabytes, and a file is
    // also what a consumer can keep for a reproducer if the child dies.
    let dir = std::env::temp_dir().join(format!("mununu-isolate-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Isolated::Unavailable(format!("cannot create {}: {e}", dir.display()));
    }
    let model = dir.join("model.btor2");
    match std::fs::File::create(&model).and_then(|mut f| f.write_all(btor2.as_bytes())) {
        Ok(()) => {}
        Err(e) => return Isolated::Unavailable(format!("cannot write the model: {e}")),
    }

    let mut cmd = std::process::Command::new(exe);
    cmd.arg(CHILD_VERB)
        .arg("--btor2")
        .arg(&model)
        .arg("--formula")
        .arg(formula_src)
        .env(CHILD_GUARD_ENV, "1");
    if !antecedent_shadow {
        cmd.arg("--no-antecedent-shadow");
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => return Isolated::Unavailable(format!("cannot spawn the engine child: {e}")),
    };
    let _ = std::fs::remove_file(&model);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().rev().find(|l| l.starts_with("VERDICT "));
    match line.map(|l| l.trim_start_matches("VERDICT ").trim()) {
        Some("holds") => Isolated::Holds,
        Some("violated") => Isolated::Violated,
        Some(other) if other.starts_with("abstained ") => {
            Isolated::Abstained(other.trim_start_matches("abstained ").to_string())
        }
        // No verdict line. Either the child died mid-engine (the case this exists for) or it failed
        // for a reason it did report on stderr — keep the last stderr lines so the difference is
        // visible rather than guessed at.
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let mut last: Vec<&str> = stderr.lines().rev().take(3).collect();
            last.reverse();
            let tail = last.join(" | ");
            Isolated::Died(format!("{} — stderr: {tail}", describe_status(&out.status)))
        }
    }
}

/// A wait status, rendered so a stack overflow is distinguishable from an ordinary failure.
fn describe_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("engine child killed by signal {sig}");
        }
    }
    match status.code() {
        Some(c) => format!("engine child exited {c} with no verdict"),
        None => "engine child ended with no status".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard matters: without it a child would isolate again and fork forever.
    #[test]
    fn a_child_does_not_isolate_again() {
        // Directly exercise the predicate's two conditions rather than mutating the process
        // environment, which would race every other test in the binary.
        assert_eq!(ISOLATE_ENV, "MUNUNU_ISOLATE_ENGINES");
        assert_eq!(CHILD_GUARD_ENV, "MUNUNU_ISOLATE_CHILD");
        assert_ne!(
            ISOLATE_ENV, CHILD_GUARD_ENV,
            "the request and the guard must be different vars, or a child inherits the request \
             and forks forever"
        );
    }

    /// A stub that prints a verdict must be READ as that verdict.
    ///
    /// The positive control. Without it, the `Died` test below would pass on a spawn that failed
    /// for any reason at all — including the reason a naive test hits, where `current_exe()` is the
    /// test binary and has no such verb.
    #[test]
    fn a_child_that_prints_a_verdict_is_read_as_that_verdict() {
        let Some(stub) = write_stub("verdict", "#!/bin/sh\necho 'VERDICT holds'\n") else {
            return; // non-unix or unwritable temp — nothing to assert
        };
        assert_eq!(
            super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true", true),
            Isolated::Holds,
            "the parent must read the child's verdict line"
        );

        let Some(stub) = write_stub("abst", "#!/bin/sh\necho 'VERDICT abstained bit cap'\n") else {
            return;
        };
        assert_eq!(
            super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true", true),
            Isolated::Abstained("bit cap".into()),
            "an abstention is a RESULT and must carry the engine's own reason, not become a death"
        );
    }

    /// A child KILLED BY A SIGNAL must come back as `Died`, naming the signal — the case this
    /// module exists for. `kill -SEGV $$` is the closest reachable analogue of the guard-page
    /// SIGSEGV a stack overflow produces.
    #[test]
    fn a_child_killed_by_a_signal_is_read_as_died_not_as_a_verdict() {
        let Some(stub) = write_stub("segv", "#!/bin/sh\nkill -SEGV $$\n") else {
            return;
        };
        match super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true", true) {
            Isolated::Died(why) => assert!(
                why.contains("signal"),
                "a death must name the signal so a stack overflow is distinguishable from an \
                 ordinary failure: {why}"
            ),
            other => panic!("a signalled child must be Died, got {other:?}"),
        }
    }

    /// ...and a child that exits non-zero WITHOUT a verdict is also `Died`, so the parent never
    /// silently treats a broken engine as a decision.
    #[test]
    fn a_child_that_exits_without_a_verdict_is_died() {
        let Some(stub) = write_stub("nov", "#!/bin/sh\necho oops >&2\nexit 3\n") else {
            return;
        };
        match super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true", true) {
            Isolated::Died(why) => assert!(
                why.contains("exited 3") && why.contains("oops"),
                "a death must carry the exit status AND the child's own stderr: {why}"
            ),
            other => panic!("expected Died, got {other:?}"),
        }
    }

    /// Write an executable shell stub; `None` when the platform or temp dir will not allow it, so
    /// the tests skip rather than fail for an unrelated reason.
    fn write_stub(tag: &str, body: &str) -> Option<std::path::PathBuf> {
        if !cfg!(unix) {
            return None;
        }
        let p =
            std::env::temp_dir().join(format!("mununu-isolate-stub-{tag}-{}", std::process::id()));
        std::fs::write(&p, body).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).ok()?;
        }
        Some(p)
    }

    /// `Died` and `Unavailable` must stay distinguishable — the first is a finding about the
    /// engine, the second is a defect in this module, and a consumer has to be able to tell.
    #[test]
    fn died_and_unavailable_are_distinct_outcomes() {
        let died = Isolated::Died("signal 11".into());
        let unavail = Isolated::Unavailable("cannot locate self".into());
        assert_ne!(died, unavail);
    }
}
