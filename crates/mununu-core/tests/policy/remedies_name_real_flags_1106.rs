//! mununu#1106 item 8 — a remedy names a flag or a variable the user can set.
//!
//! `docs/policies/failure-paths.md` Rule 5: a remedy is something the user can do, or it says
//! there is none. The mechanical half: every `--flag` and every `MUNUNU_*` name inside a
//! user-facing message string under `adapter/`, `verify/`, `composition/` and `api/` is a flag
//! some clap verb declares, or a variable `docs/env-vars.md` documents. The two positives the
//! audit found by hand (`--variable-bounds`, which no verb had — item 4; and a note that named a
//! flag of another verb) are the negative control: the checker is fed the first one verbatim
//! below and must flag it, or the test is wrong.
//!
//! "User-facing message string" is approximated as a string literal of six or more words outside
//! the file's `#[cfg(test)]` module; a subprocess argv token (`"--cc"`) has no words, and a test's
//! `#[ignore = "…"]` reason sits in the test module. A flag that belongs to another tool and is
//! named in a message because the user runs that tool (verilator, slang, yosys, cargo,
//! tree-sitter) is listed in `FOREIGN_FLAGS` with its owner, so a new foreign flag is a review
//! question, not a silent pass.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Flags of other tools that a message may name because the user runs that tool.
pub(crate) const FOREIGN_FLAGS: &[(&str, &str)] = &[
    ("assert", "verilator"),
    ("build", "verilator"),
    ("cc", "verilator"),
    ("exe", "verilator"),
    ("public-flat-rw", "verilator"),
    ("top-module", "verilator"),
    ("trace", "verilator"),
    ("ast-json", "slang"),
    ("ignore-assertions", "yosys read_slang"),
    ("features", "cargo"),
    ("language", "tree-sitter"),
];

/// Names that look like variables but are not knobs: a C macro the generated Verilator testbench
/// is compiled with, and a stderr prefix.
/// Spelled with `concat!` so that this file's own text carries no such name: the sibling policy
/// test `env_vars_documented_d1` scans every `.rs` file for `MUNUNU_*` reads.
const NOT_A_VARIABLE: &[&str] = &[
    concat!("MUNUNU", "_TRACE"),
    concat!("MUNUNU", "_DEBUG"),
    concat!("MUNUNU", "_ENV_VAR_TEST_880"),
];

pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

pub(crate) fn rust_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Every `--flag` some clap verb declares: `long = "name"`, and `#[arg(long …)]` on a field
/// (`field_name` → `field-name`).
fn clap_flags(root: &Path) -> BTreeSet<String> {
    let mut flags: BTreeSet<String> = ["help", "version"].iter().map(|s| s.to_string()).collect();
    for file in rust_files_under(&root.join("crates/mununu-cli/src")) {
        let src = std::fs::read_to_string(&file).expect("a CLI source file");
        let mut rest = src.as_str();
        while let Some(i) = rest.find("long = \"") {
            let after = &rest[i + "long = \"".len()..];
            if let Some(end) = after.find('"') {
                flags.insert(after[..end].to_string());
            }
            rest = after;
        }
        // `#[arg(long)]` / `#[arg(long, …)]` followed by the field.
        let mut rest = src.as_str();
        while let Some(i) = rest.find("#[arg(long") {
            let after = &rest[i..];
            let Some(close) = after.find(']') else { break };
            let field_region = &after[close + 1..];
            let field: String = field_region
                .trim_start()
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ")
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !field.is_empty() {
                flags.insert(field.replace('_', "-"));
            }
            rest = &after[close + 1..];
        }
    }
    flags
}

fn documented_env_vars(root: &Path) -> BTreeSet<String> {
    let table = std::fs::read_to_string(root.join("docs/env-vars.md")).expect("docs/env-vars.md");
    names(&table, "MUNUNU_", |c| {
        c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
    })
}

/// Every maximal token starting with `prefix` whose remaining chars satisfy `ok`.
fn names(text: &str, prefix: &str, ok: impl Fn(char) -> bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let start = &rest[i..];
        let body: String = start[prefix.len()..]
            .chars()
            .take_while(|c| ok(*c))
            .collect();
        out.insert(format!("{prefix}{body}"));
        rest = &start[prefix.len()..];
    }
    out
}

/// The `--flag` names a prose literal cites (`--` preceded by neither a word char nor `-`).
fn cited_flags(lit: &str) -> BTreeSet<String> {
    let bytes = lit.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let boundary = i == 0
            || !(bytes[i - 1].is_ascii_alphanumeric()
                || bytes[i - 1] == b'-'
                || bytes[i - 1] == b'_');
        if boundary && bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_lowercase()
        {
            let name: String = lit[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                .collect();
            let name = name.trim_end_matches('-').to_string();
            i += 2 + name.len();
            out.insert(name);
        } else {
            i += 1;
        }
    }
    out
}

