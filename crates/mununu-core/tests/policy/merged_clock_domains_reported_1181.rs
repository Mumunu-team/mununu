//! mununu#1181 — a lift that takes `--merge-clock-domains` steps every clock domain together
//! and ticks a derived or free-source clock on every step. A verdict on that model does not
//! transfer on its own, so the report says what was merged. Seven of the nine `sv` verbs dropped
//! the domains: only `sv verify-auto` and `sv cdc-lint` read them.
//!
//! The rule is structural, in two halves this test reads from the source:
//! 1. **In the lift** (`adapter/yosys/mod.rs`): a function that runs the flattened lift either
//!    returns the clock domains with the model (`sv_to_btor2_with_blackboxes`) or refuses the
//!    opt-in before it lifts (`refuse_merge_without_domains`). A new entry that returns the model
//!    alone and does not refuse fails here.
//! 2. **At the callers**: every file that calls `sv_to_btor2_with_blackboxes` is listed in
//!    [`CALLERS`] with the calls that carry the domains into its report. A new caller fails here
//!    until it is listed with them.
//!
//! It replaces `clock_roots_reported_1148`, which listed the setters of the retired
//! `YosysOptions::report_clock_roots`. As there, a reporter is looked for as a CALL, never as a
//! name: a file that defines or mentions the reporter does not pass for one that calls it.
//!
//! What the scan cannot see: that a caller's report reaches the verb's response. The rows of
//! `e2e_x1181_every_sv_route_reports_the_merged_clock_domains` check that on the real lift, and
//! [`REPORT_SCHEMAS`] pins the field they arrive in on every report type with a schema.

use std::path::Path;

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};
use crate::source_scan::without_test_modules;

/// The file that holds the lift, as path components under the repository root.
const LIFT_FILE: &[&str] = &["crates", "mununu-core", "src", "adapter", "yosys", "mod.rs"];

/// The flattened lift and its two inner stages. They call one another; anything else that
/// calls one of them is an entry of the lift.
const LIFT_FNS: &[&str] = &[
    "run_sv_flatten_btor2",
    "run_sv_flatten_btor2_with",
    "run_sv_flatten_btor2_concrete",
];

/// The one entry that returns the clock domains with the model.
const REPORTING_ENTRY: &str = "sv_to_btor2_with_blackboxes";

/// What every other entry calls before it lifts.
const REFUSAL: &str = "refuse_merge_without_domains";

/// The source directories whose product code may call the reporting entry.
const SCANNED: &[&[&str]] = &[
    &["crates", "mununu-core", "src"],
    &["crates", "mununu-cli", "src"],
];

/// `(a file that calls the reporting entry, the calls that carry the domains into its report)`.
/// Each call is `(file, the text of the call)`. A file with two callers lists both chains: the
/// scan is per file, so it does not tell which caller a chain belongs to.
const CALLERS: &[(&str, &[(&str, &str)])] = &[
    (
        // `lift_sv` → `prepare_model`: the merged domains and the root notes of `sv verify-auto`
        // (and of `sv plan` / `sv mutate`, which read them off that report).
        "crates/mununu-core/src/adapter/slang/verify_auto.rs",
        &[
            (
                "crates/mununu-core/src/adapter/slang/verify_auto.rs",
                "merged_clock_domain_descriptions(&clock_domains)",
            ),
            (
                "crates/mununu-core/src/adapter/slang/verify_auto.rs",
                ".extend(clock_root_notes(&clock_domains))",
            ),
            (
                "crates/mununu-core/src/planner/entry.rs",
                "clock_notes_of_report(&baseline)",
            ),
            (
                "crates/mununu-core/src/planner/entry.rs",
                "clock_notes_of_report(&m.report)",
            ),
        ],
    ),
    (
        // `SvLift::lift_domains` (every other verb's lift) and `sv_cdc_lint`.
        "crates/mununu-core/src/adapter/sv_verify.rs",
        &[
            (
                "crates/mununu-core/src/adapter/sv_verify.rs",
                "lift_notes: lift_clock_notes(&lifted.clock_domains)",
            ),
            (
                "crates/mununu-core/src/adapter/sv_verify.rs",
                "cdc_lint::cdc_lint(",
            ),
            (
                "crates/mununu-core/src/adapter/cdc_lint.rs",
                ".extend(clock_root_notes(&report.domains))",
            ),
        ],
    ),
];

/// The pinned schemas (under `docs/api-schemas/`) of the reports an `sv` verb that lifts
/// returns, other than `sv verify-auto`'s (the caveat is in its `notes`) and `sv cdc-lint`'s
/// (its `domains` and `notes`). The `btor2` routes share four of the types and leave the
/// field empty. `sv lint`'s response has no pinned schema; its row is in the e2e test.
const REPORT_SCHEMAS: &[&str] = &[
    "btor2-check-fsm-response.schema.json",
    "btor2-verify-liveness-response.schema.json",
    "btor2-verify-recoverability-response.schema.json",
    "cegar-response.schema.json",
    "sv-mutate-response.schema.json",
    "sv-plan-response.schema.json",
    "sv-verify-liveness-all-report.schema.json",
    "sv-verify-liveness-report.schema.json",
    "sv-verify-recoverability-report.schema.json",
];

