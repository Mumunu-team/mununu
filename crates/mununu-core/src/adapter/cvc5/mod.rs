//! R.5 Item 3 sub-item 3.1 (2026-06-03) — CVC5 binary discovery
//! + version probe.
//!
//! CVC5 (https://cvc5.github.io/) is an OPTIONAL runtime
//! dependency invoked via subprocess for Craig interpolation
//! queries on the R.5 CEGAR loop's `PredicateSource::CraigInterpolation`
//! path. The subprocess pattern mirrors the existing
//! `locate_yosys` / `locate_sv2v` helpers in `adapter/yosys/mod.rs`
//! (the Rust-side `cvc5` / `cvc5-sys` crates are deliberately NOT
//! used — they're younger than `z3-rs` and bring a heavy build
//! dep chain: GMP, CLN, antlr3-c, several C++ deps).
//!
//! When CVC5 is not found at runtime, callers (e.g.
//! `cegar_refine_loop`) fall back to the WP heuristic and emit
//! an [`crate::adapter::AdapterWarning`] documenting the missing
//! dep. CVC5 is never a hard requirement; the rest of mununu
//! functions identically without it.
//!
//! **Discovery order** (mirrors `adapter/yosys/mod.rs` pattern):
//! 1. `MUNUNU_CVC5_PATH` env var (explicit override).
//! 2. `cvc5` on `$PATH` (the standard install location for
//!    Homebrew `brew install cvc5`, Debian `apt install cvc5`,
//!    or the upstream pre-built binaries).
//!
//! **Sub-item 3.1 scope**: discovery + version probe ONLY. The
//! SMT-LIB query construction (sub-item 3.2), subprocess
//! invocation + interpolant parsing (sub-item 3.3), CEGAR wiring
//! (sub-item 3.4), and CLI / sidecar surface (sub-item 3.5)
//! ship in subsequent commits.

use std::path::PathBuf;
use std::time::Duration;

use crate::adapter::AdapterError;
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};

/// R.5 Item 3 sub-item 3.2 (2026-06-03) — default bit-width
/// assumed for register-equality predicates when constructing
/// the SMT-LIB interpolation query. Picked to match the
/// `u64`-typed `PredicateSpec::value` and the typical BTOR2
/// register widths we lift (8/16/32-bit).
///
/// Sub-item 3.4 will plumb the actual per-register bit-width
/// from the BTOR2 file into the query so wider registers (64-
/// bit timestamps, 128-bit AES state) get the right
/// declarations.
pub const DEFAULT_BV_WIDTH: u32 = 32;

/// R.5 Item 3 sub-item 3.1 (2026-06-03) — handle to a discovered
/// CVC5 binary + its parsed version string.
///
/// The version is captured at discovery time; it's diagnostic
/// only (mununu does not gate on version ≥ X). Returned in
/// adapter warnings + tracing logs so users can confirm which
/// binary is being invoked.
#[derive(Debug, Clone)]
pub struct Cvc5Bin {
    /// Resolved path to the CVC5 binary. May be a bare name
    /// (`cvc5`) if discovered on `$PATH`, or an absolute path if
    /// `MUNUNU_CVC5_PATH` is set.
    pub path: PathBuf,
    /// Parsed version string (e.g. `1.0.5`). `"<unparseable>"`
    /// when the `cvc5 --version` output doesn't match the
    /// expected format — discovery still succeeds.
    pub version: String,
}

/// R.5 Item 3 sub-item 3.1 (2026-06-03) — locate a usable CVC5
/// binary + probe its version. Returns `Ok(Cvc5Bin)` on success.
/// Returns `Err(AdapterError { kind: UnsupportedConstruct, ... })`
/// when the binary is absent or the version probe fails — callers
/// are expected to fall back gracefully (see sub-item 3.4 for the
/// CEGAR-loop wiring).
pub fn locate_cvc5() -> Result<Cvc5Bin, AdapterError> {
    // AR-GO-2 — shared locate body; see `crate::adapter::locate_tool`.
    let (path, version) = crate::adapter::locate_tool(
        "MUNUNU_CVC5_PATH",
        "cvc5",
        "cvc5",
        "Set MUNUNU_CVC5_PATH or install cvc5 ≥ 1.0 (Homebrew: `brew install cvc5`; \
         Debian: `apt install cvc5`).",
        parse_cvc5_version,
    )?;
    Ok(Cvc5Bin { path, version })
}

