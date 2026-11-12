//! Roadmap G1: a recipient of this repository can answer "what may I do with this file?" from
//! files in it, without reading a header. `THIRD_PARTY.md` names every directory that holds
//! third-party or derived files, with upstream, commit and licence; `LICENSES/` holds the texts.
//!
//! This test keeps that true mechanically: every tracked file that carries a third-party licence
//! header, or a `UPSTREAM_COMMIT.txt` / `PROVENANCE.md` / `ATTRIBUTION.md` beside it, lives in a
//! directory `THIRD_PARTY.md` lists. A new vendored directory fails here until it is listed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

/// Lines that mark a file as third-party or derived from third-party work.
const HEADER_MARKERS: &[&str] = &[
    "SPDX-License-Identifier:",
    "Copyright lowRISC contributors",
    "Licensed under the Apache License",
];

const PROVENANCE_FILES: &[&str] = &["UPSTREAM_COMMIT.txt", "PROVENANCE.md", "ATTRIBUTION.md"];

/// Directories the markers hit on first-party ground: the repo's own licence files and the
/// crate manifests that name dependency licences, not vendored code.
const FIRST_PARTY_MARKER_DIRS: &[&str] = &["LICENSES", "crates/mununu-core"];

/// The files to inspect: `git ls-files` when git can answer (a developer checkout), else a walk
/// of the tree that skips `.git`, build output and the directories the root `.gitignore` names.
/// In the dev image the checkout is a bind mount owned by another uid, where git refuses to
/// run ("dubious ownership") or is absent; the test must not depend on it.
fn tracked_files(root: &Path) -> Vec<PathBuf> {
    if let Ok(out) = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        && out.status.success()
    {
        return String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    walked_files(root)
}

/// Root `.gitignore` entries without a wildcard, split as git reads them: an entry with a slash
/// before its end (`/experiments/`, `.claude/reviews/`) is a path from the root; one without
/// (`scratchpad/`, `build/`) is a directory name that matches at any depth. Until this split the
/// walk honoured the first kind only when it began with `/`, so a checkout holding ignored local
/// files under `.claude/reviews/` or `scratchpad/` walked directories git never lists.
fn ignore_entries(root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let text = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
    let (mut paths, mut names) = (Vec::new(), Vec::new());
    for l in text.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') || l.starts_with('!') || l.contains('*') {
            continue;
        }
        let body = l.trim_end_matches('/');
        if body.contains('/') {
            paths.push(PathBuf::from(body.trim_start_matches('/')));
        } else {
            names.push(body.to_string());
        }
    }
    (paths, names)
}

fn walked_files(root: &Path) -> Vec<PathBuf> {
    let (ignored, ignored_names) = ignore_entries(root);
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(root.join(&dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = dir.join(entry.file_name());
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git"
                || name == "target"
                || name == ".ci-target"
                || name == "node_modules"
                || ignored.contains(&rel)
                || (entry.file_type().is_ok_and(|t| t.is_dir())
                    && ignored_names.iter().any(|n| n.as_str() == name))
            {
                continue;
            }
            if entry.path().is_dir() {
                // A nested checkout (an agent's worktree under `.claude/worktrees/`, a clone
                // beside the sources) is not part of this one, and `git ls-files` never lists it.
                if entry.path().join(".git").exists() {
                    continue;
                }
                stack.push(rel);
            } else {
                out.push(rel);
            }
        }
    }
    out
}

/// Vendored code and data: the extensions a third-party licence header can mark. Prose and
/// scripts that quote a header (`.rs`, `.md`, `.toml`, `.sh`) are first-party and skipped.
const CODE_EXTENSIONS: &[&str] = &[
    "sv", "svh", "v", "vh", "c", "h", "svd", "btor", "btor2", "yaml", "py",
];

fn carries_marker(root: &Path, rel: &Path) -> bool {
    let ext = rel.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !CODE_EXTENSIONS.contains(&ext) {
        return false;
    }
    let Ok(bytes) = std::fs::read(root.join(rel)) else {
        return false;
    };
    // Headers sit at the top; 4 KiB covers every vendored file's banner.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    HEADER_MARKERS.iter().any(|m| head.contains(m))
}

