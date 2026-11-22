//! mununu#1175 — the reach portfolio's outcome lists every member that found a path the concrete
//! simulator did not confirm (`ReachOutcome::trace_replay_failed`: a design the simulator cannot
//! carry, or a witness that did not replay). The verdict is unchanged, so a report that reads the
//! outcome and drops the list shows a result resting on an unconfirmed path exactly as it shows a
//! replayed one. Only `btor2 verify` printed it.
//!
//! What varies is WHICH REPORT READS AN OUTCOME, and there is no enum of those: they are the call
//! sites of the functions that return one. So this test derives them from the code:
//!
//! 1. the PRODUCERS — every `pub` function whose return type names a `ReachOutcome` (a derived
//!    outcome such as `FaultReachOutcome` counts: it carries the same list one layer up);
//! 2. the READERS — every file whose product code calls a producer, with the NUMBER of calls.
//!
//! Each reader is listed below with the calls that carry the unconfirmed paths into its report,
//! or with why it has none to carry. A new producer, a new reader, or one more call in a listed
//! reader fails here until someone has looked at what that call does with the list.
//!
//! A carrier is looked for as a CALL in product code (no test module, no comment line, no `fn`
//! definition), never as a name (the lesson of `clock_roots_reported_1148`), and with the NUMBER
//! of sites that make it: a reader with two reports has two carriers, and a needle that only has
//! to be present would still be satisfied by the one left after the other is deleted.
//!
//! A producer is called by its own name or through a `use … as <alias>`; a file's own function
//! of the same name that returns no outcome (the CLI's `sv_verify_liveness` is the verb) is a
//! namesake, and its calls are not counted.

use std::path::Path;

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under};
use crate::source_scan::without_test_modules;

/// The crates whose product code is scanned, relative to `crates/`.
const SCANNED: &[&str] = &["mununu-core/src", "mununu-cli/src"];

/// Every `pub` function that returns an outcome carrying the list, sorted.
const PRODUCERS: &[&str] = &[
    "decide_reach_owned_only",
    "decide_reach_owned_only_with_witness",
    "decide_reach_portfolio",
    "decide_reach_portfolio_cancellable",
    "decide_reach_portfolio_parallel_with_timeout",
    "decide_reach_portfolio_with_members",
    "reach_portfolio_rescue_diagnosed",
    "response_liveness_rescue",
    "response_liveness_rescue_atoms",
    "response_liveness_rescue_conjunction",
    "response_liveness_rescue_formula",
    "response_liveness_rescue_under_fairness",
    "response_liveness_rescue_under_fairness_cancellable",
    "sv_verify_liveness",
    "sv_verify_liveness_all",
    "sv_verify_liveness_under_fairness",
    "verify_fault_reach",
];

/// One file that calls a producer.
struct Reader {
    /// Relative to `crates/`, `/`-separated.
    file: &'static str,
    /// How many producer calls its product code makes.
    calls: usize,
    /// The calls that carry the unconfirmed paths into the file's report, each with how many
    /// sites make it. No needle is a substring of another of the same reader.
    carries: &'static [(&'static str, usize)],
    /// What the file does with an outcome — for a reader with nothing to carry, why.
    what: &'static str,
}

