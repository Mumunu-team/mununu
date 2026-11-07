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

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::adapter::btor2::PredicateSpec;
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
use crate::adapter::{AdapterError, AdapterErrorKind, SpawnMember};

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

/// R.5 Item 3 sub-item 3.2 (2026-06-03) — render a CVC5-
/// compatible SMT-LIB interpolation query that asks for a
/// predicate separating `source_cube` from `target_cube` under
/// the given `predicates` set.
///
/// **Semantics**: each cube index encodes a bit pattern over
/// the predicate set (predicate at position `i` is true at cube
/// `c` iff `(c >> i) & 1 == 1`, mirroring the lifter's
/// convention in `kmts_lift.rs`). The query asserts the source
/// cube's predicate conjunction as the "A" side, then asks CVC5
/// for an interpolant separating it from the target cube's
/// conjunction.
///
/// **Output shape** (illustrative, not literal):
/// ```text
/// (set-logic QF_BV)
/// (set-option :produce-interpolants true)
/// (declare-fun reg_a () (_ BitVec 32))
/// (assert (= reg_a (_ bv0 32)))
/// (get-interpolant I (not (= reg_a (_ bv1 32))))
/// (exit)
/// ```
///
/// **Sub-item 3.2 scope**: pure rendering. The function does
/// not invoke CVC5, parse the interpolant response, or feed it
/// back into mununu's predicate set — those are sub-items 3.3 +
/// 3.4. This function is golden-output testable via byte-equal
/// comparison against an expected string.
///
/// **Register bit-width**: uses [`DEFAULT_BV_WIDTH`] (32-bit)
/// for every register. Sub-item 3.4 will plumb the actual
/// per-register width from BTOR2.
///
/// **Cube-index interpretation**: matches the lifter's bit
/// convention. For `predicates = [p_a, p_b, p_c]` (in that
/// order):
/// - cube 0 = (¬p_a ∧ ¬p_b ∧ ¬p_c)
/// - cube 1 = (p_a ∧ ¬p_b ∧ ¬p_c)
/// - cube 2 = (¬p_a ∧ p_b ∧ ¬p_c)
/// - ...
/// - cube 7 = (p_a ∧ p_b ∧ p_c)
///
/// Each predicate is rendered as `(= <register> (_ bv<value>
/// <width>))` for the positive case or
/// `(not (= <register> (_ bv<value> <width>)))` for the negated
/// case.
pub fn build_interpolation_query(
    predicates: &[PredicateSpec],
    source_cube: usize,
    target_cube: usize,
) -> String {
    let mut out = String::new();
    out.push_str("(set-logic QF_BV)\n");
    out.push_str("(set-option :produce-interpolants true)\n");

    // Declare each distinct register. Sorted for deterministic
    // output (BTreeSet gives the iteration order; same order
    // means byte-stable golden tests across runs).
    let registers: BTreeSet<&str> = predicates.iter().map(|p| p.register.as_str()).collect();
    for reg in &registers {
        out.push_str(&format!(
            "(declare-fun {} () (_ BitVec {}))\n",
            reg, DEFAULT_BV_WIDTH
        ));
    }

    // Source cube facts (A side). Emit as a single (assert (and
    // ...)) when |predicates| > 1, or a single (assert ...) when
    // == 1, or skip entirely when == 0 (the empty conjunction
    // is trivially true).
    let source_conj = render_cube_conjunction(predicates, source_cube);
    if let Some(conj) = source_conj {
        out.push_str(&format!("(assert {conj})\n"));
    }

    // Target cube facts (B side) — passed as the get-interpolant
    // formula. CVC5's contract: returns I such that A ⊨ I and
    // I ⊨ ¬B. We pass the target cube's conjunction as B; the
    // returned interpolant separates source from target.
    let target_conj =
        render_cube_conjunction(predicates, target_cube).unwrap_or_else(|| "true".to_string());
    out.push_str(&format!("(get-interpolant I {target_conj})\n"));
    out.push_str("(exit)\n");
    out
}