/// `path` relative to `root`, with `/` between components on every host (built from the
/// components, never from the display string: mununu#1153).
fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .expect("under the repository")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// `root` joined with `components`, one at a time.
fn under(root: &Path, components: &[&str]) -> std::path::PathBuf {
    components.iter().fold(root.to_path_buf(), |p, c| p.join(c))
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

/// How many times `code` calls `name`: `name(` that is neither the definition (`fn name(`) nor
/// the tail of a longer identifier.
fn calls(code: &str, name: &str) -> usize {
    let needle = format!("{name}(");
    code.match_indices(&needle)
        .filter(|(at, _)| {
            let before = &code[..*at];
            !before.ends_with("fn ") && !before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        })
        .count()
}

/// The name a top-level `fn` line declares: `fn` at column 0, after nothing but a visibility
/// (`pub`, `pub(crate)`, …) and `const` / `async` / `unsafe`.
fn top_level_fn_name(line: &str) -> Option<&str> {
    let at = line.find("fn ")?;
    let head = &line[..at];
    let is_qualifier = |w: &str| w.starts_with("pub") || matches!(w, "const" | "async" | "unsafe");
    if line.starts_with(char::is_whitespace) || !head.split_whitespace().all(is_qualifier) {
        return None;
    }
    let rest = &line[at + "fn ".len()..];
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    Some(&rest[..end])
}

/// `code` cut at its top-level `fn` lines: `(name, the text up to the next top-level fn)`. What
/// sits between two functions (a struct, an `impl` block) is read with the function above it, so
/// a lift call there is still seen, under that function's name.
fn top_level_fns(code: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in code.lines() {
        if let Some(name) = top_level_fn_name(line) {
            out.push((name.to_string(), String::new()));
        }
        if let Some((_, body)) = out.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    out
}

/// The entries of the lift in `code` that return the model alone and do not refuse the opt-in.
fn entries_that_drop_the_domains(code: &str) -> Vec<String> {
    top_level_fns(code)
        .into_iter()
        .filter(|(name, body)| {
            LIFT_FNS.iter().any(|lift| calls(body, lift) > 0)
                && !LIFT_FNS.contains(&name.as_str())
                && name != REPORTING_ENTRY
                && calls(body, REFUSAL) == 0
        })
        .map(|(name, _)| name)
        .collect()
}

/// The files of `sources` (`(relative path, text)`) whose product code calls the reporting
/// entry, sorted.
fn callers(sources: &[(String, String)]) -> Vec<String> {
    let mut found: Vec<String> = sources
        .iter()
        .filter(|(_, src)| calls(&product_code(src), REPORTING_ENTRY) > 0)
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
fn every_lift_that_can_merge_clock_domains_reports_them() {
    let root = repo_root();

    // 1. In the lift: an entry returns the domains, or refuses the opt-in.
    let lift_src = std::fs::read_to_string(under(&root, LIFT_FILE)).expect("the lift's source");
    let lift_code = product_code(&lift_src);
    assert!(
        LIFT_FNS.iter().all(|f| calls(&lift_code, f) > 0),
        "the lift's functions {LIFT_FNS:?} are not all called in the lift's file — they were \
         renamed, and this test reads nothing"
    );
    assert!(
        calls(&lift_code, REFUSAL) >= 3,
        "`{REFUSAL}` is called by fewer than the three model-only entries it was written for"
    );
    assert_eq!(
        entries_that_drop_the_domains(&lift_code),
        Vec::<String>::new(),
        "an entry of the SystemVerilog lift returns the model without its clock domains and \
         does not call `{REFUSAL}`: under the merge opt-in its verdicts would say nothing about \
         the merged clocks. Return the domains (as `{REPORTING_ENTRY}` does) or refuse the opt-in"
    );

    // 2. At the callers: each is listed with the calls that report the domains.
    let sources: Vec<(String, String)> = SCANNED
        .iter()
        .flat_map(|dir| rust_files_under(&under(&root, dir)))
        .filter(|path| *path != under(&root, LIFT_FILE))
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &root), src)
        })
        .collect();
    let mut want: Vec<String> = CALLERS.iter().map(|(f, _)| f.to_string()).collect();
    want.sort();
    assert_eq!(
        callers(&sources),
        want,
        "a file that calls `{REPORTING_ENTRY}` holds the clock domains of a lift that may have \
         merged them, and must put them in its report; list it in CALLERS with the calls that do"
    );
    for (caller, chain) in CALLERS {
        for (file, call) in *chain {
            let path = under(&root, &file.split('/').collect::<Vec<_>>());
            let src = std::fs::read_to_string(path).expect("a reporter file");
            assert!(
                makes_the_call(&src, call),
                "{caller} calls `{REPORTING_ENTRY}`, and {file} no longer makes the call `{call}` \
                 that carries the clock domains into its report"
            );
        }
    }
}

