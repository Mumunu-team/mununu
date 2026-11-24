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
//!
//! **A group in the scope** (mununu#1203). "On the `sv` verbs" says every `sv` verb the reader
//! may be on, so the flag must be on every verb of the group that reaches the module; the finding
//! lists the ones that lack it. A sentence that cannot say that says "on the `sv` verbs that
//! take it", and then one verb of the group that reaches the module must take the flag. When no
//! verb of the group reaches the module the sentence points at another surface ("on the `btor2`
//! verbs, which `sv verify-auto` does not take"), and one verb of the group must take the flag.
//! Until #1203 a group passed whenever any one of its verbs took the flag, reaching or not.
//!
//! **A verb the CLI does not have** (mununu#1203). A token after "on" that reads as a verb of a
//! group (`sv verify-liveness-all`) and is none is a finding: the scope used to drop it, so a
//! sentence went on naming a verb after the verb was merged into another. For the same reason
//! every verb of [`REACH`] and of [`ENGINE_VALUES`] is a verb of the CLI: a row that names a
//! retired verb is skipped without a word.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::remedies_name_real_flags_1106::{
    FOREIGN_FLAGS, prose_literals, repo_root, rust_files_under,
};
use crate::source_scan::without_test_modules;

/// The verbs that reach a message-producing module, by file suffix. Read from the CLI's dispatch
/// on 2026-11-20: the SystemVerilog verbs that lift through yosys (they embed `SvLiftArgs`, or
/// build `YosysOptions` themselves), the verbs that run the predicate-cube lift, the exact engine,
/// the reach portfolio's members, and the single-verb modules. Re-read on 2026-11-24 (#1203):
/// `sv verify` left the rows (the CLI has had no such verb since 0.5) and `sv
/// emit-btor2-per-module`, which lifts through yosys, joined the first.
const REACH: &[(&str, &[&str])] = &[
    (
        "src/adapter/yosys/mod.rs",
        &[
            "sv verify-auto",
            "sv cegar",
            "sv discover",
            "sv emit-btor2-per-module",
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
            "sv verify-liveness",
            "sv verify-recoverability",
            "sv verify-auto",
        ],
    ),
    ("src/adapter/slang/verify_auto.rs", &["sv verify-auto"]),
    // The report text `verify_auto.rs` produces, moved out of it: the same callers, the same verb.
    ("src/adapter/slang/verify_auto_text.rs", &["sv verify-auto"]),
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
    // A sentence the source wraps is the same sentence: a `\` continuation leaves its newline
    // and its indentation in the literal as the scan reads it, and a verb name or a phrase
    // split there was not recognised (two include-directory messages were findings for it).
    let lit = &lit.split_whitespace().collect::<Vec<_>>().join(" ");
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

/// The backticked tokens a clause names after "on" that read as a verb of a group — two words,
/// the first a group of the CLI (`sv verify-liveness-all`) — and are no verb: retired, or
/// misspelt. [`scope_of`] leaves them out, so nothing else would say they are there.
fn verbs_named_that_the_cli_lacks(
    tail: &str,
    verbs: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let Some(at) = tail.find(" on ") else {
        return Vec::new();
    };
    tail[at + 4..]
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| !verbs.contains_key(*token))
        .filter(|token| {
            let mut words = token.split(' ');
            let (group, leaf, more) = (words.next(), words.next(), words.next());
            more.is_none()
                && leaf.is_some_and(|l| l.starts_with(|c: char| c.is_ascii_lowercase()))
                && group.is_some_and(|g| verbs.keys().any(|v| v.starts_with(&format!("{g} "))))
        })
        .map(str::to_string)
        .collect()
}

