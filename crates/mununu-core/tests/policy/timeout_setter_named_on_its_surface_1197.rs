//! mununu#1197 — a budget stop names the setting of the surface that set the budget.
//!
//! A stop that closes with "raise `--timeout-ms`" is a remedy only where that flag was given: on
//! the HTTP API the same limit is the request's `timeout_ms`, and on a verb with neither it is a
//! constant of mununu's own. The report reads the name from who set the limit (`SetBy`, carried
//! on the run budget and on the portfolio's wall), so the text is right exactly when each surface
//! tags the limits it sets with its own setter. This test holds that half: the CLI's setter is
//! named only in the CLI crate and the API's only under `api/`. A core module that named one
//! would put a flag in an API response, or a request field in a terminal.
//!
//! Scanned: product code of both crates (test modules and comment lines dropped). Not scanned:
//! the setters a surface does not own — the `MUNUNU_*` variables read the same way on both.

use std::path::Path;

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};
use crate::source_scan::blank_test_modules_keeping_lines;

/// The module that defines the setters: it names every one.
const DEFINITION: &str = "crates/mununu-core/src/adapter/run_budget.rs";

/// Each surface-owned setter with the path prefix that may name it.
const HOMES: &[(&str, &str)] = &[
    ("TimeoutFlag", "crates/mununu-cli/src/"),
    ("TimeoutField", "crates/mununu-core/src/api/"),
];

fn is_word(text: &str, at: usize, len: usize) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before = text[..at].chars().next_back().is_some_and(ident);
    let after = text[at + len..].chars().next().is_some_and(ident);
    !before && !after
}

/// The lines of one file's product code that name a setter: `(line, setter)`.
fn setters_named(src: &str) -> Vec<(usize, &'static str)> {
    let product = blank_test_modules_keeping_lines(src);
    let mut out = Vec::new();
    for (i, line) in product.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for (setter, _) in HOMES {
            let named = line
                .match_indices(setter)
                .any(|(at, m)| is_word(line, at, m.len()));
            if named {
                out.push((i + 1, *setter));
            }
        }
    }
    out
}

/// Whether the file at `rel` (repository-relative, `/`-separated) may name `setter`.
fn may_name(rel: &str, setter: &str) -> bool {
    rel == DEFINITION
        || HOMES
            .iter()
            .any(|(s, home)| *s == setter && rel.starts_with(home))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn a_surface_setter_is_named_only_by_its_surface() {
    let root = repo_root();
    let mut found: Vec<String> = Vec::new();
    let mut at_home: std::collections::BTreeSet<&'static str> = std::collections::BTreeSet::new();
    for dir in ["crates/mununu-core/src", "crates/mununu-cli/src"] {
        for path in rust_files_under(&root.join(dir)) {
            let rel = relative(&root, &path);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{rel} is readable as text: {e}"));
            for (line, setter) in setters_named(&src) {
                if !may_name(&rel, setter) {
                    found.push(format!("{rel}:{line}: {setter}"));
                } else if rel != DEFINITION {
                    at_home.insert(setter);
                }
            }
        }
    }
    // Control: the scan saw each setter where its surface tags a limit, so an empty finding list
    // is not a scan that read nothing.
    for (setter, home) in HOMES {
        assert!(
            at_home.contains(setter),
            "the scan found no `{setter}` under {home} (control)"
        );
    }
    assert!(
        found.is_empty(),
        "these lines name a setter that belongs to another surface:\n  {}\n\
         A limit's setter is passed in by the surface that read the setting (the CLI for a \
         flag, the API handler for a request field). A module both surfaces reach takes it as \
         a parameter, or the report names a setting its reader does not have.",
        found.join("\n  ")
    );
}

/// What the scan reads and what it leaves: a product line is a finding wherever it is, a test
/// module and a comment are not, and a longer identifier is not the setter.
#[test]
fn the_scan_reads_product_lines_and_skips_tests_and_comments() {
    let src = "fn f() {\n    let by = SetBy::TimeoutFlag;\n    // SetBy::TimeoutField in a comment\n    let other = NotTimeoutFlagAtAll;\n}\n#[cfg(test)]\nmod tests {\n    fn g() { let by = SetBy::TimeoutField; }\n}\nuse run_budget::SetBy::{Mununu, TimeoutField};\n";
    assert_eq!(
        setters_named(src),
        vec![(2, "TimeoutFlag"), (10, "TimeoutField")]
    );
}

/// The homes: each surface names its own setter, the definition names both, nothing else does.
#[test]
fn each_setter_has_one_home_besides_its_definition() {
    assert!(may_name("crates/mununu-cli/src/main.rs", "TimeoutFlag"));
    assert!(!may_name("crates/mununu-cli/src/main.rs", "TimeoutField"));
    assert!(may_name(
        "crates/mununu-core/src/api/handlers.rs",
        "TimeoutField"
    ));
    assert!(!may_name(
        "crates/mununu-core/src/api/handlers.rs",
        "TimeoutFlag"
    ));
    for setter in ["TimeoutFlag", "TimeoutField"] {
        assert!(may_name(DEFINITION, setter));
        assert!(!may_name(
            "crates/mununu-core/src/adapter/reach_portfolio.rs",
            setter
        ));
    }
}
