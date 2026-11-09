//! mununu#875: a timed-out or cancelled solver member is killed with everything it forked.
//!
//! The member path is a launcher that forks the solver instead of `exec`ing it, as a capture or
//! `nice` wrapper does. Before the fix the forked process outlived the member's timeout, and,
//! holding the stdout / stderr pipes, kept the member waiting well past it.
//! mununu#904 — the same two tests run on Windows: the member is a Job Object there, and the
//! launcher is a `.bat` that starts a detached PowerShell sleeper and records its pid.

use mununu_core::adapter::ric3::{Ric3Bin, run_ric3_cancellable};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One test at a time: exec'ing a script while another thread's fork still holds its write
/// descriptor fails with `ETXTBSY`.
static SERIAL: Mutex<()> = Mutex::new(());

/// The member timeout: long enough for the launcher to fork and record the pid before the kill
/// (PowerShell starts in seconds on a CI runner, `sh` in milliseconds).
const MEMBER_TIMEOUT_MS: u64 = if cfg!(windows) { 15_000 } else { 2_000 };

#[cfg(unix)]
fn forking_launcher(dir: &Path, pidfile: &Path, keep_pipes: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let redirect = if keep_pipes {
        ""
    } else {
        " </dev/null >/dev/null 2>&1"
    };
    let script = dir.join("launcher.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsleep 600{redirect} &\necho $! > '{}'\nwait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

/// The Windows launcher: a batch file whose PowerShell starts a second PowerShell that sleeps
/// (the forked solver), writes its pid, and waits for it. With `keep_pipes` the sleeper shares
/// the launcher's stdout / stderr (`-NoNewWindow`), as a forked solver holding the pipes would.
#[cfg(windows)]
fn forking_launcher(dir: &Path, pidfile: &Path, keep_pipes: bool) -> PathBuf {
    let redirect = if keep_pipes {
        ""
    } else {
        " -RedirectStandardOutput NUL -RedirectStandardError NUL"
    };
    let ps = dir.join("launcher.ps1");
    std::fs::write(
        &ps,
        format!(
            "$p = Start-Process -PassThru -NoNewWindow{redirect} powershell -ArgumentList \
             '-NoProfile','-Command','Start-Sleep 600'\r\n\
             Set-Content -Path '{}' -Value $p.Id\r\n\
             Wait-Process -Id $p.Id\r\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let bat = dir.join("launcher.bat");
    std::fs::write(
        &bat,
        format!(
            "@powershell -NoProfile -ExecutionPolicy Bypass -File \"{}\"\r\n",
            ps.display()
        ),
    )
    .unwrap();
    bat
}

#[cfg(unix)]
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn alive(pid: &str) -> bool {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
fn reap(pid: &str) {
    if alive(pid) {
        let _ = std::process::Command::new("kill")
            .args(["-9", pid])
            .status();
    }
}

#[cfg(windows)]
fn reap(pid: &str) {
    if alive(pid) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn wait_for_pid(pidfile: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(if cfg!(windows) { 30 } else { 5 });
    loop {
        if let Ok(s) = std::fs::read_to_string(pidfile)
            && !s.trim().is_empty()
        {
            return s.trim().to_string();
        }
        assert!(
            Instant::now() < deadline,
            "the launcher never wrote its pid"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_timed_out_member_leaves_no_forked_solver_behind() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("solver.pid");
    let bin = Ric3Bin {
        path: forking_launcher(dir.path(), &pidfile, false),
    };
    // 2 s, not 500 ms: on a loaded host the launcher must still get to fork and write its pid
    // before the member's timeout kills the group, or the test cannot observe the property.
    let _ = run_ric3_cancellable(
        &bin,
        "",
        "ic3",
        Duration::from_millis(MEMBER_TIMEOUT_MS),
        None,
    );
    let pid = wait_for_pid(&pidfile);
    // The group kill is issued when the member's timeout fires; on a loaded host the watchdog
    // thread and the kernel's reaping can lag a fixed 200 ms (the pre-commit hook's parallel
    // suite at a load average of 40 made this a flake, 2026-11-08). The property is "does not
    // outlive the timeout", not "is gone within 200 ms": poll, with a bound a stuck solver can
    // never meet — before the fix the forked `sleep 600` survived indefinitely.
    let bound = Duration::from_secs(5);
    let deadline = Instant::now() + bound;
    let mut survived = alive(&pid);
    while survived && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        survived = alive(&pid);
    }
    reap(&pid);
    assert!(
        !survived,
        "the forked solver {pid} outlived its member's timeout by more than {bound:?}"
    );
}

#[test]
fn a_forked_solver_holding_the_pipes_does_not_stall_the_member_past_its_timeout() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("solver.pid");
    let bin = Ric3Bin {
        path: forking_launcher(dir.path(), &pidfile, true),
    };
    // Run the member on its own thread so a stall fails the test instead of hanging it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = run_ric3_cancellable(
            &bin,
            "",
            "ic3",
            Duration::from_millis(MEMBER_TIMEOUT_MS),
            None,
        );
        let _ = tx.send(());
    });
    let returned =
        rx.recv_timeout(Duration::from_millis(MEMBER_TIMEOUT_MS) + Duration::from_secs(8));
    let pid = wait_for_pid(&pidfile);
    reap(&pid);
    assert!(
        returned.is_ok(),
        "the member stalled on pipes its forked solver kept open"
    );
}
