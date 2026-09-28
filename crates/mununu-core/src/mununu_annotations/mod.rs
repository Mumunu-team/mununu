//! Source-comment annotation grammar — Document D §D.5.
//!
//! A single tag vocabulary recognised across SystemVerilog,
//! C/C++, TypeScript, Rust, and Python (Document D §D.5.2). The
//! parsers in this module extract `MununuAnnotation` records from
//! either raw SV source text or a yosys `write_json` attribute map;
//! both producers feed the same downstream consumer (the discovery
//! pipeline in Document A §A6).
//!
//! Today the parser supports the SV-side wrappers — `(* mununu_xxx
//! [= "value"] *)` attributes and `// @mununu_xxx value` line
//! comments — plus the attribute-map shape yosys emits. C / TS /
//! Rust / Python wrappers are deliberate follow-up scope per
//! Document D §D.5.4 (ship one language first, then add more).
//!
//! # Tag table (Document D §D.5.1, SV-relevant subset)
//!
//! | Tag                       | Meaning                                                |
//! |---------------------------|--------------------------------------------------------|
//! | `@mununu_blackbox`        | Declare module as black box (no value)                 |
//! | `@mununu_assume <body>`   | Environment assumption                                 |
//! | `@mununu_guarantee <body>`| Guarantee the module provides                          |
//! | `@mununu_interface <uri>` | Reference a stored contract (sidecar path or `contract://`) |
//! | `@mununu_controllable <label>`   | Override default controllability             |
//! | `@mununu_uncontrollable <label>` | Override default controllability             |
//!
//! `@mununu_register` and `@mununu_behavior` are reserved by D.5.1 but
//! out of scope for this module today — they need Document C
//! (codesign) and the template registry, respectively.

use serde::{Deserialize, Serialize};

/// Recognised tag from the §D.5 annotation grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MununuTag {
    /// `@mununu_blackbox` — module is a closed-IP boundary.
    Blackbox,
    /// `@mununu_assume <body>` — environment assumption.
    Assume,
    /// `@mununu_guarantee <body>` — module guarantee.
    Guarantee,
    /// `@mununu_interface <uri>` — corpus / sidecar reference.
    Interface,
    /// `@mununu_controllable <label>` — controllability override.
    Controllable,
    /// `@mununu_uncontrollable <label>` — controllability override.
    Uncontrollable,
    /// `@mununu_isr` (phase L6) — declares the annotated function as
    /// an Interrupt Service Routine. The codesign synthesiser emits
    /// it as a separate top-level automaton composed
    /// asynchronously with the main-thread automaton, matching the
    /// reactive-modules ISR + main-thread interleaving in Doc C
    /// §C.5. Annotation-only; no naming-convention defaults.
    Isr,
    /// `@mununu_predicate <expr>` — an auxiliary abstraction-predicate HINT
    /// for the predicate cube (SV verify-auto). The `<expr>` is a predicate
    /// expression (`reg == value`, `reg == reg`, `reg >= K`) that seeds a cube
    /// dimension even when it does not appear in any property formula. Sound by
    /// monotonicity of predicate abstraction — a hint only refines the cube.
    Predicate,
}

impl MununuTag {
    /// Parse a tag name (without the `@mununu_` prefix) into a
    /// `MununuTag`. Returns `None` for unrecognised tags so the caller
    /// can decide whether to warn or silently skip.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "blackbox" => Some(MununuTag::Blackbox),
            "assume" => Some(MununuTag::Assume),
            "guarantee" => Some(MununuTag::Guarantee),
            "interface" => Some(MununuTag::Interface),
            "controllable" => Some(MununuTag::Controllable),
            "uncontrollable" => Some(MununuTag::Uncontrollable),
            "isr" => Some(MununuTag::Isr),
            "predicate" => Some(MununuTag::Predicate),
            _ => None,
        }
    }

    /// Display name without the `@mununu_` prefix.
    pub fn name(self) -> &'static str {
        match self {
            MununuTag::Blackbox => "blackbox",
            MununuTag::Assume => "assume",
            MununuTag::Guarantee => "guarantee",
            MununuTag::Interface => "interface",
            MununuTag::Controllable => "controllable",
            MununuTag::Uncontrollable => "uncontrollable",
            MununuTag::Isr => "isr",
            MununuTag::Predicate => "predicate",
        }
    }
}

