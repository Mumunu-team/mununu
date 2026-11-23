//! mununu#1196 — every child process a run starts is waited on by the one helper.
//!
//! The helper (`adapter/mod.rs`: `run_with_timeout`, its cancellable form, `run_to_end`) puts
//! the child in its own group, drains its pipes while it runs, and kills it when the run's
//! budget is cancelled — which is what ends the work of an HTTP request nobody is waiting for
//! any more. A wait written anywhere else (`Command::output`, a `spawn` and a loop) has none of
//! that. Nine sites had been written that way; the isolated `exact-symbolic` child was the one
//! that could run for hours.
//!
//! Two gates, both derived from the source. [`SPAWNERS`] is the table of the class: every file
//! whose product code starts a child, how many, and which helper calls wait on them — a new
//! spawn, or a file that changes how it waits, fails until its row says so. And no product code
//! outside the helper's file calls a waiting method of the standard library at all.

use crate::merged_clock_domains_reported_1181::{product_code, rel};
use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};

/// The file that holds the helper, and the only one that may wait on a child itself.
const HELPER: &str = "mununu-core/src/adapter/mod.rs";

/// What starts a child.
const STARTS_A_CHILD: &str = "Command::new(";

/// The helper's entry points, in the order a row lists them.
const HELPER_WAITS: &[&str] = &[
    "run_with_timeout(",
    "run_with_timeout_cancellable(",
    "run_to_end(",
];

/// The standard library's ways to wait on a child, and the helper's own spawn (its trait is
/// private to the helper's module, which its submodules can still see).
const OWN_WAITS: &[&str] = &[
    ".output()",
    ".spawn()",
    ".spawn_member(",
    ".wait_with_output()",
    ".try_wait()",
];

/// Two more that other types also have (an HTTP response has a status, a barrier a wait): they
/// count only in a file that names the process type.
const OWN_WAITS_ON_A_COMMAND: &[&str] = &[".status()", ".wait()"];

/// `(file, children its product code starts, the helper calls that wait on them, the stops
/// those waits have)`. Every wait through the helper has the third stop, the kill on a
/// cancelled run budget; the last column says what else it has.
const SPAWNERS: &[(&str, usize, &[&str], &str)] = &[
    (
        "mununu-core/src/adapter/btor2/native_interp.rs",
        2,
        &["run_with_timeout_cancellable("],
        "cvc5 and MathSAT queries: the query's limit as the wall, and the caller's flag",
    ),
    (
        "mununu-core/src/adapter/btormc/mod.rs",
        1,
        &["run_with_timeout_cancellable("],
        "btormc: the member's budget as the wall, and the caller's flag",
    ),
    (
        "mununu-core/src/adapter/engine_isolate.rs",
        1,
        &["run_with_timeout(", "run_to_end("],
        "the isolated exact-symbolic child: the time the property has left as the wall, and \
         none when the run has no budget",
    ),
    (
        "mununu-core/src/adapter/extraction/validate.rs",
        1,
        &["run_to_end("],
        "git rev-parse: no wall (mununu#1223)",
    ),
    (
        "mununu-core/src/adapter/pono/mod.rs",
        1,
        &["run_with_timeout_cancellable("],
        "Pono: the member's budget as the wall, and the caller's flag",
    ),
    (
        "mununu-core/src/adapter/ric3/mod.rs",
        1,
        &["run_with_timeout_cancellable("],
        "rIC3: the member's budget as the wall, and the caller's flag",
    ),
    (
        "mununu-core/src/adapter/slang/mod.rs",
        4,
        &["run_with_timeout(", "run_to_end("],
        "slang: the lift timeout as the wall, and none when the user turned it off",
    ),
    (
        "mununu-core/src/adapter/verilator/mod.rs",
        3,
        &["run_to_end("],
        "the Verilator build and the reset-simulation binary: no wall (mununu#1223)",
    ),
    (
        "mununu-core/src/adapter/verilator/replay.rs",
        1,
        &["run_to_end("],
        "make for the replay testbench: no wall (mununu#1223)",
    ),
    (
        "mununu-core/src/adapter/yosys/mod.rs",
        4,
        &["run_with_timeout(", "run_to_end("],
        "yosys and sv2v: the lift timeout as the wall, and none when the user turned it off; \
         one of the four is the test-only probe of TEST_ONLY",
    ),
    (
        "mununu-core/src/codesign/c_extract_llvm.rs",
        1,
        &["run_to_end("],
        "clang: no wall (mununu#1223)",
    ),
];

/// The children the helper's own file starts: the tool discovery probe, under the probe's wall.
const HELPER_STARTS: usize = 1;

/// Waits the scan sees that are not product code: `(file, call, how many, why)`. The scan drops
/// test MODULES; a `#[cfg(test)]` function outside one is still read.
const TEST_ONLY: &[(&str, &str, usize, &str)] = &[(
    "mununu-core/src/adapter/yosys/mod.rs",
    ".output()",
    1,
    "`yosys_can_read_slang`, a `#[cfg(test)]` function: the probe the tool-gated tests skip on",
)];

