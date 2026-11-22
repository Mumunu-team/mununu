//! mununu#1148 — `YosysOptions::report_clock_roots` lets the lift accept a clock the design holds
//! no driver for (a black box, a cut net, an undriven net) under `--merge-clock-domains`. It is a
//! promise that the caller puts the root in its report. A setter that does not is a lockstep
//! verdict with nothing said about the clock, so the setters are listed here, each with the calls
//! that carry the root into its report; a new one fails this test until it is added with them.
//!
//! The reporter is looked for as a CALL (`.extend(clock_root_notes(`), never as a name: the first
//! form of this test looked for `cdc_lint` in the file that defines `sv_cdc_lint` and for
//! `clock_root_notes` in the file that defines it, so it passed whatever the code did.

use std::path::Path;

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};
use crate::source_scan::without_test_modules;

/// What a setter writes to take the promise.
const SETS_THE_FLAG: &str = "report_clock_roots: true";

/// `(the file that sets the flag, the calls that carry the roots into that verb's report)`. Each
/// call is `(file, the text of the call)`; a chain names every link, so a setter that reaches the
/// note through another function fails when either link goes.
const SETTERS: &[(&str, &[(&str, &str)])] = &[
    (
        "src/adapter/slang/verify_auto.rs",
        &[(
            "src/adapter/slang/verify_auto.rs",
            ".extend(clock_root_notes(",
        )],
    ),
    (
        "src/adapter/sv_verify.rs",
        &[
            ("src/adapter/sv_verify.rs", "cdc_lint::cdc_lint("),
            ("src/adapter/cdc_lint.rs", ".extend(clock_root_notes("),
        ],
    ),
];

/// `path` relative to the crate, with `/` between components on every host (built from the
/// components, never from the display string: mununu#1153).
fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .expect("under the crate")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The product code of `src`: no test module, no line that is only a comment (a comment that
/// names the call is not the call).
fn product_code(src: &str) -> String {
    without_test_modules(src)
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The files of `sources` (`(relative path, text)`) whose product code sets the flag, sorted.
fn setters(sources: &[(String, String)]) -> Vec<String> {
    let mut found: Vec<String> = sources
        .iter()
        .filter(|(_, src)| product_code(src).contains(SETS_THE_FLAG))
        .map(|(file, _)| file.clone())
        .collect();
    found.sort();
    found
}

/// Whether the product code of `src` makes the call `call`.
fn makes_the_call(src: &str, call: &str) -> bool {
    product_code(src).contains(call)
}

#[test]
fn report_clock_roots_is_set_only_where_the_roots_are_reported() {
    let root = repo_root().join("crates/mununu-core");
    let sources: Vec<(String, String)> = rust_files_under(&root.join("src"))
        .into_iter()
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &root), src)
        })
        .collect();
    let mut want: Vec<String> = SETTERS.iter().map(|(f, _)| f.to_string()).collect();
    want.sort();
    assert_eq!(
        setters(&sources),
        want,
        "a lift that sets `{SETS_THE_FLAG}` must report every free-source clock root; list it in \
         SETTERS with the calls that do"
    );
    for (setter, calls) in SETTERS {
        for (file, call) in *calls {
            let src = std::fs::read_to_string(root.join(file)).expect("a reporter file");
            assert!(
                makes_the_call(&src, call),
                "{setter} sets `{SETS_THE_FLAG}`, and {file} no longer makes the call `{call}` \
                 that carries the clock roots into its report"
            );
        }
    }
}

/// The scan fails on what it exists to catch (negative control): a setter the list does not
/// name, and a file that names the reporter — in its own function's name, in the reporter's
/// definition, in a comment — without calling it. The second is the form the first version of
/// this test accepted.
#[test]
fn an_unlisted_setter_and_a_reporter_named_but_not_called_are_seen() {
    let product = "fn lift() { let y = YosysOptions { report_clock_roots: true, ..y }; }\n";
    let test_only = "fn lift() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let y = YosysOptions { \
                     report_clock_roots: true, ..y }; }\n}\n";
    let sources = vec![
        ("src/adapter/new_verb.rs".to_string(), product.to_string()),
        ("src/adapter/tested.rs".to_string(), test_only.to_string()),
    ];
    assert_eq!(
        setters(&sources),
        vec!["src/adapter/new_verb.rs".to_string()],
        "a product setter is found and a test-module one is not"
    );

    let named_not_called = "pub fn sv_cdc_lint() {}\n\
                            // the notes come from report.notes.extend(clock_root_notes(&d));\n\
                            pub(crate) fn clock_root_notes(d: &[D]) -> Vec<String> { vec![] }\n";
    assert!(named_not_called.contains("cdc_lint"));
    assert!(named_not_called.contains("clock_root_notes"));
    assert!(!makes_the_call(named_not_called, "cdc_lint::cdc_lint("));
    assert!(!makes_the_call(
        named_not_called,
        ".extend(clock_root_notes("
    ));
    let called = "fn f() { report.notes.extend(clock_root_notes(&report.domains)); }\n";
    assert!(makes_the_call(called, ".extend(clock_root_notes("));
}