/// A single annotation extracted from source. The `value` is the
/// raw payload — for `Blackbox` this is typically empty; for
/// `Assume` / `Guarantee` / `Interface` / `Controllable` /
/// `Uncontrollable` it is the rest of the tag's body. No further
/// parsing is performed here; downstream consumers (discovery
/// pipeline, HITL UX) interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MununuAnnotation {
    pub tag: MununuTag,
    /// Free-form payload following the tag. Empty for tags like
    /// `Blackbox` that take no value.
    pub value: String,
    /// 1-based source line number when known. Always set by the
    /// SV-source parser; absent when the annotation came from
    /// yosys's attribute map (yosys's `src` attribute is parsed
    /// elsewhere into the enclosing module's source line).
    #[serde(default)]
    pub source_line: Option<u32>,
    /// mununu#550 — an OPTIONAL author-given name, from the parenthesised form
    /// `@mununu_guarantee(my_name) <body>`.
    ///
    /// **Never inferred.** The consumer who asked for this asked for it to be optional rather than
    /// derived, and they were right: a name guessed from the formula text, the source line or the
    /// enclosing module would change under edits that do not change the property, which is the
    /// index-fragility this exists to remove wearing a different hat. Unnamed stays unnamed, and the
    /// property keeps its positional `ann_<tag>_<index>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl MununuAnnotation {
    pub fn new(tag: MununuTag, value: impl Into<String>) -> Self {
        Self {
            tag,
            value: value.into(),
            source_line: None,
            name: None,
        }
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.source_line = Some(line);
        self
    }

    /// Attach an author-given name (mununu#550).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Extract all mununu annotations from raw C / C++ source text.
///
/// The expected vendor convention is **Doxygen** blocks attached to
/// function / struct / typedef declarations:
///
/// ```c
/// /**
///  * Send a byte over the UART. Blocks until the peripheral is ready.
///  *
///  * @mununu_guarantee G(start -> eventually done)
///  * @mununu_assume    G(start -> !reset)
///  */
/// void uart_send(uint8_t byte);
/// ```
///
/// Recognises:
///   - `/** ... @mununu_<tag> [value] ... */` Doxygen blocks
///     (multi-line — the canonical form for vendor-supplied
///     contract annotations). Each `@mununu_*` tag is extracted
///     independently, with `source_line` set to the line the tag
///     itself appears on.
///   - `/* @mununu_<tag> [value] */` single-line block comments.
///   - `// @mununu_<tag> [value]` line comments.
///
/// Verilog-style `(* mununu_* *)` attributes are **not** recognised
/// here — they are SV syntax and would never appear in C source.
/// Use [`extract_from_sv_source`] for SV.
///
/// Unknown tags are silently skipped (the file may carry Doxygen
/// `@param` / `@return` / `@brief` and other non-mununu tags; we
/// ignore them).
pub fn extract_from_c_source(text: &str) -> Vec<MununuAnnotation> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Doxygen block `/** … */` — must check before `/*` since
        // `/**` is a strict prefix of `/*`. We treat `/**/` (empty
        // Doxygen block) as a single-line block.
        if i + 2 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' && bytes[i + 2] == b'*' {
            // Find the closing `*/`. Search starts after the opening
            // `/**` (3 bytes).
            let body_start = i + 3;
            if let Some(end_rel) = find_block_end(&bytes[body_start..]) {
                let body_end = body_start + end_rel;
                let body = &text[body_start..body_end];
                for ann in parse_doxygen_block(body, line_of(text, body_start)) {
                    out.push(ann);
                }
                i = body_end + 2; // past `*/`
                continue;
            } else {
                // Unterminated Doxygen block — give up on the rest of
                // the file rather than mis-extract.
                break;
            }
        }
        // Single-line block comment `/* … */` (NOT Doxygen).
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let body_start = i + 2;
            if let Some(end_rel) = find_block_end(&bytes[body_start..]) {
                let body_end = body_start + end_rel;
                let body = &text[body_start..body_end];
                if let Some(ann) = parse_line_comment_body(body) {
                    out.push(ann.with_line(line_of(text, body_start)));
                }
                i = body_end + 2;
                continue;
            } else {
                break;
            }
        }
        // Line comment `// …`.
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // Find end of line.
            let body_start = i + 2;
            let end_rel = bytes[body_start..]
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(bytes.len() - body_start);
            let body = &text[body_start..body_start + end_rel];
            if let Some(ann) = parse_line_comment_body(body) {
                out.push(ann.with_line(line_of(text, body_start)));
            }
            i = body_start + end_rel;
            continue;
        }
        i += 1;
    }
    out
}

