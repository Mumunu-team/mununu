//! mununu#1209 — no message carries the indentation of a source line.
//!
//! A string literal wrapped with a line continuation (a backslash at the end of the source line)
//! reads as one sentence: the compiler drops the newline and the next line's indentation. When
//! the backslash is lost and the two lines are joined — a scripted edit, a conflict resolved by
//! hand — the literal still compiles, and the indentation stays in the text. The message then
//! reaches the terminal and the JSON report with a hole in the middle of a sentence. Seven
//! literals in four files had shipped that way; no test read them, because a test that pinned
//! the wording would be the `prose-anchored-test` class.
//!
//! The gate reads the source instead: in the non-test code under `crates/*/src`, no string
//! literal holds a run of [`GAP`] or more spaces between two characters of one output line. Two
//! shapes are not a gap between two words and are passed by construction: a run that follows a
//! newline (a real one inside the literal, or an emitted `\n`) is that line's indentation, and
//! a run at either end of the literal is padding. What is left is text laid out in columns on
//! purpose — a `key:    value` block, embedded SystemVerilog — and each such site is named in
//! [`ALIGNED`] by file and item, with what it lays out. An entry that no longer matches fails
//! the test, so the list cannot outlive the code it excuses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::remedies_name_real_flags_1106::{repo_root, rust_files_under, string_literals};

/// The shortest run that is flagged. rustfmt indents the second line of a wrapped literal with
/// its argument, eight or more columns inside a function body, so a lost continuation leaves at
/// least this many spaces; a sentence typed by hand has one or two.
const GAP: usize = 6;

/// Literals that hold a run of spaces on purpose: `(file, item, what the item lays out)`. The
/// item is the function the literal sits in, or the file-level `const` / `static` it defines.
const ALIGNED: &[(&str, &str, &str)] = &[
    (
        "crates/mununu-cli/src/main.rs",
        "run_symbolic_cegar_cli",
        "a summary block, one `key:   value` per line",
    ),
    (
        "crates/mununu-cli/src/main.rs",
        "run_cegar_cli",
        "a summary block, one `key:   value` per line",
    ),
    (
        "crates/mununu-cli/src/main.rs",
        "print_soundness_report",
        "a summary block, one `key:   value` per line",
    ),
    (
        "crates/mununu-cli/src/main.rs",
        "render_licence_status",
        "a summary block, one `key:   value` per line",
    ),
    (
        "crates/mununu-core/src/adapter/slang/prim_stubs.rs",
        "PRIM_SPARSE_FSM_FLOP",
        "SystemVerilog source with its declarations in columns",
    ),
    (
        "crates/mununu-core/src/adapter/slang/prim_stubs.rs",
        "PRIM_FLOP",
        "SystemVerilog source with its declarations in columns",
    ),
    (
        "crates/mununu-core/src/adapter/slang/verify_auto.rs",
        "dump_prepared_model",
        "the header of the BTOR2 dump: comment lines indented under their heading",
    ),
    (
        "crates/mununu-core/src/adapter/systemverilog/typedef_extract.rs",
        "TYPEDEF_ENUM_RE",
        "a verbose-mode regex with a comment column",
    ),
    (
        "crates/mununu-core/src/api/ops.rs",
        "template_ref_sidecar",
        "emitted CTXDSL: a closing brace indented after a placeholder that ends its own line",
    ),
    (
        "crates/mununu-core/src/codesign/cmsis_emit.rs",
        "emit_cmsis_header",
        "a C `#define` with its value in a column",
    ),
    (
        "crates/mununu-extract/src/main.rs",
        "run_ast",
        "help text whose wrapped lines hang under the first, written as `\\x20` and spaces",
    ),
];

/// One flagged run: where it is and how wide.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Gap {
    item: String,
    line: usize,
    width: usize,
}

/// The runs of [`GAP`] or more spaces between two characters of one output line of a literal, as
/// `(lines below the literal's first, width)`. `raw` is the literal as the source writes it.
fn gaps_in(raw: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != ' ' {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        if i - start < GAP || start == 0 || i == chars.len() {
            continue;
        }
        let (before, after) = (chars[start - 1], chars[i]);
        if before.is_whitespace() || after.is_whitespace() {
            continue;
        }
        // An emitted newline — `\n` whose backslash is not itself escaped — starts an output
        // line, and the run after it is that line's indentation.
        let backslashes = chars[..start - 1]
            .iter()
            .rev()
            .take_while(|c| **c == '\\')
            .count();
        if before == 'n' && backslashes % 2 == 1 {
            continue;
        }
        let below = chars[..start].iter().filter(|c| **c == '\n').count();
        out.push((below, i - start));
    }
    out
}

