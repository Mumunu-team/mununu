//! mununu#1247 — a test that writes a script and runs it waits out a busy file first.
//!
//! A test writes a `/bin/sh` stand-in for a tool, makes it executable and runs it. A thread of
//! another test that forks while the file is open for writing leaves a child holding a copy of
//! the descriptor until that child's own `exec`, and the kernel refuses to run the script until
//! then ("Text file busy"). The test then fails for a reason that is not its subject, once in
//! many runs. `crate::adapter::stand_in_script::wait_out_a_busy_script` probes the script until
//! the kernel takes it; two writers had a loop of their own for this and two had none.
//!
//! This test reads every Rust file of the workspace's crates. A place that makes a file
//! executable calls the wait within the next few lines, or its file is listed in [`UNWAITED`]
//! with the reason no other thread can fork there. A new writer fails until it does one or the
//! other, and a listed file that no longer matches fails too.

use crate::merged_clock_domains_reported_1181::rel;
use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};

/// What makes a written file executable. In two pieces, so that this file is not a writer.
fn makes_executable() -> String {
    ["Permissions::from_", "mode(0o7"].concat()
}

/// The wait, as a caller outside the helper's module writes it.
const WAIT: &str = "stand_in_script::wait_out_a_busy_script(";

/// The wait is on one of this many lines after the line that makes the file executable.
const WITHIN_LINES: usize = 6;

/// The 1-based lines of `src` that make a file executable with no wait after them.
fn unwaited_writers(src: &str) -> Vec<usize> {
    let needle = makes_executable();
    let lines: Vec<&str> = src.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(&needle))
        .filter(|(at, _)| {
            !lines
                .iter()
                .skip(at + 1)
                .take(WITHIN_LINES)
                .any(|l| l.contains(WAIT))
        })
        .map(|(at, _)| at + 1)
        .collect()
}

/// The lines of `src` that make a file executable, waited on or not.
fn writers(src: &str) -> usize {
    src.matches(&makes_executable()).count()
}

/// `(file under crates/, the writers in it with no wait, why none is needed)`.
const UNWAITED: &[(&str, usize, &str)] = &[
    (
        "mununu-core/src/adapter/mod.rs",
        1,
        "the wait's own test: it holds the script open on purpose and asks the kernel itself",
    ),
    (
        "mununu-core/tests/member_kill_875.rs",
        1,
        "the binary's tests run one at a time under one lock, so no other thread forks while a \
         launcher is open for writing",
    ),
    (
        "mununu-core/tests/probe_timeout_876.rs",
        1,
        "the binary has one test, so no other thread forks",
    ),
];

#[test]
fn every_test_that_writes_a_script_and_runs_it_waits_out_a_busy_file() {
    let crates = repo_root().join("crates");
    let sources: Vec<(String, String)> = rust_files_under(&crates)
        .into_iter()
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &crates), src)
        })
        .collect();
    assert!(
        sources.len() > 100,
        "{} source files — the walk broke",
        sources.len()
    );

    let found: Vec<(&str, Vec<usize>)> = sources
        .iter()
        .map(|(file, src)| (file.as_str(), unwaited_writers(src)))
        .filter(|(_, lines)| !lines.is_empty())
        .collect();
    let mut counted: Vec<(&str, usize)> =
        found.iter().map(|(f, lines)| (*f, lines.len())).collect();
    counted.sort();
    let mut listed: Vec<(&str, usize)> = UNWAITED.iter().map(|(f, n, _)| (*f, *n)).collect();
    listed.sort();
    assert_eq!(
        counted, listed,
        "a file is made executable and run with no wait for a busy file (found, by line: \
         {found:?}; listed: {UNWAITED:?}). Call \
         `crate::adapter::stand_in_script::wait_out_a_busy_script(&script, <an argument the \
         script answers at once>)` right after the line that makes it executable. A test binary \
         in which no other thread can fork while the script is written goes in UNWAITED with \
         that reason; an entry there that no longer matches is removed"
    );

    // The needle still finds the writers that do wait: a renamed call would otherwise read as
    // "nothing to wait on".
    let waited: usize = sources
        .iter()
        .map(|(_, src)| writers(src) - unwaited_writers(src).len())
        .sum();
    assert!(
        waited >= 4,
        "{waited} writers call the wait — the scan no longer sees them"
    );
}

/// The scan fails on what it exists to catch (negative control), and only on that.
#[test]
fn a_script_run_with_no_wait_is_seen() {
    let chmod = format!(
        "    std::fs::set_permissions(&p, std::fs::{}55)).unwrap();\n",
        makes_executable()
    );
    let wait = "    crate::adapter::stand_in_script::wait_out_a_busy_script(&p, \"--probe\");\n";

    let bare = format!("fn write(p: &Path) {{\n{chmod}    run(p);\n}}\n");
    assert_eq!(unwaited_writers(&bare), vec![2]);
    assert_eq!(writers(&bare), 1);

    let waited = format!("fn write(p: &Path) {{\n{chmod}{wait}    run(p);\n}}\n");
    assert_eq!(unwaited_writers(&waited), Vec::<usize>::new());
    assert_eq!(writers(&waited), 1);

    let after_a_comment =
        format!("fn write(p: &Path) {{\n{chmod}    // why\n    // more\n{wait}}}\n");
    assert_eq!(unwaited_writers(&after_a_comment), Vec::<usize>::new());

    let filler = "    let _ = 0;\n".repeat(WITHIN_LINES);
    let too_far = format!("fn write(p: &Path) {{\n{chmod}{filler}{wait}}}\n");
    assert_eq!(unwaited_writers(&too_far), vec![2]);

    let one_of_two = format!("fn a(p: &Path) {{\n{chmod}{wait}}}\nfn b(p: &Path) {{\n{chmod}}}\n");
    assert_eq!(unwaited_writers(&one_of_two), vec![6]);
    assert_eq!(writers(&one_of_two), 2);

    let a_loop_of_its_own = format!(
        "fn write(p: &Path) {{\n{chmod}    for _ in 0..100 {{\n        if probe(p) {{ break; }}\n    }}\n}}\n"
    );
    assert_eq!(unwaited_writers(&a_loop_of_its_own), vec![2]);
}