/// What a clause writes after a group to keep it to the group's verbs that have the flag ("on
/// the `sv` verbs that take it"). The sentence then says itself that not every verb does.
const TO_ITS_TAKERS: &str = "verbs that take it";

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
        let mut not_on = verbs_named_that_the_cli_lacks(&tail, verbs);
        if scope.is_empty() {
            for verb in reach {
                if verbs.contains_key(*verb) && !has(verb) {
                    not_on.push((*verb).to_string());
                }
            }
        } else {
            let to_its_takers = tail.contains(TO_ITS_TAKERS);
            for token in scope {
                if verbs.contains_key(&token) {
                    if !has(&token) {
                        not_on.push(token);
                    }
                    continue;
                }
                // A group (mununu#1203): held to its verbs that reach this module.
                let prefix = format!("{token} ");
                let reaching: Vec<&str> = reach
                    .iter()
                    .copied()
                    .filter(|v| v.starts_with(&prefix) && verbs.contains_key(*v))
                    .collect();
                if reaching.is_empty() {
                    // The sentence points at another surface: the flag exists there.
                    if !verbs_matching(&token, verbs).iter().any(|v| has(v)) {
                        not_on.push(token);
                    }
                } else if to_its_takers {
                    if !reaching.iter().any(|v| has(v)) {
                        not_on.push(token);
                    }
                } else {
                    not_on.extend(
                        reaching
                            .iter()
                            .filter(|v| !has(v))
                            .map(|v| (*v).to_string()),
                    );
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

    for (suffix, reach) in REACH {
        for verb in *reach {
            assert!(
                verbs.contains_key(*verb),
                "REACH lists `{verb}` for {suffix} and the CLI has no such verb: the row is \
                 skipped for it without a word. Remove it, or name the verb it became"
            );
        }
    }
    for (verb, _) in ENGINE_VALUES {
        assert!(
            verbs.contains_key(*verb),
            "ENGINE_VALUES lists `{verb}` and the CLI has no such verb: its row is read for no \
             message. Remove it, or name the verb it became"
        );
    }

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
         flag, or say the other verbs have no such switch. A group (\"on the `sv` verbs\") \
         says every verb of the group that reaches the module; \"on the `sv` verbs that take \
         it\" says some do. A verb listed here that the CLI does not have was renamed or merged \
         into another: name the verb it became."
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

/// The verbs each scoped mention of `--<flag>` names after "on", one set per mention, with the
/// line of its literal. A bare mention (no scope) is not listed.
fn verbs_named_after(
    flag: &str,
    code: &str,
    verbs: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<(usize, BTreeSet<String>)> {
    let mut out = Vec::new();
    for (line, lit) in prose_literals(code) {
        for (named, _, tail) in flag_mentions(&lit) {
            let scope = scope_of(&tail, verbs);
            if named == flag && !scope.is_empty() {
                out.push((line, scope.into_iter().collect()));
            }
        }
    }
    out
}

/// mununu#1181 — a clock refusal names the verbs on which `--merge-clock-domains` accepts the
/// merged model. The test above proves each verb it names takes the flag; this one proves none
/// is left out, so a verb that gains the flag fails here until the sentence names it. The
/// sentence is read on surfaces without the flag too (`sv discover`, `sv cegar`, `verify`), where
/// the list is the whole remedy. `sv cdc-lint` takes the flag and is not named: the lint reads
/// the clock domains with or without it, so it never refuses.
#[test]
fn the_clock_refusal_names_every_verb_that_takes_the_merge_flag() {
    const FLAG: &str = "merge-clock-domains";
    const NEVER_REFUSES: &str = "sv cdc-lint";
    let root = repo_root();
    let verbs = verb_flags(&root);
    assert!(
        verbs.get(NEVER_REFUSES).is_some_and(|f| f.contains(FLAG)),
        "`{NEVER_REFUSES}` takes --{FLAG} (the premise of leaving it out)"
    );
    let expected: BTreeSet<String> = verbs
        .iter()
        .filter(|(verb, flags)| flags.contains(FLAG) && verb.as_str() != NEVER_REFUSES)
        .map(|(verb, _)| verb.clone())
        .collect();
    assert!(
        expected.contains("sv verify-auto") && expected.contains("sv lint"),
        "the verb scan lost the flag: {expected:?}"
    );

    let suffix = "src/adapter/yosys/mod.rs";
    let src = std::fs::read_to_string(root.join("crates/mununu-core").join(suffix))
        .unwrap_or_else(|_| panic!("{suffix} exists"));
    let named = verbs_named_after(FLAG, &without_test_modules(&src), &verbs);
    assert!(
        !named.is_empty(),
        "{suffix}: no message names the verbs --{FLAG} is on — the clock refusal moved or the \
         scan lost it"
    );
    for (line, scope) in &named {
        assert_eq!(
            scope, &expected,
            "{suffix}:{line}: left, the verbs the message names after `--{FLAG}`; right, the \
             verbs that take the flag (`{NEVER_REFUSES}` aside)"
        );
    }

    // Negative control: a sentence that leaves verbs out is seen as naming fewer.
    let short = "fn f() { m(\"the flag `--merge-clock-domains` on `sv verify-auto` / `sv lint` \
                 accepts the lockstep model explicitly\"); }\n";
    let found = verbs_named_after(FLAG, short, &verbs);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].1,
        ["sv lint", "sv verify-auto"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert_ne!(found[0].1, expected);
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

/// mununu#1203 — a group in the scope ("on the `sv` verbs") is held to every verb of the group
/// that reaches the module. Negative control: the sentence that was the blocker of #1200's
/// review, which the check passed because one `sv` verb takes the flag.
#[test]
fn a_flag_named_on_a_verb_group_is_on_every_verb_of_the_group_that_reaches() {
    let root = repo_root();
    let verbs = verb_flags(&root);
    const REACHING: &[&str] = &["sv verify-auto", "sv cegar", "sv discover", "verify"];
    for (verb, takes) in [
        ("sv verify-auto", true),
        ("sv cegar", false),
        ("sv discover", false),
    ] {
        assert_eq!(
            verbs
                .get(verb)
                .is_some_and(|f| f.contains("merge-clock-domains")),
            takes,
            "`{verb}` and --merge-clock-domains (the premise of the control)"
        );
    }

    let group = "`--merge-clock-domains` on the `sv` verbs accepts the lockstep model explicitly";
    assert_eq!(
        check_literal(group, REACHING, &verbs),
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec!["sv cegar".into(), "sv discover".into()],
        }],
        "a group is every verb of it that reaches the module, and the finding names the ones \
         without the flag"
    );
    assert!(
        check_literal(group, &["sv verify-auto", "sv lint", "verify"], &verbs).is_empty(),
        "every `sv` verb that reaches takes the flag; `verify` is not of the group"
    );

    let to_its_takers = "`--merge-clock-domains` on the `sv` verbs that take it accepts the \
                         lockstep model explicitly";
    assert!(
        check_literal(to_its_takers, REACHING, &verbs).is_empty(),
        "the sentence says itself that not every `sv` verb takes the flag"
    );
    assert_eq!(
        check_literal(to_its_takers, &["sv cegar", "sv discover"], &verbs),
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec!["sv".into()],
        }],
        "no `sv` verb that reaches takes it: \"that take it\" names nothing the reader can use"
    );

    // The same sentences as the source can wrap them: the literal the scan reads keeps the
    // newline and the indentation of the continuation.
    let wrapped_phrase = "`--merge-clock-domains` on the `sv` verbs that\n            take it \
                          accepts the lockstep model explicitly";
    assert!(
        check_literal(wrapped_phrase, REACHING, &verbs).is_empty(),
        "a phrase wrapped by the source is the same phrase"
    );
    let wrapped_verb = "`--merge-clock-domains` on `sv\n            verify-auto` accepts the \
                        lockstep model explicitly";
    assert!(
        check_literal(wrapped_verb, REACHING, &verbs).is_empty(),
        "a verb name wrapped by the source is the same verb"
    );

    let elsewhere = "`--timeout-ms` scales the nominal allowance on the `btor2` verbs";
    assert!(
        check_literal(elsewhere, &["sv verify-auto"], &verbs).is_empty(),
        "no `btor2` verb reaches: the sentence points at another surface, where the flag exists"
    );
    let nowhere = "`--merge-clock-domains` on the `btor2` verbs accepts the lockstep model";
    assert_eq!(
        check_literal(nowhere, &["sv verify-auto"], &verbs),
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec!["btor2".into()],
        }],
        "another surface that has no such flag"
    );
}