/// The directories `THIRD_PARTY.md` lists: every backtick-quoted path that names a tracked
/// directory.
fn listed_dirs(doc: &str, root: &Path) -> BTreeSet<PathBuf> {
    doc.split('`')
        .skip(1)
        .step_by(2)
        .map(|p| p.trim_end_matches('/'))
        .filter(|p| !p.is_empty() && root.join(p).is_dir())
        .map(PathBuf::from)
        .collect()
}

/// A directory counts as listed when it, or an ancestor of it, is in the document.
fn is_covered(dir: &Path, listed: &BTreeSet<PathBuf>) -> bool {
    dir.ancestors().any(|a| listed.contains(a))
}

/// Directories that need a `THIRD_PARTY.md` row: those holding a marked file or a provenance
/// file, minus the first-party ground the markers also hit.
fn directories_needing_a_row(root: &Path) -> BTreeSet<PathBuf> {
    directories_needing_a_row_from(root, tracked_files(root))
}

fn directories_needing_a_row_from(root: &Path, files: Vec<PathBuf>) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();
    for rel in files {
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let marked = PROVENANCE_FILES.contains(&name) || carries_marker(root, &rel);
        if !marked {
            continue;
        }
        let dir = rel.parent().unwrap_or(Path::new("")).to_path_buf();
        if FIRST_PARTY_MARKER_DIRS
            .iter()
            .any(|f| dir == Path::new(f) || dir.starts_with(f))
            && !dir.starts_with("crates/mununu-core/tests")
        {
            continue;
        }
        dirs.insert(dir);
    }
    dirs
}

#[test]
fn every_directory_with_third_party_files_is_listed_in_third_party_md() {
    let root = repo_root();
    let doc = std::fs::read_to_string(root.join("THIRD_PARTY.md")).expect("THIRD_PARTY.md");
    let listed = listed_dirs(&doc, &root);
    let missing: Vec<_> = directories_needing_a_row(&root)
        .into_iter()
        .filter(|d| !is_covered(d, &listed))
        .collect();
    assert!(
        missing.is_empty(),
        "directories with third-party or derived files that THIRD_PARTY.md does not list \
         (add a row naming the directory in backticks): {missing:?}"
    );
}

#[test]
fn the_apache_licence_text_is_shipped_in_full() {
    let text = std::fs::read_to_string(repo_root().join("LICENSES/Apache-2.0.txt"))
        .expect("LICENSES/Apache-2.0.txt");
    for needle in [
        "Apache License",
        "Version 2.0, January 2004",
        "4. Redistribution.",
        "APPENDIX: How to apply the Apache License to your work",
    ] {
        assert!(text.contains(needle), "the licence text lacks {needle:?}");
    }
}

/// The fallback walk must see what `git ls-files` sees: the directories needing a row are the
/// same either way, so a CI run that falls back to the walk checks the same ground.
#[test]
fn the_fallback_walk_finds_the_same_directories_as_git() {
    let root = repo_root();
    let git_ok = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .is_ok_and(|o| o.status.success());
    if !git_ok {
        eprintln!("git cannot list this checkout; the walk is the only enumeration here");
        return;
    }
    let via_walk = directories_needing_a_row_from(&root, walked_files(&root));
    let via_git = directories_needing_a_row_from(&root, tracked_files(&root));
    assert_eq!(via_walk, via_git);
}

/// Negative control: the coverage check must fail on a directory the document does not name.
#[test]
fn an_unlisted_directory_is_reported() {
    let listed: BTreeSet<PathBuf> = ["examples/verify/listed/source"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    assert!(is_covered(
        Path::new("examples/verify/listed/source/sub"),
        &listed
    ));
    assert!(!is_covered(
        Path::new("examples/verify/unlisted/source"),
        &listed
    ));
}
