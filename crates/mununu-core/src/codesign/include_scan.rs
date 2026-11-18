//! mununu#994 — the C half: under a `[server] file_root`, the `#include`s of a request's
//! `c-codesign` sources may reach only files under the root.
//!
//! #935 confines every path a verify config NAMES; this is the half the files' CONTENT names.
//! clang resolves an absolute `#include "/etc/x"` (or `<…>` form) whatever its `-I` list says,
//! and a failed compile quotes the included text in the diagnostics the verify report carries, so
//! a C file under the root could read a file outside it back to the caller. A text scan cannot
//! follow a macro, so it accepts only what it can confine: a quoted or angled literal naming a
//! relative path of plain components. The scan follows the includes it can resolve under the root
//! (the including file's directory, then the config's `include_paths`), so a header reached
//! through a confined include is scanned too; a header it cannot find under the root is the
//! compiler's own (a system header) and is not followed. Without a `file_root` the server is a
//! developer's own and takes the sources as given, as it takes paths.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

/// Every `#include` / `#include_next` / `#import` in `content` that could reach outside the
/// directories a compile searches, as `(1-based line, the directive's argument as written)`.
///
/// Accepted: `"…"` or `<…>` around a non-empty relative path of plain components. Reported: an
/// absolute path, a `..` component, a macro-built target, a directive with no argument. Comments
/// are blanked first, so a commented-out directive is not one; a directive must be the first
/// non-blank token of its line, as the preprocessor requires.
pub(crate) fn unconfined_c_includes(content: &str) -> Vec<(usize, String)> {
    directives(&strip_c_comments(content))
        .filter(|(_, _, target)| target.is_none())
        .map(|(line, arg, _)| (line, arg.to_string()))
        .collect()
}

/// The refusals of a `c-codesign` source set: every unconfined directive in `files` and in the
/// headers they reach under `root`, as `path:line: #include arg`. A file that cannot be read is
/// left to the compile (which names it); more than [`MAX_FILES`] reachable files is itself a
/// refusal, named as such.
pub(crate) fn unconfined_c_includes_in_tree(
    files: &[PathBuf],
    include_dirs: &[PathBuf],
    root: &Path,
) -> Vec<String> {
    let Ok(root) = root.canonicalize() else {
        return vec![format!(
            "the server's file_root {} is unusable",
            root.display()
        )];
    };
    let under_root = |p: &Path| p.canonicalize().is_ok_and(|real| real.starts_with(&root));
    let mut out = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: VecDeque<PathBuf> = files.iter().cloned().collect();
    while let Some(file) = queue.pop_front() {
        let key = file.canonicalize().unwrap_or_else(|_| file.clone());
        if !seen.insert(key) {
            continue;
        }
        if seen.len() > MAX_FILES {
            out.push(format!(
                "more than {MAX_FILES} files reachable through `#include` from the config's sources; \
                 refused"
            ));
            break;
        }
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, arg) in unconfined_c_includes(&content) {
            out.push(format!("{}:{line}: #include {arg}", file.display()));
        }
        // Every confined target is followed where the compiler would look under the root: the
        // including file's directory (quoted form only), then the `-I` list.
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let text = strip_c_comments(&content);
        for (target, quoted) in directives(&text).filter_map(|(_, _, t)| t) {
            let candidates = quoted
                .then(|| join_target(&dir, target))
                .into_iter()
                .chain(include_dirs.iter().map(|d| join_target(d, target)));
            if let Some(found) = candidates.filter(|c| c.is_file()).find(|c| under_root(c)) {
                queue.push_back(found);
            }
        }
    }
    out
}

/// `dir` joined with an `#include` target one component at a time, so the path the refusal names
/// uses the host's separator throughout. `Path::join` on the target text keeps its `/` verbatim,
/// so on Windows a header reached through `"inc/board.h"` read `…\fw\inc/board.h` while the same
/// header reached through `-I inc` read `…\fw\inc\board.h` (the OS matrix's Windows lane, red on
/// every `main` push since 21724845). The target is already confined to plain relative
/// components when this runs ([`include_target`]).
fn join_target(dir: &Path, target: &str) -> PathBuf {
    target
        .split('/')
        .filter(|c| !c.is_empty())
        .fold(dir.to_path_buf(), |p, c| p.join(c))
}

/// The bound on files one scan follows; a firmware tree is hundreds, never thousands.
const MAX_FILES: usize = 4_096;

/// The include directives of comment-stripped `text`: `(1-based line, the argument as written,
/// the confined target with whether it was the quoted form — `None` when unconfined)`.
fn directives(text: &str) -> impl Iterator<Item = (usize, &str, Option<(&str, bool)>)> {
    text.lines().enumerate().filter_map(|(idx, raw)| {
        let arg = include_argument(raw)?;
        Some((idx + 1, arg, include_target(arg)))
    })
}

/// The argument of an include directive when `line` is one (`#include`, `#include_next`,
/// `#import`, with any blanks after the `#`), trimmed; `None` for any other line.
fn include_argument(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.strip_prefix('#')?.trim_start();
    let rest = ["include_next", "include", "import"]
        .iter()
        .find_map(|kw| rest.strip_prefix(kw))?;
    // `#includex` is not a directive; `#include"a.h"` is.
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(rest.trim())
}

