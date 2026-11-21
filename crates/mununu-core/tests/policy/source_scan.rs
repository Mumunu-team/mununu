//! mununu#1144 — one scanner for the policy gates that read product code: a `#[cfg(test)]
//! mod … { … }` block is skipped by brace depth (strings, chars and comments honoured), not by
//! truncating the file at its first test attribute. Three gates truncated: `verify_auto.rs` has
//! a test module mid-file followed by product code, and that region held a direct
//! `std::env::var` the env-read gate never saw.

/// The source with every `#[cfg(test)] mod … { … }` block removed (brace-matched, strings and
/// comments skipped), so a file whose first test module sits mid-file is still scanned past it —
/// the sibling test truncates at the first `#[cfg(test)]` and missed seven sites in
/// `symbolic_bitblast.rs` that way (2026-11-20).
pub(crate) fn without_test_modules(src: &str) -> String {
    blank_test_modules(src, false)
}

/// mununu#1144 — [`without_test_modules`], but every skipped block is replaced by as many
/// newlines as it spanned, so a scanner that reports `file:line` keeps the file's numbering.
pub(crate) fn blank_test_modules_keeping_lines(src: &str) -> String {
    blank_test_modules(src, true)
}

fn blank_test_modules(src: &str, keep_lines: bool) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find("#[cfg(test)]") {
        let (before, from_attr) = rest.split_at(at);
        out.push_str(before);
        // The attribute applies to the next item; only a `mod … {` block is skipped whole.
        let after_attr = &from_attr["#[cfg(test)]".len()..];
        let item_start = after_attr.trim_start();
        let Some(brace_rel) = item_start.find('{') else {
            out.push_str("#[cfg(test)]");
            rest = after_attr;
            continue;
        };
        let header = &item_start[..brace_rel];
        if !header.trim_start().starts_with("mod ") && !header.trim_start().starts_with("pub mod ")
        {
            out.push_str("#[cfg(test)]");
            rest = after_attr;
            continue;
        }
        let body = &item_start[brace_rel..];
        let close = matching_brace(body);
        if keep_lines {
            let skipped = from_attr.len() - body.len() + close;
            let newlines = from_attr[..skipped].matches('\n').count();
            out.extend(std::iter::repeat_n('\n', newlines));
        }
        rest = &body[close..];
    }
    out.push_str(rest);
    out
}

/// Byte offset just past the brace that closes the one `text` starts with.
pub(crate) fn matching_brace(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i64;
    let mut i = 0;
    let mut in_str = false;
    let mut in_char = false;
    let mut line_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if line_comment {
            if c == b'\n' {
                line_comment = false;
            }
        } else if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if in_char {
            if c == b'\\' {
                i += 1;
            } else if c == b'\'' {
                in_char = false;
            }
        } else {
            match c {
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => line_comment = true,
                b'"' => in_str = true,
                b'\''
                    if i + 2 < bytes.len() && (bytes[i + 2] == b'\'' || bytes[i + 1] == b'\\') =>
                {
                    in_char = true
                }
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file with a test module in the middle: the product code after it is kept, the module
    /// body is dropped, and the line-keeping form keeps every line number.
    #[test]
    fn x1144_a_mid_file_test_module_is_skipped_and_the_code_after_it_kept() {
        let src = "fn a() { before(); }\n#[cfg(test)]\nmod t {\n    fn b() { inside(); }\n}\n\
                   fn c() { after(); }\n#[cfg(test)]\nfn not_a_module() { kept(); }\n";
        let stripped = without_test_modules(src);
        assert!(
            stripped.contains("before()") && stripped.contains("after()"),
            "{stripped}"
        );
        assert!(!stripped.contains("inside()"), "{stripped}");
        assert!(
            stripped.contains("kept()"),
            "an attribute on a fn is not a module"
        );
        let blanked = blank_test_modules_keeping_lines(src);
        assert_eq!(blanked.lines().count(), src.lines().count(), "{blanked}");
        let after_line = blanked
            .lines()
            .position(|l| l.contains("after()"))
            .map(|i| i + 1);
        assert_eq!(after_line, Some(6), "{blanked}");
        assert!(!blanked.contains("inside()"));
    }

    /// Braces inside strings, chars and comments do not close the module early.
    #[test]
    fn braces_in_strings_chars_and_comments_do_not_end_the_module() {
        let src = "#[cfg(test)]\nmod t {\n    const S: &str = \"}\";\n    const C: char = '}';\n    \
                   // }\n    fn b() { inside(); }\n}\nfn after() {}\n";
        let stripped = without_test_modules(src);
        assert!(!stripped.contains("inside()"), "{stripped}");
        assert!(stripped.contains("fn after()"), "{stripped}");
    }
}
