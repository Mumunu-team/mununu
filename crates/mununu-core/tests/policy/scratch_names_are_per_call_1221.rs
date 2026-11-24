//! mununu#1221 — a file or directory one call makes for itself in a directory other calls use
//! too is named by the one helper.
//!
//! The helper (`adapter/mod.rs`: `per_call_name`) builds the name from a counter of the process,
//! the process id and the clock, so two calls at once never get the same one. The isolated
//! `exact-symbolic` child was handed a model at a path made from the process id alone: one file
//! for every evaluation of the process, and with two evaluations at once a child could read the
//! other one's model. The replay directory of an HTTP request was named the same way.
//!
//! Three gates, all derived from the source. [`NAMERS`] is the table of the class: every file
//! whose product code names such a file or directory, and how many — a new site fails until it
//! has a row. No product code outside the helper's file reads the process id, which is what
//! every hand-made name started from. And a file whose product code reads one of the shared
//! directories is either a row of [`NAMERS`] or listed in [`NAMES_NOTHING_THERE`] with what it
//! does there instead.

use crate::child_waits_go_through_one_helper_1196::shipped_sources;

/// The file that holds the helper, and the only one that may read the process id.
const HELPER: &str = "mununu-core/src/adapter/mod.rs";

/// The helper.
const NAMES_FOR_ONE_CALL: &str = "per_call_name(";

/// What every name made by hand started from.
const PROCESS_ID: &str = "process::id()";

/// The directories more than one call uses: the system temporary directory, the staging root
/// (`MUNUNU_LIFT_STAGING_ROOT`, else the temporary directory) and the Verilator scratch root on
/// top of it.
const SHARED_DIRECTORIES: &[&str] = &["temp_dir()", "staging_root()", "scratch_root()"];

/// `(file, the names its product code asks the helper for, what is named)`.
const NAMERS: &[(&str, usize, &str)] = &[
    (
        "mununu-core/src/adapter/engine_isolate.rs",
        1,
        "the model file handed to the isolated exact-symbolic child",
    ),
    (
        "mununu-core/src/adapter/pono/mod.rs",
        1,
        "the BTOR2 file Pono reads",
    ),
    (
        "mununu-core/src/adapter/ric3/mod.rs",
        1,
        "the BTOR2 file rIC3 reads",
    ),
    (
        "mununu-core/src/adapter/slang/extract.rs",
        1,
        "the directory the sources of one SVA extraction are staged in",
    ),
    (
        "mununu-core/src/adapter/verilator/mod.rs",
        1,
        "the work directory of one Verilator build",
    ),
    (
        "mununu-core/src/adapter/yosys/lift_cache.rs",
        1,
        "the staging directory of one cache store, renamed into place when it is complete",
    ),
    (
        "mununu-core/src/adapter/yosys/mod.rs",
        1,
        "the staging directory of one lift",
    ),
    (
        "mununu-core/src/api/handlers.rs",
        1,
        "the replay directory of one property of one request",
    ),
];

/// Files whose product code reads a shared directory and names nothing in it for one call:
/// `(file, what it does there)`. The helper's own file defines the staging root.
const NAMES_NOTHING_THERE: &[(&str, &str)] = &[];

/// How many times `code` calls the helper: its name, not as the definition.
fn names_asked_for(code: &str) -> usize {
    code.match_indices(NAMES_FOR_ONE_CALL)
        .filter(|(at, _)| !code[..*at].ends_with("fn "))
        .count()
}

/// The files of `sources` that ask the helper for a name, with how many, sorted.
fn namers(sources: &[(String, String)]) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = sources
        .iter()
        .map(|(file, code)| (file.clone(), names_asked_for(code)))
        .filter(|(_, n)| *n > 0)
        .collect();
    found.sort();
    found
}

/// The files of `sources`, other than the helper's, whose code contains one of `needles`,
/// sorted.
fn files_with(sources: &[(String, String)], needles: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = sources
        .iter()
        .filter(|(file, code)| file != HELPER && needles.iter().any(|n| code.contains(n)))
        .map(|(file, _)| file.clone())
        .collect();
    found.sort();
    found
}

