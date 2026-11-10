//! Roadmap G5: consumer-private names stay out of shipped output.
//!
//! A consumer's ask numbers, agent ids and issue slugs (`<consumer> ask 26`, `<consumer>-45`,
//! `<consumer>#partsel`) mean nothing to a reader of a published schema or of `--help`; the
//! reference is the mununu issue number. Consumer names belong in the briefings, which are
//! addressed to that consumer and name it by repository. This test is the roadmap's falsifier
//! made mechanical: the published schemas and every `///` / `//!` doc comment in the crates (the
//! source of the schemas' descriptions and of clap's `--help`) carry no consumer name at all.

use std::path::{Path, PathBuf};

const CONSUMERS: &[&str] = &["monono", "rosf"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

fn files_under(root: &Path, dir: &str, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.join(dir)];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name != "target" && name != ".git" {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn hits(text: &str, path: &Path, only_doc_comments: bool) -> Vec<String> {
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim_start();
        if only_doc_comments && !(t.starts_with("///") || t.starts_with("//!")) {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if CONSUMERS.iter().any(|c| lower.contains(c)) {
            found.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
        }
    }
    found
}

#[test]
fn the_published_schemas_name_no_consumer() {
    let root = repo_root();
    let mut found = Vec::new();
    for f in files_under(&root, "docs/api-schemas", "json") {
        let text = std::fs::read_to_string(&f).expect("schema file");
        found.extend(hits(&text, f.strip_prefix(&root).unwrap_or(&f), false));
    }
    assert!(
        found.is_empty(),
        "consumer names in published schemas (they come from doc comments in api/models.rs; \
         use the mununu issue number there and regenerate): {found:#?}"
    );
}

#[test]
fn no_doc_comment_in_the_crates_names_a_consumer() {
    let root = repo_root();
    let mut found = Vec::new();
    for f in files_under(&root, "crates", "rs") {
        let text = std::fs::read_to_string(&f).expect("source file");
        found.extend(hits(&text, f.strip_prefix(&root).unwrap_or(&f), true));
    }
    assert!(
        found.is_empty(),
        "consumer names in `///` or `//!` doc comments (these feed the schemas and --help; a \
         plain `//` comment may name a consumer by repository): {found:#?}"
    );
}

/// Negative control: the scan finds what it is for, in both modes, and ignores a plain comment
/// in doc-comment mode.
#[test]
fn the_scan_reports_a_consumer_name_and_respects_the_doc_comment_mode() {
    let src = "/// the monono ask 26 field\n// monono's `pulse_cdc` (repository name, plain comment)\nfn f() {}\n";
    assert_eq!(hits(src, Path::new("x.rs"), true).len(), 1);
    assert_eq!(hits(src, Path::new("x.rs"), false).len(), 2);
    assert!(hits("/// mununu#553 explains it\n", Path::new("x.rs"), true).is_empty());
}
