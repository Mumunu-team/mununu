//! `server-side-file-read` (docs/defect-classes.md) — under a `[server] file_root`, the layer
//! that takes a request's strings resolves every path through one function,
//! `deployment::resolve_under`.
//!
//! Confinement arrived one path class at a time: the request's path fields, then the paths
//! inside a verify config (mununu#935), then the `#include`s inside the files (mununu#994). Each
//! was a path the API layer handed on that nobody had listed. This test is the list. It holds
//! four tables, each exact, so adding a site without classifying it fails here:
//!
//! - the callers of `resolve_under`, over the whole crate;
//! - the calls that confine a request's path, by the field they confine;
//! - every place the scanned layer turns text into a path or reads the filesystem;
//! - every path-like field name of the API's types.
//!
//! Scope: the scanned layer is `src/api/` and `src/codesign/include_scan.rs`. It is a scan of
//! that layer, not of the code it calls.

use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `path` relative to `src`, with `/` between components on every host (built from the
/// components, never from the display string: mununu#1153).
fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .expect("under src")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
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

/// The product code of `text`, line by line: test modules blanked (wherever they sit), line
/// comments cut.
fn product_lines(text: &str) -> Vec<(usize, String)> {
    crate::source_scan::blank_test_modules_keeping_lines(text)
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.split("//").next().unwrap_or("").to_string()))
        .collect()
}

/// `(file relative to src, product lines)` for every file of the crate, or of the scanned layer.
fn sources(scanned_only: bool) -> Vec<(String, Vec<(usize, String)>)> {
    let root = src_root();
    rust_files(&root)
        .into_iter()
        .map(|f| (rel(&f, &root), f))
        .filter(|(r, _)| !scanned_only || in_scanned_layer(r))
        .map(|(r, f)| {
            let text = std::fs::read_to_string(&f).expect("source file");
            (r, product_lines(&text))
        })
        .collect()
}

fn in_scanned_layer(rel: &str) -> bool {
    rel.starts_with("api/") || rel == "codesign/include_scan.rs"
}

// ---------------------------------------------------------------------------------------------
// 1. The callers of the one function.
// ---------------------------------------------------------------------------------------------

/// `(file, what it resolves)` — every call of `resolve_under` in product code.
const RESOLVE_CALLERS: &[(&str, &str)] = &[
    (
        "api/handlers.rs",
        "confine_under: a request's path field, `base_dir`, a path inside a verify config",
    ),
    (
        "codesign/include_scan.rs",
        "a header the C include scan is about to follow",
    ),
];

fn calls_of(lines: &[(usize, String)], name: &str) -> Vec<usize> {
    let call = format!("{name}(");
    let definition = format!("fn {name}(");
    lines
        .iter()
        .filter(|(_, l)| l.contains(&call) && !l.contains(&definition))
        .map(|(n, _)| *n)
        .collect()
}

#[test]
fn the_callers_of_resolve_under_are_the_listed_ones() {
    let mut found: Vec<String> = Vec::new();
    for (file, lines) in sources(false) {
        for _ in calls_of(&lines, "resolve_under") {
            found.push(file.clone());
        }
    }
    found.sort();
    let mut want: Vec<String> = RESOLVE_CALLERS.iter().map(|(f, _)| f.to_string()).collect();
    want.sort();
    assert_eq!(
        found, want,
        "the callers of `deployment::resolve_under` changed. A new caller is a new way a request \
         reaches a server-side path: add it to RESOLVE_CALLERS with what it resolves, and to the \
         `server-side-file-read` entry of docs/defect-classes.md."
    );
}

// ---------------------------------------------------------------------------------------------
// 2. The calls that confine a request's path, by field.
// ---------------------------------------------------------------------------------------------

/// `(function, the field argument as written)` — every call in the API layer.
const CONFINED_FIELDS: &[(&str, &str)] = &[
    ("confine_request_path", "\"sv_source_path\""),
    ("confine_request_path", "\"sidecar_path\""),
    ("confine_under", "\"base_dir\""),
    // `confine_request_path` and `confine_config_paths` pass the field they were given.
    ("confine_under", "field"),
    ("confine_under", "field"),
];

/// The `index`-th argument of every call of `name` in `text` (calls on one line, as rustfmt
/// leaves these).
fn call_arguments(lines: &[(usize, String)], name: &str, index: usize) -> Vec<String> {
    let call = format!("{name}(");
    let definition = format!("fn {name}(");
    let mut out = Vec::new();
    for (_, l) in lines {
        if l.contains(&definition) {
            continue;
        }
        let mut rest = l.as_str();
        while let Some(at) = rest.find(&call) {
            let after = &rest[at + call.len()..];
            let arg = after.split(',').nth(index).unwrap_or("").trim();
            out.push(arg.trim_end_matches(')').to_string());
            rest = after;
        }
    }
    out
}