#[test]
fn every_name_made_for_one_call_is_listed_and_comes_from_the_helper() {
    let sources = shipped_sources();
    assert!(
        sources.len() > 100,
        "{} source files — the walk broke",
        sources.len()
    );
    let helper = &sources
        .iter()
        .find(|(file, _)| file == HELPER)
        .expect("the helper's file")
        .1;
    assert!(
        helper.contains(&format!("fn {NAMES_FOR_ONE_CALL}")),
        "`{HELPER}` defines the helper"
    );
    let want: Vec<(String, usize)> = NAMERS
        .iter()
        .map(|(file, n, _)| (file.to_string(), *n))
        .collect();
    assert_eq!(
        namers(&sources),
        want,
        "the files that name a file or directory for one call changed: give the new site a row \
         in NAMERS (or correct the count) saying what is named"
    );
}

#[test]
fn no_product_code_builds_a_name_from_the_process_id_by_itself() {
    assert_eq!(
        files_with(&shipped_sources(), &[PROCESS_ID]),
        Vec::<String>::new(),
        "product code outside `{HELPER}` reads the process id: a file or directory named from \
         it is one for every call of the process. Name it with \
         `crate::adapter::per_call_name(<tag>)`"
    );
}

#[test]
fn a_file_that_reads_a_shared_directory_names_what_it_puts_there_with_the_helper() {
    let mut want: Vec<String> = NAMERS
        .iter()
        .map(|(file, ..)| *file)
        .chain(NAMES_NOTHING_THERE.iter().map(|(file, _)| *file))
        // The cache names its staging directory inside its own directory, which the caller
        // hands it.
        .filter(|file| *file != "mununu-core/src/adapter/yosys/lift_cache.rs")
        .map(str::to_string)
        .collect();
    want.sort();
    assert_eq!(
        files_with(&shipped_sources(), SHARED_DIRECTORIES),
        want,
        "the files whose product code reads the system temporary directory, the staging root or \
         the Verilator scratch root changed: a file or directory a call makes for itself there \
         is named with `crate::adapter::per_call_name(<tag>)` and gets a row in NAMERS; a file \
         that names nothing there goes in NAMES_NOTHING_THERE with what it does"
    );
}

/// The scans see what they exist to catch (negative control): a hand-made name in product code
/// is found by its process id and by the directory it reads; the same in a test module or a
/// comment is not; a call of the helper is counted and its definition is not.
#[test]
fn a_hand_made_name_in_product_code_is_seen_and_one_in_a_test_or_a_comment_is_not() {
    use crate::merged_clock_domains_reported_1181::product_code;
    let sources = vec![
        (
            "a/by_hand.rs".to_string(),
            product_code(
                "fn model() -> std::path::PathBuf {\n    \
                 std::env::temp_dir().join(format!(\"m-{}\", std::process::id()))\n}\n\
                 // let old = std::env::temp_dir().join(std::process::id().to_string());\n\
                 #[cfg(test)]\nmod tests {\n    fn t() {\n        \
                 let _ = std::env::temp_dir().join(std::process::id().to_string());\n    }\n}\n",
            ),
        ),
        (
            "a/helped.rs".to_string(),
            product_code(
                "fn model() -> std::path::PathBuf {\n    \
                 staging_root().join(crate::adapter::per_call_name(\"m\"))\n}\n",
            ),
        ),
        (
            "a/tested.rs".to_string(),
            product_code(
                "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        \
                 let _ = std::env::temp_dir().join(per_call_name(\"t\"));\n    }\n}\n",
            ),
        ),
        (
            HELPER.to_string(),
            product_code(
                "pub(crate) fn per_call_name(tag: &str) -> String {\n    \
                 format!(\"{tag}-{}\", std::process::id())\n}\n",
            ),
        ),
    ];
    assert_eq!(namers(&sources), vec![("a/helped.rs".to_string(), 1)]);
    assert_eq!(files_with(&sources, &[PROCESS_ID]), vec!["a/by_hand.rs"]);
    assert_eq!(
        files_with(&sources, SHARED_DIRECTORIES),
        vec!["a/by_hand.rs", "a/helped.rs"]
    );
}