/// The product code of every source file of the workspace's crates, as `(path under crates/,
/// code)`.
fn shipped_sources() -> Vec<(String, String)> {
    let crates = repo_root().join("crates");
    ["mununu-core", "mununu-cli", "mununu-extract"]
        .iter()
        .flat_map(|krate| rust_files_under(&crates.join(krate).join("src")))
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &crates), product_code(&src))
        })
        .collect()
}

/// The files of `sources` that start a child, with how many, sorted.
fn spawners(sources: &[(String, String)]) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = sources
        .iter()
        .map(|(file, code)| (file.clone(), code.matches(STARTS_A_CHILD).count()))
        .filter(|(_, n)| *n > 0)
        .collect();
    found.sort();
    found
}

/// The waits `sources` make by themselves, outside the helper's file: `(file, call, how many)`,
/// sorted.
fn own_waits(sources: &[(String, String)]) -> Vec<(String, String, usize)> {
    let mut found = Vec::new();
    for (file, code) in sources {
        if file == HELPER {
            continue;
        }
        let on_a_command: &[&str] = if code.contains("Command") {
            OWN_WAITS_ON_A_COMMAND
        } else {
            &[]
        };
        for call in OWN_WAITS.iter().chain(on_a_command) {
            let n = code.matches(call).count();
            if n > 0 {
                found.push((file.clone(), call.to_string(), n));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_child_a_run_starts_is_listed_with_the_helper_that_waits_on_it() {
    let sources = shipped_sources();
    assert!(
        sources.len() > 100,
        "{} source files — the walk broke",
        sources.len()
    );
    let mut want: Vec<(String, usize)> = SPAWNERS
        .iter()
        .map(|(file, n, ..)| (file.to_string(), *n))
        .chain([(HELPER.to_string(), HELPER_STARTS)])
        .collect();
    want.sort();
    assert_eq!(
        spawners(&sources),
        want,
        "the files that start a child process changed: give the new spawn a row in SPAWNERS \
         (or correct the count) with the helper call that waits on it"
    );
    for (file, _, waits, stops) in SPAWNERS {
        let code = &sources
            .iter()
            .find(|(f, _)| f == file)
            .expect("a listed file")
            .1;
        let used: Vec<&str> = HELPER_WAITS
            .iter()
            .copied()
            .filter(|call| code.contains(call))
            .collect();
        assert!(
            !waits.is_empty(),
            "{file}: a row names the helper call that waits"
        );
        assert_eq!(
            &used, waits,
            "{file} is listed as waiting with {waits:?} ({stops}) and its code calls {used:?}"
        );
    }
}

#[test]
fn no_product_code_waits_on_a_child_by_itself() {
    let want: Vec<(String, String, usize)> = TEST_ONLY
        .iter()
        .map(|(file, call, n, _)| (file.to_string(), call.to_string(), *n))
        .collect();
    assert_eq!(
        own_waits(&shipped_sources()),
        want,
        "a child is waited on outside `{HELPER}`: run it through `crate::adapter::run_to_end` \
         (no wall) or `run_with_timeout` (a wall), so that a cancelled run ends it. A wait in a \
         `#[cfg(test)]` function goes in TEST_ONLY; an entry there that no longer matches is \
         removed"
    );
}

/// The scans see what they exist to catch (negative control): a spawn and a bare wait in product
/// code are counted; the same in a test module or a comment are not; a status that is not a
/// child's is not; and the helper's own file is not asked how it waits.
#[test]
fn a_bare_wait_in_product_code_is_seen_and_one_in_a_test_or_a_comment_is_not() {
    let sources = vec![
        (
            "a/bare.rs".to_string(),
            product_code(
                "fn run() {\n    let out = std::process::Command::new(\"tool\").output();\n}\n\
                 // let old = Command::new(\"tool\").output();\n\
                 #[cfg(test)]\nmod tests {\n    fn t() {\n        \
                 std::process::Command::new(\"true\").status().unwrap();\n    }\n}\n",
            ),
        ),
        (
            "a/status.rs".to_string(),
            product_code("fn run(c: &mut std::process::Command) {\n    let _ = c.status();\n}\n"),
        ),
        (
            "a/http.rs".to_string(),
            product_code(
                "fn code(response: &Response) -> u16 {\n    response.status().as_u16()\n}\n",
            ),
        ),
        (
            HELPER.to_string(),
            product_code("fn helper(c: &mut std::process::Command) {\n    let _ = c.spawn();\n}\n"),
        ),
    ];
    assert_eq!(spawners(&sources), vec![("a/bare.rs".to_string(), 1)]);
    assert_eq!(
        own_waits(&sources),
        vec![
            ("a/bare.rs".to_string(), ".output()".to_string(), 1),
            ("a/status.rs".to_string(), ".status()".to_string(), 1),
        ]
    );
}