/// The name a line declares: a function at any depth, or a `const` / `static` at file level (one
/// declared inside a function body does not own the literals that follow it).
fn declared_name(line: &str) -> Option<&str> {
    let file_level = !line.starts_with([' ', '\t']);
    let mut words = line.split_whitespace().peekable();
    while words
        .next_if(|w| matches!(*w, "pub" | "pub(crate)" | "pub(super)" | "async" | "unsafe"))
        .is_some()
    {}
    let mut kind = words.next()?;
    if kind == "const" && words.peek() == Some(&"fn") {
        kind = words.next()?;
    }
    let name = match kind {
        "fn" => words.next()?,
        "const" | "static" if file_level => words.next()?,
        _ => return None,
    };
    let end = name
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(name.len());
    (end > 0).then(|| &name[..end])
}

/// Every flagged run of a source file, each under the item its literal opens in.
fn gaps_of(src: &str) -> Vec<Gap> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (line, raw) in string_literals(src) {
        let found = gaps_in(&raw);
        if found.is_empty() {
            continue;
        }
        let item = lines[..line.min(lines.len())]
            .iter()
            .rev()
            .find_map(|l| declared_name(l))
            .unwrap_or("(no item above)")
            .to_string();
        for (below, width) in found {
            out.push(Gap {
                item: item.clone(),
                line: line + below,
                width,
            });
        }
    }
    out
}

/// A path under the repository with `/` between its components on every OS.
fn rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn no_message_keeps_the_indentation_of_a_wrapped_source_line() {
    let root = repo_root();
    let mut crates: Vec<_> = std::fs::read_dir(root.join("crates"))
        .expect("the crates directory")
        .flatten()
        .map(|e| e.path().join("src"))
        .filter(|src| src.is_dir())
        .collect();
    crates.sort();

    let allowed: BTreeSet<(&str, &str)> = ALIGNED.iter().map(|(f, i, _)| (*f, *i)).collect();
    assert_eq!(
        allowed.len(),
        ALIGNED.len(),
        "ALIGNED names one (file, item) twice"
    );
    let mut matched: BTreeSet<(String, String)> = BTreeSet::new();
    let mut report: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut scanned = 0usize;
    for dir in &crates {
        for file in rust_files_under(dir) {
            let src = std::fs::read_to_string(&file).expect("a source file");
            let name = rel(&root, &file);
            scanned += 1;
            for gap in gaps_of(&src) {
                if allowed.contains(&(name.as_str(), gap.item.as_str())) {
                    matched.insert((name.clone(), gap.item));
                } else {
                    report.entry(name.clone()).or_default().push(format!(
                        "    line {}, in `{}`: {} spaces",
                        gap.line, gap.item, gap.width
                    ));
                }
            }
        }
    }
    assert!(
        scanned > 100,
        "{scanned} source files under crates/*/src — the walk broke"
    );

    let mut text = String::new();
    for (file, sites) in &report {
        text.push_str(&format!("  {file}\n{}\n", sites.join("\n")));
    }
    assert!(
        text.is_empty(),
        "a string literal holds a run of {GAP} or more spaces between two words of one line:\n\
         {text}\
         If the run is the indentation of a wrapped source line (a lost `\\` continuation), make \
         it one space, or put the backslash back at the end of the line. If the text is laid out \
         in columns on purpose, add its (file, item) to ALIGNED with what it lays out."
    );

    let stale: Vec<String> = ALIGNED
        .iter()
        .filter(|(f, i, _)| !matched.contains(&(f.to_string(), i.to_string())))
        .map(|(f, i, _)| format!("  {f} — `{i}`"))
        .collect();
    assert!(
        stale.is_empty(),
        "ALIGNED excuses an item that holds no such literal any more (renamed, moved or \
         re-worded) — remove or re-point the entry:\n{}",
        stale.join("\n")
    );
}

/// The control: the scan flags a literal whose continuation was lost and a column layout, under
/// the item each sits in, and passes a wrapped literal, an emitted line's indentation, padding
/// at the ends of a literal and a test module.
#[test]
fn the_scan_tells_a_lost_continuation_from_a_line_the_program_indents() {
    let probe = r##"fn joined() -> &'static str {
    "the budget expired.                  The verdict is not known"
}
fn wrapped() -> &'static str {
    "the budget expired. \
                  The verdict is not known"
}
fn emitted(a: &str) -> String {
    format!("context c {{\n      over {a};\n}}\n")
}
fn escaped_backslash() -> &'static str {
    "a path ends in \\n      and the text goes on"
}
fn padded() -> &'static str {
    "        starts and ends with spaces        "
}
fn table() {
    const WIDTH: usize = 3;
    println!("  key:          {WIDTH}");
}
pub(crate) const HEADER: &str = "name            value";
#[cfg(test)]
mod tests {
    const T: &str = "a test may      align what it likes";
}
"##;
    let found: Vec<(String, usize, usize)> = gaps_of(probe)
        .into_iter()
        .map(|g| (g.item, g.line, g.width))
        .collect();
    assert_eq!(
        found,
        vec![
            ("joined".to_string(), 2, 18),
            ("escaped_backslash".to_string(), 12, 6),
            ("table".to_string(), 19, 10),
            ("HEADER".to_string(), 21, 12),
        ]
    );
}
