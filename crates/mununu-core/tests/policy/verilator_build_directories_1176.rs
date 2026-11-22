//! mununu#1176 — Verilator's `make` build refuses a directory whose path has whitespace, and its
//! message is about a `make` variable. So every place that starts that build asks
//! `build_dir_refusal` about the directory first, and every place that CHOOSES the directory
//! takes it from the one root that is checked (`scratch_root`) or checks the one the user gave.
//! Both lists are derived from the source: a new `make` spawn, a new caller of the replay runner
//! or of the work-directory type, or a Verilator module that goes back to the system temporary
//! directory on its own, fails here until it is listed with its check.

use crate::clock_roots_reported_1148::{product_code, rel};
use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};

/// What starts the build.
const SPAWNS_MAKE: &str = "Command::new(\"make\")";

/// `(file, the check it makes on the directory it hands to make)` — one spawn per file.
const MAKERS: &[(&str, &str)] = &[
    (
        "mununu-core/src/adapter/verilator/mod.rs",
        "build_dir_refusal(workdir)",
    ),
    (
        "mununu-core/src/adapter/verilator/replay.rs",
        "build_dir_refusal(dir)",
    ),
];

/// `(what is called, file, how many calls, how that file's directory is chosen or checked)`.
/// The definitions' own files are not callers.
const CHOOSERS: &[(&str, &str, usize, &str)] = &[
    // The HTTP replay: a scratch directory under the checked root.
    (
        "run_replay(",
        "mununu-core/src/api/handlers.rs",
        1,
        "crate::adapter::verilator::scratch_root()",
    ),
    // The CLI replay: the user's `--emit-testbench` directory, checked, with the flag as remedy.
    (
        "run_replay(",
        "mununu-cli/src/main.rs",
        1,
        "mununu_core::adapter::verilator::build_dir_refusal(dir)",
    ),
    // The reset simulation: the work-directory type, which is made under the checked root.
    (
        "VerilatorTempDir::new()",
        "mununu-core/src/adapter/btor2/bit_blast.rs",
        1,
        "crate::adapter::verilator::VerilatorTempDir::new()",
    ),
];

/// Where each chooser is defined (its own file is skipped when counting callers).
const DEFINED_IN: &[(&str, &str)] = &[
    ("run_replay(", "mununu-core/src/adapter/verilator/replay.rs"),
    (
        "VerilatorTempDir::new()",
        "mununu-core/src/adapter/verilator/mod.rs",
    ),
];

/// The product code of every source file of the two crates that ship, as `(path under
/// crates/, code)`.
fn shipped_sources() -> Vec<(String, String)> {
    let crates = repo_root().join("crates");
    ["mununu-core/src", "mununu-cli/src"]
        .iter()
        .flat_map(|dir| rust_files_under(&crates.join(dir)))
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &crates), product_code(&src))
        })
        .collect()
}

/// The files of `sources` with a `needle` in their product code, with the count, sorted.
fn sites(sources: &[(String, String)], needle: &str) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = sources
        .iter()
        .map(|(file, code)| (file.clone(), code.matches(needle).count()))
        .filter(|(_, n)| *n > 0)
        .collect();
    found.sort();
    found
}

#[test]
fn every_make_build_asks_about_its_directory_first() {
    let sources = shipped_sources();
    let want: Vec<(String, usize)> = MAKERS.iter().map(|(f, _)| (f.to_string(), 1)).collect();
    assert_eq!(
        sites(&sources, SPAWNS_MAKE),
        want,
        "a new `make` spawn: list it in MAKERS with the `build_dir_refusal` call on its directory"
    );
    for (file, check) in MAKERS {
        let code = &sources
            .iter()
            .find(|(f, _)| f == file)
            .expect("a listed file")
            .1;
        assert_eq!(
            code.matches(check).count(),
            1,
            "{file} starts `make` and no longer makes the check `{check}` exactly once"
        );
        let (ask, spawn) = (
            code.find(check).expect("counted above"),
            code.find(SPAWNS_MAKE).expect("a listed spawn"),
        );
        assert!(ask < spawn, "{file}: the check comes before the spawn");
    }
}

#[test]
fn every_chooser_of_a_build_directory_takes_a_checked_one() {
    let sources = shipped_sources();
    for (call, defined_in) in DEFINED_IN {
        let callers: Vec<(String, usize)> = sites(&sources, call)
            .into_iter()
            .filter(|(f, _)| f != defined_in)
            .collect();
        let mut want: Vec<(String, usize)> = CHOOSERS
            .iter()
            .filter(|(c, ..)| c == call)
            .map(|(_, f, n, _)| (f.to_string(), *n))
            .collect();
        want.sort();
        assert_eq!(
            callers, want,
            "a new caller of `{call}`: list it in CHOOSERS with the call that chooses or checks \
             its directory"
        );
    }
    for (call, file, _, how) in CHOOSERS {
        let code = &sources
            .iter()
            .find(|(f, _)| f == file)
            .expect("a listed file")
            .1;
        assert!(
            code.contains(how),
            "{file} calls `{call}` and no longer has `{how}`, which chose or checked its directory"
        );
    }
    // The Verilator modules take their root from `scratch_root`, never from the system
    // temporary directory on their own (that is how the staging-root variable missed them).
    let own_temp: Vec<(String, usize)> = sites(&sources, "temp_dir()")
        .into_iter()
        .filter(|(f, _)| f.starts_with("mununu-core/src/adapter/verilator/"))
        .collect();
    assert_eq!(own_temp, Vec::new(), "use `scratch_root()`");
}

/// The scan sees what it exists to catch (negative control): a spawn in product code is counted
/// and one in a test module or a comment is not, and a file that only names the check in a
/// comment does not make it.
#[test]
fn a_make_spawn_in_product_code_is_seen_and_one_in_a_test_or_a_comment_is_not() {
    let product =
        "fn build(d: &Path) {\n    let mut c = std::process::Command::new(\"make\");\n}\n";
    let test_only = "fn build() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let c = \
                     Command::new(\"make\"); }\n}\n";
    let comment_only = "// it used to call Command::new(\"make\") after build_dir_refusal(dir)\n\
                        fn build() {}\n";
    let sources: Vec<(String, String)> = [
        ("a/new_build.rs", product),
        ("a/tested.rs", test_only),
        ("a/commented.rs", comment_only),
    ]
    .iter()
    .map(|(f, src)| (f.to_string(), product_code(src)))
    .collect();
    assert_eq!(
        sites(&sources, SPAWNS_MAKE),
        vec![("a/new_build.rs".to_string(), 1)]
    );
    assert_eq!(sites(&sources, "build_dir_refusal(dir)"), Vec::new());
}
