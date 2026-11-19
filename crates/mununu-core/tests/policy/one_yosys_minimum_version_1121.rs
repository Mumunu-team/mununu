//! mununu#1121 (Track U2, row 5) — one minimum yosys version in every message and every page.
//!
//! `docs/policies/failure-paths.md` Rule 5: a remedy the user can follow. The audit of
//! 2026-11-19 found the yosys locator asking for `yosys ≥ 0.40` and, twenty lines later, the
//! `read_slang` hint asking for `yosys ≥ 0.67`; a user who installed the first was sent back by
//! the second. The code now carries one constant (`adapter::yosys::YOSYS_MIN_VERSION`) and this
//! test holds every explicit `yosys ≥ N.NN` in the tree — Rust string literals, the docs, the
//! wiki, the README — to a single number, so the next drift between a message and a page fails
//! here instead of on a user's host.
//!
//! Out of scope, by construction: `docs/policies/` (the policy text quotes the audit's finding,
//! both numbers, as history), `docs/consumer-briefings/` and `docs/archive/` (dated records).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

fn files_to_scan(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("CLAUDE.md"), root.join("README.md")];
    for (dir, ext) in [
        ("crates", "rs"),
        ("examples", "md"),
        ("docs", "md"),
        ("wiki", "md"),
    ] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if p.is_dir() {
                    if !matches!(
                        name.as_str(),
                        "target" | "consumer-briefings" | "archive" | "policies"
                    ) {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == ext)
                    && !p.ends_with("one_yosys_minimum_version_1121.rs")
                {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Every `yosys ≥ N.NN` / `yosys >= N.NN` in `text`, with its line number.
fn version_mentions(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let mut rest = line;
        while let Some(at) = rest.find("yosys ") {
            let after = &rest[at + "yosys ".len()..];
            let after = after
                .strip_prefix("≥ ")
                .or_else(|| after.strip_prefix("≥"))
                .or_else(|| after.strip_prefix(">= "))
                .or_else(|| after.strip_prefix(">="));
            if let Some(after) = after {
                let version: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if version.contains('.') {
                    out.push((version, ln + 1));
                }
            }
            rest = &rest[at + "yosys ".len()..];
        }
    }
    out
}

#[test]
fn every_message_and_page_names_the_same_minimum_yosys_version() {
    let root = repo_root();
    let mut by_version: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in files_to_scan(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (version, line) in version_mentions(&text) {
            by_version
                .entry(version)
                .or_default()
                .push(format!("{rel}:{line}"));
        }
    }
    assert!(
        !by_version.is_empty(),
        "the tree names a minimum yosys version somewhere (the locator's message at least)"
    );
    assert_eq!(
        by_version.len(),
        1,
        "the tree names {} different minimum yosys versions; a user who installs the one a \
         message asks for must not be sent back by the next message or page:\n{}",
        by_version.len(),
        by_version
            .iter()
            .map(|(v, sites)| format!("  yosys ≥ {v}: {}", sites.join(", ")))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Negative control: the scanner sees both spellings and both numbers in a text that carries
/// the audit's pair, so an empty result above would not be the scanner missing them.
#[test]
fn the_scanner_finds_both_spellings_and_both_numbers() {
    let text = "install yosys ≥ 0.40\nuse yosys >= 0.67 (built in)\nyosys 0.59 has neither\n";
    let found = version_mentions(text);
    assert_eq!(
        found,
        vec![("0.40".to_string(), 1), ("0.67".to_string(), 2)],
        "{found:?}"
    );
}
