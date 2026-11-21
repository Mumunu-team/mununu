//! mununu#1121 U2.11 — a remedy names a flag of the verb that produced the message.
//!
//! `docs/policies/failure-paths.md` Rule 5 says "a fix that exists on the surface that produced
//! it". The sibling test (`remedies_name_real_flags_1106`) checks a cited `--flag` against the
//! union of every verb's flags, so a message that names `--merge-clock-domains` (an `sv
//! verify-auto` flag) passes it although `sv cegar` reaches the same refusal, and "`--engine
//! exact-symbolic`" passes although `btor2 cegar` refuses that value. Both were live on
//! 10172037 (the laptop's cadence review of 2026-11-20) and are this test's negative controls.
//!
//! The per-verb half, in three parts the reader can audit:
//! 1. **Which verb has which flag** is read from `crates/mununu-cli/src/main.rs`: each
//!    `struct …Args` block's flags (plus the structs it `#[command(flatten)]`s and the global
//!    `Cli` flags), mapped to a verb through the `enum …Command` variants (`VerifyAuto(Box<…>)`
//!    under `SvCommand` is `sv verify-auto`; a `#[command(name = "…")]` wins).
//! 2. **Which verbs reach a message-producing module** is the hand-kept [`REACH`] table — the
//!    honest part: a module not listed is checked against the union, as before, and a row is
//!    added when a sweep reads the dispatch for it.
//! 3. **Scope in the sentence**: a flag followed, before the next `.` `;` `,` or `)`, by
//!    "on `<verb>`" (or "on the `<group>` verbs") is checked against those verbs only — that is
//!    how a message shared by several verbs names each verb's flag. A bare flag must be on every
//!    verb that reaches the module. A `--engine <value>` is also checked against the values the
//!    verb accepts ([`ENGINE_VALUES`], read from the CLI's runtime refusals).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::remedies_name_real_flags_1106::{
    FOREIGN_FLAGS, prose_literals, repo_root, rust_files_under,
};
use crate::source_scan::without_test_modules;

/// The verbs that reach a message-producing module, by file suffix. Read from the CLI's dispatch
/// on 2026-11-20: the SystemVerilog verbs that lift through yosys (they embed `SvLiftArgs`, or
/// build `YosysOptions` themselves), the verbs that run the predicate-cube lift, the exact engine,
/// the reach portfolio's members, and the single-verb modules.
const REACH: &[(&str, &[&str])] = &[
    (
        "src/adapter/yosys/mod.rs",
        &[
            "sv verify-auto",
            "sv cegar",
            "sv discover",
            "sv verify",
            "sv verify-liveness",
            "sv verify-recoverability",
            "sv check-fsm",
            "sv lint",
            "sv cdc-lint",
            "sv mutate",
            "verify",
        ],
    ),
    (
        "src/adapter/btor2/kmts_lift.rs",
        &[
            "btor2 cegar",
            "sv cegar",
            "btor2 lift-kmts",
            "btor2 verify",
            "sv verify-auto",
            "sv mutate",
        ],
    ),
    (
        "src/adapter/btor2/symbolic_bitblast.rs",
        &[
            "btor2 verify",
            "btor2 verify-liveness",
            "btor2 verify-recoverability",
            "btor2 check-fsm",
            "btor2 game",
            "btor2 inject-fault",
            "sv verify",
            "sv verify-liveness",
            "sv verify-recoverability",
            "sv check-fsm",
            "sv verify-auto",
            "sv mutate",
        ],
    ),
    (
        "src/adapter/btor2/native_bmc.rs",
        &[
            "btor2 verify",
            "btor2 verify-liveness",
            "btor2 verify-recoverability",
            "sv verify",
            "sv verify-liveness",
            "sv verify-recoverability",
            "sv verify-auto",
        ],
    ),
    (
        "src/adapter/reach_portfolio.rs",
        &[
            "btor2 verify",
            "btor2 verify-liveness",
            "btor2 verify-recoverability",
            "sv verify",
            "sv verify-liveness",
            "sv verify-recoverability",
            "sv verify-auto",
        ],
    ),
    ("src/adapter/slang/verify_auto.rs", &["sv verify-auto"]),
    ("src/adapter/xstate/mod.rs", &["verify"]),
    ("src/adapter/tlsf/parser.rs", &["verify"]),
    (
        "src/adapter/aiger/parser.rs",
        &["verify", "btor2 from-aiger"],
    ),
];