/// Every pinned report of a lifting `sv` verb has `lift_notes`: an array of notes, and
/// optional, so a report on a design with one input clock is byte-for-byte what it was. The
/// schema-drift tests tie each file to its Rust type; this one says which types must have it.
#[test]
fn every_pinned_report_of_a_lifting_sv_verb_has_optional_lift_notes() {
    let root = repo_root();
    for file in REPORT_SCHEMAS {
        let path = under(&root, &["docs", "api-schemas", file]);
        let text = std::fs::read_to_string(&path).expect("a pinned schema");
        let schema: serde_json::Value = serde_json::from_str(&text).expect("a JSON schema");
        let field = &schema["properties"]["lift_notes"];
        assert_eq!(
            field["type"], "array",
            "{file}: the report of a verb that lifts SystemVerilog has no `lift_notes` array, \
             so a lift under --merge-clock-domains has nowhere to say what it merged"
        );
        assert!(
            field["items"]["$ref"]
                .as_str()
                .is_some_and(|r| r.ends_with("/VerificationNoteView")),
            "{file}: `lift_notes` holds the typed notes `sv verify-auto` reports, with their \
             `level`: {field}"
        );
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        assert!(
            !required.iter().any(|r| r == "lift_notes"),
            "{file}: `lift_notes` is omitted when empty; a schema that requires it rejects \
             every report on a single-clock design"
        );
    }
}

/// The scan fails on what it exists to catch (negative control): an entry that lifts and
/// neither returns the domains nor refuses; a caller the list does not name; and a reporter
/// that is named — in a definition, in a comment — without being called.
#[test]
fn a_lift_entry_that_drops_the_domains_and_an_unlisted_caller_are_seen() {
    let lift = "\
fn run_sv_flatten_btor2(c: &str, y: &YosysOptions) -> R {
    run_sv_flatten_btor2_with(c, y, None)
}
fn run_sv_flatten_btor2_with(c: &str, y: &YosysOptions, k: K) -> R {
    run_sv_flatten_btor2_concrete(c, y)
}
fn run_sv_flatten_btor2_concrete(c: &str, y: &YosysOptions) -> R { todo() }
pub fn sv_to_btor2(c: &str, y: &YosysOptions) -> Result<String, E> {
    refuse_merge_without_domains(y)?;
    run_sv_flatten_btor2(c, y).map(|a| a.btor2)
}
fn refuse_merge_without_domains(y: &YosysOptions) -> Result<(), E> { Ok(()) }
pub fn sv_to_btor2_with_blackboxes(c: &str, y: &YosysOptions) -> Result<SvLiftResult, E> {
    let a = run_sv_flatten_btor2(c, y)?;
    Ok(SvLiftResult { clock_domains: a.clock_domains })
}
";
    assert_eq!(
        entries_that_drop_the_domains(lift),
        Vec::<String>::new(),
        "the lift's own chain, an entry that refuses and the entry that reports all pass"
    );
    let dropped = format!(
        "{lift}pub fn sv_register_names(c: &str, y: &YosysOptions) -> Result<Vec<String>, E> {{\n    \
         // refuse_merge_without_domains(y)?; was here\n    \
         let a = run_sv_flatten_btor2_with(c, y, None)?;\n    Ok(a.names)\n}}\n"
    );
    assert_eq!(
        entries_that_drop_the_domains(&product_code(&dropped)),
        vec!["sv_register_names".to_string()],
        "an entry that lifts through an inner stage and only mentions the refusal in a comment"
    );

    let product =
        "fn lift(s: &str, y: &YosysOptions) { let l = sv_to_btor2_with_blackboxes(s, y); }\n";
    let test_only = "fn lift() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let l = \
                     sv_to_btor2_with_blackboxes(S, &y); }\n}\n";
    let definer = "pub fn sv_to_btor2_with_blackboxes(c: &str) {}\n";
    let longer = "fn f() { my_sv_to_btor2_with_blackboxes(S); }\n";
    let sources = vec![
        ("src/adapter/new_verb.rs".to_string(), product.to_string()),
        ("src/adapter/tested.rs".to_string(), test_only.to_string()),
        ("src/adapter/definer.rs".to_string(), definer.to_string()),
        ("src/adapter/longer.rs".to_string(), longer.to_string()),
    ];
    assert_eq!(
        callers(&sources),
        vec!["src/adapter/new_verb.rs".to_string()],
        "a product caller is found; a test-module one, the definition and a longer name are not"
    );

    let named_not_called = "pub fn sv_cdc_lint() {}\n\
                            // the notes come from report.notes.extend(clock_root_notes(&report.domains));\n\
                            pub(crate) fn clock_root_notes(d: &[D]) -> Vec<String> { vec![] }\n";
    assert!(named_not_called.contains("clock_root_notes"));
    assert!(!makes_the_call(
        named_not_called,
        ".extend(clock_root_notes(&report.domains))"
    ));
    let called = "fn f() { report.notes.extend(clock_root_notes(&report.domains)); }\n";
    assert!(makes_the_call(
        called,
        ".extend(clock_root_notes(&report.domains))"
    ));
}
