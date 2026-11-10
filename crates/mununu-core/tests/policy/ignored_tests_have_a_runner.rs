//! mununu#952 / #953: every `#[ignore]`d test has a runner.
//!
//! An ignored test runs only when something asks for it. The e2e sweep (`make e2e`,
//! `e2e-oneshot`, `e2e-parallel`) asks for the tests whose name contains `e2e_` in the binaries
//! `scripts/e2e-binaries.txt` lists; nothing asks for any other ignored test. Two reproducers
//! sat outside that selection for weeks, never run (the #849 reproducer in its own binary, the
//! past-shadow non-vacuity control in `adapters` without the prefix). So an ignored test is
//! either selected by the sweep, or says it is run by hand: its reason starts with `manual:`
//! (a probe, a measurement, a study over an external corpus, a tool the image does not carry).
//! Anything else fails here, naming the test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The binaries the sweep builds, from the list every e2e runner reads (`lib` = the crate's
/// unit tests).
fn swept_binaries() -> BTreeSet<String> {
    let text = std::fs::read_to_string(repo_root().join("scripts/e2e-binaries.txt"))
        .expect("scripts/e2e-binaries.txt is readable");
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next().map(str::to_string))
        .collect()
}

/// One `#[ignore]`d test: where it is, which binary holds it, its name and its reason.
#[derive(Debug)]
struct Ignored {
    file: String,
    line: usize,
    binary: String,
    name: String,
    reason: Option<String>,
}

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

/// The binary a mununu-core source file compiles into: `lib` under `src/`, the first path
/// component under `tests/` (`tests/x.rs` → `x`, `tests/x/…` → `x`).
fn binary_of(rel: &Path) -> Option<String> {
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    match parts.next()?.as_str() {
        "src" => Some("lib".into()),
        "tests" => parts.next().map(|f| f.trim_end_matches(".rs").to_string()),
        _ => None,
    }
}

fn ignored_tests() -> Vec<Ignored> {
    let crates = repo_root().join("crates");
    let mut out = Vec::new();
    for krate in std::fs::read_dir(&crates)
        .expect("crates/ is readable")
        .flatten()
    {
        let base = krate.path();
        let crate_name = krate.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        rust_files(&base.join("src"), &mut files);
        rust_files(&base.join("tests"), &mut files);
        for path in files {
            let rel = path.strip_prefix(&base).expect("under the crate");
            let Some(binary) = binary_of(rel) else {
                continue;
            };
            let text = std::fs::read_to_string(&path).expect("a source file is UTF-8");
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if !t.starts_with("#[ignore") || t.starts_with("#[ignore = $") {
                    continue; // not an attribute, or a macro's pattern
                }
                // The attribute, joined across lines until its closing `]`.
                let mut attr = t.to_string();
                let mut k = i;
                while !attr.trim_end().ends_with(']') && k + 1 < lines.len() {
                    k += 1;
                    attr.push(' ');
                    attr.push_str(lines[k].trim());
                }
                let reason = attr
                    .split_once('"')
                    .and_then(|(_, rest)| rest.rsplit_once('"'))
                    .map(|(r, _)| r.to_string());
                let name = lines[k + 1..lines.len().min(k + 15)]
                    .iter()
                    .find_map(|l| {
                        let (_, after) = l.split_once("fn ")?;
                        Some(
                            after
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect::<String>(),
                        )
                    })
                    .unwrap_or_default();
                out.push(Ignored {
                    file: format!(
                        "crates/{crate_name}/{}",
                        rel.to_string_lossy().replace('\\', "/")
                    ),
                    line: i + 1,
                    binary: if crate_name == "mununu-core" {
                        binary.clone()
                    } else {
                        format!("{crate_name}:{binary}")
                    },
                    name,
                    reason,
                });
            }
        }
    }
    out
}

#[test]
fn every_ignored_test_is_swept_or_declared_manual() {
    let swept = swept_binaries();
    let found = ignored_tests();
    assert!(
        found.len() > 50,
        "the scan found only {} ignored tests — the walker is broken, not the tree clean",
        found.len()
    );
    let orphans: Vec<String> = found
        .iter()
        .filter(|t| {
            let selected = swept.contains(&t.binary) && t.name.contains("e2e_");
            let manual = t
                .reason
                .as_deref()
                .is_some_and(|r| r.starts_with("manual:"));
            !selected && !manual
        })
        .map(|t| {
            format!(
                "{}:{} `{}` (binary `{}`, reason {:?})",
                t.file, t.line, t.name, t.binary, t.reason
            )
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "{} ignored test(s) no runner selects — give each the `e2e_` prefix in a binary listed \
         in scripts/e2e-binaries.txt, or a reason starting `manual:` that says how it is run:\n  {}",
        orphans.len(),
        orphans.join("\n  ")
    );
}

/// A test the sweep would select but that also says `manual:` is contradictory: one of the two
/// is stale.
#[test]
fn no_swept_test_also_claims_to_be_manual() {
    let swept = swept_binaries();
    let both: Vec<String> = ignored_tests()
        .into_iter()
        .filter(|t| {
            swept.contains(&t.binary)
                && t.name.contains("e2e_")
                && t.reason
                    .as_deref()
                    .is_some_and(|r| r.starts_with("manual:"))
        })
        .map(|t| format!("{}:{} `{}`", t.file, t.line, t.name))
        .collect();
    assert!(
        both.is_empty(),
        "swept and manual at once:\n  {}",
        both.join("\n  ")
    );
}

/// Every e2e runner reads the list, so the gate's notion of "swept" is the runners'.
#[test]
fn every_e2e_runner_reads_the_binary_list() {
    let root = repo_root();
    let sweep = std::fs::read_to_string(root.join("scripts/e2e-sweep.sh")).expect("sweep");
    assert!(sweep.contains("e2e-binaries.txt"), "scripts/e2e-sweep.sh");
    let make = std::fs::read_to_string(root.join("Makefile")).expect("Makefile");
    // At line start: the `e2e e2e-oneshot e2e-parallel: export …` line names them too.
    for target in ["\ne2e-oneshot:", "\ne2e-parallel:"] {
        let body: String = make
            .split(target)
            .nth(1)
            .unwrap_or_else(|| panic!("Makefile has no {}", target.trim()))
            .lines()
            .skip(1)
            .take_while(|l| l.starts_with('\t'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("e2e-binaries.txt"),
            "Makefile {} does not read the list",
            target.trim()
        );
    }
}