/// R.5 Item 3 sub-item 3.1 (2026-06-03) — extract the version
/// token from `cvc5 --version` output.
///
/// CVC5's `--version` output format (observed across 1.0.x and
/// 1.1.x releases):
/// ```text
/// This is cvc5 version 1.0.5 [git tag v1.0.5 branch HEAD]
/// Copyright (C) 2009-2023 by the authors and their institutional affiliations
/// ...
/// ```
///
/// Some packaged builds (e.g. older Homebrew bottles) drop the
/// leading "This is " and start directly with "cvc5 version".
/// Both forms are accepted.
///
/// Returns `Some("1.0.5")` on success or `None` when the format
/// doesn't match — the caller treats this as `"<unparseable>"`
/// rather than a hard failure (the binary may still be usable
/// for actual queries; the version string is diagnostic-only).
pub fn parse_cvc5_version(output: &str) -> Option<String> {
    let first_line = output.lines().next()?;
    let trimmed = first_line.trim();
    // Accept either "This is cvc5 version X.Y.Z..." or
    // "cvc5 version X.Y.Z...".
    let after_version = trimmed
        .strip_prefix("This is cvc5 version ")
        .or_else(|| trimmed.strip_prefix("cvc5 version "))?;
    // The version token is the first whitespace- or
    // bracket-delimited word.
    let end = after_version
        .find(|c: char| c.is_whitespace() || c == '[')
        .unwrap_or(after_version.len());
    Some(after_version[..end].to_string())
}

/// R.5 Item 3 sub-item 3.3 (2026-06-03) — options controlling
/// a CVC5 interpolant query invocation. Default timeout is 30s,
/// matching the breakdown doc's sub-item 3.3 spec.
#[derive(Debug, Clone)]
pub struct InterpolantQueryOptions {
    /// Wall-clock (and, via `--tlimit`, cvc5-internal) budget for one interpolant
    /// query. cvc5 abandons the SyGuS search gracefully at this budget; the hard
    /// wall-kill is a backstop. Default **10 s** — most decidable interpolants return
    /// in well under a second, and a genuinely hard one (deep-grammar `bvurem`, ~100 s)
    /// is out of reach at any practical budget, so a tight bound fails those *fast*
    /// (abstain in seconds) rather than pinning a multi-query discovery loop.
    pub timeout: Duration,
}

impl Default for InterpolantQueryOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}

/// sub-item 3.4 (2026-07-12) — parse CVC5's interpolant response into the richer
/// [`PredicateExpr`] (relational + compound), the representation the cube path
/// uses for emergent-K discovery.
///
/// CVC5 1.3.4's `(get-interpolant I <B>)` emits `(define-fun I () Bool <expr>)`
/// where `<expr>` is a SyGuS-grammar term. Real shapes observed on 1.3.4:
/// - `(and (= a #b00000101) true)` — a REG==VALUE atom wrapped in `(and .. true)`;
/// - `(and (= a b) true)` — a REG==REG relation (→ [`PredicateExpr::CmpReg`]);
/// - `(not (bvult b a))` — a negated inequality;
/// - `(and (= x (bvadd y #b01)) ..)` — an arithmetic-addend relation
///   (→ [`PredicateExpr::CmpRegAddend`]).
///
/// Returns `None` for trivial (`true`/`false`) interpolants, the `(error ..)`
/// no-interpolant reply, and any term outside the mapped grammar (bitwise ops,
/// multiplication, `ite`, …) — the caller then falls back to WP.
///
/// **Soundness.** The returned predicate is only a *candidate cube dimension*.
/// Adding any well-formed predicate refines the abstraction monotonically
/// (Shoham–Grumberg) and can never flip a definite verdict — so a mis-parse
/// costs *refinement power*, never soundness.
pub fn parse_interpolant_to_predicate_expr(stdout: &str) -> Option<PredicateExpr> {
    let define_marker = "(define-fun I () Bool ";
    let start = stdout.find(define_marker)?;
    let after = &stdout[start + define_marker.len()..];
    let expr = extract_balanced_expr(after)?;
    let toks = tokenize_sexpr(expr.trim());
    let mut pos = 0usize;
    let sexpr = parse_sexpr(&toks, &mut pos)?;
    sexpr_to_predicate_expr(&sexpr)
}