const READERS: &[Reader] = &[
    Reader {
        file: "mununu-cli/src/main.rs",
        calls: 9,
        carries: &[
            ("TraceReplayFailureView::list(&outcome)", 1),
            ("liveness_rescue::member_notes(", 3),
            (
                ".map(mununu_core::api::models::TraceReplayFailureView::from)",
                1,
            ),
            (".map(fsm_register_json)", 2),
            ("row[\"notes\"] = serde_json::json!(f.notes)", 1),
            ("unconfirmed.note()", 1),
        ],
        what: "`btor2 verify` (portfolio and owned-only, one view) prints the typed list; the \
               response verbs print `member_notes` (the btor2 row printer, the sv conjunction's \
               rows, the sv single row); `--fault … --target` prints the adversary's typed list; \
               `check-fsm` (btor2 and sv) prints each register row through `fsm_register_json`, \
               which writes the row's notes (the `--liveness` inventory row is serialised from \
               its own type); `sv verify-auto`'s text report prints one line per unconfirmed path",
    },
    Reader {
        file: "mununu-core/src/api/handlers.rs",
        calls: 8,
        carries: &[
            ("TraceReplayFailureView::list(&outcome)", 1),
            ("liveness_rescue::member_notes(", 5),
            ("o.trace_replay_failed.iter().map(Into::into)", 1),
            ("notes: f.notes.clone()", 2),
        ],
        what: "the API peers of the CLI rows above (`sv verify-auto`'s rows are serialised from \
               the report's own type)",
    },
    Reader {
        file: "mununu-core/src/adapter/btor2/kliveness_monitor.rs",
        calls: 1,
        carries: &[("unconfirmed_at(label, &outcome)", 1)],
        what: "the k-liveness ladder: a reachable `bad` at one width only moves the ladder to \
               the next and the one verdict it gives is the proof (`Unreachable`) — but `<bound> \
               reachable` reaches the abstention note of an undecided row, so the unconfirmed \
               path is said beside it (and beside a contradiction)",
    },
    Reader {
        file: "mununu-core/src/adapter/btor2/witness.rs",
        calls: 1,
        carries: &[("&again.trace_replay_failed", 2)],
        what: "the witness fallback's re-run: it reads the list itself, to tell a member that \
               found a path from one whose path replayed",
    },
    Reader {
        file: "mununu-core/src/adapter/cdc_lint.rs",
        calls: 1,
        carries: &[
            ("gray_step_violated_detail(&name, &outcome)", 1),
            ("outcome.unconfirmed_notes()", 1),
        ],
        what: "`sv cdc-lint`: a two-bit step's detail names the unconfirmed path",
    },
    Reader {
        file: "mununu-core/src/adapter/fault_adversary.rs",
        calls: 2,
        carries: &[("trace_replay_failed: outcome.unconfirmed()", 2)],
        what: "the k-fault adversary, with and without a target: the typed list on its outcome",
    },
    Reader {
        file: "mununu-core/src/adapter/fsm_scan.rs",
        calls: 2,
        carries: &[(".unconfirmed_notes()", 2)],
        what: "`check-fsm`: an illegal encoding (one site) and, with `--liveness`, a reached \
               encoding (the other) found on an unconfirmed path carries the note on its row",
    },
    Reader {
        file: "mununu-core/src/adapter/justice.rs",
        calls: 1,
        carries: &[("l2s_unconfirmed_notes(&outcome)", 2)],
        what: "the justice l2s rung: a lasso the simulator did not confirm is a note on the \
               verdict, on a `violated` and on an undecided rung (two sites); the serial ladder \
               takes the rung's notes and the parallel merge keeps every arrival's",
    },
    Reader {
        file: "mununu-core/src/adapter/liveness_rescue.rs",
        calls: 6,
        carries: &[("outcome.unconfirmed_notes()", 1)],
        what: "the response rescues return the outcome to their caller (they are producers); \
               `member_notes` is the line every response verb prints from it",
    },
    Reader {
        file: "mununu-core/src/adapter/reach_portfolio.rs",
        calls: 4,
        carries: &[],
        what: "the portfolio itself: one entry point calling another. It is the carrier's home \
               (`ReachOutcome::unconfirmed` and `unconfirmed_notes`), which every row above \
               calls — nothing here reports",
    },
    Reader {
        file: "mununu-core/src/adapter/reach_rescue.rs",
        calls: 1,
        carries: &[],
        what: "the safety rescue returns the outcome to its caller (it is a producer)",
    },
    Reader {
        file: "mununu-core/src/adapter/recoverability.rs",
        calls: 2,
        carries: &[(".unconfirmed_notes()", 1)],
        what: "the non-vacuity gate of a discovered assumption puts the lines on the assumption; \
               the vacuity probe reads only the proof (`Unreachable`)",
    },
    Reader {
        file: "mununu-core/src/adapter/slang/verify_auto.rs",
        calls: 5,
        carries: &[
            ("trace_replay_failed: reach.unconfirmed()", 1),
            ("prop.portfolio = Some(PortfolioAttribution::of(", 4),
            (".chain(outcome.unconfirmed_notes())", 1),
            ("cert.reach_unconfirmed", 1),
        ],
        what: "`sv verify-auto`: the four rescues attribute the row through \
               `PortfolioAttribution::of`; the antecedent-vacuity probe lists the lines in its \
               note; the trap certificate's lines (put on the certificate by \
               `trap_certificate.rs`, below) become items of the certificate's note",
    },
    Reader {
        file: "mununu-core/src/adapter/sv_verify.rs",
        calls: 3,
        carries: &[],
        what: "the `sv` response verbs return the outcome to the CLI and the API (producers)",
    },
    Reader {
        file: "mununu-core/src/adapter/trap_certificate.rs",
        calls: 2,
        carries: &[("reach.unconfirmed_notes()", 1)],
        what: "the trap certificate: the reach step's lines go on the certificate (the closure \
               step claims only its proof; a candidate it rejects claims nothing)",
    },
];

