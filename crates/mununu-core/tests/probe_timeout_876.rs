//! mununu#876: a tool that hangs on its discovery probe makes the member unavailable, with the
//! reason, instead of hanging `mununu` before any budget applies.
//!
//! This binary holds one test and sets `MUNUNU_RIC3_PATH` for itself only (an integration test
//! is its own process, so the write races nothing; mununu#880 is the lib-test class). It drives
//! the public `locate_ric3`, which exists on the parent commit, so `make verify-repro` can show
//! the hang: there the probe waits for the launcher's whole `sleep`.
//! mununu#904 — it runs on Windows too, with a `.bat` + PowerShell launcher that starts a
//! detached sleeper the same way.

use mununu_core::adapter::ric3::locate_ric3;
use std::time::{Duration, Instant};

#[test]
fn a_tool_that_hangs_on_its_probe_is_reported_unavailable_within_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("fork.pid");
    // Ignores its arguments and forks a sleeper, as a capture or site wrapper might.
    let launcher = hanging_launcher(dir.path(), &pidfile);
    // SAFETY: the only env write in this process; the test owns the whole binary.
    unsafe { std::env::set_var("MUNUNU_RIC3_PATH", &launcher) };

    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(locate_ric3().map(|b| b.path).map_err(|e| e.message));
    });
    let answer = rx.recv_timeout(Duration::from_secs(25));
    let elapsed = started.elapsed();

    let forked = std::fs::read_to_string(&pidfile).unwrap_or_default();
    let forked_alive = !forked.trim().is_empty() && alive(forked.trim());
    if forked_alive {
        reap(forked.trim());
    }

    let answer = answer.expect("locate_ric3 must return before the launcher's sleep ends");
    let message = answer.expect_err("a tool that never answers its probe is unavailable");
    assert!(
        message.contains("no answer within"),
        "the reason names the probe timeout: {message}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the probe budget is 10 s; locate took {elapsed:?}"
    );
    assert!(
        !forked_alive,
        "the launcher's forked sleeper outlived the probe"
    );
}

#[cfg(unix)]
fn hanging_launcher(dir: &std::path::Path, pidfile: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let launcher = dir.join("hangs.sh");
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nsleep 40 </dev/null >/dev/null 2>&1 &\necho $! > '{}'\nwait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    launcher
}

#[cfg(windows)]
fn hanging_launcher(dir: &std::path::Path, pidfile: &std::path::Path) -> std::path::PathBuf {
    let ps = dir.join("hangs.ps1");
    std::fs::write(
        &ps,
        format!(
            "$p = Start-Process -PassThru -NoNewWindow -RedirectStandardOutput NUL \
             -RedirectStandardError NUL powershell -ArgumentList '-NoProfile','-Command',\
             'Start-Sleep 40'\r\nSet-Content -Path '{}' -Value $p.Id\r\nWait-Process -Id $p.Id\r\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let bat = dir.join("hangs.bat");
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
    let _ = std::process::Command::new("kill")
        .args(["-9", pid])
        .status();
}

#[cfg(windows)]
fn reap(pid: &str) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", pid])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
