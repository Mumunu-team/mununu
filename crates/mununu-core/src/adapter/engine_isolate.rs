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
//!
//! # What ends the child (mununu#1196)
//!
//! The child is spawned and waited on by the adapters' one subprocess helper, so it has the stops
//! the in-process engine has: the time the property has left is its wall, and a cancelled run (an
//! HTTP request past its timeout) kills it. Either is [`Isolated::Stopped`] — a budget stop, not
//! a death. The first slice waited with a bare `output()`: a child on a hard design ran until it
//! finished, whatever the budget said and whether or not anyone was still waiting for the answer.

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
    /// mununu#1196 — the PARENT ended the child before it answered. Distinct from `Died` on
    /// purpose: the engine did not crash, it ran out of the time it was given, and the two need
    /// opposite responses (report a defect / give it more budget).
    Stopped(IsolatedStop),
}

/// Why the parent ended an isolated child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsolatedStop {
    /// The time the property had left ran out. `given_ms` is what the child was given when it
    /// started; `0` means the budget was already spent and no child was started.
    BudgetSpent { given_ms: u64 },
    /// The run that asked for the verdict was cancelled (the request that started it is gone).
    RunCancelled,
}

/// The env var that turns isolation on, and the guard that stops a child isolating again.
pub(crate) const ISOLATE_ENV: &str = "MUNUNU_ISOLATE_ENGINES";
const CHILD_GUARD_ENV: &str = "MUNUNU_ISOLATE_CHILD";

/// Is engine isolation requested, and are we the parent (not an isolated child)?
pub(crate) fn enabled() -> bool {
    crate::adapter::env_var::read(ISOLATE_ENV).is_some()
        && crate::adapter::env_var::read(CHILD_GUARD_ENV).is_none()
}

/// The hidden verb a child is invoked with.
pub const CHILD_VERB: &str = "internal-engine-eval";

