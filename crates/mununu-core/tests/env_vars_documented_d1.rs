//! Roadmap D1: the knob table and the code agree, both ways.
//!
//! `docs/env-vars.md` is "every `MUNUNU_*` variable the code reads, one line each". A variable the
//! crates read but the table lacks is a knob a consumer cannot set from the table alone (the four
//! budget variables that gate a verdict-shaped `unknown` were in that state); a row for a variable
//! nothing reads is a control that does nothing (two were). A row whose text names `scripts/`
//! documents a variable only a script reads, and is exempt from the second direction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Names the crates mention that are not runtime knobs: test-internal names, and the one
/// `build.rs` sets for `clap` (documented as build-time, so it is allowed to appear in the table).
const NOT_A_RUNTIME_KNOB: &[&str] = &["MUNUNU_ENV_VAR_TEST_880"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("crates")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name() != "target" {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p.ends_with("env_vars_documented_d1.rs")
            {
                // This file's own control strings are not knobs.
                out.push(p);
            }
        }
    }
    out
}

/// Every `"MUNUNU_…"` string literal in the crates.
fn names_read_by_the_code(root: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for f in rust_files(root) {
        let text = std::fs::read_to_string(&f).expect("source file");
        for name in literals(&text) {
            if !NOT_A_RUNTIME_KNOB.contains(&name.as_str()) {
                names.insert(name);
            }
        }
    }
    names
}

fn literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("\"MUNUNU_") {
        let after = &rest[i + 1..];
        let end = after
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(after.len());
        if after[..end].len() > "MUNUNU_".len() && after[end..].starts_with('"') {
            out.push(after[..end].to_string());
        }
        rest = &after[end..];
    }
    out
}

/// A table row: the backticked names in its first cell, and whether its text names `scripts/`.
fn rows(doc: &str) -> Vec<(Vec<String>, bool)> {
    doc.lines()
        .filter(|l| l.starts_with("| `MUNUNU_"))
        .map(|l| {
            let first_cell = l.trim_start_matches('|').split('|').next().unwrap_or("");
            let names = first_cell
                .split('`')
                .skip(1)
                .step_by(2)
                .filter(|n| n.starts_with("MUNUNU_"))
                .map(String::from)
                .collect();
            (names, l.contains("scripts/"))
        })
        .collect()
}

#[test]
fn every_variable_the_code_reads_has_a_row() {
    let root = repo_root();
    let doc = std::fs::read_to_string(root.join("docs/env-vars.md")).expect("docs/env-vars.md");
    let documented: BTreeSet<String> = rows(&doc).into_iter().flat_map(|(n, _)| n).collect();
    let missing: Vec<_> = names_read_by_the_code(&root)
        .difference(&documented)
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "variables the crates read with no row in docs/env-vars.md: {missing:?}"
    );
}

#[test]
fn every_row_names_a_variable_something_reads() {
    let root = repo_root();
    let doc = std::fs::read_to_string(root.join("docs/env-vars.md")).expect("docs/env-vars.md");
    let read = names_read_by_the_code(&root);
    let dead: Vec<_> = rows(&doc)
        .into_iter()
        .filter(|(_, script)| !script)
        .flat_map(|(names, _)| names)
        .filter(|n| !read.contains(n) && n != "MUNUNU_VERSION_LINE")
        .collect();
    assert!(
        dead.is_empty(),
        "rows in docs/env-vars.md for variables nothing in the crates reads (drop the row, or say \
         which `scripts/` file reads it): {dead:?}"
    );
}

#[test]
fn zero_means_off_for_every_budget_knob() {
    let root = repo_root();
    let doc = std::fs::read_to_string(root.join("docs/env-vars.md")).expect("docs/env-vars.md");
    for knob in [
        "MUNUNU_BDD_ITER_BUDGET",
        "MUNUNU_BDD_FIXPOINT_NODES",
        "MUNUNU_BDD_FIXPOINT_ITERS",
        "MUNUNU_BDD_TIME_BUDGET_MS",
    ] {
        let row = doc
            .lines()
            .find(|l| l.starts_with(&format!("| `{knob}`")))
            .unwrap_or_else(|| panic!("no row for {knob}"));
        assert!(
            row.contains("`0` = off"),
            "{knob}'s row must say `0` = off (one spelling for \"no budget\", B-1): {row}"
        );
    }
}

/// Negative control: the literal scanner and the row parser find what they are for.
#[test]
fn the_scanner_and_the_row_parser_work() {
    assert_eq!(
        literals("std::env::var(\"MUNUNU_FOO_1\") + \"MUNUNU_\" + \"MUNUNU_BAR\""),
        vec!["MUNUNU_FOO_1", "MUNUNU_BAR"]
    );
    let parsed =
        rows("| `MUNUNU_A`, `MUNUNU_B` | read by scripts/x.sh |\n| `MUNUNU_C` | a knob |\n");
    assert_eq!(parsed[0].0, vec!["MUNUNU_A", "MUNUNU_B"]);
    assert!(parsed[0].1);
    assert_eq!(parsed[1].0, vec!["MUNUNU_C"]);
    assert!(!parsed[1].1);
}
