//! mununu#1148 — `YosysOptions::report_clock_roots` lets the lift accept a clock the design holds
//! no driver for (a black box, a cut net, an undriven net) under `--merge-clock-domains`. It is a
//! promise that the caller puts the root in its report. A setter that does not is a lockstep
//! verdict with nothing said about the clock, so the setters are listed here, each with the code
//! that reports the root; a new one fails this test until it is added with its reporter.

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};
use crate::source_scan::without_test_modules;

/// `(file suffix, the function that reports the roots for that setter)`.
const SETTERS: &[(&str, &str)] = &[
    ("src/adapter/slang/verify_auto.rs", "clock_root_notes"),
    ("src/adapter/sv_verify.rs", "cdc_lint"),
];

#[test]
fn report_clock_roots_is_set_only_where_the_roots_are_reported() {
    let root = repo_root().join("crates/mununu-core");
    let mut found = Vec::new();
    for path in rust_files_under(&root.join("src")) {
        let src = std::fs::read_to_string(&path).expect("readable source");
        if without_test_modules(&src).contains("report_clock_roots: true") {
            found.push(
                path.strip_prefix(&root)
                    .expect("under the crate")
                    .display()
                    .to_string(),
            );
        }
    }
    found.sort();
    let mut want: Vec<String> = SETTERS.iter().map(|(f, _)| f.to_string()).collect();
    want.sort();
    assert_eq!(
        found, want,
        "a lift that sets `report_clock_roots: true` must report every free-source clock root; \
         list it in SETTERS with the function that does"
    );
    for (file, reporter) in SETTERS {
        let src = std::fs::read_to_string(root.join(file)).expect("a setter file");
        assert!(
            without_test_modules(&src).contains(reporter),
            "{file} sets report_clock_roots but does not call {reporter}"
        );
    }
}