/// `path` relative to `root`, with `/` between components on every host (built from the
/// components, never from the display string: mununu#1153).
fn rel(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .expect("under crates/")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The product code of `src`: no test module, no line that is only a comment.
fn product_code(src: &str) -> String {
    without_test_modules(src)
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The names of the `pub` (any visibility wider than private) functions of `code` whose return
/// type names a `ReachOutcome`.
fn producers_in(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = code[from..].find("fn ") {
        let start = from + at;
        from = start + 3;
        // `fn` as a word, on a line that says `pub`.
        if code[..start].chars().next_back().is_some_and(is_ident) {
            continue;
        }
        let line_start = code[..start].rfind('\n').map_or(0, |i| i + 1);
        if !code[line_start..start].contains("pub") {
            continue;
        }
        let rest = &code[from..];
        let name: String = rest.chars().take_while(|c| is_ident(*c)).collect();
        if name.is_empty() {
            continue;
        }
        // The signature ends at the body's `{` or at a `;`.
        let end = rest.find(['{', ';']).unwrap_or(rest.len());
        let signature = &rest[..end];
        if signature
            .rsplit_once("->")
            .is_some_and(|(_, ret)| ret.contains("ReachOutcome"))
        {
            out.push(name);
        }
    }
    out
}

/// Where `word` stands in `code` as a whole identifier.
fn word_sites<'a>(code: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    code.match_indices(word)
        .map(|(at, _)| at)
        .filter(move |at| {
            !code[..*at].chars().next_back().is_some_and(is_ident)
                && !code[at + word.len()..].chars().next().is_some_and(is_ident)
        })
}

/// The names `code` calls the producers `names` by: a producer's own name — unless the file
/// defines a function of that name that returns no outcome (a namesake) — and every alias a
/// `use … <name> as <alias>` gives one.
fn call_names(code: &str, names: &[&str]) -> Vec<String> {
    let local_producers = producers_in(code);
    let mut out = Vec::new();
    for name in names {
        let defined_here = word_sites(code, name).any(|at| code[..at].trim_end().ends_with("fn"));
        if !defined_here || local_producers.iter().any(|p| p == name) {
            out.push((*name).to_string());
        }
        for at in word_sites(code, name) {
            if let Some(rest) = code[at + name.len()..].strip_prefix(" as ") {
                let alias: String = rest.chars().take_while(|c| is_ident(*c)).collect();
                if !alias.is_empty() {
                    out.push(alias);
                }
            }
        }
    }
    out
}

/// How many times `code` calls one of the producers `names` (by any of [`call_names`]; `name(`
/// as a whole word, not its definition).
fn calls_in(code: &str, names: &[&str]) -> usize {
    call_names(code, names)
        .iter()
        .map(|name| {
            word_sites(code, name)
                .filter(|at| {
                    code[at + name.len()..].starts_with('(')
                        && !code[..*at].trim_end().ends_with("fn")
                })
                .count()
        })
        .sum()
}

/// How many sites of `code` make the carrier call `needle` (its definition is not one).
fn carrier_sites(code: &str, needle: &str) -> usize {
    code.match_indices(needle)
        .filter(|(at, _)| !code[..*at].trim_end().ends_with("fn"))
        .count()
}

fn scanned_sources() -> Vec<(String, String)> {
    let crates = repo_root().join("crates");
    let mut sources: Vec<(String, String)> = SCANNED
        .iter()
        .flat_map(|dir| {
            let mut under = crates.clone();
            under.extend(dir.split('/'));
            rust_files_under(&under)
        })
        .map(|path| {
            let src = std::fs::read_to_string(&path).expect("readable source");
            (rel(&path, &crates), product_code(&src))
        })
        .collect();
    sources.sort();
    sources
}

#[test]
fn every_function_that_returns_a_reach_outcome_is_a_listed_producer() {
    let mut found: Vec<String> = scanned_sources()
        .iter()
        .flat_map(|(_, code)| producers_in(code))
        .collect();
    found.sort();
    found.dedup();
    let want: Vec<String> = PRODUCERS.iter().map(|p| p.to_string()).collect();
    assert_eq!(
        found, want,
        "a `pub` function returns a `ReachOutcome` (or an outcome derived from one): list it in \
         PRODUCERS, then classify every caller in READERS"
    );
}

#[test]
fn every_report_that_reads_a_reach_outcome_carries_its_unconfirmed_paths() {
    let sources = scanned_sources();
    let found: Vec<(String, usize)> = sources
        .iter()
        .map(|(file, code)| (file.clone(), calls_in(code, PRODUCERS)))
        .filter(|(_, calls)| *calls > 0)
        .collect();
    let mut want: Vec<(String, usize)> = READERS
        .iter()
        .map(|r| (r.file.to_string(), r.calls))
        .collect();
    want.sort();
    assert_eq!(
        found, want,
        "a call that reads a reach outcome was added, removed or moved. Look at what the call \
         does with `ReachOutcome::unconfirmed()` — a report that drops it shows a result resting \
         on an unconfirmed path as if it were replayed — then update READERS"
    );
    for reader in READERS {
        assert!(
            !reader.what.is_empty(),
            "{}: say what it does with an outcome",
            reader.file
        );
        let code = &sources
            .iter()
            .find(|(file, _)| file == reader.file)
            .expect("a listed reader is a scanned file")
            .1;
        for (call, sites) in reader.carries {
            assert_eq!(
                carrier_sites(code, call),
                *sites,
                "{} reads a reach outcome ({}): the number of sites making the call `{call}` \
                 that carries the unconfirmed paths into its report changed. A site removed \
                 is a report that dropped them; a site added is a new report to describe here",
                reader.file,
                reader.what
            );
            assert!(
                reader
                    .carries
                    .iter()
                    .all(|(other, _)| other == call || !other.contains(call)),
                "{}: the needle `{call}` is part of another needle of the same reader, so the \
                 two would count the same site",
                reader.file
            );
        }
    }
}

/// The scan sees what it exists to catch (negative control): a producer by its return type and
/// not by a parameter, a call and not a definition or a longer name, and nothing inside a test
/// module or a comment.
#[test]
fn the_scan_tells_a_producer_from_a_reader_and_a_call_from_a_name() {
    let code = product_code(
        "pub fn decide_reach_portfolio(file: &F) -> ReachOutcome { collect() }\n\
         pub(crate) fn rescue(\n    a: &str,\n) -> Result<(V, ReachOutcome), D> { todo() }\n\
         pub fn derived() -> Result<FaultReachOutcome, String>;\n\
         fn private() -> ReachOutcome { x }\n\
         pub fn reads(outcome: &ReachOutcome) -> Vec<String> { vec![] }\n\
         // pub fn commented() -> ReachOutcome {}\n\
         #[cfg(test)]\nmod tests {\n    pub fn in_a_test() -> ReachOutcome { x }\n}\n",
    );
    assert_eq!(
        producers_in(&code),
        vec![
            "decide_reach_portfolio".to_string(),
            "rescue".to_string(),
            "derived".to_string()
        ]
    );

    let reader = product_code(
        "pub fn decide_reach_portfolio(file: &F) -> ReachOutcome { x }\n\
         fn a() { let o = decide_reach_portfolio(&f); }\n\
         fn b() { let o = m::decide_reach_portfolio(&f); my_decide_reach_portfolio(&f); }\n\
         fn c() { decide_reach_portfolio_with_members(&f); }\n\
         // decide_reach_portfolio(&f)\n\
         #[cfg(test)]\nmod tests {\n    fn t() { decide_reach_portfolio(&f); }\n}\n",
    );
    assert_eq!(
        calls_in(&reader, &["decide_reach_portfolio"]),
        2,
        "the calls in `a` and `b`; not the definition, the longer names, the comment or the test"
    );

    // A producer imported under another name is called by that name; a file's own function of
    // the producer's name that returns no outcome is a namesake, and calling it reads nothing.
    let aliased = product_code(
        "use a::decide_reach_portfolio as d;\n\
         fn decide_reach_portfolio(args: Args) -> Result<(), String> { d(&f); Ok(()) }\n\
         fn verb() { decide_reach_portfolio(args); d2(&f); }\n",
    );
    assert_eq!(
        call_names(&aliased, &["decide_reach_portfolio"]),
        vec!["d".to_string()]
    );
    assert_eq!(
        calls_in(&aliased, &["decide_reach_portfolio"]),
        1,
        "the aliased call; not the namesake's call, nor `d2`"
    );
}

/// The carrier count sees what it exists to catch (negative control): a call site and not the
/// carrier's definition, a comment or a test — so deleting one of two sites changes the count.
#[test]
fn a_carrier_is_counted_per_call_site_and_never_at_its_definition() {
    let two = "fn detail(o: &O) -> String { o.unconfirmed_notes().join(\" \") }\n\
               fn a() { row.notes = detail(&outcome); }\n\
               fn b() { other.notes = detail(&outcome); }\n\
               // detail(&outcome)\n\
               #[cfg(test)]\nmod tests {\n    fn t() { detail(&outcome); }\n}\n";
    assert_eq!(carrier_sites(&product_code(two), "detail(&outcome)"), 2);
    assert_eq!(carrier_sites(&product_code(two), "detail("), 2);
    let one = two.replace("fn b() { other.notes = detail(&outcome); }\n", "");
    assert_eq!(carrier_sites(&product_code(&one), "detail(&outcome)"), 1);
}
