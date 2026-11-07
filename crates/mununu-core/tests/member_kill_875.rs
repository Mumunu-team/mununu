//! mununu#875: a timed-out or cancelled solver member is killed with everything it forked.
//!
//! The member path is a launcher that forks the solver instead of `exec`ing it, as a capture or
//! `nice` wrapper does. Before the fix the forked process outlived the member's timeout, and,
//! holding the stdout / stderr pipes, kept the member waiting well past it.
#![cfg(unix)]

use mununu_core::adapter::ric3::{Ric3Bin, run_ric3_cancellable};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One test at a time: exec'ing a script while another thread's fork still holds its write
/// descriptor fails with `ETXTBSY`.
static SERIAL: Mutex<()> = Mutex::new(());

fn forking_launcher(dir: &Path, pidfile: &Path, keep_pipes: bool) -> PathBuf {
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

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn reap(pid: &str) {
    if alive(pid) {
        let _ = std::process::Command::new("kill")
            .args(["-9", pid])
            .status();
    }
}

fn wait_for_pid(pidfile: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
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
    let _ = run_ric3_cancellable(&bin, "", "ic3", Duration::from_millis(500), None);
    let pid = wait_for_pid(&pidfile);
    std::thread::sleep(Duration::from_millis(200));
    let survived = alive(&pid);
    reap(&pid);
    assert!(
        !survived,
        "the forked solver {pid} outlived its member's timeout"
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
        let _ = run_ric3_cancellable(&bin, "", "ic3", Duration::from_millis(500), None);
        let _ = tx.send(());
    });
    let returned = rx.recv_timeout(Duration::from_secs(10));
    let pid = wait_for_pid(&pidfile);
    reap(&pid);
    assert!(
        returned.is_ok(),
        "the member stalled on pipes its forked solver kept open"
    );
}
