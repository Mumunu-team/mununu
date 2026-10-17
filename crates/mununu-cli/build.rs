//! mununu#715 — record the commit this binary is built from, and whether the tree was clean.
//!
//! Order: an explicit `MUNUNU_BUILD_SHA` (+ `MUNUNU_BUILD_DIRTY`, `0`/`1`) wins — the Docker
//! production image builds without `.git` (it is in `.dockerignore`) and passes them as build
//! args. Otherwise git is asked. When neither answers, nothing is recorded and the binary reports
//! no build identity: absent, never guessed.
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = manifest.join("../..");
    let version = std::env::var("CARGO_PKG_VERSION").expect("set by cargo");

    println!("cargo:rerun-if-env-changed=MUNUNU_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=MUNUNU_BUILD_DIRTY");

    let (sha, dirty) = match std::env::var("MUNUNU_BUILD_SHA") {
        Ok(sha) if !sha.trim().is_empty() => (
            Some(sha.trim().to_string()),
            std::env::var("MUNUNU_BUILD_DIRTY").ok(),
        ),
        _ => {
            let sha = git(&root, &["rev-parse", "HEAD"]);
            let dirty = sha.as_ref().and_then(|_| {
                git(&root, &["status", "--porcelain", "--untracked-files=no"])
                    .map(|s| if s.is_empty() { "0" } else { "1" }.to_string())
            });
            // Rebuild when HEAD moves (a commit, a checkout) or a tracked file changes (the dirty
            // bit). The git dirs are resolved through git so a worktree's `.git` file works.
            if sha.is_some() {
                for (flag, file) in [
                    ("--git-dir", "HEAD"),
                    ("--git-dir", "index"),
                    ("--git-common-dir", "packed-refs"),
                ] {
                    if let Some(dir) = git(&root, &["rev-parse", flag]) {
                        println!(
                            "cargo:rerun-if-changed={}",
                            root.join(dir).join(file).display()
                        );
                    }
                }
                if let (Some(dir), Some(head_ref)) = (
                    git(&root, &["rev-parse", "--git-common-dir"]),
                    git(&root, &["symbolic-ref", "-q", "HEAD"]),
                ) {
                    println!(
                        "cargo:rerun-if-changed={}",
                        root.join(dir).join(head_ref).display()
                    );
                }
                for tracked in ["crates", "src", "Cargo.toml", "Cargo.lock"] {
                    println!("cargo:rerun-if-changed={}", root.join(tracked).display());
                }
            }
            (sha, dirty)
        }
    };

    let short = sha.as_deref().map(|s| &s[..s.len().min(7)]);
    let version_line = match (short, dirty.as_deref()) {
        (Some(s), Some("1")) => format!("{version} ({s}-dirty)"),
        (Some(s), Some("0")) => format!("{version} ({s})"),
        _ => version,
    };
    println!("cargo:rustc-env=MUNUNU_VERSION_LINE={version_line}");
    if let (Some(sha), Some(dirty)) = (sha, dirty) {
        println!("cargo:rustc-env=MUNUNU_BUILD_SHA={sha}");
        println!("cargo:rustc-env=MUNUNU_BUILD_DIRTY={dirty}");
    }
}