/// The `--engine` values a verb accepts, read from the CLI: the cegar verbs refuse
/// `exact-symbolic` and the portfolio modes at run time (`crates/mununu-cli/src/main.rs`,
/// "available on `sv verify-auto` only"); `btor2 verify` takes a different enum.
const ENGINE_VALUES: &[(&str, &[&str])] = &[
    ("btor2 cegar", &["explicit", "symbolic"]),
    ("sv cegar", &["explicit", "symbolic"]),
    (
        "sv verify-auto",
        &[
            "explicit",
            "symbolic",
            "exact-symbolic",
            "portfolio-sequential",
            "portfolio-parallel",
        ],
    ),
    (
        "sv mutate",
        &[
            "explicit",
            "symbolic",
            "exact-symbolic",
            "portfolio-sequential",
            "portfolio-parallel",
        ],
    ),
    ("btor2 verify", &["portfolio", "cube", "ic3"]),
];

const ALL_ENGINE_VALUES: &[&str] = &[
    "explicit",
    "symbolic",
    "exact-symbolic",
    "portfolio-sequential",
    "portfolio-parallel",
    "portfolio",
    "cube",
    "ic3",
];

fn kebab(variant: &str) -> String {
    let mut out = String::new();
    for (i, c) in variant.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The flags each `struct` in `main.rs` declares, and the structs it flattens.
fn struct_flags(src: &str) -> BTreeMap<String, (BTreeSet<String>, Vec<String>)> {
    let mut out: BTreeMap<String, (BTreeSet<String>, Vec<String>)> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut pending_long = false;
    let mut pending_flatten = false;
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("pub struct ")
            .or_else(|| t.strip_prefix("struct "))
            && line.starts_with(|c: char| !c.is_whitespace())
            && rest.ends_with('{')
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            current = Some(name.clone());
            out.entry(name).or_default();
            continue;
        }
        if line == "}" {
            current = None;
            continue;
        }
        let Some(name) = current.as_ref() else {
            continue;
        };
        let entry = out.get_mut(name).expect("entry");
        if t.starts_with("#[arg(") {
            let mut rest = t;
            while let Some(i) = rest.find("long = \"") {
                let after = &rest[i + "long = \"".len()..];
                if let Some(end) = after.find('"') {
                    entry.0.insert(after[..end].to_string());
                }
                rest = after;
            }
            if t.starts_with("#[arg(long") && !t.contains("long = \"") {
                pending_long = true;
            }
            continue;
        }
        if t == "#[command(flatten)]" {
            pending_flatten = true;
            continue;
        }
        if t.starts_with("#[") || t.starts_with("///") || t.starts_with("//") {
            continue;
        }
        // A field line: `name: Type,`
        if let Some((field, ty)) = t.trim_start_matches("pub ").split_once(':') {
            let field = field.trim();
            if pending_long && !field.is_empty() {
                entry.0.insert(field.replace('_', "-"));
            }
            if pending_flatten {
                let ty: String = ty
                    .trim()
                    .trim_start_matches("Box<")
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                entry.1.push(ty);
            }
        }
        pending_long = false;
        pending_flatten = false;
    }
    out
}

fn resolve_flags(
    name: &str,
    structs: &BTreeMap<String, (BTreeSet<String>, Vec<String>)>,
    depth: usize,
) -> BTreeSet<String> {
    let Some((flags, flattened)) = structs.get(name) else {
        return BTreeSet::new();
    };
    let mut out = flags.clone();
    if depth < 4 {
        for f in flattened {
            out.extend(resolve_flags(f, structs, depth + 1));
        }
    }
    out
}

