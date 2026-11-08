//! Track H1: a licence never touches a verdict. The engines, the adapters, the planner, the
//! verify framework and the CTXDSL evaluator do not read `mununu_core::licence`; only the
//! reporting surfaces (`api/`, the CLI) do. This test keeps that structural, so a verdict can
//! never depend on a date, a signature or a file's presence.
//!
//! Negative control first: the scan must see a reference where one is known to be (the report
//! model carries the licence summary), or it is scanning nothing.
use std::path::{Path, PathBuf};

fn core_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The directories whose code decides verdicts: nothing here may name the licence module.
const VERDICT_DIRS: &[&str] = &[
    "adapter",
    "mu_calculus",
    "planner",
    "verify",
    "context",
    "context_dsl",
    "composition",
    "clts",
    "ltl",
    "verdict",
    "trace",
    "abstraction",
    "guard",
    "controllability",
];

/// The spellings a reference to the module takes.
const REFERENCES: &[&str] = &["licence::", "crate::licence", "mununu_core::licence"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn references_in(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .enumerate()
        .filter(|(_, line)| REFERENCES.iter().any(|r| line.contains(r)))
        .map(|(i, line)| format!("{}:{}: {}", path.display(), i + 1, line.trim()))
        .collect()
}

#[test]
fn no_verdict_module_reads_the_licence() {
    let src = core_src();
    // Negative control: the report model and the API handlers reference the module.
    let control: Vec<String> = ["api/models.rs", "api/handlers.rs"]
        .iter()
        .flat_map(|f| references_in(&src.join(f)))
        .collect();
    assert!(
        control.len() >= 2,
        "the scan sees no licence reference in api/models.rs + api/handlers.rs, where there are \
         two by construction; the patterns {REFERENCES:?} no longer match the code"
    );

    let mut files = Vec::new();
    for dir in VERDICT_DIRS {
        let d = src.join(dir);
        if d.is_dir() {
            rust_files(&d, &mut files);
        } else if d.with_extension("rs").is_file() {
            files.push(d.with_extension("rs"));
        }
    }
    assert!(
        files.len() > 50,
        "only {} verdict files found under {}",
        files.len(),
        src.display()
    );
    let hits: Vec<String> = files.iter().flat_map(|f| references_in(f)).collect();
    assert!(
        hits.is_empty(),
        "a verdict path reads the licence module; a verdict must never depend on a licence \
         state (Track H1):\n{}",
        hits.join("\n")
    );
}