/// The relative target of a confined include argument (`"a/b.h"` → `a/b.h`, `<c.h>` → `c.h`),
/// with whether it was the quoted form; `None` when the argument is not confined.
fn include_target(arg: &str) -> Option<(&str, bool)> {
    let (target, quoted) = if let Some(inner) = arg.strip_prefix('"') {
        (inner.split_once('"')?.0, true)
    } else if let Some(inner) = arg.strip_prefix('<') {
        (inner.split_once('>')?.0, false)
    } else {
        return None;
    };
    let p = Path::new(target);
    let plain = !target.is_empty()
        && p.components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    plain.then_some((target, quoted))
}

/// `content` with every `//` and `/* … */` comment blanked (newlines kept, so line numbers
/// hold); string literals are kept, so a `//` inside one does not open a comment.
fn strip_c_comments(content: &str) -> String {
    let b = content.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                out.push(b'"');
                i += 1;
                while i < b.len() && b[i] != b'"' && b[i] != b'\n' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        out.push(b[i]);
                        i += 1;
                    }
                    out.push(b[i]);
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                out.extend_from_slice(b"  ");
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
                if i < b.len() {
                    out.extend_from_slice(b"  ");
                    i += 2;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_reports_every_form_that_could_leave_the_search_dirs_and_nothing_else() {
        let src = "#include <stdint.h>\n\
                   #include \"regs.h\"\n\
                   #include \"./hal/gpio.h\"\n\
                   #include \"/etc/hostname\"\n\
                   #include </etc/passwd>\n\
                   #include \"../../outside.h\"\n\
                   #  include_next <../up.h>\n\
                   #import \"/abs.h\"\n\
                   #include HEADER\n\
                   #include\n\
                   // #include \"/commented.h\"\n\
                   /* #include \"/blocked.h\"\n   #include \"/still.h\" */\n\
                   const char *s = \"#include \\\"/in-a-string.h\\\"\";\n\
                   #includex \"/not-a-directive.h\"\n\
                   #define X #include \"/not-first-token.h\"\n";
        let got = unconfined_c_includes(src);
        let expect = vec![
            (4, "\"/etc/hostname\"".to_string()),
            (5, "</etc/passwd>".to_string()),
            (6, "\"../../outside.h\"".to_string()),
            (7, "<../up.h>".to_string()),
            (8, "\"/abs.h\"".to_string()),
            (9, "HEADER".to_string()),
            (10, String::new()),
        ];
        assert_eq!(got, expect);
    }

    #[test]
    fn the_tree_scan_follows_confined_includes_under_the_root_and_names_the_header_that_escapes() {
        let root = tempfile::tempdir().unwrap();
        let fw = root.path().join("fw");
        std::fs::create_dir_all(fw.join("inc")).unwrap();
        std::fs::write(
            fw.join("main.c"),
            "#include <stdint.h>\n#include \"inc/board.h\"\n#include \"hal.h\"\nint main(void){return 0;}\n",
        )
        .unwrap();
        std::fs::write(
            fw.join("inc/board.h"),
            "#pragma once\n#include \"/etc/hostname\"\n#include \"board2.h\"\n",
        )
        .unwrap();
        std::fs::write(fw.join("inc/board2.h"), "#include \"../../../escape.h\"\n").unwrap();
        std::fs::write(fw.join("inc/hal.h"), "#include <board.h>\n").unwrap();
        let files = vec![fw.join("main.c")];
        let incs = vec![fw.join("inc")];
        let got = unconfined_c_includes_in_tree(&files, &incs, root.path());
        assert_eq!(
            got,
            vec![
                format!(
                    "{}:2: #include \"/etc/hostname\"",
                    fw.join("inc").join("board.h").display()
                ),
                format!(
                    "{}:1: #include \"../../../escape.h\"",
                    fw.join("inc").join("board2.h").display()
                ),
            ]
        );
        // The same tree with the two escapes removed is clean, and `hal.h` (reached through the
        // `-I` dir) and `board.h` (reached through `<…>` on the `-I` dir) were both followed.
        std::fs::write(fw.join("inc/board.h"), "#include \"board2.h\"\n").unwrap();
        std::fs::write(fw.join("inc/board2.h"), "#include \"/etc/hostname\"\n").unwrap();
        let got = unconfined_c_includes_in_tree(&files, &incs, root.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("board2.h:1:"), "{got:?}");
        std::fs::write(fw.join("inc/board2.h"), "int x;\n").unwrap();
        assert!(unconfined_c_includes_in_tree(&files, &incs, root.path()).is_empty());
    }

    #[test]
    fn a_header_outside_the_root_is_not_followed() {
        // A confined-looking relative include that resolves (through an `-I` dir outside the
        // root) to a file outside it is not followed: the scan cannot vouch for it, the config's
        // `include_paths` are confined by #935 before this runs, and the compiler's system dirs
        // are the server's own.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sys.h"), "#include \"/etc/hostname\"\n").unwrap();
        std::fs::write(root.path().join("a.c"), "#include <sys.h>\n").unwrap();
        let got = unconfined_c_includes_in_tree(
            &[root.path().join("a.c")],
            &[outside.path().to_path_buf()],
            root.path(),
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn an_unreadable_source_is_left_to_the_compile() {
        let root = tempfile::tempdir().unwrap();
        let got = unconfined_c_includes_in_tree(&[root.path().join("missing.c")], &[], root.path());
        assert!(got.is_empty(), "{got:?}");
    }
}