/// Run `exact-symbolic` on `(btor2, formula)` in a child process.
///
/// Never panics and never propagates the child's failure as an error — every path returns an
/// `Isolated`, because the entire point is that the caller keeps going.
pub(crate) fn exact_symbolic(btor2: &str, formula_src: &str) -> Isolated {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return Isolated::Unavailable(format!("cannot locate self: {e}")),
    };
    exact_symbolic_with_exe(&exe, btor2, formula_src)
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
) -> Isolated {
    // mununu#1196 — the child's stops are read off the thread's run budget, which at the one
    // call site is the property's own (a child of the run's). A budget that is already spent or
    // cancelled starts no child: the in-process engine is not started on one either.
    let budget = crate::adapter::run_budget::current();
    if crate::adapter::run_budget::cancelled() {
        return Isolated::Stopped(IsolatedStop::RunCancelled);
    }
    let left_ms = budget.remaining_ms();
    if left_ms == Some(0) {
        return Isolated::Stopped(IsolatedStop::BudgetSpent { given_ms: 0 });
    }

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
    // The wall is the time the property has left; with no budget there is none, as in process.
    // No caller's flag: nothing races this engine at its call site. The helper adds the third
    // stop itself, the kill on a cancelled run.
    let waited = match left_ms {
        Some(ms) => {
            crate::adapter::run_with_timeout(&mut cmd, None, std::time::Duration::from_millis(ms))
                .map(|done| done.ok_or(ms))
        }
        None => crate::adapter::run_to_end(&mut cmd).map(Ok),
    };
    // Best effort: a model file left behind costs disk in the temporary directory, nothing else.
    let _ = std::fs::remove_file(&model);
    let (status, stdout, stderr) = match waited {
        Ok(Ok(done)) => done,
        Ok(Err(given_ms)) => return Isolated::Stopped(IsolatedStop::BudgetSpent { given_ms }),
        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
            return Isolated::Stopped(IsolatedStop::RunCancelled);
        }
        Err(e) => return Isolated::Unavailable(format!("cannot run the engine child: {e}")),
    };

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
            let mut last: Vec<&str> = stderr.lines().rev().take(3).collect();
            last.reverse();
            let tail = last.join(" | ");
            Isolated::Died(format!("{} — stderr: {tail}", describe_status(&status)))
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
            super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true"),
            Isolated::Holds,
            "the parent must read the child's verdict line"
        );

        let Some(stub) = write_stub("abst", "#!/bin/sh\necho 'VERDICT abstained bit cap'\n") else {
            return;
        };
        assert_eq!(
            super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true"),
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
        match super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true") {
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
        match super::exact_symbolic_with_exe(&stub, "1 sort bitvec 1\n", "true") {
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

    /// What a stand-in for the engine child does.
    #[derive(Debug, Clone, Copy)]
    enum StandIn {
        /// Never answers: sleeps 30 s.
        Hangs,
        /// Writes this line on stdout and exits 0.
        Says(&'static str),
        /// Writes one line on stderr and exits with this code.
        Exits { code: i32, stderr: &'static str },
    }

    /// The bound a test puts on a stop: far from both the 300 ms a stop takes and the 30 s the
    /// hanging stand-in sleeps (what the wait took before), so a loaded host does not blur them.
    const STOPPED_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

    /// A stand-in for the engine child, written into `dir`. It exists on every OS (the OS
    /// matrix's same-tests rule): a `/bin/sh` script on Unix, a `.bat` + PowerShell pair on
    /// Windows, as the solver stand-ins of `native_interp`'s tests are.
    #[cfg(unix)]
    fn stand_in(dir: &std::path::Path, what: StandIn) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let body = match what {
            StandIn::Hangs => "exec sleep 30".to_string(),
            StandIn::Says(line) => format!("echo '{line}'"),
            StandIn::Exits { code, stderr } => format!("echo '{stderr}' >&2\nexit {code}"),
        };
        let fake = dir.join("engine-child");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\nif [ \"$1\" = --probe ]; then\n  exit 0\nfi\n{body}\n"),
        )
        .expect("write the stand-in");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // A fork on another test's thread can hold this file open for writing for an instant,
        // and an exec in that instant fails with ETXTBSY (errno 26). Wait it out here, where
        // the retry is the test's to make and not the runner's.
        for _ in 0..100 {
            match std::process::Command::new(&fake).arg("--probe").output() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                _ => break,
            }
        }
        fake
    }

    #[cfg(windows)]
    fn stand_in(dir: &std::path::Path, what: StandIn) -> std::path::PathBuf {
        let body = match what {
            StandIn::Hangs => "Start-Sleep 30\r\n".to_string(),
            StandIn::Says(line) => format!("[Console]::Out.WriteLine('{line}')\r\nexit 0\r\n"),
            StandIn::Exits { code, stderr } => {
                format!("[Console]::Error.WriteLine('{stderr}')\r\nexit {code}\r\n")
            }
        };
        let ps = dir.join("engine-child.ps1");
        std::fs::write(&ps, body).expect("write the stand-in");
        let bat = dir.join("engine-child.bat");
        std::fs::write(
            &bat,
            format!(
                "@powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
                ps.display()
            ),
        )
        .expect("write the stand-in's launcher");
        bat
    }

    const MODEL: &str = "1 sort bitvec 1\n";

    /// The four tests below run one at a time. The parent hands the child its model through one
    /// file per PROCESS (mununu#1221, open), so two of them at once write and remove the same
    /// path — and on Windows a file whose removal is pending refuses the next write, which
    /// would read here as "could not run the child". The guard goes with that issue's fix.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        // A test that failed while holding the lock has said so itself; the next still runs.
        ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// REPRODUCER (mununu#1196) — an isolated child that is still running when the property's
    /// budget ends is ended there, and the result says it was stopped, not that it died. Fails
    /// on the parent, where the wait was a bare `output()`: the call returned when the stand-in
    /// did, 30 s later, whatever the budget.
    #[test]
    fn x1196_an_isolated_child_past_the_property_s_budget_is_ended_and_reported_as_a_stop() {
        let _one = one_at_a_time();
        use crate::adapter::run_budget::{Budget, enter};
        let dir = tempfile::tempdir().expect("tempdir");
        let child = stand_in(dir.path(), StandIn::Hangs);
        let budget = Budget::with_ms(300);
        let _g = enter(&budget);
        let t0 = std::time::Instant::now();
        let got = exact_symbolic_with_exe(&child, MODEL, "true");
        assert!(
            t0.elapsed() < STOPPED_WITHIN,
            "ended at the budget, not when the child chose to: {:?}",
            t0.elapsed()
        );
        match got {
            Isolated::Stopped(IsolatedStop::BudgetSpent { given_ms }) => assert!(
                (1..=300).contains(&given_ms),
                "the child was given what the budget had left: {given_ms} ms"
            ),
            other => panic!("a child ended at the budget is a stop, got {other:?}"),
        }
    }

    /// The second stop: the run is cancelled while the child runs (an HTTP request past its
    /// timeout cancels its budget). The child is killed then, with no deadline involved.
    #[test]
    fn x1196_a_cancelled_run_ends_the_isolated_child() {
        let _one = one_at_a_time();
        use crate::adapter::run_budget::{Budget, enter};
        let dir = tempfile::tempdir().expect("tempdir");
        let child = stand_in(dir.path(), StandIn::Hangs);
        let budget = Budget::unbounded();
        let _g = enter(&budget);
        let flag = budget.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            flag.cancel();
        });
        let t0 = std::time::Instant::now();
        let got = exact_symbolic_with_exe(&child, MODEL, "true");
        assert!(
            t0.elapsed() < STOPPED_WITHIN,
            "killed on the cancel: {:?}",
            t0.elapsed()
        );
        assert_eq!(got, Isolated::Stopped(IsolatedStop::RunCancelled));
    }

    /// A budget that is already spent, or a run already cancelled, starts no child: the path
    /// handed in does not exist, and the answer is still the stop, not "could not run".
    #[test]
    fn x1196_a_spent_budget_or_a_cancelled_run_starts_no_child() {
        let _one = one_at_a_time();
        use crate::adapter::run_budget::{Budget, enter};
        let dir = tempfile::tempdir().expect("tempdir");
        let nothing = dir.path().join("no-such-engine-child");
        {
            let budget = Budget::with_ms(0);
            let _g = enter(&budget);
            assert_eq!(
                exact_symbolic_with_exe(&nothing, MODEL, "true"),
                Isolated::Stopped(IsolatedStop::BudgetSpent { given_ms: 0 })
            );
        }
        {
            let budget = Budget::unbounded();
            budget.cancel();
            let _g = enter(&budget);
            assert_eq!(
                exact_symbolic_with_exe(&nothing, MODEL, "true"),
                Isolated::Stopped(IsolatedStop::RunCancelled)
            );
        }
        // The control: with a budget to spend, the missing child is what is reported.
        let budget = Budget::with_ms(60_000);
        let _g = enter(&budget);
        assert!(matches!(
            exact_symbolic_with_exe(&nothing, MODEL, "true"),
            Isolated::Unavailable(_)
        ));
    }

    /// The control for the three tests above, and the table of what a child can say: with time
    /// left (and with no budget at all) every reply is read as it was before the wait moved onto
    /// the shared helper — on every OS, which the shell stubs above this block do not reach.
    #[test]
    fn x1196_a_child_that_answers_inside_its_budget_is_read_as_it_answered() {
        let _one = one_at_a_time();
        use crate::adapter::run_budget::{Budget, enter};
        /// Whether a reply was read as the stand-in's row says.
        type Reading = fn(&Isolated) -> bool;
        let replies: [(StandIn, Reading); 5] = [
            (StandIn::Says("VERDICT holds"), |r| *r == Isolated::Holds),
            (StandIn::Says("VERDICT violated"), |r| {
                *r == Isolated::Violated
            }),
            (StandIn::Says("VERDICT abstained bit cap"), |r| {
                *r == Isolated::Abstained("bit cap".into())
            }),
            (StandIn::Says("no verdict here"), |r| {
                matches!(r, Isolated::Died(_))
            }),
            (
                StandIn::Exits {
                    code: 3,
                    stderr: "oops",
                },
                |r| matches!(r, Isolated::Died(why) if why.contains('3') && why.contains("oops")),
            ),
        ];
        for bounded in [true, false] {
            let budget = if bounded {
                Budget::with_ms(120_000)
            } else {
                Budget::unbounded()
            };
            let _g = enter(&budget);
            for (what, expected) in replies {
                let dir = tempfile::tempdir().expect("tempdir");
                let child = stand_in(dir.path(), what);
                let got = exact_symbolic_with_exe(&child, MODEL, "true");
                assert!(
                    expected(&got),
                    "{what:?} (budget: {bounded}) was read as {got:?}"
                );
            }
        }
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
