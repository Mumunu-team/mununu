//! mununu#1257 — what `sv verify-auto` does to a report after the verdicts has a budget of its
//! own, or starts no search.
//!
//! The verdicts of a run are fixed when its last property is decided. The passes that follow
//! explain them: a path to a violation, a shorter path, the registers a repair would touch, a
//! check that an antecedent can fire. The path pass ran its search under the run budget alone,
//! so a run that decided eight rows in a second spent the rest of its hour on two paths, and a
//! row whose turn came with the budget spent got neither a path nor a note.
//!
//! This test reads `adapter/slang/verify_auto.rs` and takes every free function whose first
//! parameter is the report, mutably: those are the passes. [`PASSES`] says of each one whether
//! it starts a search and, if it does, which text of its body puts the search under a budget. A
//! new pass fails until it has a row; a row its function no longer matches fails too.
//!
//! What the scan does not see: a pass with another signature (the notes builders of
//! `verify_auto_text.rs` take the notes, not the report), and a search a pass reaches through
//! a name the file imports rather than through a `crate::adapter::` path.

use crate::remedies_name_real_flags_1106::repo_root;
use crate::source_scan::{matching_brace, without_test_modules};

/// The file of the passes, from the repository root.
const FILE: &[&str] = &[
    "crates",
    "mununu-core",
    "src",
    "adapter",
    "slang",
    "verify_auto.rs",
];

/// How a free function starts, at the first column.
const FN_HEADS: &[&str] = &["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "];

/// The first parameter of a pass.
const REPORT_PARAM: &str = "report: &mut AutoVerifyReport";

/// What puts a search under a budget of its own: the two helpers that enter a child of the
/// rescue slice, and a child budget made on the spot.
const BUDGET_MARKERS: &[&str] = &[
    "safety_path_under_slice(",
    "under_rescue_slice(",
    "Budget::child(",
];

/// The `crate::adapter::` paths a pass that starts no search may name: parsing, cone walks,
/// shape reductions and the resume plan. Each is a prefix.
const NO_SEARCH_PATHS: &[&str] = &[
    "btor2::ast::",
    "btor2::dep_graph::",
    "btor2::model_facts::",
    "btor2::parser::",
    "btor2::symbolic_bitblast::formula_seed_atoms",
    "liveness_rescue::reduce_response_shape",
    "resume::",
    "trap_certificate::named_inputs_in_cone",
    "trap_certificate::undriven_nets_in_cone",
];

