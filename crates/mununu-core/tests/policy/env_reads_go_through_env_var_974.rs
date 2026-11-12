//! mununu#974 — product code in `mununu-core` reads the environment through
//! `crate::adapter::env_var::read`, never `std::env::var` / `std::env::var_os` directly.
//!
//! `env_var::read` lets a test override a variable on its own thread instead of writing the
//! process environment, which libtest shares across a binary's parallel tests (#871, #880). A
//! direct read is a knob a test can only control by a process-global write — the race this
//! closes. Test modules (from a file's first `#[cfg(test)] mod`) may read directly: a manual
//! probe reading its own input races nothing.
//!
//! The allowlist is the reads that must stay `OsString`: a filesystem path from the environment
//! (`env_var::read` returns `String`, so a non-UTF-8 path would read as unset) and two non-knob
//! variables. A new entry needs that reason.

use std::path::{Path, PathBuf};

/// `(file suffix, argument as written)` — the direct reads that stay direct, and why.
const ALLOWED: &[(&str, &str, &str)] = &[
    ("adapter/env_var.rs", "name", "the one sanctioned read"),
    (
        "adapter/yosys/lift_cache.rs",
        "\"PATH\"",
        "PATH is split as an OsString",
    ),
    (
        "deployment.rs",
        "\"MUNUNU_DEPLOYMENT_CONFIG\"",
        "a filesystem path, kept as OsString",
    ),
    (
        "adapter/reach_rescue.rs",
        "RESCUE_MONITOR_DUMP_ENV",
        "a filesystem path, kept as OsString",
    ),
    (
        "bench_support.rs",
        "\"CARGO_TARGET_DIR\"",
        "cargo's own variable, bench helper only",
    ),
];

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
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
    out
}

/// The product part of a source file: everything before its first `#[cfg(test)]` line that
/// opens a module.
fn product_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[cfg(test)]")
            && lines.get(i + 1).is_some_and(|n| {
                let n = n.trim_start();
                n.starts_with("mod ")
                    || n.starts_with("pub mod ")
                    || n.starts_with("pub(crate) mod ")
            })
        {
            break;
        }
        out.push((i + 1, *l));
    }
    out
}

/// Every direct read in product code: `(file, line, argument)`.
fn direct_reads() -> Vec<(String, usize, String)> {
    let root = src_root();
    let mut found = Vec::new();
    for f in rust_files(&root) {
        let text = std::fs::read_to_string(&f).expect("source file");
        let rel = f
            .strip_prefix(&root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        for (n, line) in product_lines(&text) {
            let code = line.split("//").next().unwrap_or("");
            for pat in ["std::env::var(", "std::env::var_os("] {
                let mut rest = code;
                while let Some(at) = rest.find(pat) {
                    let after = &rest[at + pat.len()..];
                    let arg = after.split(')').next().unwrap_or("").trim().to_string();
                    found.push((rel.clone(), n, arg));
                    rest = after;
                }
            }
        }
    }
    found
}

fn allowed(file: &str, arg: &str) -> bool {
    ALLOWED
        .iter()
        .any(|(suffix, a, _)| file.ends_with(suffix) && *a == arg)
}

#[test]
fn product_code_reads_the_environment_through_env_var() {
    let offenders: Vec<String> = direct_reads()
        .into_iter()
        .filter(|(f, _, arg)| !allowed(f, arg))
        .map(|(f, n, arg)| format!("crates/mununu-core/src/{f}:{n}: std::env::var*({arg})"))
        .collect();
    assert!(
        offenders.is_empty(),
        "read these through `crate::adapter::env_var::read` (mununu#974), or add the read to \
         ALLOWED with the reason it must stay direct:\n{}",
        offenders.join("\n")
    );
}

/// The gate's own controls: the scan sees a direct read, stops at a test module, skips a
/// comment, and every allowlist entry still names a real read (a stale entry is a hole).
#[test]
fn the_env_read_gate_sees_what_it_must_and_its_allowlist_is_live() {
    let sample = "fn a() { let x = std::env::var(\"GATE_CONTROL_VAR\"); }\n\
                  // std::env::var(\"IN_A_COMMENT\")\n\
                  #[cfg(test)]\nmod tests { fn b() { std::env::var(\"IN_A_TEST\"); } }\n";
    let seen: Vec<&str> = product_lines(sample)
        .into_iter()
        .map(|(_, l)| l.split("//").next().unwrap_or(""))
        .filter(|l| l.contains("std::env::var("))
        .collect();
    assert_eq!(seen.len(), 1, "exactly the product read: {seen:?}");
    assert!(seen[0].contains("GATE_CONTROL_VAR"));

    let reads = direct_reads();
    for (suffix, arg, why) in ALLOWED {
        assert!(
            reads
                .iter()
                .any(|(f, _, a)| f.ends_with(suffix) && a == arg),
            "ALLOWED entry ({suffix}, {arg}, {why}) matches no read; remove it"
        );
    }
}