#[test]
fn the_fields_the_api_confines_are_the_listed_ones() {
    let mut found: Vec<(String, String)> = Vec::new();
    for (file, lines) in sources(true) {
        if !file.starts_with("api/") {
            continue;
        }
        for arg in call_arguments(&lines, "confine_request_path", 0) {
            found.push(("confine_request_path".to_string(), arg));
        }
        for arg in call_arguments(&lines, "confine_under", 1) {
            found.push(("confine_under".to_string(), arg));
        }
    }
    found.sort();
    let mut want: Vec<(String, String)> = CONFINED_FIELDS
        .iter()
        .map(|(f, a)| (f.to_string(), a.to_string()))
        .collect();
    want.sort();
    assert_eq!(
        found, want,
        "the calls that confine a request's path changed; keep CONFINED_FIELDS and the \
         `server-side-file-read` entry of docs/defect-classes.md in step"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Every place the scanned layer turns text into a path or reads the filesystem.
// ---------------------------------------------------------------------------------------------

/// What the scan looks for: text becoming a path, a canonicalisation, a read.
const TOUCHES: [&str; 6] = [
    "Path::new(",
    "PathBuf::from(",
    ".canonicalize(",
    "fs::read",
    "File::open(",
    "read_dir(",
];

/// `(file, text the line contains, why it needs no confinement of its own)` — one entry per
/// site. A site that resolves a caller's path belongs behind `resolve_under`, not here.
const CLASSIFIED: &[(&str, &str, &str)] = &[
    (
        "api/handlers.rs",
        "detect_format_by_extension(std::path::Path::new(f))",
        "the request's `filename`: read for its extension, never opened",
    ),
    (
        "api/handlers.rs",
        "onto_base(std::path::Path::new(p))",
        "an `include_paths` entry already rewritten by `confine_config_paths`; a search \
         directory for the include scan, which resolves what it follows",
    ),
    (
        "api/handlers.rs",
        "return Ok(std::path::PathBuf::from(raw));",
        "`confine_under` with no root: a developer's own server takes the path as given",
    ),
    (
        "api/handlers.rs",
        "let p = std::path::Path::new(raw);",
        "`confine_under` itself: the path it hands to `resolve_under`",
    ),
    (
        "codesign/include_scan.rs",
        "let key = file.canonicalize()",
        "the scan's visited-set key; it decides nothing",
    ),
    (
        "codesign/include_scan.rs",
        "std::fs::read_to_string(&file)",
        "a config source (confined by `confine_config_paths` before the scan runs) or a header \
         `resolve_under` accepted",
    ),
    (
        "codesign/include_scan.rs",
        "let p = Path::new(target);",
        "an `#include` target, inspected component by component; never opened as written",
    ),
];

/// The touches of `lines` no entry of `classified` for `file` accounts for.
fn unclassified(
    file: &str,
    lines: &[(usize, String)],
    classified: &[(&str, &str, &str)],
) -> Vec<String> {
    lines
        .iter()
        .filter(|(_, l)| TOUCHES.iter().any(|t| l.contains(t)))
        .filter(|(_, l)| {
            !classified
                .iter()
                .any(|(f, needle, _)| *f == file && l.contains(needle))
        })
        .map(|(n, l)| format!("crates/mununu-core/src/{file}:{n}: {}", l.trim()))
        .collect()
}

#[test]
fn every_path_the_scanned_layer_builds_or_reads_is_classified() {
    let all = sources(true);
    let offenders: Vec<String> = all
        .iter()
        .flat_map(|(file, lines)| unclassified(file, lines, CLASSIFIED))
        .collect();
    assert!(
        offenders.is_empty(),
        "under a `[server] file_root` a path the server resolves for a caller goes through \
         `crate::deployment::resolve_under` (the request's fields: `confine_request_path`). \
         Route these through it, or, when the site resolves nothing a caller named, add it to \
         CLASSIFIED with the reason:\n{}",
        offenders.join("\n")
    );
    // A stale entry is a hole: each must still name a line.
    for (file, needle, _) in CLASSIFIED {
        let live = all
            .iter()
            .any(|(f, lines)| f == file && lines.iter().any(|(_, l)| l.contains(needle)));
        assert!(live, "CLASSIFIED names `{needle}` in {file}, which is gone");
    }
}

// ---------------------------------------------------------------------------------------------
// 4. Every path-like field name of the API's types.
// ---------------------------------------------------------------------------------------------

/// A field name with one of these `_`-separated words names a path, a directory or a file.
const PATH_WORDS: [&str; 8] = [
    "path", "paths", "dir", "dirs", "file", "files", "root", "filename",
];

/// `(field name, what it is)`. A field that names a server-side path is in CONFINED_FIELDS too.
const PATH_LIKE_FIELDS: &[(&str, &str)] = &[
    ("sv_source_path", "confined: `confine_request_path`"),
    ("sidecar_path", "confined: `confine_request_path`"),
    ("base_dir", "confined: `confine_under`"),
    ("filename", "a name for format detection, never opened"),
    ("path", "a response field (the cells of a CDC path)"),
    ("file", "a response or report field"),
];

/// The `pub` field names of `lines` that carry a path word.
fn path_like_fields(lines: &[(usize, String)]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|(_, l)| {
            let rest = l.trim_start().strip_prefix("pub ")?;
            let (name, _) = rest.split_once(": ")?;
            let plain = name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit());
            (plain && name.split('_').any(|w| PATH_WORDS.contains(&w))).then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_path_like_field_of_the_api_types_is_classified() {
    let mut found: Vec<String> = sources(true)
        .iter()
        .filter(|(f, _)| f == "api/models.rs" || f == "api/handlers.rs")
        .flat_map(|(_, lines)| path_like_fields(lines))
        .collect();
    found.sort();
    found.dedup();
    let mut want: Vec<String> = PATH_LIKE_FIELDS
        .iter()
        .map(|(n, _)| n.to_string())
        .collect();
    want.sort();
    assert_eq!(
        found, want,
        "a path-like field of the API's types appeared or went. A request field that names a \
         server-side path must be confined (`confine_request_path`) and listed in \
         CONFINED_FIELDS; any other gets its reason in PATH_LIKE_FIELDS."
    );
    for (name, what) in PATH_LIKE_FIELDS {
        if what.starts_with("confined") {
            let quoted = format!("\"{name}\"");
            assert!(
                CONFINED_FIELDS.iter().any(|(_, a)| *a == quoted),
                "`{name}` is listed as confined and no call confines it"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The scan's own controls.
// ---------------------------------------------------------------------------------------------

/// The scan sees a read and a path built from request text, in product code that follows a test
/// module; skips the test module and a comment; and an empty classification accounts for nothing.
#[test]
fn the_scan_sees_an_unlisted_read_and_skips_tests_and_comments() {
    let sample = "fn a(req: &Req) { let p = std::path::PathBuf::from(&req.design_dir); }\n\
                  // std::fs::read_to_string(\"in a comment\")\n\
                  #[cfg(test)]\nmod tests { fn t() { std::fs::read(\"in a test\").ok(); } }\n\
                  fn b(p: &std::path::Path) { let _ = std::fs::read_to_string(p); }\n\
                  fn c(p: &std::path::Path) { let _ = crate::deployment::resolve_under(p, p); }\n\
                  pub struct Req {\n    pub design_dir: String,\n    pub profiles: u8,\n}\n";
    let lines = product_lines(sample);
    let seen = unclassified("api/sample.rs", &lines, &[]);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].contains("sample.rs:1:") && seen[0].contains("PathBuf::from("));
    assert!(seen[1].contains("sample.rs:5:") && seen[1].contains("read_to_string(p)"));
    // A classification of the first site leaves the second.
    let one = [("api/sample.rs", "PathBuf::from(&req.design_dir)", "control")];
    assert_eq!(unclassified("api/sample.rs", &lines, &one).len(), 1);
    // The same needle under another file's name accounts for nothing.
    let other = [("api/other.rs", "PathBuf::from(&req.design_dir)", "control")];
    assert_eq!(unclassified("api/sample.rs", &lines, &other).len(), 2);
    assert_eq!(calls_of(&lines, "resolve_under"), vec![6]);
    assert_eq!(path_like_fields(&lines), vec!["design_dir".to_string()]);
    assert_eq!(
        call_arguments(
            &product_lines(
                "fn f() { confine_under(root.as_deref(), \"base_dir\", &r.base_dir)?; }\n"
            ),
            "confine_under",
            1
        ),
        vec!["\"base_dir\"".to_string()]
    );
}

/// The relative name is built from path components, so it reads the same on every host.
#[test]
fn a_scanned_file_is_named_with_forward_slashes_on_every_host() {
    let root = src_root();
    let nested = root.join("api").join("handlers.rs");
    assert_eq!(rel(&nested, &root), "api/handlers.rs");
    assert!(in_scanned_layer("api/handlers.rs"));
    assert!(in_scanned_layer("codesign/include_scan.rs"));
    assert!(!in_scanned_layer("codesign/mod.rs"));
}
