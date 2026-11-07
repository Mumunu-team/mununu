//! Roadmap G4: no tracked document points a reader into a path git does not track.
//!
//! 44 Markdown links in design docs and example READMEs cited plans under `.claude/plans/` and
//! review artefacts under `.claude/reviews/`, where git tracks nothing: the author sees the file
//! through a local symlink, a reader of the repository gets nothing. `/docs-traceability` saw none
//! of them, because it validated anchor lines only and tested presence on disk, not tracking.
//! This test is the mechanical half of the fix (D3's other half is the skill's procedure): every
//! `.claude/plans/` or `.claude/reviews/` mention, and every link into a directory the root
//! `.gitignore` names, fails here.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

/// The covered documents: wiki, docs (not the archive, which is history, nor the briefings, which
/// quote other repositories), the README, example READMEs and the skills.
fn covered(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("README.md")];
    let mut stack = vec![
        root.join("wiki"),
        root.join("docs"),
        root.join("examples"),
        root.join(".claude/skills"),
    ];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name != "archive" && name != "consumer-briefings" && name != "target" {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "md") {
                let under_examples = p.strip_prefix(root.join("examples")).is_ok();
                if !under_examples || name == "README.md" {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Root `.gitignore` entries that name a directory, as repo-relative prefixes.
fn ignored_roots(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/') && !l.contains('*'))
        .map(|l| l.trim_matches('/').to_string())
        .collect()
}

/// Lines in `text` that mention an untracked path.
fn offending_lines(text: &str, ignored: &[String]) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            l.contains(".claude/plans/")
                || l.contains(".claude/reviews/")
                || ignored.iter().any(|r| {
                    l.contains(&format!("]({r}/"))
                        || l.contains(&format!("](../{r}/"))
                        || l.contains(&format!("](../../{r}/"))
                        || l.contains(&format!("](../../../{r}/"))
                })
        })
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect()
}

#[test]
fn no_covered_document_points_into_an_untracked_path() {
    let root = repo_root();
    let ignored = ignored_roots(&root);
    let mut found = Vec::new();
    for f in covered(&root) {
        if f.ends_with("no_links_into_untracked_paths_g4.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        let rel = f.strip_prefix(&root).unwrap_or(&f).display().to_string();
        for (ln, line) in offending_lines(&text, &ignored) {
            found.push(format!("{rel}:{ln}: {line}"));
        }
    }
    assert!(
        found.is_empty(),
        "documents pointing into paths git does not track (promote the note under docs/archive/ \
         with a `> Status: historical` banner, or make the sentence name no path): {found:#?}"
    );
}

/// Negative control: a link into a plans path, and into an ignored root, are both caught; a link
/// into a tracked path is not.
#[test]
fn the_scan_catches_plans_links_and_ignored_roots_only() {
    let ignored = vec!["experiments".to_string()];
    let text = "see [x](../../.claude/plans/x.md)\nand [y](../experiments/y.md)\nand [z](../../docs/design/z.md)\n";
    let got: Vec<usize> = offending_lines(text, &ignored)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(got, vec![1, 2]);
}