/// mununu#1203 — a sentence that names a verb the CLI does not have is a finding. Negative
/// control: the clock refusal as it read on 18aed7f7, which went on naming `sv
/// verify-liveness-all` after 0.5 merged that verb into `sv verify-liveness --response`.
#[test]
fn a_verb_the_cli_does_not_have_is_a_finding() {
    let root = repo_root();
    let verbs = verb_flags(&root);
    const RETIRED: &str = "sv verify-liveness-all";
    assert!(
        !verbs.contains_key(RETIRED) && verbs.contains_key("sv verify-liveness"),
        "the premise of the control: `{RETIRED}` is no verb and `sv verify-liveness` is one"
    );

    let old = "`--merge-clock-domains` on `sv verify-auto` / `sv verify-liveness` / \
               `sv verify-liveness-all` / `sv verify-recoverability` (API \
               `lift.merge_clock_domains`) accepts the lockstep model explicitly";
    assert_eq!(
        check_literal(old, &["sv verify-auto", "sv cegar"], &verbs),
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec![RETIRED.into()],
        }]
    );
    let alone = "pass `--merge-clock-domains` on `sv verify-liveness-all`";
    assert_eq!(
        check_literal(alone, &["sv verify-liveness-all"], &verbs),
        vec![Finding {
            flag: "--merge-clock-domains".into(),
            not_on: vec![RETIRED.into()],
        }],
        "the only verb named, and the only one said to reach"
    );

    // What is not a verb of a group is left alone: a key, an API field, a verb with its flag.
    for clean in [
        "`--top` on `sv verify-auto` and `top` in a `mununu verify` config",
        "`--no-input-state` on `sv verify-auto` (API `input_state: false`)",
        "`--top` on `sv verify-auto --plan-only`",
        "`--top` on `sv verify-auto` or the `SV_TOP` of the design",
    ] {
        assert!(
            check_literal(clean, &["sv verify-auto"], &verbs).is_empty(),
            "{clean}"
        );
    }
}
