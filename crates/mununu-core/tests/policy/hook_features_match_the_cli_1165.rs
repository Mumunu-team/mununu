//! mununu#1165 — the pre-commit hook builds mununu-core with the features the workspace unifies.
//!
//! `scripts/pre-commit` runs `cargo test -p mununu-core --features <list>` so a commit runs the
//! same test set as CI's workspace build, where mununu-cli's dependency edge switches the
//! features on. The two lists were kept by hand: `mcp` joined the CLI's and not the hook's, a
//! test then named `api::mcp`, and from that commit on the hook's build failed with E0433 on
//! every commit touching the core while CI stayed green. This test holds the hook's list to the
//! CLI's, so the next feature added to one and not the other fails here, in CI, instead of on
//! the next contributor's first commit.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root")
}

/// The features mununu-cli asks of mununu-core: the `features = [ … ]` array on the
/// `mununu-core = { … }` line of its manifest.
fn cli_features(manifest: &str) -> BTreeSet<String> {
    let line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("mununu-core") && l.contains("features"))
        .expect("mununu-cli depends on mununu-core with a features list");
    let open = line.find("features").expect("checked above");
    let rest = &line[open..];
    let start = rest.find('[').expect("a features array");
    let end = rest.find(']').expect("a closed features array");
    rest[start + 1..end]
        .split(',')
        .map(|f| f.trim().trim_matches('"').to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

/// The features the hook passes for mununu-core: the list after `--features` on its
/// `features="--features …"` assignment.
fn hook_features(script: &str) -> BTreeSet<String> {
    let line = script
        .lines()
        .find(|l| l.trim_start().starts_with("features=\"--features "))
        .expect("the hook assigns features=\"--features …\" for mununu-core");
    let list = line
        .trim()
        .trim_start_matches("features=\"--features ")
        .trim_end_matches('"');
    list.split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect()
}

#[test]
fn the_pre_commit_hook_asks_for_the_features_the_cli_unifies() {
    let root = repo_root();
    let manifest = std::fs::read_to_string(root.join("crates/mununu-cli/Cargo.toml"))
        .expect("the CLI manifest");
    let script =
        std::fs::read_to_string(root.join("scripts/pre-commit")).expect("the pre-commit hook");
    let cli = cli_features(&manifest);
    let hook = hook_features(&script);
    assert!(
        cli.contains("api"),
        "the scan read the CLI's list (control): {cli:?}"
    );
    assert_eq!(
        hook, cli,
        "scripts/pre-commit builds mununu-core with {hook:?}, and the workspace build (through \
         crates/mununu-cli/Cargo.toml) with {cli:?}. A test that compiles in one and not the \
         other breaks every commit or hides from the hook: make the two lists the same."
    );
}

/// The scan fails on a drift (negative control): the list the hook carried before the fix.
#[test]
fn a_hook_list_without_a_cli_feature_is_seen() {
    let cli = cli_features("mununu-core = { path = \"x\", features = [\"api\", \"mcp\"] }\n");
    let hook = hook_features("        features=\"--features api\"\n");
    assert_ne!(hook, cli);
    assert_eq!(cli.len(), 2);
    assert_eq!(hook.len(), 1);
}