/// A minimal S-expression tree for the CVC5 interpolant grammar.
enum SExpr {
    Atom(String),
    List(Vec<SExpr>),
}

/// Tokenize an SMT-LIB term into parens + atoms. Whitespace separates atoms;
/// `#b…` / `#x…` literals and identifiers stay whole.
fn tokenize_sexpr(s: &str) -> Vec<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' | ')' => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
                toks.push(ch.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks
}

/// Recursive-descent parse of a token stream into an [`SExpr`]. Advances `pos`.
fn parse_sexpr(toks: &[String], pos: &mut usize) -> Option<SExpr> {
    let t = toks.get(*pos)?;
    if t == "(" {
        *pos += 1;
        let mut items = Vec::new();
        while toks.get(*pos).is_some_and(|x| x != ")") {
            items.push(parse_sexpr(toks, pos)?);
        }
        toks.get(*pos)?; // must be the closing ")" (else unbalanced → None)
        *pos += 1;
        Some(SExpr::List(items))
    } else if t == ")" {
        None
    } else {
        *pos += 1;
        Some(SExpr::Atom(t.clone()))
    }
}

/// Map a parsed interpolant [`SExpr`] to a [`PredicateExpr`]. `None` for trivial
/// or out-of-grammar terms.
fn sexpr_to_predicate_expr(e: &SExpr) -> Option<PredicateExpr> {
    match e {
        // A bare atom (`true` / `false` / a lone register) is not a usable
        // boolean predicate.
        SExpr::Atom(_) => None,
        SExpr::List(items) => {
            let SExpr::Atom(op) = items.first()? else {
                return None;
            };
            match op.as_str() {
                // n-ary `and` — drop trivial `true` conjuncts (the cvc5
                // `(and X true)` idiom); fold the rest into a right-nested And.
                "and" => fold_bool_children(&items[1..], /* is_and */ true),
                "or" => fold_bool_children(&items[1..], /* is_and */ false),
                "not" => Some(PredicateExpr::Not(Box::new(sexpr_to_predicate_expr(
                    items.get(1)?,
                )?))),
                "=" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Eq),
                "distinct" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Ne),
                "bvult" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Lt),
                "bvule" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Le),
                "bvugt" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Gt),
                "bvuge" => cmp_from_operands(items.get(1)?, items.get(2)?, CmpOp::Ge),
                _ => None,
            }
        }
    }
}

/// Fold the children of an `and`/`or` node. Trivial identity children (`true`
/// for `and`, `false` for `or`) are dropped; every remaining child must parse
/// (an unparseable conjunct/disjunct ⇒ `None`, so we never fabricate a partial
/// predicate). One survivor collapses to itself; ≥2 fold right-nested.
fn fold_bool_children(children: &[SExpr], is_and: bool) -> Option<PredicateExpr> {
    let identity = if is_and { "true" } else { "false" };
    let mut parsed: Vec<PredicateExpr> = Vec::new();
    for c in children {
        if let SExpr::Atom(a) = c
            && a == identity
        {
            continue; // drop the identity element
        }
        parsed.push(sexpr_to_predicate_expr(c)?);
    }
    let mut iter = parsed.into_iter();
    let first = iter.next()?;
    Some(iter.fold(first, |acc, next| {
        if is_and {
            PredicateExpr::And(Box::new(acc), Box::new(next))
        } else {
            PredicateExpr::Or(Box::new(acc), Box::new(next))
        }
    }))
}