/// R.5 Item 3 sub-item 3.2 (2026-06-03) — render the conjunction
/// of predicate facts at a given cube index. Returns:
/// - `None` if `predicates` is empty (the empty conjunction is
///   trivially true; caller decides whether to emit `true` or
///   skip the assert entirely).
/// - `Some(<single-fact>)` for a 1-element predicate set.
/// - `Some((and <facts>))` for multi-element sets.
///
/// Each fact is `(= <register> (_ bv<value> <width>))` for the
/// positive bit and `(not (= <register> (_ bv<value> <width>)))`
/// for the negated bit.
fn render_cube_conjunction(predicates: &[PredicateSpec], cube: usize) -> Option<String> {
    if predicates.is_empty() {
        return None;
    }
    let facts: Vec<String> = predicates
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let positive = (cube >> i) & 1 == 1;
            let eq = format!("(= {} (_ bv{} {}))", p.register, p.value, DEFAULT_BV_WIDTH);
            if positive { eq } else { format!("(not {eq})") }
        })
        .collect();
    if facts.len() == 1 {
        Some(facts.into_iter().next().unwrap())
    } else {
        Some(format!("(and {})", facts.join(" ")))
    }
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

/// R.5 Item 3 sub-item 3.3 (2026-06-03) — invoke CVC5 on an
/// SMT-LIB interpolation query (built by [`build_interpolation_query`])
/// and parse the returned interpolant into a [`PredicateSpec`].
///
/// Returns:
/// - `Ok(Some(predicate))` — CVC5 produced an interpolant we
///   can parse into mununu's `PredicateSpec` shape (the MVP
///   parser handles `(= <register> <bv-literal>)` exactly;
///   compound interpolants like `(and ...)` / `(or ...)` /
///   `(not ...)` return `Ok(None)` as a soft fall-through so
///   the CEGAR loop can fall back to WP).
/// - `Ok(None)` — CVC5 reported no interpolant exists (the
///   input wasn't UNSAT-incompatible), OR the interpolant has
///   a compound shape our MVP parser doesn't decode.
/// - `Err(_)` — subprocess failure, timeout exceeded, or
///   non-zero CVC5 exit status.
///
/// **MVP scope (sub-item 3.3)**: handles the single-equality
/// interpolant shape only. Sub-item 3.4 will widen the parser
/// to handle compound shapes by emitting multiple `PredicateSpec`s
/// where structurally feasible.
pub fn invoke_cvc5_for_interpolant(
    bin: &Cvc5Bin,
    query: &str,
    opts: &InterpolantQueryOptions,
) -> Result<Option<PredicateSpec>, AdapterError> {
    // Spawn CVC5 with the SMT-LIB query piped to stdin. `--tlimit` is cvc5's OWN
    // budget (ms): without it, cvc5's SyGuS interpolant search runs unbounded and is
    // only stopped by the hard wall-kill below — burning the full budget on a
    // pathological deep-grammar interpolant (the `gen43`-class `bvurem` query took 105 s
    // when unbounded). With `--tlimit` cvc5 abandons the search *gracefully* within the
    // budget (a clean "no interpolant", not a mid-search kill), so a hard query fails
    // fast instead of pinning the discovery loop. Matched to the wall budget.
    let tlimit_ms = opts.timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    let (mut child, group) = Command::new(&bin.path)
        .args([
            "--lang=smt2",
            "--produce-interpolants",
            &format!("--tlimit={tlimit_ms}"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_member()
        .map_err(|e| AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: format!(
                "adapter/cvc5: failed to spawn `{} --lang=smt2 --produce-interpolants`: {e}",
                bin.path.display()
            ),
        })?;

    // Write the query to stdin in a separate thread so the main
    // thread can poll for timeout via `try_wait` on the child.
    // Without a writer thread, a query that fills the kernel
    // stdin buffer would deadlock (writer blocks, no one reads
    // stdout to drain progress).
    let stdin = child.stdin.take().ok_or_else(|| AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        location: None,
        message: "adapter/cvc5: failed to capture child stdin handle".to_string(),
    })?;
    let query_bytes = query.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        let _ = stdin.write_all(&query_bytes);
        // Dropping stdin closes the pipe, signalling EOF to CVC5.
    });

    // Poll loop with timeout. 50 ms granularity is fine for
    // a 30-second timeout; the overhead is negligible.
    let start = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: format!("adapter/cvc5: try_wait on child failed: {e}"),
        })? {
            Some(s) => break s,
            None => {
                if start.elapsed() >= opts.timeout {
                    group.kill_and_reap(&mut child);
                    let _ = writer.join();
                    return Err(AdapterError {
                        kind: AdapterErrorKind::UnsupportedConstruct,
                        location: None,
                        message: format!(
                            "adapter/cvc5: query exceeded {}s timeout — killed",
                            opts.timeout.as_secs()
                        ),
                    });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let _ = writer.join();

    // Capture stdout (the interpolant response) + stderr (for
    // diagnostics on non-zero exit).
    let mut stdout_data = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_end(&mut stdout_data);
    }
    let mut stderr_data = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_end(&mut stderr_data);
    }
    let stdout_str = String::from_utf8_lossy(&stdout_data);
    let stderr_str = String::from_utf8_lossy(&stderr_data);

    if !status.success() {
        // CVC5 may exit non-zero AND print "(error ...)" on
        // stdout for queries it can't handle (e.g. no
        // interpolant exists for SAT inputs). Distinguish:
        // if stdout contains an error sexpr, treat as Ok(None);
        // otherwise return Err.
        if stdout_str.contains("(error") || stderr_str.contains("(error") {
            return Ok(None);
        }
        return Err(AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            location: None,
            message: format!(
                "adapter/cvc5: subprocess exited with status {}; stderr: {}",
                status,
                stderr_str.trim()
            ),
        });
    }

    // Parse the interpolant response.
    Ok(parse_cvc5_interpolant_response(&stdout_str))
}