/// Find the byte offset of the closing `*/` inside `body`. Returns
/// `None` if no closing sequence is found (unterminated comment).
fn find_block_end(body: &[u8]) -> Option<usize> {
    let mut j = 0usize;
    while j + 1 < body.len() {
        if body[j] == b'*' && body[j + 1] == b'/' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// 1-based line number of `byte_offset` within `text`.
fn line_of(text: &str, byte_offset: usize) -> u32 {
    text[..byte_offset.min(text.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Parse the body of a `/** … */` Doxygen block. Walks line-by-line,
/// strips the leading ` * ` decoration, and runs each line through
/// the same `@mununu_<tag>` extractor used for `// …` comments.
///
/// Multiple `@mununu_*` tags may appear in the same block. Each
/// tag's `source_line` is set to the line within the file where the
/// tag itself sits (not the block's opening `/**`).
fn parse_doxygen_block(body: &str, block_start_line: u32) -> Vec<MununuAnnotation> {
    let mut out = Vec::new();
    for (line_idx, raw_line) in body.lines().enumerate() {
        // Strip Doxygen's leading ` * ` decoration. We tolerate any
        // number of leading spaces and either `*` or no leading
        // marker (some authors don't add the per-line `*`).
        let trimmed = raw_line.trim_start();
        let cleaned = trimmed
            .strip_prefix('*')
            .map(|s| s.trim_start())
            .unwrap_or(trimmed);
        if let Some(ann) = parse_line_comment_body(cleaned) {
            out.push(ann.with_line(block_start_line + line_idx as u32));
        }
    }
    out
}

/// Extract all mununu annotations from raw SystemVerilog source text.
///
/// Recognises:
///   - `(* mununu_<tag> [= "value"] *)` attribute syntax. The `value`
///     part is optional (e.g. `(* mununu_blackbox *)`).
///   - `// @mununu_<tag> [value]` line-comment syntax. The value is
///     the rest of the line.
///   - `/* @mununu_<tag> [value] */` block-comment syntax — single-
///     line only; multi-line block comments are tolerated but only
///     the first line's content is captured.
///
/// Both forms can coexist in the same file. Unknown tags are
/// silently skipped (they may belong to another tool's
/// `(* synthesis_attribute *)` namespace).
pub fn extract_from_sv_source(text: &str) -> Vec<MununuAnnotation> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        // First, scan for `(* mununu_xxx [= "value"] *)` attributes.
        let mut cursor = 0usize;
        while let Some(start) = line[cursor..].find("(*") {
            let abs_start = cursor + start + 2;
            let Some(end_rel) = line[abs_start..].find("*)") else {
                break;
            };
            let body = &line[abs_start..abs_start + end_rel];
            for ann in parse_attribute_body(body) {
                out.push(ann.with_line(line_no));
            }
            cursor = abs_start + end_rel + 2;
        }
        // Then, scan for `// @mununu_xxx ...` line comments. The `//`
        // form must NOT be inside a `(* ... *)` block we already
        // processed — we approximate by requiring the `//` to start
        // before the first `(*`.
        if let Some(comment_start) = line.find("//")
            && let Some(annot) = parse_line_comment_body(&line[comment_start + 2..])
        {
            out.push(annot.with_line(line_no));
        }
        // Block comments: `/* @mununu_xxx ... */` on a single line.
        if let Some(b_start) = line.find("/*")
            && let Some(b_end_rel) = line[b_start + 2..].find("*/")
        {
            let body = &line[b_start + 2..b_start + 2 + b_end_rel];
            if let Some(annot) = parse_line_comment_body(body) {
                out.push(annot.with_line(line_no));
            }
        }
    }
    out
}

/// Parse the body of a `(* ... *)` attribute block. The body may
/// contain comma-separated attributes (e.g.
/// `(* mununu_blackbox, mununu_assume = "G(req → ack)" *)`).
fn parse_attribute_body(body: &str) -> Vec<MununuAnnotation> {
    let mut out = Vec::new();
    for raw in body.split(',') {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix("mununu_") {
            // Two shapes: `mununu_<tag>` or `mununu_<tag> = "value"`.
            let (tag_name, value) = match rest.find('=') {
                None => (rest.trim(), String::new()),
                Some(eq) => {
                    let tag_name = rest[..eq].trim();
                    let raw_value = rest[eq + 1..].trim();
                    let v = unquote(raw_value).to_string();
                    (tag_name, v)
                }
            };
            if let Some(tag) = MununuTag::from_name(tag_name) {
                out.push(MununuAnnotation::new(tag, value));
            }
        }
    }
    out
}

/// Parse the body of a `// @mununu_xxx ...` line comment. Returns
/// `None` if the body doesn't start with an `@mununu_` tag.
fn parse_line_comment_body(body: &str) -> Option<MununuAnnotation> {
    let trimmed = body.trim_start();
    let rest = trimmed.strip_prefix("@mununu_")?;
    // Either `<tag>` alone, or `<tag> <value>`.
    let (tag_token, value) = match rest.find(|c: char| c.is_whitespace()) {
        None => (rest.trim_end(), String::new()),
        Some(idx) => {
            let tag_token = &rest[..idx];
            let raw_value = rest[idx..].trim();
            (tag_token, unquote(raw_value).to_string())
        }
    };
    // mununu#550 — `<tag>(<name>)` carries an optional author-given name.
    //
    // The name is taken off the TAG token, not out of the value. A separator inside the value (a
    // leading `name:`, say) would collide with the body: a mu-calculus formula can legitimately
    // contain a colon since mununu#565 admitted bit-slice atoms (`wdata[7:0] == 8'hA5`), so any
    // split rule over the value has a real ambiguity. The parentheses have none.
    let (tag_name, name) = split_tag_name(tag_token);
    let ann = MununuTag::from_name(tag_name).map(|tag| MununuAnnotation::new(tag, value))?;
    Some(match name {
        Some(n) => ann.with_name(n),
        None => ann,
    })
}

/// Split `guarantee(p_foo)` into `("guarantee", Some("p_foo"))`; `guarantee` into
/// `("guarantee", None)`.
///
/// A malformed or empty parenthesis group leaves the token intact, so it fails `from_name` and the
/// annotation is skipped with the same diagnostic as any other unrecognised tag — rather than being
/// silently accepted under a truncated name. Before mununu#550 the whole `guarantee(p_foo)` token was
/// handed to `from_name`, which returned `None`, and the annotation vanished with no message at all.
fn split_tag_name(token: &str) -> (&str, Option<&str>) {
    let Some(open) = token.find('(') else {
        return (token, None);
    };
    if !token.ends_with(')') {
        return (token, None);
    }
    let name = token[open + 1..token.len() - 1].trim();
    // A name must be a plain identifier: it becomes a pin a consumer types on a command line.
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if ok {
        (&token[..open], Some(name))
    } else {
        (token, None)
    }
}

/// Strip surrounding double quotes from a string, if present.
fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Extract mununu annotations from a yosys `write_json`
/// `attributes` map. Each entry whose key starts with `mununu_`
/// produces one `MununuAnnotation`; the value is the bitstring or
/// string yosys serialised.
///
/// Yosys serialises bare-flag attributes (like `(* blackbox *)`) as
/// a bitstring ending in `1`. We detect that pattern and emit an
/// empty `value`. Everything else is treated as a literal string
/// value.
pub fn extract_from_yosys_attributes(attrs: &serde_json::Value) -> Vec<MununuAnnotation> {
    let Some(map) = attrs.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in map {
        let Some(rest) = key.strip_prefix("mununu_") else {
            continue;
        };
        let Some(tag) = MununuTag::from_name(rest) else {
            continue;
        };
        let raw = value.as_str().unwrap_or("");
        // Yosys's bare-flag encoding: bitstring of '0's and '1's
        // (the bit width of the attribute's value). For boolean
        // flags this is `...01`. Treat any string that is exclusively
        // '0' / '1' characters as a flag with empty body.
        let value_text = if raw.chars().all(|c| c == '0' || c == '1') && !raw.is_empty() {
            String::new()
        } else {
            raw.to_string()
        };
        out.push(MununuAnnotation::new(tag, value_text));
    }
    out
}

#[cfg(test)]
mod tests {
    /// mununu#550 — the parenthesised name parses, and an unnamed annotation is unchanged.
    #[test]
    fn a_parenthesised_name_is_taken_off_the_tag() {
        let named =
            parse_line_comment_body("@mununu_guarantee(budget_bound) nu X. ((c <= 1) && [] X)")
                .expect("named guarantee parses");
        assert_eq!(named.tag, MununuTag::Guarantee);
        assert_eq!(named.name.as_deref(), Some("budget_bound"));
        assert_eq!(
            named.value, "nu X. ((c <= 1) && [] X)",
            "the name must not be left in the body"
        );

        let plain = parse_line_comment_body("@mununu_guarantee nu X. ((c <= 1) && [] X)")
            .expect("unnamed guarantee still parses");
        assert_eq!(plain.name, None, "unnamed stays unnamed — never inferred");
        assert_eq!(plain.value, "nu X. ((c <= 1) && [] X)");
    }

    /// A formula containing a bit-slice must survive, which is why the name rides on the TAG.
    ///
    /// mununu#565 admitted slice atoms, so a body can legitimately contain `:` — any rule that split
    /// a name out of the VALUE on a separator would have a real ambiguity with `wdata[7:0]`. This
    /// test is the reason the parentheses were chosen over a `name:` prefix.
    #[test]
    fn a_bit_slice_in_the_body_is_not_mistaken_for_a_name() {
        let a = parse_line_comment_body("@mununu_guarantee nu X. ((wdata[7:0] == 165) && [] X)")
            .expect("slice body parses");
        assert_eq!(a.name, None);
        assert_eq!(a.value, "nu X. ((wdata[7:0] == 165) && [] X)");

        let b = parse_line_comment_body(
            "@mununu_guarantee(slice_hold) nu X. ((wdata[7:0] == 165) && [] X)",
        )
        .expect("named slice body parses");
        assert_eq!(b.name.as_deref(), Some("slice_hold"));
        assert_eq!(b.value, "nu X. ((wdata[7:0] == 165) && [] X)");
    }

    /// A malformed name group must NOT be silently accepted under a truncated tag.
    ///
    /// Before mununu#550 the whole `guarantee(x)` token went to `from_name`, returned `None`, and the
    /// annotation vanished with no message. Leaving the token intact keeps that failure VISIBLE —
    /// it is skipped with the normal unrecognised-tag path rather than half-accepted.
    #[test]
    fn a_malformed_name_leaves_the_tag_unrecognised_rather_than_guessing() {
        for bad in [
            "@mununu_guarantee() nu X. (p && [] X)",    // empty
            "@mununu_guarantee(a b) nu X. (p && [] X)", // not an identifier
            "@mununu_guarantee(oops nu X. (p && [] X)", // unclosed
        ] {
            assert!(
                parse_line_comment_body(bad).is_none(),
                "a malformed name must not produce an annotation: {bad}"
            );
        }
    }

    #[test]
    fn the_name_accepts_the_characters_a_pin_needs() {
        for n in ["p_foo", "tier3.bound", "budget-bound", "P0"] {
            let src = format!("@mununu_guarantee({n}) nu X. (p && [] X)");
            assert_eq!(
                parse_line_comment_body(&src)
                    .and_then(|a| a.name)
                    .as_deref(),
                Some(n),
                "{n} is a usable pin name"
            );
        }
    }

    use super::*;
    use serde_json::json;

    // ----- SV source parser -----

    #[test]
    fn extract_recognises_attribute_blackbox_flag() {
        let sv = "(* mununu_blackbox *) module foo();\nendmodule";
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Blackbox);
        assert_eq!(anns[0].value, "");
        assert_eq!(anns[0].source_line, Some(1));
    }

    #[test]
    fn extract_recognises_attribute_with_quoted_value() {
        let sv = r#"(* mununu_guarantee = "G(req -> ack)" *) module foo(); endmodule"#;
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Guarantee);
        assert_eq!(anns[0].value, "G(req -> ack)");
    }

    #[test]
    fn extract_handles_multiple_attributes_in_one_block() {
        let sv = r#"(* mununu_blackbox, mununu_interface = "contract://rtl/x@1" *)"#;
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].tag, MununuTag::Blackbox);
        assert_eq!(anns[1].tag, MununuTag::Interface);
        assert_eq!(anns[1].value, "contract://rtl/x@1");
    }

    #[test]
    fn extract_recognises_line_comment() {
        let sv = "// @mununu_assume G(reset -> idle_within_8_cycles)\nmodule foo();";
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Assume);
        assert_eq!(anns[0].value, "G(reset -> idle_within_8_cycles)");
        assert_eq!(anns[0].source_line, Some(1));
    }

    #[test]
    fn extract_recognises_block_comment() {
        let sv = "/* @mununu_controllable reset_n */ module bar();";
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Controllable);
        assert_eq!(anns[0].value, "reset_n");
    }

    #[test]
    fn extract_ignores_non_mununu_attributes() {
        let sv = r#"(* synthesis, keep="1" *) module foo();"#;
        assert!(extract_from_sv_source(sv).is_empty());
    }

    #[test]
    fn extract_records_line_numbers_per_annotation() {
        let sv =
            "(* mununu_blackbox *)\nmodule foo(input clk);\n// @mununu_guarantee X\nendmodule\n";
        let anns = extract_from_sv_source(sv);
        assert_eq!(anns.len(), 2);
        assert_eq!(anns[0].source_line, Some(1));
        assert_eq!(anns[1].source_line, Some(3));
    }

    #[test]
    fn extract_handles_unknown_tag_gracefully() {
        let sv = "(* mununu_unknownthing = \"x\" *) module foo();";
        // Unknown tag is silently skipped.
        assert!(extract_from_sv_source(sv).is_empty());
    }

    // ----- yosys attribute parser -----

    #[test]
    fn extract_yosys_blackbox_flag() {
        let attrs = json!({
            "blackbox": "00000000000000000000000000000001",
            "mununu_blackbox": "1",
            "mununu_guarantee": "G(awvalid -> awready)"
        });
        let anns = extract_from_yosys_attributes(&attrs);
        assert_eq!(anns.len(), 2);
        let bb = anns.iter().find(|a| a.tag == MununuTag::Blackbox).unwrap();
        assert_eq!(bb.value, "", "bitstring-only attribute → empty value");
        let g = anns.iter().find(|a| a.tag == MununuTag::Guarantee).unwrap();
        assert_eq!(g.value, "G(awvalid -> awready)");
    }

    #[test]
    fn extract_yosys_ignores_non_mununu_keys() {
        let attrs = json!({
            "src": "foo.sv:10",
            "top": "1"
        });
        assert!(extract_from_yosys_attributes(&attrs).is_empty());
    }

    #[test]
    fn extract_yosys_empty_attributes() {
        let attrs = json!({});
        assert!(extract_from_yosys_attributes(&attrs).is_empty());
    }

    // ========================================================================
    // C / Doxygen wrapper tests — Document C task C5, slice 1.
    // ========================================================================

    #[test]
    fn c_extract_single_line_doxygen() {
        let src = "/** @mununu_blackbox */\nvoid foo(void);\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Blackbox);
        assert_eq!(anns[0].value, "");
    }

    #[test]
    fn c_extract_multiline_doxygen_with_multiple_tags() {
        let src = "/**\n\
                   * Send a byte over UART.\n\
                   *\n\
                   * @mununu_guarantee G(start -> eventually done)\n\
                   * @mununu_assume    G(start -> !reset)\n\
                   */\n\
                   void uart_send(uint8_t byte);\n";
        let anns = extract_from_c_source(src);
        // Two tags inside the block.
        assert_eq!(anns.len(), 2);
        let by_tag: std::collections::HashMap<_, _> = anns.iter().map(|a| (a.tag, a)).collect();
        assert_eq!(
            by_tag[&MununuTag::Guarantee].value,
            "G(start -> eventually done)"
        );
        assert_eq!(by_tag[&MununuTag::Assume].value, "G(start -> !reset)");
    }

    #[test]
    fn c_extract_line_comment() {
        let src = "// @mununu_assume G(req -> ack)\nvoid foo(void);\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Assume);
        assert_eq!(anns[0].value, "G(req -> ack)");
    }

    #[test]
    fn c_extract_single_line_non_doxygen_block_comment() {
        let src = "/* @mununu_interface contract://rtl_crypto/aes_ctr@1.0.0 */\nvoid foo(void);\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].tag, MununuTag::Interface);
        assert_eq!(anns[0].value, "contract://rtl_crypto/aes_ctr@1.0.0");
    }

    #[test]
    fn c_extract_mixed_forms_in_one_file() {
        let src = "/** @mununu_blackbox */\n\
                   /* @mununu_interface contract://x/y@1 */\n\
                   // @mununu_guarantee G(a -> b)\n\
                   void foo(void);\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 3);
        let tags: std::collections::HashSet<_> = anns.iter().map(|a| a.tag).collect();
        assert!(tags.contains(&MununuTag::Blackbox));
        assert!(tags.contains(&MununuTag::Interface));
        assert!(tags.contains(&MununuTag::Guarantee));
    }

    #[test]
    fn c_extract_ignores_verilog_style_attributes() {
        // SV syntax `(* mununu_blackbox *)` would be a syntax error
        // in C; we must not accidentally pick it up here.
        let src = "(* mununu_blackbox *)\nvoid foo(void);\n";
        let anns = extract_from_c_source(src);
        assert!(
            anns.is_empty(),
            "Verilog attributes must not be recognised in C"
        );
    }

    #[test]
    fn c_extract_ignores_unrelated_doxygen_tags() {
        let src = "/**\n\
                   * @param byte  the data byte to send\n\
                   * @return 0 on success\n\
                   * @brief Sends a byte\n\
                   */\n\
                   int uart_send(uint8_t byte);\n";
        let anns = extract_from_c_source(src);
        assert!(
            anns.is_empty(),
            "non-mununu Doxygen tags must be ignored, got: {anns:?}"
        );
    }

    #[test]
    fn c_extract_line_numbers_point_to_the_tag_not_the_block_start() {
        let src = "/**\n\
                   * @mununu_guarantee G(start -> eventually done)\n\
                   */\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 1);
        // The Doxygen block starts at line 1; the tag is on line 2.
        assert_eq!(anns[0].source_line, Some(2));
    }

    #[test]
    fn c_extract_doxygen_block_without_leading_stars_is_tolerated() {
        // Some authors write Doxygen without the per-line `*`.
        let src = "/**\n\
                   @mununu_blackbox\n\
                   @mununu_guarantee G(p -> q)\n\
                   */\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 2);
    }

    #[test]
    fn c_extract_unterminated_block_does_not_panic() {
        // Defensive: a malformed file should produce zero annotations
        // (or at most the well-formed ones before the bad block) but
        // never panic.
        let src = "/** @mununu_blackbox\nvoid never_closed(void);\n";
        let _anns = extract_from_c_source(src);
        // No assertion on count — just that we didn't panic.
    }

    #[test]
    fn c_extract_quoted_values_are_unquoted() {
        let src = "// @mununu_interface \"contract://x/y@1\"\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns[0].value, "contract://x/y@1");
    }

    #[test]
    fn c_extract_distinguishes_double_star_from_single_star_block() {
        // Both `/**` and `/*` are valid block-comment openers; the
        // parser must pick the right scanning rule. Doxygen blocks
        // can carry multiple tags; single-`/*` blocks are treated as
        // a single line.
        let src = "/* @mununu_blackbox */\n\
                   /** @mununu_guarantee G(a -> b) */\n";
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 2);
    }

    #[test]
    fn c_extract_realistic_uart_send_header() {
        let src = r#"
            /**
             * @brief  Send a byte over UART.
             * @param  byte the payload
             * @return 0 on success
             *
             * @mununu_assume    G(uart_send -> !uart_in_reset)
             * @mununu_guarantee G(uart_send -> eventually uart_idle)
             */
            int uart_send(uint8_t byte);

            /**
             * @mununu_interface contract://rtl_peripheral/uart_lite@1.0
             */
            extern struct uart_t * const UART;
        "#;
        let anns = extract_from_c_source(src);
        assert_eq!(anns.len(), 3);
        let by_tag: std::collections::HashSet<_> = anns.iter().map(|a| a.tag).collect();
        assert!(by_tag.contains(&MununuTag::Assume));
        assert!(by_tag.contains(&MununuTag::Guarantee));
        assert!(by_tag.contains(&MununuTag::Interface));
    }
}