/// Build a comparison [`PredicateExpr`] from two operand s-expressions and an
/// operator. Handles register/literal/`(bvadd reg lit)` operands in either
/// order (swapping the operator when the register is on the right).
fn cmp_from_operands(lhs: &SExpr, rhs: &SExpr, op: CmpOp) -> Option<PredicateExpr> {
    let l = classify_operand(lhs);
    let r = classify_operand(rhs);
    match (l, r) {
        // reg <op> literal
        (Operand::Register(reg), Operand::Literal(value, _)) => Some(PredicateExpr::Cmp {
            register: reg,
            op,
            value,
        }),
        // literal <op> reg  ⇒  reg <swap(op)> literal
        (Operand::Literal(value, _), Operand::Register(reg)) => Some(PredicateExpr::Cmp {
            register: reg,
            op: swap_cmp_op(op),
            value,
        }),
        // reg <op> reg
        (Operand::Register(lhs), Operand::Register(rhs)) => {
            Some(PredicateExpr::CmpReg { lhs, op, rhs })
        }
        // reg <op> (bvadd reg lit)
        (Operand::Register(lhs), Operand::RegAddend(rhs, addend, width)) => {
            Some(PredicateExpr::CmpRegAddend {
                lhs,
                op,
                rhs,
                addend,
                width,
            })
        }
        // (bvadd reg lit) <op> reg  ⇒  reg <swap(op)> (bvadd reg lit)
        (Operand::RegAddend(rhs, addend, width), Operand::Register(lhs)) => {
            Some(PredicateExpr::CmpRegAddend {
                lhs,
                op: swap_cmp_op(op),
                rhs,
                addend,
                width,
            })
        }
        _ => None,
    }
}

/// A classified interpolant operand.
enum Operand {
    Register(String),
    Literal(u64, u32),
    /// `(bvadd reg lit)` — register `.0` plus constant `.1` at width `.2`.
    RegAddend(String, u64, u32),
    Other,
}

fn classify_operand(e: &SExpr) -> Operand {
    match e {
        SExpr::Atom(a) => {
            if let Some((v, w)) = parse_bv_literal_sexpr(a) {
                Operand::Literal(v, w)
            } else if is_register_ident(a) {
                Operand::Register(a.clone())
            } else {
                Operand::Other
            }
        }
        SExpr::List(items) => {
            // `(_ bv<n> <w>)` literal.
            if let Some((v, w)) = parse_underscore_literal(items) {
                return Operand::Literal(v, w);
            }
            // `(bvadd A B)` with one register + one literal.
            if let Some(SExpr::Atom(head)) = items.first()
                && head == "bvadd"
                && items.len() == 3
            {
                let a = classify_operand(&items[1]);
                let b = classify_operand(&items[2]);
                return match (a, b) {
                    (Operand::Register(reg), Operand::Literal(k, w))
                    | (Operand::Literal(k, w), Operand::Register(reg)) => {
                        Operand::RegAddend(reg, k, w)
                    }
                    _ => Operand::Other,
                };
            }
            Operand::Other
        }
    }
}

/// Is `a` a plausible register identifier (not a literal, not a keyword)?
fn is_register_ident(a: &str) -> bool {
    !a.is_empty()
        && !a.starts_with('#')
        && a != "true"
        && a != "false"
        && a != "_"
        && parse_bv_literal_sexpr(a).is_none()
}

/// Parse a `#b…` / `#x…` bitvector literal, returning `(value, width_bits)`.
fn parse_bv_literal_sexpr(s: &str) -> Option<(u64, u32)> {
    if let Some(bits) = s.strip_prefix("#b") {
        return Some((u64::from_str_radix(bits, 2).ok()?, bits.len() as u32));
    }
    if let Some(hex) = s.strip_prefix("#x") {
        return Some((u64::from_str_radix(hex, 16).ok()?, (hex.len() as u32) * 4));
    }
    None
}

