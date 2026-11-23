//! mununu#1242 — a tool a CI job or an image build compiles from source names its version.
//!
//! `cargo install <tool>` with no `--version` builds the newest release with the toolchain the
//! repository pins. The day a tool publishes a release whose minimum rustc is above that pin,
//! the step fails on every pull request, whatever the pull request changes: `dependency-check`
//! went red that way, and the dev image's `cargo-nextest` install was one release away from the
//! same failure in the image build. This test reads every workflow and every Dockerfile and
//! fails on a `cargo install` that runs with no version, so the next one is refused here.
//!
//! Not scanned: a tool installed as a prebuilt binary (nothing is compiled, so the pinned
//! toolchain is no input), a download checked against a recorded checksum, and the install
//! hints a script prints for a developer (they are text, not a step of a build).

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

/// The logical lines of a workflow or a Dockerfile that RUN: comment lines dropped, a line ending
/// in a backslash joined to the next. Each comes with the number of its first physical line.
fn executed_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('#') {
            // A comment inside a continued Dockerfile RUN does not end the instruction.
            continue;
        }
        let (body, continues) = match line.strip_suffix('\\') {
            Some(b) => (b.trim_end(), true),
            None => (line, false),
        };
        let (start, mut joined) = open.take().unwrap_or((i + 1, String::new()));
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(body);
        if continues {
            open = Some((start, joined));
        } else if !joined.is_empty() {
            out.push((start, joined));
        }
    }
    if let Some(last) = open {
        out.push(last);
    }
    out
}

/// The `cargo install` commands of one logical line: each runs from its `cargo install` to the
/// next shell separator.
fn cargo_installs(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("cargo install") {
        let from = &rest[at..];
        let end = from
            .find("&&")
            .into_iter()
            .chain(from.find("||"))
            .chain(from.find(';'))
            .chain(from.find('|'))
            .min()
            .unwrap_or(from.len());
        out.push(from[..end].trim().to_string());
        rest = &from[end..];
    }
    out
}

/// Whether a `cargo install` command fixes what it builds: a `--version`, or a source the tree or
/// a revision fixes (`--path`, `--rev`, `--tag`).
fn is_pinned(command: &str) -> bool {
    command.split_whitespace().any(|word| {
        ["--version", "--path", "--rev", "--tag"]
            .iter()
            .any(|flag| word == *flag || word.starts_with(&format!("{flag}=")))
    })
}

/// Every `cargo install` a file runs with no fixed version: `(line, command)`.
fn unpinned_installs(text: &str) -> Vec<(usize, String)> {
    executed_lines(text)
        .into_iter()
        .flat_map(|(n, line)| {
            cargo_installs(&line)
                .into_iter()
                .filter(|c| !is_pinned(c))
                .map(move |c| (n, c))
        })
        .collect()
}

/// The files a CI job or an image build executes: every workflow and every Dockerfile.
fn build_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (dir, wanted) in [
        (".github/workflows", &[".yml", ".yaml"][..]),
        ("docker", &["Dockerfile"][..]),
    ] {
        let entries = std::fs::read_dir(root.join(dir))
            .unwrap_or_else(|e| panic!("the directory {dir} is readable: {e}"));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let is_wanted = wanted
                .iter()
                .any(|w| name.ends_with(w) || name.starts_with(w));
            if path.is_file() && is_wanted {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn every_tool_a_build_compiles_from_source_names_its_version() {
    let root = repo_root();
    let files = build_files(&root);
    let names: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    for expected in [".github/workflows/ci.yml", "docker/Dockerfile.dev"] {
        assert!(
            names.iter().any(|n| n == expected),
            "the scan read {expected} (control): {names:?}"
        );
    }
    let mut found: Vec<String> = Vec::new();
    for (path, name) in files.iter().zip(&names) {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{name} is readable as text: {e}"));
        for (line, command) in unpinned_installs(&text) {
            found.push(format!("{name}:{line}: {command}"));
        }
    }
    assert!(
        found.is_empty(),
        "these steps build a tool from source at whatever version is newest that day:\n  {}\n\
         Add `--version <x.y.z>` (the newest release whose minimum rustc the pinned toolchain \
         satisfies), or install the tool as a prebuilt binary. Without a version, the tool's \
         next release can fail this step on every pull request.",
        found.join("\n  ")
    );
}

/// The scan sees the two lines the repository carried before the fix (negative control), in the
/// form each file writes them: a workflow `run:` and a Dockerfile `RUN`.
#[test]
fn an_install_with_no_version_is_seen_in_a_workflow_and_in_a_dockerfile() {
    let workflow = "      - name: Install cargo-outdated\n        run: cargo install cargo-outdated --locked\n";
    assert_eq!(
        unpinned_installs(workflow),
        vec![(2, "cargo install cargo-outdated --locked".to_string())]
    );
    let dockerfile = "# the runner\nRUN cargo install --locked cargo-nextest\n";
    assert_eq!(
        unpinned_installs(dockerfile),
        vec![(2, "cargo install --locked cargo-nextest".to_string())]
    );
}

/// A version on a continuation line belongs to the command; one on a neighbouring command of the
/// same line does not.
#[test]
fn the_version_is_read_from_the_command_it_belongs_to() {
    let continued = "RUN cargo install --locked \\\n    # the runner\n    cargo-nextest \\\n    --version 0.9.152\n";
    assert!(unpinned_installs(continued).is_empty());
    let two = "RUN cargo install a --version 1.0.0 && cargo install b --locked\n";
    assert_eq!(
        unpinned_installs(two),
        vec![(1, "cargo install b --locked".to_string())]
    );
    let block = "        run: |\n          cargo install a --locked\n          cargo install b --version=2.0.0\n";
    assert_eq!(
        unpinned_installs(block),
        vec![(2, "cargo install a --locked".to_string())]
    );
}

/// What the scan leaves alone: a comment that names the command, and an install whose source the
/// tree or a revision fixes.
#[test]
fn a_comment_and_a_fixed_source_are_not_findings() {
    let text = "      # `rustsec/audit-check@v2` runs `cargo install cargo-audit`\n        run: cargo install --path crates/mununu-cli\n        run: cargo install --git https://example.invalid/t --rev 0123abc t\n";
    assert!(unpinned_installs(text).is_empty());
}