/// Every verb and the flags it takes, read from `crates/mununu-cli/src/main.rs`.
fn verb_flags(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let src = std::fs::read_to_string(root.join("crates/mununu-cli/src/main.rs")).expect("main.rs");
    let structs = struct_flags(&src);
    let global = resolve_flags("Cli", &structs, 0);
    let mut verbs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut group: Option<String> = None;
    let mut explicit_name: Option<String> = None;
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("enum ")
            && rest.ends_with('{')
        {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            group = if name == "Commands" {
                Some(String::new())
            } else {
                name.strip_suffix("Command").map(|g| g.to_ascii_lowercase())
            };
            continue;
        }
        if line == "}" {
            group = None;
            continue;
        }
        let Some(g) = group.as_ref() else { continue };
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#[command(name = \"") {
            explicit_name = rest.split('"').next().map(str::to_string);
            continue;
        }
        if t.starts_with("#[") || t.starts_with("///") {
            continue;
        }
        // `Variant(Args),` or `Variant(Box<Args>),`
        let Some((variant, rest)) = t.split_once('(') else {
            explicit_name = None;
            continue;
        };
        if !variant.chars().all(|c| c.is_ascii_alphanumeric()) || variant.is_empty() {
            explicit_name = None;
            continue;
        }
        let ty: String = rest
            .trim_start_matches("Box<")
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let leaf = explicit_name.take().unwrap_or_else(|| kebab(variant));
        let verb = if g.is_empty() {
            leaf
        } else {
            format!("{g} {leaf}")
        };
        let mut flags = resolve_flags(&ty, &structs, 0);
        flags.extend(global.iter().cloned());
        verbs.insert(verb, flags);
    }
    verbs
}

/// Each `--flag` a prose literal cites, with the text that follows it (to the clause end) and,
/// for `--engine`, the value named.
fn flag_mentions(lit: &str) -> Vec<(String, Option<String>, String)> {
    let bytes = lit.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let boundary = i == 0
            || !(bytes[i - 1].is_ascii_alphanumeric()
                || bytes[i - 1] == b'-'
                || bytes[i - 1] == b'_');
        if boundary && bytes[i] == b'-' && bytes[i + 1] == b'-' && bytes[i + 2].is_ascii_lowercase()
        {
            let name: String = lit[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                .collect();
            let name = name.trim_end_matches('-').to_string();
            let mut after = &lit[i + 2 + name.len()..];
            let mut value = None;
            if name == "engine" {
                let token: String = after
                    .trim_start_matches(['`', ' ', '='])
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                    .collect();
                if ALL_ENGINE_VALUES.contains(&token.as_str()) {
                    value = Some(token.clone());
                    let at = after.find(&token).unwrap_or(0);
                    after = &after[at + token.len()..];
                }
            }
            let end = after.find(['.', ';', ',', ')']).unwrap_or(after.len());
            out.push((name.clone(), value, after[..end].to_string()));
            i += 2 + name.len();
        } else {
            i += 1;
        }
    }
    out
}