/// What a pass does with the time it is given.
enum Time {
    /// It reads and rewrites the report and starts no search.
    NoSearch,
    /// It starts a search, and this text of its body puts the search under a budget.
    Bounded(&'static str),
    /// Its searches are those of these passes, which it calls.
    Through(&'static [&'static str]),
    /// It starts a search under the run budget alone: the issue that holds it.
    Open(&'static str),
}

/// Every pass of the file, in the file's order.
const PASSES: &[(&str, Time)] = &[
    ("finish_assumptions", Time::NoSearch),
    ("refresh_coverage_summary", Time::NoSearch),
    ("attach_engine_failure_reason", Time::NoSearch),
    ("flag_self_contradictory_verdicts", Time::NoSearch),
    ("flag_vacuous_guarantees", Time::NoSearch),
    ("refresh_vacuity_notes", Time::NoSearch),
    (
        "attach_safety_witnesses",
        Time::Through(&["attach_safety_witnesses_under"]),
    ),
    (
        "attach_safety_witnesses_under",
        Time::Bounded("safety_path_under_slice("),
    ),
    ("downgrade_violated_on_unestablished_init", Time::NoSearch),
    ("withhold_violated_over_free_leaves", Time::NoSearch),
    ("attach_repair_candidates", Time::Open("mununu#1260")),
    ("attach_involves", Time::NoSearch),
    ("drop_stale_bottom_reasons", Time::NoSearch),
    ("refresh_abstraction_posture", Time::NoSearch),
    (
        "replan_merged",
        Time::Through(&["attach_safety_witnesses", "escalate_bottom"]),
    ),
    ("apply_resume", Time::NoSearch),
    ("fill_pinned", Time::NoSearch),
    (
        "check_antecedent_vacuity",
        Time::Bounded("under_rescue_slice("),
    ),
    (
        "finish_fault_posture",
        Time::Through(&["check_antecedent_vacuity"]),
    ),
    ("reopen_violations_decided_without_fairness", Time::NoSearch),
    (
        "escalate_bottom",
        Time::Through(&["escalate_bottom_with_slice"]),
    ),
    (
        "escalate_bottom_with_slice",
        Time::Bounded("under_rescue_slice("),
    ),
    ("rescue_skipped_via_exact", Time::Bounded("Budget::child(")),
];

/// `(name, body)` of every free function of `src` whose first parameter is the report,
/// mutably. The body has no line that is only a comment: a comment that names a call is not
/// the call.
fn passes(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for line in src.split_inclusive('\n') {
        let item = &src[offset..];
        offset += line.len();
        let Some(after_fn) = FN_HEADS.iter().find_map(|head| line.strip_prefix(head)) else {
            continue;
        };
        let name: String = after_fn
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // From the name on: a visibility has a parenthesis of its own.
        let signature = &item[line.len() - after_fn.len() + name.len()..];
        let Some(paren) = signature.find('(') else {
            continue;
        };
        if !signature[paren + 1..]
            .trim_start()
            .starts_with(REPORT_PARAM)
        {
            continue;
        }
        let Some(brace) = signature.find('{') else {
            continue;
        };
        let body = &signature[brace..];
        let body = &body[..matching_brace(body)];
        let code: Vec<&str> = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        out.push((name, code.join("\n")));
    }
    out
}

/// Whether `code` calls `name`: `name(` that is not the tail of a longer identifier.
fn calls(code: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    code.match_indices(&needle).any(|(at, _)| {
        !code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The `crate::adapter::` paths `code` names, without that prefix.
fn adapter_paths(code: &str) -> Vec<String> {
    const PREFIX: &str = "crate::adapter::";
    code.match_indices(PREFIX)
        .map(|(at, _)| {
            code[at + PREFIX.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect::<String>()
        })
        .collect()
}

/// What `table` gets wrong about `found`, one line per finding.
fn check(found: &[(String, String)], table: &[(&str, Time)]) -> Vec<String> {
    let mut findings = Vec::new();
    for (name, _) in found {
        if !table.iter().any(|(listed, _)| listed == name) {
            findings.push(format!(
                "`{name}` takes the report mutably and has no row: say whether it starts a \
                 search and, if it does, which budget bounds it"
            ));
        }
    }
    for (listed, time) in table {
        let Some((_, code)) = found.iter().find(|(name, _)| name == listed) else {
            findings.push(format!(
                "`{listed}` has a row and is no longer a pass: remove the row"
            ));
            continue;
        };
        let mut searching: Vec<&str> = table
            .iter()
            .filter(|(other, time)| other != listed && !matches!(time, Time::NoSearch))
            .map(|(other, _)| *other)
            .filter(|other| calls(code, other))
            .collect();
        searching.sort_unstable();
        let entered: Vec<&str> = BUDGET_MARKERS
            .iter()
            .copied()
            .filter(|marker| code.contains(marker))
            .collect();
        match time {
            Time::NoSearch => {
                if !searching.is_empty() {
                    findings.push(format!(
                        "`{listed}` is listed as starting no search and calls {searching:?}"
                    ));
                }
                if !entered.is_empty() {
                    findings.push(format!(
                        "`{listed}` is listed as starting no search and enters a budget \
                         ({entered:?})"
                    ));
                }
                for path in adapter_paths(code) {
                    if !NO_SEARCH_PATHS.iter().any(|ok| path.starts_with(ok)) {
                        findings.push(format!(
                            "`{listed}` is listed as starting no search and names \
                             `crate::adapter::{path}`: a path that starts none goes in \
                             NO_SEARCH_PATHS, a search goes under a budget"
                        ));
                    }
                }
            }
            Time::Bounded(marker) => {
                if !code.contains(marker) {
                    findings.push(format!(
                        "`{listed}` is listed as bounded by `{marker}` and its body no longer \
                         has it"
                    ));
                }
            }
            Time::Through(passes) => {
                let mut listed_passes = passes.to_vec();
                listed_passes.sort_unstable();
                if searching != listed_passes {
                    findings.push(format!(
                        "`{listed}` is listed as searching through {listed_passes:?} and calls \
                         {searching:?}"
                    ));
                }
            }
            Time::Open(issue) => {
                if !entered.is_empty() {
                    findings.push(format!(
                        "`{listed}` is listed as open ({issue}) and now enters a budget \
                         ({entered:?}): its row says which"
                    ));
                }
            }
        }
    }
    findings
}

#[test]
fn every_pass_over_a_decided_report_is_bounded_or_starts_no_search() {
    let path = FILE.iter().fold(repo_root(), |p, c| p.join(c));
    let src = std::fs::read_to_string(&path).expect("readable source");
    let found = passes(&without_test_modules(&src));
    let findings = check(&found, PASSES);
    assert!(
        findings.is_empty(),
        "the passes over a decided report and their table disagree:\n{}\nA pass that starts a \
         search runs it under a budget of its own (a child of the rescue slice or of the \
         property's budget) and reports the stop in a note; one that cannot yet is listed as \
         open with its issue",
        findings.join("\n")
    );
}

/// The scan fails on what it exists to catch (negative control), and only on that.
#[test]
fn a_pass_the_table_does_not_describe_is_seen() {
    const SOURCE: &str = "\
fn tidy(report: &mut AutoVerifyReport) {
    // search(report, design) is not called here.
    report.notes.clear();
}

fn search(
    report: &mut AutoVerifyReport,
    design: &str,
) {
    let path = under_rescue_slice(|| find(design));
    report.paths.push(path);
}

pub(crate) fn finish(report: &mut AutoVerifyReport, design: &str) {
    search(report, design);
    tidy(report);
}

fn reads(report: &AutoVerifyReport) -> usize {
    report.notes.len()
}
";
    const TABLE: &[(&str, Time)] = &[
        ("tidy", Time::NoSearch),
        ("search", Time::Bounded("under_rescue_slice(")),
        ("finish", Time::Through(&["search"])),
    ];
    let findings_of = |src: &str, table: &[(&str, Time)]| check(&passes(src), table);
    // Exactly one finding, and it names the function.
    let one_on = |findings: Vec<String>, name: &str| {
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains(&format!("`{name}`")), "{findings:?}");
    };

    assert_eq!(findings_of(SOURCE, TABLE), Vec::<String>::new());

    // A new pass.
    let added = format!("{SOURCE}\nfn extra(report: &mut AutoVerifyReport) {{}}\n");
    one_on(findings_of(&added, TABLE), "extra");

    // A row whose function is gone.
    let with_a_stale_row: Vec<(&str, Time)> = vec![
        ("tidy", Time::NoSearch),
        ("search", Time::Bounded("under_rescue_slice(")),
        ("finish", Time::Through(&["search"])),
        ("gone", Time::NoSearch),
    ];
    one_on(findings_of(SOURCE, &with_a_stale_row), "gone");

    // A pass listed as starting no search that calls one that does.
    let calls_a_search = SOURCE.replace("report.notes.clear();", "search(report, \"\");");
    one_on(findings_of(&calls_a_search, TABLE), "tidy");

    // ... or that names a path of the adapters that is not a parse or a cone walk.
    let names_a_search = SOURCE.replace(
        "report.notes.clear();",
        "crate::adapter::reach_rescue::safety_witness(report);",
    );
    one_on(findings_of(&names_a_search, TABLE), "tidy");
    let names_a_parse = SOURCE.replace(
        "report.notes.clear();",
        "crate::adapter::btor2::parser::parse(report);",
    );
    assert_eq!(findings_of(&names_a_parse, TABLE), Vec::<String>::new());

    // A search that lost its budget.
    let unbounded = SOURCE.replace("under_rescue_slice(|| find(design))", "find(design)");
    one_on(findings_of(&unbounded, TABLE), "search");

    // A pass that stopped calling the search it is listed for.
    let no_longer_through = SOURCE.replace("    search(report, design);\n", "");
    one_on(findings_of(&no_longer_through, TABLE), "finish");

    // A pass listed as open that got a budget.
    let open: Vec<(&str, Time)> = vec![
        ("tidy", Time::NoSearch),
        ("search", Time::Open("mununu#0")),
        ("finish", Time::Through(&["search"])),
    ];
    one_on(findings_of(SOURCE, &open), "search");
}