/// R.5 Item 3 sub-item 3.3 (2026-06-03) — parse CVC5's
/// interpolant response into a [`PredicateSpec`].
///
/// CVC5's `(get-interpolant I <formula>)` produces output of
/// shape:
/// ```text
/// (define-fun I () Bool <interpolant-expression>)
/// ```
///
/// **MVP parser handles**:
/// - `(= <register-name> #b<binary-literal>)` — bitvector
///   binary literal form.
/// - `(= <register-name> (_ bv<value> <width>))` — bitvector
///   decimal literal form.
///
/// **Returns `None` for**:
/// - Compound expressions: `(and ...)`, `(or ...)`, `(not ...)`,
///   `(=> ...)`. The CEGAR loop falls back to WP in these
///   cases.
/// - `true` / `false` trivial interpolants (no useful
///   refinement signal).
/// - Any shape the parser doesn't recognise (defensive
///   fallthrough).
/// - The `(error "...")` reply CVC5 emits when no interpolant
///   exists.
pub fn parse_cvc5_interpolant_response(stdout: &str) -> Option<PredicateSpec> {
    // Back-compat shim: the PredicateSpec-typed Craig flow only carries
    // `register == value` atoms. Parse the full interpolant, then downcast a
    // simple `Cmp { .., Eq, .. }` to a PredicateSpec; anything relational or
    // compound returns None here (the richer path is
    // [`parse_interpolant_to_predicate_expr`]). This also fixes the cvc5 1.3.4
    // `(and <atom> true)` wrapping that the old bare-`(= ..)` parser rejected.
    match parse_interpolant_to_predicate_expr(stdout)? {
        PredicateExpr::Cmp {
            register,
            op: CmpOp::Eq,
            value,
        } => Some(PredicateSpec {
            name: "craig_interp".to_string(),
            register,
            value,
        }),
        _ => None,
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
        // Force a definitely-missing path via the env var. The
        // function MUST return a structured AdapterError (not
        // panic) so callers can fall back gracefully.
        // SAFETY: env vars are process-global; this test sets +
        // unsets to avoid leaking the bogus path to other tests
        // that may discover via PATH. Tests in the same crate
        // run in parallel by default but `--test-threads=1`
        // serializes if collisions show up.
        // Use a path that's guaranteed not to exist + not
        // executable.
        let original = std::env::var("MUNUNU_CVC5_PATH").ok();
        // SAFETY: required for env var manipulation in tests; the
        // value is restored before the test returns.
        unsafe {
            std::env::set_var(
                "MUNUNU_CVC5_PATH",
                "/nonexistent/path/to/cvc5/binary/definitely/not/here",
            );
        }
        let result = locate_cvc5();
        unsafe {
            match original {
                Some(v) => std::env::set_var("MUNUNU_CVC5_PATH", v),
                None => std::env::remove_var("MUNUNU_CVC5_PATH"),
            }
        }
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
    #[ignore = "requires cvc5 binary installed; run with --ignored when available"]
    fn locate_cvc5_succeeds_when_binary_available() {
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

    fn pred(name: &str, register: &str, value: u64) -> PredicateSpec {
        PredicateSpec {
            name: name.to_string(),
            register: register.to_string(),
            value,
        }
    }

    #[test]
    fn r5_subitem_32_render_empty_predicates_yields_trivial_query() {
        // Edge case: no predicates ⇒ empty conjunction ⇒ no
        // assert; the get-interpolant target is `true`.
        let got = build_interpolation_query(&[], 0, 0);
        let expected = "\
(set-logic QF_BV)
(set-option :produce-interpolants true)
(get-interpolant I true)
(exit)
";
        assert_eq!(got, expected);
    }

    #[test]
    fn r5_subitem_32_render_single_predicate_positive_source_negative_target() {
        // |P| = 1, source cube 1 (predicate holds), target cube
        // 0 (predicate doesn't hold). Source assertion is the
        // positive equality; target is the negation.
        let preds = vec![pred("p_zero", "reg_a", 0)];
        let got = build_interpolation_query(&preds, 1, 0);
        let expected = "\
(set-logic QF_BV)
(set-option :produce-interpolants true)
(declare-fun reg_a () (_ BitVec 32))
(assert (= reg_a (_ bv0 32)))
(get-interpolant I (not (= reg_a (_ bv0 32))))
(exit)
";
        assert_eq!(got, expected);
    }

    #[test]
    fn r5_subitem_32_render_single_predicate_negative_source_positive_target() {
        // |P| = 1, source cube 0 (predicate doesn't hold),
        // target cube 1 (predicate holds). Source assertion is
        // the negation; target is the positive equality.
        let preds = vec![pred("p_zero", "reg_a", 0)];
        let got = build_interpolation_query(&preds, 0, 1);
        let expected = "\
(set-logic QF_BV)
(set-option :produce-interpolants true)
(declare-fun reg_a () (_ BitVec 32))
(assert (not (= reg_a (_ bv0 32))))
(get-interpolant I (= reg_a (_ bv0 32)))
(exit)
";
        assert_eq!(got, expected);
    }

    #[test]
    fn r5_subitem_32_render_multi_predicate_both_registers() {
        // |P| = 2, two different registers. Cube 3 (binary 11)
        // = both predicates hold; cube 0 = neither holds.
        // Conjunction is rendered as (and <facts>).
        let preds = vec![pred("p_a_zero", "reg_a", 0), pred("p_b_one", "reg_b", 1)];
        let got = build_interpolation_query(&preds, 3, 0);
        let expected = "\
(set-logic QF_BV)
(set-option :produce-interpolants true)
(declare-fun reg_a () (_ BitVec 32))
(declare-fun reg_b () (_ BitVec 32))
(assert (and (= reg_a (_ bv0 32)) (= reg_b (_ bv1 32))))
(get-interpolant I (and (not (= reg_a (_ bv0 32))) (not (= reg_b (_ bv1 32)))))
(exit)
";
        assert_eq!(got, expected);
    }

    #[test]
    fn r5_subitem_32_render_predicates_sharing_same_register_dedupes_decl() {
        // Two predicates over the same register: should declare
        // the register only once (BTreeSet dedupes).
        let preds = vec![pred("p_zero", "reg_a", 0), pred("p_one", "reg_a", 1)];
        let got = build_interpolation_query(&preds, 1, 2);
        // Source cube 1 = (p_zero holds, p_one doesn't) =
        //   reg_a == 0 AND reg_a != 1 (consistent: reg_a == 0).
        // Target cube 2 = (p_zero doesn't hold, p_one holds) =
        //   reg_a != 0 AND reg_a == 1 (consistent: reg_a == 1).
        // The declare-fun for reg_a appears EXACTLY ONCE.
        let expected = "\
(set-logic QF_BV)
(set-option :produce-interpolants true)
(declare-fun reg_a () (_ BitVec 32))
(assert (and (= reg_a (_ bv0 32)) (not (= reg_a (_ bv1 32)))))
(get-interpolant I (and (not (= reg_a (_ bv0 32))) (= reg_a (_ bv1 32))))
(exit)
";
        assert_eq!(got, expected);
    }

    #[test]
    fn r5_subitem_32_render_is_deterministic_across_calls() {
        // The function MUST be pure + deterministic. Calling it
        // twice with the same input must produce byte-identical
        // output (BTreeSet iteration order is sorted/stable).
        let preds = vec![
            pred("p_c", "reg_c", 5),
            pred("p_a", "reg_a", 1),
            pred("p_b", "reg_b", 3),
        ];
        let q1 = build_interpolation_query(&preds, 5, 2);
        let q2 = build_interpolation_query(&preds, 5, 2);
        assert_eq!(q1, q2, "build_interpolation_query MUST be deterministic");
        // And the declare-fun order MUST be alphabetical by
        // register name (BTreeSet sort), even though predicates
        // were given in c, a, b order.
        let lines: Vec<&str> = q1.lines().collect();
        let declare_lines: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with("(declare-fun"))
            .copied()
            .collect();
        assert_eq!(
            declare_lines,
            vec![
                "(declare-fun reg_a () (_ BitVec 32))",
                "(declare-fun reg_b () (_ BitVec 32))",
                "(declare-fun reg_c () (_ BitVec 32))",
            ],
            "declare-fun lines MUST be sorted alphabetically by register name"
        );
    }

    #[test]
    fn r5_subitem_32_render_cube_index_bit_convention_matches_lifter() {
        // The cube-index bit convention MUST match the lifter's
        // (kmts_lift.rs ~line 587):
        //   predicate at position `i` holds at cube `c` iff
        //   `(c >> i) & 1 == 1`.
        // For |P| = 3, cube 5 (binary 101) = predicates at
        // positions 0 and 2 hold, position 1 doesn't.
        let preds = vec![
            pred("p0", "reg_x", 10),
            pred("p1", "reg_y", 20),
            pred("p2", "reg_z", 30),
        ];
        let got = build_interpolation_query(&preds, 5, 0);
        // Source = cube 5 = (p0 holds, p1 doesn't, p2 holds)
        //        = (reg_x == 10) AND (reg_y != 20) AND (reg_z == 30)
        assert!(
            got.contains(
                "(assert (and (= reg_x (_ bv10 32)) (not (= reg_y (_ bv20 32))) (= reg_z (_ bv30 32))))"
            ),
            "source-cube assertion MUST encode cube index 5 = binary 101 correctly; got:\n{got}"
        );
    }

    // ─────────────────────────────────────────────────────────────
    // R.5 Item 3 sub-item 3.3 tests — CVC5 subprocess invocation
    // + interpolant parsing. Parser tests run unconditionally;
    // the subprocess integration test is gated on CVC5 binary
    // availability.
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn r5_subitem_33_parse_interpolant_binary_literal() {
        // CVC5's `(get-interpolant I ...)` output for a simple
        // equality interpolant.
        let stdout = "(define-fun I () Bool (= reg_a #b00000000000000000000000000000000))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(
            got,
            Some(PredicateSpec {
                name: "craig_interp".to_string(),
                register: "reg_a".to_string(),
                value: 0,
            }),
        );
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_hex_literal() {
        // Hex form: #x notation. 0x0a = 10 decimal.
        let stdout = "(define-fun I () Bool (= reg_b #x0a))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(
            got,
            Some(PredicateSpec {
                name: "craig_interp".to_string(),
                register: "reg_b".to_string(),
                value: 10,
            }),
        );
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_underscored_decimal() {
        // (_ bv<n> <w>) form.
        let stdout = "(define-fun I () Bool (= reg_c (_ bv42 32)))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(
            got,
            Some(PredicateSpec {
                name: "craig_interp".to_string(),
                register: "reg_c".to_string(),
                value: 42,
            }),
        );
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_returns_none_on_compound_and() {
        // A compound `(and A B)` decodes to PredicateExpr::And (see
        // `parse_interpolant_to_predicate_expr`), but the PredicateSpec shim
        // only carries a single REG==VALUE atom, so it declines the compound.
        let stdout = "(define-fun I () Bool (and (= reg_a #b0) (= reg_b #b1)))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(got, None);
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_returns_none_on_negation() {
        // A negation decodes to PredicateExpr::Not, but the PredicateSpec shim
        // only carries a positive REG==VALUE atom, so it declines it here.
        let stdout = "(define-fun I () Bool (not (= reg_a #b00)))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(got, None);
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_returns_none_on_trivial_true() {
        // Trivial `true` interpolant — no useful refinement
        // signal.
        let stdout = "(define-fun I () Bool true)\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(got, None);
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_returns_none_on_trivial_false() {
        let stdout = "(define-fun I () Bool false)\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(got, None);
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_returns_none_when_marker_absent() {
        // CVC5 sometimes emits `(error "...")` instead of a
        // define-fun when no interpolant exists. The parser
        // returns None.
        let stdout = "(error \"No interpolant exists for the given query.\")\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert_eq!(got, None);
    }

    #[test]
    fn r5_subitem_33_parse_interpolant_handles_prelude_lines() {
        // CVC5 may echo set-info / set-option lines before the
        // get-interpolant response. The parser scans for the
        // `(define-fun I` marker.
        let stdout = "(:status sat)\n\
                      ; some comment\n\
                      (define-fun I () Bool (= reg_a #b00))\n";
        let got = parse_cvc5_interpolant_response(stdout);
        assert!(got.is_some(), "parser must skip prelude lines; got None");
        assert_eq!(got.unwrap().register, "reg_a");
    }

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

    #[test]
    fn interp_expr_unwraps_and_true_reg_value() {
        // cvc5 1.3.4 wraps a REG==VALUE interpolant as `(and (= a #b..) true)`.
        let stdout = "(define-fun I () Bool (and (= a #b00000101) true))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::Cmp {
                register: "a".to_string(),
                op: CmpOp::Eq,
                value: 5,
            })
        );
        // …and the PredicateSpec shim now decodes it too (the 1.3.4 fix).
        let spec = parse_cvc5_interpolant_response(stdout).expect("REG==VALUE decodes");
        assert_eq!(spec.register, "a");
        assert_eq!(spec.value, 5);
    }

    #[test]
    fn interp_expr_reg_eq_reg_relation() {
        // `(and (= a b) true)` — a REG==REG relation (emergent relational K).
        let stdout = "(define-fun I () Bool (and (= a b) true))\n";
        assert_eq!(
            parse_interpolant_to_predicate_expr(stdout),
            Some(PredicateExpr::CmpReg {
                lhs: "a".to_string(),
                op: CmpOp::Eq,
                rhs: "b".to_string(),
            })
        );
        // A REG==REG relation is not a PredicateSpec — the shim declines it.
        assert_eq!(parse_cvc5_interpolant_response(stdout), None);
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

    #[test]
    #[ignore = "requires cvc5 binary installed; run with --ignored when available"]
    fn r5_subitem_33_invoke_cvc5_for_known_interpolation_query() {
        // R.5 Item 3 sub-item 3.3 — end-to-end integration
        // test. Ignored by default; run with `cargo test --
        // --ignored` after `brew install cvc5`.
        //
        // Builds the query from sub-item 3.2's renderer for a
        // simple source/target cube pair, invokes CVC5,
        // verifies the parsed interpolant matches expectations.
        let bin = locate_cvc5().expect("CVC5 must be available for this test");
        let preds = vec![PredicateSpec {
            name: "p_zero".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        // Source cube 1 = (reg_a == 0); target cube 0 = (reg_a != 0).
        // CVC5 should produce an interpolant like (= reg_a #b0...0).
        let query = build_interpolation_query(&preds, 1, 0);
        let result = invoke_cvc5_for_interpolant(&bin, &query, &InterpolantQueryOptions::default());
        match result {
            Ok(Some(spec)) => {
                assert_eq!(spec.register, "reg_a");
                assert_eq!(spec.value, 0);
            }
            Ok(None) => {
                panic!("expected CVC5 to produce an interpolant for source=1 target=0; got None")
            }
            Err(e) => panic!("invoke_cvc5_for_interpolant failed: {}", e.message),
        }
    }

    #[test]
    #[ignore = "requires cvc5 binary installed; run with --ignored when available"]
    fn r5_subitem_33_invoke_cvc5_timeout_kills_subprocess() {
        // R.5 Item 3 sub-item 3.3 — timeout integration test.
        // Pass a 1ms timeout that's guaranteed to be exceeded
        // even by CVC5's startup time; verifies the subprocess
        // is killed + the function returns Err.
        let bin = locate_cvc5().expect("CVC5 must be available for this test");
        let preds = vec![PredicateSpec {
            name: "p".to_string(),
            register: "reg_a".to_string(),
            value: 0,
        }];
        let query = build_interpolation_query(&preds, 1, 0);
        let opts = InterpolantQueryOptions {
            timeout: Duration::from_millis(1),
        };
        let result = invoke_cvc5_for_interpolant(&bin, &query, &opts);
        match result {
            Err(e) => {
                assert!(
                    e.message.contains("timeout"),
                    "expected timeout error; got: {}",
                    e.message
                );
            }
            Ok(_) => panic!("expected timeout Err; got Ok"),
        }
    }
}