/// The verbs (or verb groups) a clause names after "on": the backticked tokens that are verbs.
fn scope_of(tail: &str, verbs: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    let Some(at) = tail.find(" on ") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = &tail[at + 4..];
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let token = &after[..end];
        if verbs.contains_key(token) || verbs.keys().any(|v| v.starts_with(&format!("{token} "))) {
            out.push(token.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn verbs_matching<'a>(
    token: &'a str,
    verbs: &'a BTreeMap<String, BTreeSet<String>>,
) -> Vec<&'a str> {
    if verbs.contains_key(token) {
        vec![token]
    } else {
        verbs
            .keys()
            .filter(|v| v.starts_with(&format!("{token} ")))
            .map(String::as_str)
            .collect()
    }
}

fn accepts_value(verb: &str, flag: &str, value: &str) -> bool {
    if flag != "engine" {
        return true;
    }
    ENGINE_VALUES
        .iter()
        .find(|(v, _)| *v == verb)
        .is_none_or(|(_, values)| values.contains(&value))
}

/// One finding: the flag (with its value when one was named) and the verbs it is not on.
#[derive(Debug, PartialEq, Eq)]
struct Finding {
    flag: String,
    not_on: Vec<String>,
}

/// Checks one literal against the verbs that reach it.
fn check_literal(
    lit: &str,
    reach: &[&str],
    verbs: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Finding> {
    let foreign: BTreeSet<&str> = FOREIGN_FLAGS.iter().map(|(f, _)| *f).collect();
    let mut out = Vec::new();
    for (flag, value, tail) in flag_mentions(lit) {
        if foreign.contains(flag.as_str()) {
            continue;
        }
        let shown = match &value {
            Some(v) => format!("--{flag} {v}"),
            None => format!("--{flag}"),
        };
        let scope = scope_of(&tail, verbs);
        let has = |verb: &str| {
            verbs.get(verb).is_some_and(|f| f.contains(&flag))
                && value
                    .as_deref()
                    .is_none_or(|v| accepts_value(verb, &flag, v))
        };
        let mut not_on = Vec::new();
        if scope.is_empty() {
            for verb in reach {
                if verbs.contains_key(*verb) && !has(verb) {
                    not_on.push((*verb).to_string());
                }
            }
        } else {
            for token in scope {
                let candidates = verbs_matching(&token, verbs);
                if !candidates.iter().any(|v| has(v)) {
                    not_on.push(token.clone());
                }
            }
        }
        if !not_on.is_empty() {
            out.push(Finding {
                flag: shown,
                not_on,
            });
        }
    }
    out
}

#[test]
fn every_flag_a_message_names_is_on_the_verb_that_produced_it() {
    let root = repo_root();
    let verbs = verb_flags(&root);
    assert!(
        verbs.len() > 30,
        "the verb scan found {} verbs — the parser broke: {:?}",
        verbs.len(),
        verbs.keys().collect::<Vec<_>>()
    );
    for (verb, flag) in [
        ("sv verify-auto", "merge-clock-domains"),
        ("sv verify-auto", "engine"),
        ("btor2 cegar", "engine"),
        ("btor2 verify", "timeout-ms"),
        ("sv cegar", "json"),
    ] {
        assert!(
            verbs.get(verb).is_some_and(|f| f.contains(flag)),
            "`{verb}` takes --{flag} on main.rs; the per-verb scan missed it: {:?}",
            verbs.get(verb)
        );
    }
    assert!(
        !verbs["sv cegar"].contains("merge-clock-domains"),
        "sv cegar does not take --merge-clock-domains (the negative control's premise)"
    );

    let mut report = String::new();
    let mut checked_files = 0usize;
    for (suffix, reach) in REACH {
        let path = root.join("crates/mununu-core").join(suffix);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{suffix} exists"));
        checked_files += 1;
        for (line, lit) in prose_literals(&without_test_modules(&src)) {
            for f in check_literal(&lit, reach, &verbs) {
                report.push_str(&format!(
                    "  {suffix}:{line}: names `{}`, which is not on: {}\n",
                    f.flag,
                    f.not_on.join(", ")
                ));
            }
        }
    }
    assert_eq!(checked_files, REACH.len());
    assert!(
        report.is_empty(),
        "a message names a flag (or an --engine value) of another verb (failure-paths Rule 5, \
         \"on the surface that produced it\"):\n{report}\
         Name the verb after the flag (\"`--flag` on `sv verify-auto`\"), name each verb's own \
         flag, or say the other verbs have no such switch."
    );
    // The union check still covers every other module; make sure the files listed here are a
    // subset of what the sibling test scans, so nothing escapes both.
    // mununu#1153 — compared by path components, not display strings: on Windows the scan's
    // paths carry backslashes and a `/`-spelled literal never matched (five red OS-matrix runs).
    let scanned = rust_files_under(&root.join("crates/mununu-core/src/adapter"));
    for (suffix, _) in REACH {
        let tail = Path::new("crates/mununu-core").join(suffix);
        assert!(
            scanned.iter().any(|p| p.ends_with(&tail)),
            "{suffix} is outside the sibling test's scan"
        );
    }
}

/// The scan reaches past a mid-file test module (the sibling test stops at the first one).
#[test]
fn the_scan_skips_every_test_module_and_keeps_the_code_between_them() {
    let src = "fn a() { m(\"first message with six words in it --alpha\"); }\n\
               #[cfg(test)]\nmod t1 { fn x() { let s = \"a } brace in a test string\"; } }\n\
               fn b() { m(\"second message with six words in it --beta\"); }\n\
               #[cfg(test)]\nmod t2 { fn y() {} }\n";
    let kept = without_test_modules(src);
    assert!(
        kept.contains("--alpha") && kept.contains("--beta"),
        "{kept}"
    );
    assert!(
        !kept.contains("brace in a test string") && !kept.contains("fn y()"),
        "{kept}"
    );
    let lits = prose_literals(&kept);
    assert_eq!(lits.len(), 2, "{lits:?}");
}

/// Negative controls — the two live positives of 2026-11-20 as they read on 10172037 — and the
/// scoped sentences the series already writes, which must pass.
#[test]
fn the_per_verb_check_flags_the_two_live_positives_and_passes_a_scoped_sentence() {
    let root = repo_root();
    let verbs = verb_flags(&root);

    let yosys_old = "Model the gating as a clock enable (`if (en) q <= d;` on the root clock), \
                     or pass `--merge-clock-domains` to accept the lockstep model explicitly.";
    let found = check_literal(yosys_old, &["sv verify-auto", "sv cegar"], &verbs);
    assert_eq!(
        found,
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec!["sv cegar".into()],
        }],
        "a bare flag of one verb in a message two verbs reach"
    );

    let kmts_old = "the exact engine reads the memory itself when the cone fits its bit cap — \
                    decide the row with `--engine exact-symbolic`, or supply an `init` without \
                    `sort`";
    let found = check_literal(kmts_old, &["btor2 cegar", "sv verify-auto"], &verbs);
    assert_eq!(
        found,
        vec![Finding {
            flag: "--engine exact-symbolic".into(),
            not_on: vec!["btor2 cegar".into()],
        }],
        "an --engine value the cegar verbs refuse"
    );

    let scoped = "`--merge-clock-domains` on `sv verify-auto` accepts the lockstep model \
                  explicitly; the other verbs have no such switch.";
    assert!(
        check_literal(scoped, &["sv verify-auto", "sv cegar"], &verbs).is_empty(),
        "a flag scoped to its verb passes"
    );
    let group = "`MUNUNU_NATIVE_QUERY_RLIMIT` raises it on this verb (`--timeout-ms` scales the \
                 nominal allowance on the `btor2` verbs, which `sv verify-auto` does not take)";
    assert!(
        check_literal(group, &["sv verify-auto"], &verbs).is_empty(),
        "a flag scoped to a verb group passes"
    );
    let engines = "the predicate-cube engine (`--engine explicit` on `sv verify-auto` and `sv \
                   mutate`; `--engine cube` on `btor2 verify`), or the SAT members";
    assert!(
        check_literal(engines, &["btor2 verify", "sv verify-liveness"], &verbs).is_empty(),
        "each --engine value scoped to the verbs that accept it passes"
    );
    let wrong = "decide it with `--engine cube` on `sv verify-auto`";
    assert_eq!(
        check_literal(wrong, &["sv verify-auto"], &verbs),
        vec![Finding {
            flag: "--engine cube".into(),
            not_on: vec!["sv verify-auto".into()],
        }],
        "a value scoped to a verb that rejects it is flagged"
    );
}
