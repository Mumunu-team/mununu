//! mununu#1181 — a lift that takes `--merge-clock-domains` steps every clock domain together
//! and ticks a derived or free-source clock on every step. A verdict on that model does not
//! transfer on its own, so the report says what was merged. Seven of the nine `sv` verbs dropped
//! the domains: only `sv verify-auto` and `sv cdc-lint` read them.
//!
//! The rule is structural, in three parts this test reads from the source:
//! 1. **In the lift** (`adapter/yosys/mod.rs`): a function that runs the flattened lift either
//!    returns the clock domains with the model (`sv_to_btor2_with_blackboxes`) or refuses the
//!    opt-in before it lifts (`refuse_merge_without_domains`). A new entry that returns the model
//!    alone and does not refuse fails here.
//!    Each also tells the lift whether its caller can take the opt-in (`MergeOptIn`), which
//!    decides how a default refusal words the flag: the reporting entry says it is offered,
//!    the others that it is not. An entry that says the wrong one fails here.
//! 2. **At the callers**: every file that calls `sv_to_btor2_with_blackboxes` is listed in
//!    [`CALLERS`] with the calls that carry the domains into its report, and how many times its
//!    product code makes each. A new caller fails here until it is listed with them; so does
//!    one of two identical calls going away.
//! 3. **At the surfaces** (mununu#1189): a `Lifted` holds a result beside the notes its lift
//!    owes the report, and product code takes neither without the other. Every read of a field
//!    named `result` is listed in [`RESULT_READS`] with what it is read off, and every
//!    `Lifted { … }` pattern names `lift_notes`. `sv check-fsm --liveness` lifted twice and
//!    reported the inventory of the second lift under the notes of the first, which were worded
//!    for the encoding scan alone; the second lift's notes were lost at a `.result`.
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
use crate::source_scan::{blank_test_modules_keeping_lines, matching_brace, without_test_modules};

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

/// One call of a [`CALLERS`] chain: `(file, the text of the call, how many times the file's
/// product code makes it)`.
type ReportingCall = (&'static str, &'static str, usize);

/// What an entry passes the lift when its caller can take the opt-in, and when it cannot.
const OPT_IN_OFFERED: &str = "MergeOptIn::Offered";
const OPT_IN_NOT_OFFERED: &str = "MergeOptIn::NotOffered";