/// Parse a `(_ bv<n> <w>)` literal list into `(value, width)`.
fn parse_underscore_literal(items: &[SExpr]) -> Option<(u64, u32)> {
    if items.len() == 3
        && let (SExpr::Atom(us), SExpr::Atom(bv), SExpr::Atom(w)) =
            (&items[0], &items[1], &items[2])
        && us == "_"
    {
        let value = bv.strip_prefix("bv")?.parse::<u64>().ok()?;
        let width = w.parse::<u32>().ok()?;
        return Some((value, width));
    }
    None
}

/// Swap a comparison operator for operand reordering (`k < r` ⇒ `r > k`).
fn swap_cmp_op(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Eq => CmpOp::Eq,
        CmpOp::Ne => CmpOp::Ne,
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
    }
}

/// R.5 Item 3 sub-item 3.3 (2026-06-03) — extract a balanced
/// s-expression from `input`, accounting for nested parens.
/// Returns the substring from position 0 up to (but not
/// including) the closing paren that balances the open paren
/// the parent statement is inside.
///
/// Example: given input `"(= reg_a #b00) ...)"`, returns
/// `"(= reg_a #b00)"`. Given input `"true)\n..."`, returns
/// `"true"`.
fn extract_balanced_expr(input: &str) -> Option<&str> {
    // The interpolant expression starts immediately after the
    // marker; it's either an atom (true/false/etc.) or an
    // s-expression starting with `(`. We need to find where it
    // ends (before the closing `)` of `(define-fun I () Bool
    // <expr>)`).
    let mut depth: i32 = 0;
    let mut end: Option<usize> = None;
    for (i, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    end = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    end.map(|e| &input[..e])
}

// (2026-07-12) The bare-`(= reg lit)` string parser + its `split_first_token` /
// `parse_bv_literal` helpers were superseded by the recursive
// [`parse_interpolant_to_predicate_expr`] s-expression parser above (which also
// unwraps the cvc5 1.3.4 `(and X true)` idiom and handles relational forms).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterErrorKind;

    #[test]
    fn parse_cvc5_version_extracts_from_standard_format() {
        let out = "This is cvc5 version 1.0.5 [git tag v1.0.5 branch HEAD]\n\
                   Copyright (C) 2009-2023 by the authors\n";
        assert_eq!(parse_cvc5_version(out).as_deref(), Some("1.0.5"));
    }

    #[test]
    fn parse_cvc5_version_extracts_from_short_format() {
        let out = "cvc5 version 1.1.2\n";
        assert_eq!(parse_cvc5_version(out).as_deref(), Some("1.1.2"));
    }

    #[test]
    fn parse_cvc5_version_handles_trailing_whitespace_only() {
        let out = "This is cvc5 version 1.0.0\n";
        assert_eq!(parse_cvc5_version(out).as_deref(), Some("1.0.0"));
    }

    #[test]
    fn parse_cvc5_version_returns_none_on_unknown_format() {
        let out = "Something else entirely\n";
        assert_eq!(parse_cvc5_version(out), None);
    }

    #[test]
    fn parse_cvc5_version_returns_none_on_empty_output() {
        assert_eq!(parse_cvc5_version(""), None);
    }

    #[test]
    fn locate_cvc5_returns_structured_error_when_binary_absent() {
        // A bogus path, overridden for this thread only (mununu#880). The function must
        // return a structured AdapterError, never panic, so callers fall back gracefully.
        let _env = crate::adapter::env_var::override_for_test(
            "MUNUNU_CVC5_PATH",
            "/nonexistent/path/to/cvc5/binary/definitely/not/here",
        );
        let result = locate_cvc5();
        assert!(
            result.is_err(),
            "locate_cvc5 MUST return Err when the binary is absent; got {result:?}"
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        assert!(
            err.message.contains("cvc5"),
            "error message MUST mention cvc5 for diagnosability; got: {}",
            err.message
        );
    }

    #[test]
    #[ignore = "requires the mununu-sva image (cvc5); run by the e2e sweep"]
    fn e2e_locate_cvc5_succeeds_when_binary_available() {
        // R.5 Item 3 sub-item 3.1 — integration test that's
        // ignored by default. Run with `cargo test -- --ignored`
        // when CVC5 is installed locally (e.g. `brew install
        // cvc5` first).
        let result = locate_cvc5();
        match result {
            Ok(bin) => {
                assert!(
                    bin.version != "<unparseable>",
                    "expected a parseable version from `cvc5 --version`; got {}",
                    bin.version
                );
            }
            Err(e) => panic!(
                "locate_cvc5 failed when CVC5 should be installed (run \
                 `brew install cvc5` or set MUNUNU_CVC5_PATH first): {}",
                e.message
            ),
        }
    }

    // ─────────────────────────────────────────────────────────────
    // R.5 Item 3 sub-item 3.2 tests — SMT-LIB interpolation query
    // construction. Golden-output tests; byte-equal comparison
    // against expected strings.
    // ─────────────────────────────────────────────────────────────

    // ─────────────────────────────────────────────────────────────
    // R.5 Item 3 sub-item 3.3 tests — CVC5 subprocess invocation
    // + interpolant parsing. Parser tests run unconditionally;
    // the subprocess integration test is gated on CVC5 binary
    // availability.
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r5_subitem_33_extract_balanced_expr_handles_nested_parens() {
        // Internal helper test: balanced extraction stops at
        // the outermost matching paren.
        let input = "(= reg_a (_ bv0 32))) trailing junk";
        let got = extract_balanced_expr(input);
        assert_eq!(got, Some("(= reg_a (_ bv0 32))"));
    }

    #[test]
    fn r5_subitem_33_extract_balanced_expr_handles_atom() {
        // Atom (true/false) — no nesting; stops at the first
        // closing paren (which belongs to the parent
        // define-fun).
        let input = "true)\n";
        let got = extract_balanced_expr(input);
        assert_eq!(got, Some("true"));
    }

    // === sub-item 3.4 (2026-07-12): relational + compound interpolant parser ===
    // Every input below is REAL cvc5 1.3.4 `get-interpolant` output (captured
    // from live queries), the shapes the PredicateSpec-only parser could not
    // decode — including the `(and X true)` wrapping cvc5 1.3.4 puts even a
    // plain REG==VALUE interpolant in.

    /// mununu#977 — the interpolant parser's literal and envelope cases, ported from the removed
    /// `PredicateSpec` shim's tests to the parser the transition-aware refinement uses.
    #[test]
    fn interp_expr_decodes_binary_hex_and_underscored_literals() {
        let eq = |register: &str, value: u64| {
            Some(PredicateExpr::Cmp {
                register: register.to_string(),
                op: CmpOp::Eq,
                value,
            })
        };
        assert_eq!(
            parse_interpolant_to_predicate_expr(
                "(define-fun I () Bool (= reg_a #b00000000000000000000000000000000))\n"
            ),
            eq("reg_a", 0)
        );
        assert_eq!(
            parse_interpolant_to_predicate_expr("(define-fun I () Bool (= reg_b #x0a))\n"),
            eq("reg_b", 10)
        );
        assert_eq!(
            parse_interpolant_to_predicate_expr("(define-fun I () Bool (= reg_c (_ bv42 32)))\n"),
            eq("reg_c", 42)
        );
        // cvc5 1.3.4 wraps a REG==VALUE interpolant as `(and (= a #b..) true)`.
        assert_eq!(
            parse_interpolant_to_predicate_expr(
                "(define-fun I () Bool (and (= a #b00000101) true))\n"
            ),
            eq("a", 5)
        );
        // cvc5 may print set-info / comment lines before the answer; the parser scans for the
        // `(define-fun I` marker.
        assert_eq!(
            parse_interpolant_to_predicate_expr(
                "(:status sat)\n; some comment\n(define-fun I () Bool (= reg_a #b00))\n"
            ),
            eq("reg_a", 0)
        );
    }

    #[test]
    fn interp_expr_declines_trivial_and_missing_interpolants() {
        for stdout in [
            "(define-fun I () Bool true)\n",
            "(define-fun I () Bool false)\n",
            "(error \"No interpolant exists for the given query.\")\n",
        ] {
            assert_eq!(
                parse_interpolant_to_predicate_expr(stdout),
                None,
                "{stdout}"
            );
        }
    }

    /// `(and (= a b) true)` — a REG==REG relation (emergent relational K).
    #[test]
    fn interp_expr_reg_eq_reg_relation() {
        assert_eq!(
            parse_interpolant_to_predicate_expr("(define-fun I () Bool (and (= a b) true))\n"),
            Some(PredicateExpr::CmpReg {
                lhs: "a".to_string(),
                op: CmpOp::Eq,
                rhs: "b".to_string(),
            })
        );
    }

    #[test]
    fn interp_expr_negated_inequality() {
        // `(not (bvult b a))` = ¬(b < a) — a negated relational comparison.
        let stdout = "(define-fun I () Bool (not (bvult b a)))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::Not(Box::new(PredicateExpr::CmpReg {
                lhs: "b".to_string(),
                op: CmpOp::Lt,
                rhs: "a".to_string(),
            })))
        );
    }

    #[test]
    fn interp_expr_addend_relation_and_conjunction() {
        // Real cvc5 output for A=(x==y+1 & y<200) ⟹ B=(x>y):
        //   (and (= x (bvadd y #b00000001)) (bvult y (bvadd #b00000001 y)))
        // → And( x == y+1 , y < y+1 ) — both addend relations.
        let stdout = "(define-fun I () Bool (and (= x (bvadd y #b00000001)) \
                      (bvult y (bvadd #b00000001 y))))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::And(
                Box::new(PredicateExpr::CmpRegAddend {
                    lhs: "x".to_string(),
                    op: CmpOp::Eq,
                    rhs: "y".to_string(),
                    addend: 1,
                    width: 8,
                }),
                Box::new(PredicateExpr::CmpRegAddend {
                    lhs: "y".to_string(),
                    op: CmpOp::Lt,
                    rhs: "y".to_string(),
                    addend: 1,
                    width: 8,
                }),
            ))
        );
    }

    #[test]
    fn interp_expr_literal_on_left_swaps_operator() {
        // `(bvult #b00000101 a)` = 5 < a  ⟹  a > 5 (operator swapped).
        let stdout = "(define-fun I () Bool (bvult #b00000101 a))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::Cmp {
                register: "a".to_string(),
                op: CmpOp::Gt,
                value: 5,
            })
        );
    }

    #[test]
    fn interp_expr_underscore_literal_form() {
        // `(= r (_ bv42 32))` — the underscored decimal literal form.
        let stdout = "(define-fun I () Bool (= r (_ bv42 32)))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::Cmp {
                register: "r".to_string(),
                op: CmpOp::Eq,
                value: 42,
            })
        );
    }

    #[test]
    fn interp_expr_trivial_and_out_of_grammar_return_none() {
        assert_eq!(
            parse_interpolant_to_predicate_expr("(define-fun I () Bool true)\n"),
            None
        );
        assert_eq!(
            parse_interpolant_to_predicate_expr("(define-fun I () Bool false)\n"),
            None
        );
        // A bitwise op is outside the mapped grammar.
        assert_eq!(
            parse_interpolant_to_predicate_expr(
                "(define-fun I () Bool (= a (bvand b #b00001111)))\n"
            ),
            None
        );
        // No define-fun marker (the `(error ..)` no-interpolant reply).
        assert_eq!(
            parse_interpolant_to_predicate_expr("(error \"no interpolant\")\n"),
            None
        );
    }

    #[test]
    fn default_interpolant_timeout_is_ten_seconds() {
        // Fail-fast budget: cvc5 `--tlimit` + wall backstop at 10 s so a pathological
        // deep-grammar interpolant abstains in seconds rather than pinning the loop.
        let opts = InterpolantQueryOptions::default();
        assert_eq!(opts.timeout, Duration::from_secs(10));
    }
}
