//! Roadmap D2: every `docs/…md` and `wiki/…md` path the repository cites exists.
//!
//! Five design docs deleted in `ce1da0c6` (2026-05-16) stayed cited from `CLAUDE.md`, nine Rust
//! module docs and two example validation gates for four months, and `/docs-traceability` saw
//! one of them (it checks anchor lines only). This is the one-liner the roadmap asked for, as a
//! test: a path that looks like a repository doc and does not exist fails here.
//!
//! Out of scope, by construction: `docs/consumer-briefings/` quotes paths in the consumer's own
//! repository; a path preceded by `mununu-private/` or by another repository's `blob/<branch>/`
//! is not ours; `docs/archive/` is history and may cite whatever it cited.

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
                    if name != "target" && name != "consumer-briefings" && name != "archive" {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == ext)
                    && !p.ends_with("docs_references_resolve_d2.rs")
                {
                    // This file's own control strings are not citations.
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// The `docs/…md` / `wiki/…md` paths in `text` that are this repository's to resolve, each with
/// the line it sits on.
fn cited_paths(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut from = 0;
        while let Some(i) = line[from..]
            .find("docs/")
            .or_else(|| line[from..].find("wiki/"))
        {
            let start = from + i;
            // The candidate runs over path characters and must end in `.md`.
            let end = start
                + line[start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || "_./-".contains(c)))
                    .unwrap_or(line.len() - start);
            let cand = line[start..end].trim_end_matches('.');
            let prefix = &line[..start];
            let foreign = prefix.ends_with("mununu-private/")
                || prefix.ends_with("blob/main/")
                || prefix.ends_with("blob/master/")
                || (start > 0 && bytes[start - 1] == b':');
            if cand.ends_with(".md") && !foreign {
                out.push((cand.to_string(), ln + 1));
            }
            from = end.max(start + 1);
        }
    }
    out
}

#[test]
fn every_cited_repository_doc_exists() {
    let root = repo_root();
    let mut missing: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for f in files_to_scan(&root) {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
        for (path, ln) in cited_paths(&text) {
            if !root.join(&path).is_file() {
                missing.entry(path).or_default().push(format!("{rel}:{ln}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "cited repository docs that do not exist (restore the file under docs/archive/ with a \
         `> Status: historical` banner, redirect the citation, or remove it): {missing:#?}"
    );
}

/// Negative control: the extractor finds a missing path and skips the foreign ones.
#[test]
fn the_extractor_reports_ours_and_skips_foreign_paths() {
    let text = "see docs/design/gone.md and mununu-private/docs/x.md and \
                https://github.com/o/r/blob/main/docs/y.md and `ce1da0c6^:docs/old.md`";
    let got: Vec<String> = cited_paths(text).into_iter().map(|(p, _)| p).collect();
    assert_eq!(got, vec!["docs/design/gone.md"]);
}