/// `(a file that calls the reporting entry, the calls that carry the domains into its report)`.
/// Each call is `(file, the text of the call, how many times the file's product code makes
/// it)`. A file with two callers lists both chains: the scan is per file, so it does not tell
/// which caller a chain belongs to — the count is what tells that one of two callers with the
/// same call text stopped making it.
const CALLERS: &[(&str, &[ReportingCall])] = &[
    (
        // `lift_sv` → `prepare_model`: the merged domains and the root notes of `sv verify-auto`
        // (and of `sv plan` / `sv mutate`, which read them off that report).
        "crates/mununu-core/src/adapter/slang/verify_auto.rs",
        &[
            (
                "crates/mununu-core/src/adapter/slang/verify_auto.rs",
                "merged_clock_domain_descriptions(&clock_domains)",
                1,
            ),
            (
                "crates/mununu-core/src/adapter/slang/verify_auto.rs",
                ".extend(clock_root_notes(&clock_domains))",
                1,
            ),
            // `sv mutate` and `sv plan` read the lift's decisions off that report
            // (mununu#1189); the clock notes are among them.
            (
                "crates/mununu-core/src/planner/entry.rs",
                "LiftDecisions::of_report(&baseline, yosys_opts)",
                1,
            ),
            (
                "crates/mununu-core/src/planner/entry.rs",
                "LiftDecisions::of_report(&m.report, yosys_opts)",
                1,
            ),
            (
                "crates/mununu-core/src/adapter/lift_decisions.rs",
                "clock_notes: clock_notes_of_report(report)",
                1,
            ),
        ],
    ),
    (
        // `SvLift::lift_domains` (every other verb's lift) and `sv_cdc_lint`.
        "crates/mununu-core/src/adapter/sv_verify.rs",
        &[
            (
                "crates/mununu-core/src/adapter/sv_verify.rs",
                // `lift_domains` (the notes of every verb's lift) and `sv_cdc_lint` (its lines
                // for the lift's other decisions; its domains go by the two calls below).
                "LiftDecisions::of_lift(",
                2,
            ),
            (
                "crates/mununu-core/src/adapter/lift_decisions.rs",
                "clock_notes: lift_clock_notes(&lifted.clock_domains)",
                1,
            ),
            (
                "crates/mununu-core/src/adapter/sv_verify.rs",
                "cdc_lint::cdc_lint(",
                1,
            ),
            (
                "crates/mununu-core/src/adapter/cdc_lint.rs",
                ".extend(clock_root_notes(&report.domains))",
                1,
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
pub(crate) fn rel(path: &Path, root: &Path) -> String {
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
pub(crate) fn product_code(src: &str) -> String {
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

/// The entries of the lift in `code` that tell it the wrong thing about the opt-in: the
/// reporting entry is the one whose callers take the flag; an entry that returns the model
/// alone refuses the flag, so its default refusal must not offer it.
fn entries_that_misstate_the_opt_in(code: &str) -> Vec<String> {
    top_level_fns(code)
        .into_iter()
        .filter(|(name, body)| {
            LIFT_FNS.iter().any(|lift| calls(body, lift) > 0) && !LIFT_FNS.contains(&name.as_str())
        })
        .filter(|(name, body)| {
            let (says, must_not_say) = if name == REPORTING_ENTRY {
                (OPT_IN_OFFERED, OPT_IN_NOT_OFFERED)
            } else {
                (OPT_IN_NOT_OFFERED, OPT_IN_OFFERED)
            };
            !body.contains(says) || body.contains(must_not_say)
        })
        .map(|(name, _)| name)
        .collect()
}

/// How many times the product code of `src` makes the call `call`.
fn times_called(src: &str, call: &str) -> usize {
    product_code(src).matches(call).count()
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
    assert_eq!(
        entries_that_misstate_the_opt_in(&lift_code),
        Vec::<String>::new(),
        "an entry of the SystemVerilog lift tells the lift the wrong thing about the merge \
         opt-in: `{REPORTING_ENTRY}` passes `{OPT_IN_OFFERED}` (its callers take the flag), \
         every other entry `{OPT_IN_NOT_OFFERED}` (it refuses the flag, so a default refusal \
         that offered it would name a fix its caller does not have)"
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
        for (file, call, times) in *chain {
            let path = under(&root, &file.split('/').collect::<Vec<_>>());
            let src = std::fs::read_to_string(path).expect("a reporter file");
            assert_eq!(
                times_called(&src, call),
                *times,
                "{caller} calls `{REPORTING_ENTRY}`, and {file} does not make the call `{call}` \
                 that carries the clock domains into its report the {times} time(s) listed: a \
                 lift stopped reporting, or a new one reports and is not counted"
            );
        }
    }
}

/// `src` with its test modules and its comment-only lines blanked, the line numbers kept.
fn product_lines(src: &str) -> String {
    blank_test_modules_keeping_lines(src)
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The 1-based lines of `src` where product code reads a field named `result`, off anything: a
/// call (`…)?.result`), a bound name (`l.result`), a closure argument, across a line break too.
/// A method named `result`, a longer field name and a struct's `..result` are not this.
fn result_field_reads(src: &str) -> Vec<usize> {
    let code = product_lines(src);
    const FIELD: &str = ".result";
    code.match_indices(FIELD)
        .filter(|(at, _)| {
            let after = &code[at + FIELD.len()..];
            !code[..*at].ends_with('.')
                && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                && !after.trim_start().starts_with('(')
        })
        .map(|(at, _)| code[..at].matches('\n').count() + 1)
        .collect()
}

/// The 1-based lines of `src` where product code writes `Lifted { … }`, as a pattern or as a
/// value, without naming `lift_notes`: `let Lifted { result, .. } = …` drops the notes the way
/// `.result` does.
fn lifted_sites_without_notes(src: &str) -> Vec<usize> {
    let code = product_lines(src);
    const NAME: &str = "Lifted";
    code.match_indices(NAME)
        .filter(|(at, _)| {
            let own_word = !code[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            let body = code[at + NAME.len()..].trim_start();
            own_word
                && body.starts_with('{')
                && !body[..matching_brace(body)].contains("lift_notes")
        })
        .map(|(at, _)| code[..at].matches('\n').count() + 1)
        .collect()
}

/// Every read of a field named `result` in product code: `(file, how many, what they are read
/// off)`. A `Lifted` has two fields, so the only ways to take its result are this field and a
/// `Lifted { … }` pattern; the scan cannot tell a `Lifted` from another type with a field of
/// that name, so every read is listed and a new one fails until someone says what it is read
/// off. Only the last row reads a `Lifted`.
const RESULT_READS: &[(&str, usize, &str)] = &[
    (
        "crates/mununu-cli/src/main.rs",
        2,
        "a written replay row: the outcome of the Verilator run, absent when it did not run",
    ),
    (
        "crates/mununu-core/src/adapter/btor2/kmts_lift.rs",
        5,
        "`self.result`, the predicate-cube lift an eager lazy lift wraps",
    ),
    (
        "crates/mununu-core/src/adapter/justice.rs",
        1,
        "the outcome of the exact engine's ladder (`LadderOutcome`)",
    ),
    (
        "crates/mununu-core/src/adapter/recoverability.rs",
        1,
        "the outcome of the exact engine's ladder (`LadderOutcome`)",
    ),
    (
        "crates/mununu-core/src/adapter/slang/verify_auto.rs",
        2,
        "the outcome of the exact engine's ladder (`LadderOutcome`)",
    ),
    (
        "crates/mununu-core/src/adapter/sv_verify.rs",
        2,
        "`Lifted::map` and `Lifted::try_map`, which build the new `Lifted` with the same notes",
    ),
];

/// mununu#1189 — the notes of a lift reach the report that holds its result. A caller that
/// takes the result of a `Lifted` and not its notes has dropped them, and nothing downstream
/// can tell: the report reads as one on a lift that decided nothing.
///
/// What the scan cannot see: a pattern that names the notes and then ignores them
/// (`lift_notes: _`). A bound and unused `lift_notes` is the compiler's warning, which the
/// gate refuses.
#[test]
fn no_product_code_reads_a_lifts_result_and_drops_its_notes() {
    let root = repo_root();
    let sources: Vec<(String, String)> = SCANNED
        .iter()
        .flat_map(|dir| rust_files_under(&under(&root, dir)))
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &root), src)
        })
        .collect();

    let mut reads: Vec<(&str, Vec<usize>)> = sources
        .iter()
        .map(|(file, src)| (file.as_str(), result_field_reads(src)))
        .filter(|(_, lines)| !lines.is_empty())
        .collect();
    reads.sort();
    let counted: Vec<(&str, usize)> = reads.iter().map(|(f, lines)| (*f, lines.len())).collect();
    let mut listed: Vec<(&str, usize)> = RESULT_READS.iter().map(|(f, n, _)| (*f, *n)).collect();
    listed.sort();
    assert_eq!(
        counted, listed,
        "product code reads a field named `result` and RESULT_READS does not list the read \
         (found, by line: {reads:?}; listed: {RESULT_READS:?}). If the value is a `Lifted`, its \
         `lift_notes` are dropped there and the report says nothing about what the lift \
         decided: take both (`let Lifted {{ lift_notes, result }} = …`) and put the notes in \
         the report that holds the result. If it is another type, count the read in \
         RESULT_READS with what it is read off"
    );

    let dropped: Vec<String> = sources
        .iter()
        .flat_map(|(file, src)| {
            lifted_sites_without_notes(src)
                .into_iter()
                .map(move |line| format!("{file}:{line}"))
        })
        .collect();
    assert_eq!(
        dropped,
        Vec::<String>::new(),
        "product code takes a `Lifted` apart without naming `lift_notes`: the notes of that \
         lift are dropped there. Bind them and put them in the report that holds the result"
    );
}

/// The scans above fail on what they exist to catch (negative control), and only on that.
#[test]
fn a_result_read_off_a_call_is_seen() {
    let chained = "fn f() -> R {\n    let inv = scan(&lift)?\n        .result;\n    inv\n}\n";
    assert_eq!(result_field_reads(chained), vec![3]);
    let same_line = "fn f() -> R { scan(&lift).result }\n";
    assert_eq!(result_field_reads(same_line), vec![1]);
    let of_a_name = "fn f() {\n    let l = scan(&lift)?;\n    let r = l.result;\n}\n";
    assert_eq!(result_field_reads(of_a_name), vec![3]);
    let in_a_closure = "fn f() { scan(&lift).map(|l| l.result) }\n";
    assert_eq!(result_field_reads(in_a_closure), vec![1]);

    let destructured = "fn f() { let Lifted { lift_notes, result } = scan(&lift)?; }\n";
    let a_method = "fn f() { let r = scan(&lift).result(); }\n";
    let a_longer_field = "fn f() { let r = scan(&lift).result_count; }\n";
    let the_rest_of_a_struct = "fn f() { let r = R { a, ..result }; }\n";
    let in_a_comment = "fn f() {\n    // scan(&lift)?.result\n}\n";
    let in_a_test = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { scan(&l).result; }\n}\n";
    for clean in [
        destructured,
        a_method,
        a_longer_field,
        the_rest_of_a_struct,
        in_a_comment,
        in_a_test,
    ] {
        assert_eq!(result_field_reads(clean), Vec::<usize>::new(), "{clean}");
    }

    let notes_left_out = "fn f() {\n    let Lifted { result, .. } = scan(&lift)?;\n}\n";
    assert_eq!(lifted_sites_without_notes(notes_left_out), vec![2]);
    let by_its_path = "fn f() {\n    let sv_verify::Lifted {\n        result,\n        ..\n    } = scan(&lift)?;\n}\n";
    assert_eq!(lifted_sites_without_notes(by_its_path), vec![2]);

    let built = "fn f() -> Lifted<u8> { Lifted { lift_notes: Vec::new(), result: 0 } }\n";
    let another_type = "fn f() { let NotLifted { result, .. } = g(); }\n";
    let its_own_impl = "impl<T> Lifted<T> { fn get(self) -> T { self.0 } }\n";
    let a_test_pattern = "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let Lifted { result, .. } = g(); }\n}\n";
    for clean in [
        destructured,
        built,
        another_type,
        its_own_impl,
        a_test_pattern,
    ] {
        assert_eq!(
            lifted_sites_without_notes(clean),
            Vec::<usize>::new(),
            "{clean}"
        );
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
/// neither returns the domains nor refuses; an entry that offers an opt-in it refuses; a caller
/// the list does not name; a reporter that is named — in a definition, in a comment — without
/// being called; and one of two identical reporting calls going away.
#[test]
fn a_lift_entry_that_drops_the_domains_and_an_unlisted_caller_are_seen() {
    let lift = "\
fn run_sv_flatten_btor2(c: &str, y: &YosysOptions, o: MergeOptIn) -> R {
    run_sv_flatten_btor2_with(c, y, None, o)
}
fn run_sv_flatten_btor2_with(c: &str, y: &YosysOptions, k: K, o: MergeOptIn) -> R {
    run_sv_flatten_btor2_concrete(c, y, o)
}
fn run_sv_flatten_btor2_concrete(c: &str, y: &YosysOptions, o: MergeOptIn) -> R { todo() }
pub fn sv_to_btor2(c: &str, y: &YosysOptions) -> Result<String, E> {
    refuse_merge_without_domains(y)?;
    run_sv_flatten_btor2(c, y, MergeOptIn::NotOffered).map(|a| a.btor2)
}
fn refuse_merge_without_domains(y: &YosysOptions) -> Result<(), E> { Ok(()) }
pub fn sv_to_btor2_with_blackboxes(c: &str, y: &YosysOptions) -> Result<SvLiftResult, E> {
    let a = run_sv_flatten_btor2(c, y, MergeOptIn::Offered)?;
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
         let a = run_sv_flatten_btor2_with(c, y, None, MergeOptIn::NotOffered)?;\n    \
         Ok(a.names)\n}}\n"
    );
    assert_eq!(
        entries_that_drop_the_domains(&product_code(&dropped)),
        vec!["sv_register_names".to_string()],
        "an entry that lifts through an inner stage and only mentions the refusal in a comment"
    );

    assert_eq!(
        entries_that_misstate_the_opt_in(lift),
        Vec::<String>::new(),
        "the reporting entry offers the opt-in and the entry that refuses it does not"
    );
    let offers_what_it_refuses = lift.replace("MergeOptIn::NotOffered", "MergeOptIn::Offered");
    assert_eq!(
        entries_that_misstate_the_opt_in(&offers_what_it_refuses),
        vec!["sv_to_btor2".to_string()],
        "an entry that refuses the opt-in and has its default refusal offer it"
    );
    let reporter_that_denies = lift.replace("MergeOptIn::Offered", "MergeOptIn::NotOffered");
    assert_eq!(
        entries_that_misstate_the_opt_in(&reporter_that_denies),
        vec!["sv_to_btor2_with_blackboxes".to_string()],
        "the reporting entry telling its callers the flag is elsewhere"
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
    assert_eq!(
        times_called(
            named_not_called,
            ".extend(clock_root_notes(&report.domains))"
        ),
        0
    );
    let called = "fn f() { report.notes.extend(clock_root_notes(&report.domains)); }\n";
    assert_eq!(
        times_called(called, ".extend(clock_root_notes(&report.domains))"),
        1
    );

    // Two lifts with the same reporting call: when one drops its notes the text still occurs,
    // and only the count says so.
    let both = "fn lift_named() -> L { L { lift_notes: notes_of(&lifted) } }\n\
                fn lift_under() -> L { L { lift_notes: notes_of(&lifted) } }\n";
    let one_dropped = "fn lift_named() -> L { L { lift_notes: Vec::new() } }\n\
                       fn lift_under() -> L { L { lift_notes: notes_of(&lifted) } }\n";
    assert_eq!(times_called(both, "lift_notes: notes_of(&lifted)"), 2);
    assert_eq!(
        times_called(one_dropped, "lift_notes: notes_of(&lifted)"),
        1
    );
}