/// The string literals of a file's non-test code that read as prose (six or more words).
pub(crate) fn prose_literals(src: &str) -> Vec<(usize, String)> {
    let body = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    };
    let mut out = Vec::new();
    let mut in_lit: Option<usize> = None;
    let mut buf = String::new();
    let mut escaped = false;
    for (i, c) in body.char_indices() {
        match in_lit {
            None => {
                if c == '"' {
                    in_lit = Some(i);
                    buf.clear();
                    escaped = false;
                }
            }
            Some(start) => {
                if escaped {
                    escaped = false;
                    buf.push(c);
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    let words = buf
                        .split(|ch: char| !ch.is_ascii_alphabetic())
                        .filter(|w| w.len() >= 3)
                        .count();
                    if words >= 6 {
                        out.push((body[..start].matches('\n').count() + 1, buf.clone()));
                    }
                    in_lit = None;
                } else {
                    buf.push(c);
                }
            }
        }
    }
    out
}

struct Findings {
    flags: BTreeMap<String, Vec<String>>,
    vars: BTreeMap<String, Vec<String>>,
}

fn check(
    root: &Path,
    files: &[PathBuf],
    flags: &BTreeSet<String>,
    envs: &BTreeSet<String>,
) -> Findings {
    let foreign: BTreeSet<&str> = FOREIGN_FLAGS.iter().map(|(f, _)| *f).collect();
    let mut found = Findings {
        flags: BTreeMap::new(),
        vars: BTreeMap::new(),
    };
    for file in files {
        let src = std::fs::read_to_string(file).expect("a core source file");
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();
        for (line, lit) in prose_literals(&src) {
            for flag in cited_flags(&lit) {
                if !flags.contains(&flag) && !foreign.contains(flag.as_str()) {
                    found
                        .flags
                        .entry(flag)
                        .or_default()
                        .push(format!("{rel}:{line}"));
                }
            }
            for var in names(&lit, "MUNUNU_", |c| {
                c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
            }) {
                if !envs.contains(&var) && !NOT_A_VARIABLE.contains(&var.as_str()) {
                    found
                        .vars
                        .entry(var)
                        .or_default()
                        .push(format!("{rel}:{line}"));
                }
            }
        }
    }
    found
}

#[test]
fn every_flag_and_variable_a_message_names_exists() {
    let root = repo_root();
    let flags = clap_flags(&root);
    assert!(
        flags.len() > 100,
        "the clap flag scan found {} flags — the parser broke",
        flags.len()
    );
    assert!(
        flags.contains("json") && flags.contains("timeout-ms"),
        "{flags:?}"
    );
    let envs = documented_env_vars(&root);
    assert!(
        envs.len() > 50,
        "{} documented variables — the table scan broke",
        envs.len()
    );

    let mut files = Vec::new();
    for dir in ["adapter", "verify", "composition", "api"] {
        files.extend(rust_files_under(
            &root.join("crates/mununu-core/src").join(dir),
        ));
    }
    let found = check(&root, &files, &flags, &envs);
    let mut report = String::new();
    for (flag, sites) in &found.flags {
        report.push_str(&format!(
            "  --{flag} is on no clap verb: {}\n",
            sites.join(", ")
        ));
    }
    for (var, sites) in &found.vars {
        report.push_str(&format!(
            "  {var} is not in docs/env-vars.md: {}\n",
            sites.join(", ")
        ));
    }
    assert!(
        report.is_empty(),
        "a message names a remedy the user cannot take (failure-paths Rule 5):\n{report}\
         Name the flag a verb has (or the annotation / config key that exists), add the variable \
         to docs/env-vars.md, or — for another tool's flag — list it in FOREIGN_FLAGS with its owner."
    );
}

/// Negative control: the checker flags the audit's hand-found positive (the item-4 message as
/// it was on 4106c14f) and a fabricated variable, and lets a real flag and a foreign one pass.
#[test]
fn the_checker_flags_the_known_positives() {
    let root = repo_root();
    let flags = clap_flags(&root);
    let envs = documented_env_vars(&root);
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("probe.rs");
    let knob = concat!("MUNUNU", "_NO_SUCH_KNOB_XYZ");
    let probe = r#"fn f() {
    let a = format!("Context variable '{v}' is numeric with no bounds annotation; skipping (add bounds via __mununu.bounds or --variable-bounds)");
    let b = "set KNOB to raise the budget this member hit before it decided";
    let c = "pass --json to get the same report as a document the gate can read";
    let d = "verilator needs --top-module to pick the design when several modules are present";
    let e = "--cc";
}
#[cfg(test)]
mod tests { const R: &str = "run it with --ignored --nocapture to see the probe lines printed"; }
"#
    .replace("KNOB", knob);
    std::fs::write(&file, probe).unwrap();
    let found = check(dir.path(), &[file], &flags, &envs);
    assert_eq!(
        found.flags.keys().cloned().collect::<Vec<_>>(),
        vec!["variable-bounds".to_string()],
        "{:?}",
        found.flags
    );
    assert_eq!(
        found.vars.keys().cloned().collect::<Vec<_>>(),
        vec![knob.to_string()],
        "{:?}",
        found.vars
    );
}
