//! XL.1b/XL.1c — Tier-1 SVA → mu-calculus translation over slang's `--ast-json`.
//!
//! Walks the JSON [`crate::adapter::slang::run_ast_json`] produces, finds every
//! `ConcurrentAssertion`, and translates the Tier-1 fragment to a mu-calculus
//! formula string that [`crate::mu_calculus::parser::parse`] accepts and the
//! cube/CEGAR evaluator can check.
//!
//! **Tier-1 fragment + encoding** (the [XL.0] schema mapping):
//!
//! | SVA | mu-calculus |
//! |---|---|
//! | `assert property (b)` | `nu X. (b && [] X)` (AG b) |
//! | `a \|-> b` | `nu X. ((!a \|\| b) && [] X)` (AG(a→b)) |
//! | `a \|=> b` | `nu X. ((!a \|\| [] b) && [] X)` (AG(a→AX b)) |
//! | `a \|-> ##k b` (XL.4) | `nu X. ((!a \|\| []…[] b) && [] X)` — k boxes (AG(a→AXᵏ b)) |
//! | `a \|-> ##[m:n] b` (XL.4) | `nu X. ((!a \|\| []ᵐ(b\|\|[](b\|\|…\|\|[]b))) && [] X)` — b within [m,n] |
//! | `cover property (b)` | `mu X. (b \|\| <> X)` (EF b) |
//! | `cover property (b)` **recoverability lens** (XL.2) | `nu Y. ((mu X. (b \|\| <> X)) && [] Y)` (AG EF b) |
//! | `disable iff (r) P` | gate the body: `(r \|\| body)` (vacuous while disabled) |
//! | **Tier-2 history (XL.3)** `$stable(x)` | `(x == x__past)` |
//! | `$changed(x)` | `(x != x__past)` |
//! | `$rose(x)` (1-bit) | `(x && !x__past)` |
//! | `$fell(x)` (1-bit) | `(!x && x__past)` |
//! | `$past(x)` (depth 1) | the shadow atom `x__past` (a value; used inside a comparison) |
//!
//! Implication `a → b` is emitted as `!a || b` — the mu-calculus parser has no
//! `->` operator, and the rewrite is exact.
//!
//! **Tier-2 shadow registers (XL.3).** `$past`/`$stable`/`$changed`/`$rose`/
//! `$fell` read a signal's *previous-cycle* value, encoded as an atom over a
//! 1-step shadow `<sig>__past`. The translator emits the atom and records the
//! requirement in [`TranslationReport::required_shadows`]; the BTOR2
//! model-augmentation step (XL.3b) synthesises the shadow flop
//! (`next(<sig>__past) = <sig>`) so the atom binds. The Tier-2 rewrites are
//! exact (added flops, no abstraction). `$past` depth > 1 is rejected.
//!
//! **Atoms (XL.1b + XL.1c).** A boolean leaf is one of:
//! - a 1-bit signal (`NamedValue` → identifier atom);
//! - a multi-bit signal in boolean position (vector → `(sig != 0)`, the implicit
//!   reduction-or SV applies to a vector condition);
//! - a comparison `sig OP k` (`==`/`!=`/`<`/`>`/`<=`/`>=` against an integer);
//! - a **reduction** (XL.1c): `|x`→`(x != 0)`, `~|x`→`(x == 0)`, `&x`→`(x == 2^W-1)`,
//!   `~&x`→`(x != 2^W-1)` (width `W` from the operand's slang type);
//! - a **compare-to-zero of a boolean expr** (XL.1c): `bexpr !== '0`→`bexpr`,
//!   `bexpr === '0`→`!bexpr` — this is how OpenTitan's `` `ASSERT `` macro encodes
//!   `disable iff ((!rst_ni) !== '0)`.
//!
//! Every XL.1c rewrite above is **exact** (semantics-preserving), so it adds no
//! new soundness regime over XL.1b. Boolean structure (`!`, `&&`, `||`) recurses.
//! `$onehot0(x)` / `$onehot(x)` expand to the exact value-set predicate
//! `x ∈ {0,1,2,4,…}` / `x ∈ {1,2,4,…}` (one `Or`-of-`Cmp` atom). Anything still
//! outside the fragment — bit-arithmetic, bit-select indexing (`sig[i]`),
//! reduction-or/xor (`|x`, `^x`), system calls (`$isunknown`, `$countones`),
//! unbounded `##[m:$]` / `[*n:$]`, goto `[->n]` / non-consecutive `[=n]`
//! repetition, and sequence *antecedents* (`a ##1 c |-> …`), etc. — is **rejected
//! with a reason, never silently dropped** (claims-integrity), and every emitted
//! formula is validated through the mu-calculus parser as a safety net. (Bounded
//! sequence *consequents* — `##k`/`##[m:n]` delays, fixed `[*n]` repetition, and
//! multi-element `##` chains of booleans — ARE supported: XL.4, `consequent_expr`.)
//!
//! [XL.0]: ../../../../.claude/plans/measurements/XL-0-sva-parser-spike-2026-06-26.md

use serde_json::Value;

use crate::adapter::{AdapterError, AdapterErrorKind};

/// The three concurrent-assertion kinds slang reports (`assertionKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvaKind {
    Assert,
    Assume,
    Cover,
}

/// A successfully-translated assertion.
#[derive(Debug, Clone)]
pub struct TranslatedAssertion {
    /// Positional name: `<module>_sva_<index>`. Stable within one run, but an insertion
    /// mid-file re-points every later name — see [`Self::label`], which does not move.
    pub name: String,
    /// mununu#544 — the assertion's SV **label**, when it has one.
    ///
    /// The label is what a human wrote (`a_reseed_only_at_a_span_edge:`), so it survives a
    /// reorder and a rename becomes a hard error instead of a silent re-point. Consumers should
    /// prefer it over [`Self::name`] when pinning an expectation.
    ///
    /// `None` for a genuinely unlabelled assertion, which keeps only its positional name.
    pub label: Option<String>,
    pub kind: SvaKind,
    /// mu-calculus formula; guaranteed to parse via
    /// [`crate::mu_calculus::parser::parse`].
    pub formula: String,
    /// XL.2 (= Track I.3) recoverability companion — `Some` only for `Cover`
    /// (`EF φ`) assertions. Carries the `AG EF φ` lens
    /// `nu Y. ((mu X. (φ || <>X)) && [] Y)`: "from every reachable state, φ is
    /// still reachable." Surfaces the Track-B recoverability wedge directly from
    /// the design's own covers — a branching question the SVA author could not
    /// phrase. Sound at this νμ alternation only on the must-edge path
    /// (`btor2 cegar --must-edge-inference smt-hyper-must`; cf. the V.7-c csrng
    /// showcase). Forming it is XL.2; auto-checking it on a TRUE cover and
    /// attaching the I.1 countertrace is the XL.6 endpoint's job.
    pub recoverability_companion: Option<String>,
}

/// An assertion outside the Tier-1 fragment, recorded (never silently dropped).
#[derive(Debug, Clone)]
pub struct UnsupportedAssertion {
    pub name: String,
    pub kind: Option<SvaKind>,
    pub reason: String,
}

/// XL.3 (Tier-2): a base signal whose previous-cycle value a translated
/// assertion needs. The BTOR2 model-augmentation step (XL.3b) must synthesise a
/// `depth`-stage shadow shift chain for it — `next(<base>__past) = <base>`,
/// `next(<base>__past2) = <base>__past`, … up to `<base>__past{depth}` — so the
/// shadow atoms the translator emits (`past_shadow_name(base, k)` for every
/// `k <= depth`) actually bind. `depth` is the DEEPEST history any assertion
/// reads for this base; the chain covers every shallower `$past(base, j)` too.
/// Reported only for *successfully* translated assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowSignal {
    pub base: String,
    pub width: u32,
    /// Deepest `$past` depth read for `base` (1 for `$stable`/`$rose`/… and
    /// bare `$past(base)`; `k` for `$past(base, k)`).
    pub depth: u32,
}

/// A reset signal recognized in a `disable iff (...)` guard: the input to pin
/// to verify the running (post-reset) design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetSignal {
    /// The reset input signal name (matches the lifted BTOR2 input).
    pub signal: String,
    /// The value that makes the disable condition FALSE — i.e. reset *inactive*
    /// (the design running). `1` for active-low (`disable iff (!rst_n)`); `0`
    /// for active-high (`disable iff (rst)`). Pin the input here so the body is
    /// verified only while not in reset, matching `disable iff` semantics.
    pub inactive_value: u64,
}

/// Result of translating a whole `--ast-json` document.
#[derive(Debug, Clone, Default)]
pub struct TranslationReport {
    pub translated: Vec<TranslatedAssertion>,
    pub unsupported: Vec<UnsupportedAssertion>,
    /// XL.3: 1-step `__past` shadow registers the translated formulas reference
    /// (deduped by base). The XL.3b BTOR2 augmentation consumes this; an empty
    /// vec means no Tier-2 history was used.
    pub required_shadows: Vec<ShadowSignal>,
    /// Reset signals recognized in `disable iff` guards (deduped). Always
    /// recorded; whether the guard is dropped from the formula is controlled by
    /// [`TranslateOptions::gate_reset`]. The verify-auto path pins these inputs
    /// inactive at the model level.
    pub reset_signals: Vec<ResetSignal>,
    /// Every `parameter` / `localparam` NAME declared anywhere in the design (from
    /// the slang AST). The verify-auto `--param` path validates each requested
    /// override name against this set — the slang front-end's `-G` override
    /// silently ignores an unknown name, so this is where an unapplicable `--param`
    /// is turned into a hard error rather than a silent drop. Deduped.
    pub parameter_names: Vec<String>,
}

/// Options controlling translation.
#[derive(Debug, Clone, Default)]
pub struct TranslateOptions {
    /// When `true`, a recognizable `disable iff (reset)` guard is **dropped**
    /// from the property body — the consumer is expected to pin the reset input
    /// inactive at the model level instead (the general form of V.7-c's
    /// `connect -set rst_ni 1'b1`). Dropping the guard removes the otherwise
    /// unbindable reset-input atom, so the body becomes a pure state-cell
    /// property. When `false` (default), the guard is kept as
    /// `(reset_cond || body)`. Either way, detected resets are recorded in
    /// [`TranslationReport::reset_signals`].
    pub gate_reset: bool,
}

impl TranslationReport {
    /// Total concurrent assertions seen (translated + unsupported).
    pub fn total(&self) -> usize {
        self.translated.len() + self.unsupported.len()
    }
}

/// Translate every concurrent assertion in a slang `--ast-json` document.
pub fn translate_ast_json(json: &str) -> Result<TranslationReport, AdapterError> {
    translate_ast_json_with_options(json, &TranslateOptions::default())
}

/// Translate every concurrent assertion, honoring [`TranslateOptions`] (e.g.
/// `gate_reset` to drop `disable iff` guards for model-level reset pinning).
pub fn translate_ast_json_with_options(
    json: &str,
    opts: &TranslateOptions,
) -> Result<TranslationReport, AdapterError> {
    // slang emits deeply-nested ASTs on some designs (e.g. `wishbone_high_performance_z80`),
    // and serde_json's default 128-level recursion limit rejects them. The AST is trusted
    // (our own `slang --ast-json` invocation), so disable the limit — the actual nesting is
    // bounded by the design's expression depth, well within the thread stack.
    let root: Value = {
        use serde::Deserialize as _;
        let mut de = serde_json::Deserializer::from_str(json);
        de.disable_recursion_limit();
        Value::deserialize(&mut de).map_err(|e| AdapterError {
            kind: AdapterErrorKind::ParseError,
            message: format!("adapter/slang/translate: --ast-json is not valid JSON: {e}"),
            location: None,
        })?
    };

    let mut found: Vec<(String, Option<String>, &Value)> = Vec::new();
    collect_assertions(&root, "design", None, &mut found);

    // XL.6b follow-up — enum-constant resolution. slang keeps an enum-member
    // reference (e.g. `MainSmError`) as a `NamedValue`, NOT folded to its value,
    // so `state_q == MainSmError` reads as signal-vs-signal. Map every enum
    // member to its integer value so the pre-pass can rewrite those references
    // to integer literals (enum-typed FSMs are ubiquitous in real RTL).
    let enum_values = collect_enum_values(&root);

    let mut report = TranslationReport::default();
    for (idx, (module, label, node)) in found.iter().enumerate() {
        let name = format!("{module}_sva_{idx}");
        let label = label.clone();
        let kind = match node.get("assertionKind").and_then(Value::as_str) {
            Some("Assert") => SvaKind::Assert,
            Some("Assume") => SvaKind::Assume,
            Some(s) if s.starts_with("Cover") => SvaKind::Cover,
            other => {
                report.unsupported.push(UnsupportedAssertion {
                    name,
                    kind: None,
                    reason: format!("unknown assertionKind: {other:?}"),
                });
                continue;
            }
        };
        let Some(spec) = node.get("propertySpec") else {
            report.unsupported.push(UnsupportedAssertion {
                name,
                kind: Some(kind),
                reason: "assertion has no propertySpec".to_string(),
            });
            continue;
        };
        // Rewrite enum-member references to integer literals before translating.
        let spec = resolve_enum_refs(spec, &enum_values);
        let spec = &spec;
        match translate_one(spec, kind, opts, &mut report.reset_signals) {
            Ok(formula) => {
                // XL.2: a cover's `EF φ` gets its `AG EF φ` recoverability lens.
                // (Omitted if the companion somehow fails to parse — never ship
                // a formula the evaluator can't read.)
                let recoverability_companion = (kind == SvaKind::Cover)
                    .then(|| recoverability_companion_formula(&formula).ok())
                    .flatten();
                // XL.3: record the `__past` shadow flops this assertion needs
                // (only for assertions that actually translated).
                collect_shadow_signals(spec, &mut report.required_shadows);
                report.translated.push(TranslatedAssertion {
                    name,
                    label,
                    kind,
                    formula,
                    recoverability_companion,
                });
            }
            Err(reason) => report.unsupported.push(UnsupportedAssertion {
                name,
                kind: Some(kind),
                reason,
            }),
        }
    }
    // Design-wide parameter names (for `--param` override validation), collected
    // once from the whole AST rather than per-assertion.
    collect_parameter_names(&root, &mut report.parameter_names);
    Ok(report)
}

/// Collect every `parameter` / `localparam` NAME in a slang `--ast-json` document
/// (slang serialises both as a `"kind": "Parameter"` node with a `"name"`). Used
/// to validate `--param` override names — the slang `-G` override silently ignores
/// an unknown one, so an unapplicable override must be caught here. Deduped.
fn collect_parameter_names(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("Parameter")
                && let Some(name) = map.get("name").and_then(Value::as_str)
                && !name.is_empty()
                && !out.iter().any(|n| n == name)
            {
                out.push(name.to_string());
            }
            for v in map.values() {
                collect_parameter_names(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_parameter_names(v, out);
            }
        }
        _ => {}
    }
}

/// Recursively collect `ConcurrentAssertion` nodes, tracking the nearest
/// enclosing `name` (the module) for a best-effort assertion name.
/// mununu#544 — recover an assertion's SV label from slang's `--ast-json`.
///
/// The label is NOT a field on the `ConcurrentAssertion` node, which carries only
/// `assertionKind` / `ifTrue` / `kind` / `propertySpec` — which is why it was once believed
/// absent. slang emits a labelled statement as an enclosing `Block` whose `block` field is the
/// symbol-table entry, rendered as `"<address> <label>"`:
///
/// ```text
/// ProceduralBlock → body: Block { block: "6338700059712 ap_bool", body: ConcurrentAssertion }
/// ```
///
/// So the nearest enclosing `Block` names the assertion. Returns `None` when the field carries
/// no identifier tail — an UNLABELLED assertion, whose tail is the bare address. A leading digit
/// is a reliable discriminator because an SV identifier cannot start with one.
fn block_label(block: &str) -> Option<String> {
    let tail = block.rsplit(' ').next()?;
    let mut chars = tail.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return None;
    }
    Some(tail.to_string())
}

/// Collect every `ConcurrentAssertion`, with the module that encloses it and its SV label (see
/// [`block_label`]) when it has one.
fn collect_assertions<'a>(
    node: &'a Value,
    enclosing: &str,
    label: Option<&str>,
    out: &mut Vec<(String, Option<String>, &'a Value)>,
) {
    match node {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("ConcurrentAssertion") {
                out.push((enclosing.to_string(), label.map(str::to_string), node));
            }
            // Update the enclosing name for descendants when this node names one.
            let next = map
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(enclosing);
            // A `Block` carrying a symbol-table entry re-labels its descendants; the NEAREST
            // enclosing one wins, so a nested block shadows an outer label rather than leaking it.
            let owned = (map.get("kind").and_then(Value::as_str) == Some("Block"))
                .then(|| {
                    map.get("block")
                        .and_then(Value::as_str)
                        .and_then(block_label)
                })
                .flatten();
            let next_label = owned.as_deref().or(label);
            for v in map.values() {
                collect_assertions(v, next, next_label, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_assertions(v, enclosing, label, out);
            }
        }
        _ => {}
    }
}

/// XL.6b follow-up — collect every enum member's integer value (`name → value`)
/// from the `--ast-json` `EnumValue` nodes (`value` is an SV literal like
/// `6'b101001`). Lets [`resolve_enum_refs`] fold enum-member references in
/// comparisons (`state_q == MainSmError`) to integer literals.
fn collect_enum_values(node: &Value) -> std::collections::HashMap<String, i64> {
    fn walk(n: &Value, out: &mut std::collections::HashMap<String, i64>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("EnumValue")
                    && let Some(name) = m.get("name").and_then(Value::as_str)
                    && let Some(v) = m
                        .get("value")
                        .and_then(Value::as_str)
                        .and_then(parse_sv_literal)
                {
                    out.entry(name.to_string()).or_insert(v);
                }
                for v in m.values() {
                    walk(v, out);
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::HashMap::new();
    walk(node, &mut out);
    out
}

/// XL.6b follow-up — deep-clone an expression tree, replacing every `NamedValue`
/// referencing an enum member (by name, in `enums`) with an `IntegerLiteral` of
/// its value. A signal *of* an enum type (e.g. `state_q`) is not an enum-member
/// name, so it is left untouched — only the constants fold.
fn resolve_enum_refs(node: &Value, enums: &std::collections::HashMap<String, i64>) -> Value {
    match node {
        Value::Object(m) => {
            if m.get("kind").and_then(Value::as_str) == Some("NamedValue")
                && let Some(sym) = m.get("symbol").and_then(Value::as_str)
                && let Some(&v) = enums.get(sym.rsplit(' ').next().unwrap_or(sym))
            {
                return serde_json::json!({
                    "kind": "IntegerLiteral",
                    "constant": v.to_string(),
                    "value": v.to_string(),
                });
            }
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                obj.insert(k.clone(), resolve_enum_refs(v, enums));
            }
            Value::Object(obj)
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| resolve_enum_refs(v, enums)).collect()),
        other => other.clone(),
    }
}

/// XL.3: walk a (translated) assertion's spec and record every base signal a
/// Tier-2 history call (`$past`/`$stable`/`$changed`/`$rose`/`$fell`) reads, so
/// the BTOR2 augmentation knows which `__past` shadow flops to synthesise.
/// Deduped by base name. Only call on specs that translated successfully.
fn collect_shadow_signals(node: &Value, out: &mut Vec<ShadowSignal>) {
    match node {
        Value::Object(map) => {
            let sub = map.get("subroutine").and_then(Value::as_str);
            if map.get("kind").and_then(Value::as_str) == Some("Call")
                && matches!(
                    sub,
                    Some("$past" | "$stable" | "$changed" | "$rose" | "$fell")
                )
            {
                // `$past` carries a depth; the others are depth-1. Record the
                // DEEPEST depth per base (the shift chain covers all shallower).
                let resolved = if sub == Some("$past") {
                    past_call_arg(node).ok()
                } else {
                    call_arg_signal(node).ok().map(|(b, w)| (b, w, 1))
                };
                if let Some((base, width, depth)) = resolved {
                    if let Some(existing) = out.iter_mut().find(|s| s.base == base) {
                        existing.depth = existing.depth.max(depth);
                    } else {
                        out.push(ShadowSignal {
                            base: base.to_string(),
                            width,
                            depth,
                        });
                    }
                }
            }
            for v in map.values() {
                collect_shadow_signals(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_shadow_signals(v, out);
            }
        }
        _ => {}
    }
}

/// Translate one assertion's `propertySpec` to a full mu-calculus formula, then
/// validate it parses. `Err(reason)` for any out-of-fragment construct.
fn translate_one(
    spec: &Value,
    kind: SvaKind,
    opts: &TranslateOptions,
    reset_signals: &mut Vec<ResetSignal>,
) -> Result<String, String> {
    let body = property_body(spec, opts, reset_signals)?;
    let formula = match kind {
        SvaKind::Assert | SvaKind::Assume => format!("nu X. (({body}) && [] X)"),
        SvaKind::Cover => format!("mu X. (({body}) || <> X)"),
    };
    // Safety net: only ship formulas the evaluator's parser accepts.
    crate::mu_calculus::parser::parse(&formula)
        .map_err(|e| format!("emitted formula failed to parse ({e:?}): {formula}"))?;
    Ok(formula)
}

/// XL.2 (= Track I.3): the *recoverability companion* of a cover's `EF φ`.
///
/// Wraps the already-formed `EF φ` (`mu X. (φ || <>X)`) into `AG EF φ`
/// = `nu Y. ((EF φ) && [] Y)` — "from every reachable state, φ is still
/// reachable." The formula is identical in shape to the one the V.7-c csrng
/// recoverability showcase checks by hand; this just forms it automatically
/// from a `cover` assertion. Validated through the mu-calculus parser so a cover
/// only carries a companion the evaluator can actually run.
fn recoverability_companion_formula(ef_formula: &str) -> Result<String, String> {
    let companion = format!("nu Y. (({ef_formula}) && [] Y)");
    crate::mu_calculus::parser::parse(&companion)
        .map_err(|e| format!("recoverability companion failed to parse ({e:?}): {companion}"))?;
    Ok(companion)
}

/// Recognize a `disable iff` condition that is a single (possibly negated,
/// possibly `!= 0`-wrapped) reset signal, and return the signal + the value
/// that makes the condition FALSE (reset inactive). Returns `None` for complex
/// conditions — those are left as a kept guard rather than gated, so we never
/// silently mis-pin a multi-signal disable condition.
///
/// Recognized shapes (the dominant SVA idioms):
/// - `rst` (1-bit `NamedValue`) → active-high; inactive value `0`.
/// - `!rst_n` (`LogicalNot`/`BitwiseNot` of a 1-bit signal) → active-low;
///   inactive value `1`.
/// - `(<lhs>) !== '0` (`Inequality`/`CaseInequality` with a `0` RHS — the
///   OpenTitan ``ASSERT`` macro form) → same polarity as `<lhs>`.
fn extract_reset_signal(cond: &Value) -> Option<ResetSignal> {
    let cond = unwrap(cond);
    let kind = cond.get("kind").and_then(Value::as_str)?;
    match kind {
        "NamedValue" => {
            if signal_width(cond) != 1 {
                return None;
            }
            let name = signal_name(cond).ok()?;
            Some(ResetSignal {
                signal: name.to_string(),
                inactive_value: 0,
            })
        }
        "UnaryOp" => {
            let op = cond.get("op").and_then(Value::as_str)?;
            if !matches!(op, "LogicalNot" | "BitwiseNot") {
                return None;
            }
            let operand = unwrap(child(cond, "operand").ok()?);
            if operand.get("kind").and_then(Value::as_str)? != "NamedValue"
                || signal_width(operand) != 1
            {
                return None;
            }
            let name = signal_name(operand).ok()?;
            Some(ResetSignal {
                signal: name.to_string(),
                inactive_value: 1,
            })
        }
        // `(<lhs>) !== '0` — the `!= 0` wrapper preserves the LHS's truth, so
        // the reset polarity is the LHS's.
        "BinaryOp" => {
            let op = cond.get("op").and_then(Value::as_str)?;
            if !matches!(op, "Inequality" | "CaseInequality") {
                return None;
            }
            let right = unwrap(child(cond, "right").ok()?);
            if sv_integer(right) != Some(0) {
                return None;
            }
            extract_reset_signal(child(cond, "left").ok()?)
        }
        _ => None,
    }
}

/// Translate an `AssertionExpr` into the per-step propositional body (the part
/// inside `nu X. (BODY && []X)` / `mu X. (BODY || <>X)`).
fn property_body(
    spec: &Value,
    opts: &TranslateOptions,
    reset_signals: &mut Vec<ResetSignal>,
) -> Result<String, String> {
    let k = spec.get("kind").and_then(Value::as_str).unwrap_or("?");
    match k {
        // `@(posedge clk) P` — one CLTS step is one clock edge; strip the clock.
        "Clocking" => property_body(child(spec, "expr")?, opts, reset_signals),
        // `disable iff (r) P` — vacuously true while disabled: `(r || body)`.
        // When `gate_reset` and the condition is a recognizable single-signal
        // reset, drop the guard (consumer pins the reset inactive at the model
        // level) and record the reset signal. Detected resets are recorded
        // regardless of gating.
        "DisableIff" => {
            let cond_node = child(spec, "condition")?;
            let inner = property_body(child(spec, "expr")?, opts, reset_signals)?;
            if let Some(rs) = extract_reset_signal(cond_node) {
                if !reset_signals.contains(&rs) {
                    reset_signals.push(rs);
                }
                if opts.gate_reset {
                    return Ok(inner);
                }
            }
            let cond = bool_expr(cond_node)?;
            Ok(format!("({cond} || {inner})"))
        }
        // `a |-> <conseq>` / `a |=> <conseq>` — a boolean antecedent (Tier-1
        // `Simple`) implies a boolean OR a bounded cycle-delay consequent
        // (`##k b` / `##[m:n] b`, XL.4). The antecedent stays boolean; a sequence
        // antecedent is out of fragment (Tier-3).
        "Binary" => {
            let op = spec.get("op").and_then(Value::as_str).unwrap_or("?");
            let conseq = consequent_expr(child(spec, "right")?)?;
            // The consequent aligns to the antecedent's match end; `|=>` delays it
            // one further cycle. `|->` evaluates it there directly.
            let conseq = match op {
                "OverlappedImplication" => conseq,
                "NonOverlappedImplication" => format!("[] {conseq}"),
                other => return Err(format!("unsupported property operator: {other}")),
            };
            antecedent_implies(child(spec, "left")?, &conseq)
        }
        "Simple" => bool_expr(child(spec, "expr")?),
        other => Err(format!("unsupported property kind: {other}")),
    }
}

/// The maximum cycle any bounded-sequence unroll may reach — each cycle is a
/// modality, so a runaway `##[m:$]` / `[*n:$]` (slang encodes `$` as a string
/// `max`, caught earlier) or an absurd fixed count is rejected rather than
/// exploding the formula.
const MAX_SEQ_UNROLL: u64 = 64;

/// XL.4 — an implication *consequent*: a boolean leaf (`Simple`), a bounded
/// cycle-delay (`##k b` / `##[m:n] b`), a bounded consecutive repetition
/// (`b[*n]`), or a bounded multi-element `##` chain of length-1 boolean elements
/// (`b ##1 c ##2 d`). slang serialises these as a `Simple` (optionally with a
/// `repetition` field) or a `SequenceConcat` (`elements: [{sequence, min, max}]`,
/// cumulative gap delays — verified against slang 11.0.0 `--ast-json`).
///
/// Lowering (all sound: `[]φ = ∀ may-successors`, so a definite verdict transfers,
/// Bruns–Godefroid — the boxes only lengthen the still-step-exact window):
/// - fixed `##k b` → `[]…[] b` (k boxes); range `##[m:n] b` → `[]^m(b||[](b||…))`;
/// - `b[*n]` → `(b && [](b && … && [] b))` (n consecutive);
/// - `b ##δ₁ c ##δ₂ d` → `b && []^δ₁(c && []^δ₂ d)` (each element length-1).
///
/// Out of fragment (Tier-3b — rejected with a reason, never dropped): unbounded
/// `##[m:$]` / `[*n:$]` (needs a sequence automaton), goto `[->n]` / non-
/// consecutive `[=n]` repetition, a repetition or nested sub-sequence *inside* a
/// multi-element chain (would need length composition), and sequence
/// *antecedents* (`a ##1 b |-> …` — handled in the `Binary` arm, still boolean).
fn consequent_expr(spec: &Value) -> Result<String, String> {
    match spec.get("kind").and_then(Value::as_str) {
        // A boolean leaf, possibly with a consecutive repetition (`b[*n]`).
        Some("Simple") => seq_leaf(spec),
        Some("SequenceConcat") => {
            let elements = spec
                .get("elements")
                .and_then(Value::as_array)
                .ok_or("SequenceConcat without an `elements` array")?;
            if elements.is_empty() {
                return Err("empty SequenceConcat".into());
            }
            if elements.len() == 1 {
                // `##k <seq>` / `##[m:n] <seq>` — a single delayed sub-sequence
                // (a boolean, or a consecutive repetition of one).
                let el = &elements[0];
                let seq = el
                    .get("sequence")
                    .ok_or("sequence element without a `sequence`")?;
                let leaf = seq_leaf(seq)?;
                let (min, max) = elem_delay(el)?;
                Ok(delayed(&leaf, min, max))
            } else {
                // A multi-element `##` chain. To keep every element's cycle offset
                // unambiguous, each element must be a length-1 boolean (no
                // repetition / nested sub-sequence — those compose lengths, Tier-3b).
                let mut leaves: Vec<String> = Vec::with_capacity(elements.len());
                let mut delays: Vec<(u32, u32)> = Vec::with_capacity(elements.len());
                for el in elements {
                    let seq = el
                        .get("sequence")
                        .ok_or("sequence element without a `sequence`")?;
                    if seq.get("kind").and_then(Value::as_str) != Some("Simple")
                        || seq.get("repetition").is_some()
                    {
                        return Err("a multi-element `##` chain element must be a length-1 \
                             boolean (a repetition or nested sub-sequence inside a chain \
                             needs length composition — Tier-3b)"
                            .into());
                    }
                    leaves.push(bool_expr(child(seq, "expr")?)?);
                    delays.push(elem_delay(el)?);
                }
                // Fold from the last element: S(i) = leafᵢ && delayed(S(i+1), gapᵢ₊₁),
                // then prefix the first element's own gap.
                let n = leaves.len();
                let mut s = leaves[n - 1].clone();
                for i in (1..n).rev() {
                    let (min, max) = delays[i];
                    s = format!("({} && {})", leaves[i - 1], delayed(&s, min, max));
                }
                let (min0, max0) = delays[0];
                Ok(delayed(&s, min0, max0))
            }
        }
        other => Err(format!(
            "unsupported implication consequent (expected a boolean, a `##k`/`##[m:n]` \
             delayed boolean, a `b[*n]` repetition, or a `##` chain of booleans, got {other:?})"
        )),
    }
}

/// A single sequence leaf: a `Simple` boolean, optionally with a **consecutive**
/// repetition (`b[*n]` → n consecutive cycles). Returns a "matches starting here"
/// formula. Rejects goto / non-consecutive / range / unbounded repetition (Tier-3b).
fn seq_leaf(seq: &Value) -> Result<String, String> {
    if seq.get("kind").and_then(Value::as_str) != Some("Simple") {
        return Err(format!(
            "nested sequence element (expected a boolean leaf, got {:?}) — Tier-3b",
            seq.get("kind").and_then(Value::as_str)
        ));
    }
    let b = bool_expr(child(seq, "expr")?)?;
    match seq.get("repetition") {
        None => Ok(b),
        Some(rep) => lower_repetition(&b, rep),
    }
}

/// The fixed consecutive count `n` of a `[*n]` repetition. Only fixed consecutive
/// (`min == max == n`, n ≥ 1) is in fragment; a range `[*m:n]`, unbounded `[*n:$]`
/// (slang `max` is the string `"$"` → not a `u64`), goto `[->n]`, or non-
/// consecutive `[=n]` need a sequence automaton (Tier-3b) and are rejected.
fn fixed_consecutive_count(rep: &Value) -> Result<u32, String> {
    let kind = rep.get("kind").and_then(Value::as_str).unwrap_or("?");
    if kind != "Consecutive" {
        return Err(format!(
            "`{kind}` repetition (goto `[->n]` / non-consecutive `[=n]`) needs a \
             sequence automaton — Tier-3b"
        ));
    }
    let (Some(min), Some(max)) = (
        rep.get("min").and_then(Value::as_u64),
        rep.get("max").and_then(Value::as_u64),
    ) else {
        return Err("unbounded repetition `[*n:$]` needs a sequence automaton — Tier-3b".into());
    };
    if min != max {
        return Err(format!(
            "range repetition `[*{min}:{max}]` needs a sequence automaton — Tier-3b; \
             a fixed `[*n]` is supported"
        ));
    }
    if min == 0 {
        return Err("empty repetition `[*0]` is out of fragment".into());
    }
    if max > MAX_SEQ_UNROLL {
        return Err(format!(
            "repetition count {max} exceeds the unroll cap {MAX_SEQ_UNROLL}"
        ));
    }
    Ok(min as u32)
}

/// Lower a consecutive repetition `b[*n]` → `(b && [](b && … && [] b))` — b held n
/// consecutive cycles (a "matches starting here" formula).
fn lower_repetition(b: &str, rep: &Value) -> Result<String, String> {
    let n = fixed_consecutive_count(rep)?;
    let mut out = b.to_string();
    for _ in 1..n {
        out = format!("({b} && [] {out})");
    }
    Ok(out)
}

/// The `(min, max)` cycle-delay of a `SequenceConcat` element, validated against
/// the unroll cap and the `max ≥ min` invariant. An unbounded `$` (a string `max`)
/// is not a `u64` and is rejected as Tier-3b.
fn elem_delay(el: &Value) -> Result<(u32, u32), String> {
    let (Some(min), Some(max)) = (
        el.get("min").and_then(Value::as_u64),
        el.get("max").and_then(Value::as_u64),
    ) else {
        return Err("unbounded `##[m:$]` delay needs a sequence automaton — Tier-3b".into());
    };
    if max < min {
        return Err(format!("sequence delay range max {max} < min {min}"));
    }
    if max > MAX_SEQ_UNROLL {
        return Err(format!(
            "sequence delay {max} exceeds the bounded-unroll cap {MAX_SEQ_UNROLL}"
        ));
    }
    Ok((min as u32, max as u32))
}

/// Lower `##[min:max] f` (a delayed sub-formula `f`) to nested `[]` modalities.
/// Fixed (`min == max == k`) → `[]…[] f` (k boxes). Range → `[]^min ( f || [](f ||
/// … || [] f) )` — "f matches at some cycle in [min,max]". SOUNDNESS: over the
/// may-relation `[]φ = ∀ may-succ φ`, so a definite verdict transfers
/// (Bruns–Godefroid), exactly as the shipped `|=>` single-`[]` case. `f` may be a
/// compound sub-sequence formula (already parenthesised).
fn delayed(f: &str, min: u32, max: u32) -> String {
    let mut out = f.to_string();
    for _ in 0..(max - min) {
        out = format!("({f} || [] {out})");
    }
    for _ in 0..min {
        out = format!("[] {out}");
    }
    out
}

/// Prefix `n` box modalities: `[] [] … f`.
fn nest_boxes(n: u32, f: &str) -> String {
    let mut out = f.to_string();
    for _ in 0..n {
        out = format!("[] {out}");
    }
    out
}

/// XL.5 — an implication whose *antecedent* may be a bounded sequence:
/// `<antecedent> |-> <conseq>`, where `conseq` is the already-lowered consequent
/// (aligned to the antecedent's match end; the caller adds the `|=>` extra `[]`).
/// A boolean antecedent gives the shipped `(!a || conseq)`; a fixed-delay `##`
/// chain / fixed `[*n]` gives the nested implication
/// `!e₀ || []^δ₁(!e₁ || []^δ₂(… || conseq))` — "whenever the antecedent matches,
/// the consequent holds at the match end". SOUNDNESS: the antecedent atoms sit in
/// negative position under boxes evaluated ∀-may; the whole is an ordinary
/// mu-calculus formula the KMTS evaluator decides soundly (definite verdicts
/// transfer). A RANGE delay / range repetition in an antecedent creates multiple
/// match ends (Tier-3b) and is rejected in [`antecedent_elements`].
fn antecedent_implies(left: &Value, conseq: &str) -> Result<String, String> {
    let elems = antecedent_elements(left)?;
    let n = elems.len();
    // Innermost: the last antecedent element implies the consequent at its cycle.
    let mut acc = format!("(!({}) || {conseq})", elems[n - 1].0);
    // Walk back, prefixing each preceding element's gap-to-the-next.
    for i in (1..n).rev() {
        let (b, _) = &elems[i - 1];
        let gap = elems[i].1;
        acc = format!("(!({b}) || {})", nest_boxes(gap, &acc));
    }
    // The first element's own leading gap (usually 0).
    Ok(nest_boxes(elems[0].1, &acc))
}

/// Flatten a bounded FIXED-delay antecedent into `(boolean, gap-before)` length-1
/// elements: a boolean → one element; a fixed `a[*n]` → n elements (gaps
/// `0,1,…,1`); a fixed `##` chain of booleans → one element per link. Rejects
/// range delays / range repetition (multiple match ends — Tier-3b), goto/non-
/// consecutive/unbounded repetition, and nested sub-sequences.
fn antecedent_elements(left: &Value) -> Result<Vec<(String, u32)>, String> {
    match left.get("kind").and_then(Value::as_str) {
        Some("Simple") => {
            let b = bool_expr(child(left, "expr")?)?;
            match left.get("repetition") {
                None => Ok(vec![(b, 0)]),
                Some(rep) => {
                    let n = fixed_consecutive_count(rep)?;
                    Ok((0..n)
                        .map(|i| (b.clone(), if i == 0 { 0 } else { 1 }))
                        .collect())
                }
            }
        }
        Some("SequenceConcat") => {
            let elements = left
                .get("elements")
                .and_then(Value::as_array)
                .ok_or("SequenceConcat without an `elements` array")?;
            let mut v = Vec::with_capacity(elements.len());
            for el in elements {
                let seq = el
                    .get("sequence")
                    .ok_or("sequence element without a `sequence`")?;
                if seq.get("kind").and_then(Value::as_str) != Some("Simple")
                    || seq.get("repetition").is_some()
                {
                    return Err("a sequence-antecedent element must be a length-1 boolean \
                         (a repetition or nested sub-sequence in an antecedent is Tier-3b)"
                        .into());
                }
                let (min, max) = elem_delay(el)?;
                if min != max {
                    return Err(format!(
                        "a range delay `##[{min}:{max}]` in an ANTECEDENT creates multiple \
                         match ends — Tier-3b; a fixed `##k` antecedent is supported"
                    ));
                }
                v.push((bool_expr(child(seq, "expr")?)?, min));
            }
            if v.is_empty() {
                return Err("empty SequenceConcat antecedent".into());
            }
            Ok(v)
        }
        other => Err(format!(
            "unsupported implication antecedent (expected a boolean or a fixed `##` chain \
             / `[*n]`, got {other:?})"
        )),
    }
}

/// Translate a boolean `Expression` into mu-calculus atom/connective text.
fn bool_expr(expr: &Value) -> Result<String, String> {
    let expr = unwrap(expr);
    let k = expr.get("kind").and_then(Value::as_str).unwrap_or("?");
    match k {
        // A bare signal in boolean position: 1-bit → identifier atom; a vector
        // is implicitly reduction-or'd by SV in a condition → `(sig != 0)`.
        "NamedValue" => {
            let name = signal_name(expr)?;
            if signal_width(expr) > 1 {
                Ok(format!("({name} != 0)"))
            } else {
                Ok(name.to_string())
            }
        }
        "UnaryOp" => {
            let op = expr.get("op").and_then(Value::as_str).unwrap_or("?");
            let operand = unwrap(child(expr, "operand")?);
            match op {
                "LogicalNot" => Ok(format!("(!({}))", bool_expr(operand)?)),
                // XL.1c reductions over a vector — exact rewrites to comparisons.
                "BitwiseOr" => reduction_to_cmp(operand, "!=", false), //  |x  → x != 0
                "BitwiseNor" => reduction_to_cmp(operand, "==", false), // ~|x  → x == 0
                "BitwiseAnd" => reduction_to_cmp(operand, "==", true), //  &x  → x == all-ones
                "BitwiseNand" => reduction_to_cmp(operand, "!=", true), // ~&x → x != all-ones
                // `~x` (full bitwise-not): logical-not on a 1-bit operand only.
                "BitwiseNot" => bitwise_not(operand),
                "BitwiseXor" | "BitwiseXnor" => Err(format!(
                    "reduction-{} (parity) is not expressible as a propositional atom",
                    if op == "BitwiseXor" { "xor" } else { "xnor" }
                )),
                other => Err(format!("unsupported unary op: {other}")),
            }
        }
        "BinaryOp" => {
            let op = expr.get("op").and_then(Value::as_str).unwrap_or("?");
            match op {
                "LogicalAnd" => {
                    let l = bool_expr(child(expr, "left")?)?;
                    let r = bool_expr(child(expr, "right")?)?;
                    Ok(format!("({l} && {r})"))
                }
                "LogicalOr" => {
                    let l = bool_expr(child(expr, "left")?)?;
                    let r = bool_expr(child(expr, "right")?)?;
                    Ok(format!("({l} || {r})"))
                }
                "Equality" | "CaseEquality" => compare(expr, "=="),
                "Inequality" | "CaseInequality" => compare(expr, "!="),
                "LessThan" => compare(expr, "<"),
                "GreaterThan" => compare(expr, ">"),
                "LessThanEqual" => compare(expr, "<="),
                "GreaterThanEqual" => compare(expr, ">="),
                other => Err(format!("unsupported binary op: {other}")),
            }
        }
        // `sig[i]` — three distinct refusals, never silently dropped. mununu#514: this used to
        // report ONLY the two bit-select cases, so an ARRAY read (`mem[0]`, an unpacked
        // dimension) was reported as a "constant bit-select" and the reader was pointed at a
        // bit-level model predicate — a workaround that cannot apply to array CONTENT. Tell the
        // three apart, because they have different causes and different (non-)remedies.
        "ElementSelect" => {
            let selector = unwrap(child(expr, "selector")?);
            let base = unwrap(child(expr, "value")?);
            if is_unpacked_array(base) {
                let name = signal_name(base).unwrap_or("<expr>");
                Err(format!(
                    "`{name}` is an unpacked array, so `{name}[…]` reads array CONTENT, not a \
                     bit. mununu cannot express an array-content atom from SVA at ANY index — \
                     constant or signal. This is a translation limit, not an undecidable \
                     property: the atom never reaches an engine. See mununu#514"
                ))
            } else if sv_integer(selector).is_some() {
                Err(
                    "constant bit-select `sig[k]` needs a bit-level model predicate \
                     (H.1 auto-seeding); not a Tier-1c atom"
                        .to_string(),
                )
            } else {
                Err("dynamic bit-select `sig[idx]` (index is a signal) is not \
                     expressible as a propositional atom"
                    .to_string())
            }
        }
        // XL.3 (Tier-2 history). `$stable`/`$changed`/`$rose`/`$fell`/`$past`
        // lower to atoms over a 1-step shadow register `<sig>__past` (the XL.3b
        // BTOR2 model-augmentation step synthesises that flop). All exact.
        "Call" => {
            let sub = expr
                .get("subroutine")
                .and_then(Value::as_str)
                .unwrap_or("?");
            match sub {
                "$stable" | "$changed" => {
                    let (sig, _w) = call_arg_signal(expr)?;
                    let shadow = past_shadow_name(sig, 1);
                    let cmp = if sub == "$stable" { "==" } else { "!=" };
                    Ok(format!("({sig} {cmp} {shadow})"))
                }
                "$rose" | "$fell" => {
                    let (sig, w) = call_arg_signal(expr)?;
                    if w != 1 {
                        return Err(format!(
                            "{sub}(<{w}-bit>) needs a bit-select; Tier-2 supports \
                             {sub} on 1-bit signals only"
                        ));
                    }
                    let shadow = past_shadow_name(sig, 1);
                    Ok(if sub == "$rose" {
                        format!("({sig} && (!({shadow})))") // 1 now, 0 last cycle
                    } else {
                        format!("((!({sig})) && {shadow})") // 0 now, 1 last cycle
                    })
                }
                // `$past(x[, k])` is a value; in boolean position a vector → `!= 0`.
                // `k` (default 1) selects the depth-`k` shadow along the shift chain.
                "$past" => {
                    let (sig, w, depth) = past_call_arg(expr)?;
                    let shadow = past_shadow_name(sig, depth);
                    Ok(if w > 1 {
                        format!("({shadow} != 0)")
                    } else {
                        shadow
                    })
                }
                // `$onehot0(x)` / `$onehot(x)` — expand to a value-set predicate over
                // the single signal `x`: at-most-one / exactly-one bit set means
                // `x ∈ {0,1,2,4,…,2^(w-1)}` / `x ∈ {1,2,4,…,2^(w-1)}`. This is one
                // `Or`-of-`Cmp` atom (a single cube dimension, NOT w dimensions), so
                // it lands squarely in the supported predicate fragment. Exact — the
                // disjunction is the definition of one-hot. The common secure use is a
                // one-hot STATE encoding (a cube dimension over `state_q`); an input
                // one-hot vector binds via the derived-label path.
                "$onehot0" | "$onehot" => {
                    let (sig, w) = call_arg_signal(expr)?;
                    // Cap the width: the expansion is w (+1) terms, and each
                    // `sig == 2^k` is a value comparison the abstraction must track.
                    // 32 covers realistic one-hot vectors (state encodings, N-way
                    // selects); wider is rejected honestly rather than emitting a
                    // huge disjunction.
                    if w == 0 || w > 32 {
                        return Err(format!(
                            "{sub}(<{w}-bit>) — one-hot expansion supports 1..=32-bit \
                             operands (a {w}-bit one-hot enumerates too many values)"
                        ));
                    }
                    let mut terms: Vec<String> = Vec::new();
                    if sub == "$onehot0" {
                        terms.push(format!("({sig} == 0)"));
                    }
                    for k in 0..w {
                        terms.push(format!("({sig} == {})", 1u64 << k));
                    }
                    Ok(format!("({})", terms.join(" || ")))
                }
                other => Err(format!(
                    "system/subroutine call `{other}` (e.g. `$isunknown`, `$countones`) \
                     is not in the Tier-1c/Tier-2 fragment"
                )),
            }
        }
        // `x inside {v1, v2, [lo:hi], …}` — expand to the already-supported comparison
        // fragment: a single value `v` → `(x == v)`, a simple `[lo:hi]` range →
        // `((x >= lo) && (x <= hi))`, OR-ed across the set. Every atom produced (`==`,
        // `>=`, `<=`, `||`) is in the Tier-1 fragment, so `inside` needs no new predicate
        // machinery. Exact — the disjunction IS the definition of `inside`. Members /
        // bounds must be signal-or-literal atoms (the ubiquitous value-set / range form
        // in real + LLM-generated SVA); a member that is itself a complex expression, or
        // a non-simple (`$`/dist) range, is rejected honestly rather than mis-translated.
        "Inside" => {
            let atom = |e: &Value| -> Option<String> {
                cmp_signal_atom(e).or_else(|| sv_integer(e).map(|n| n.to_string()))
            };
            let lhs = atom(child(expr, "left")?)
                .ok_or_else(|| "inside: left operand is not a signal/literal atom".to_string())?;
            let members = child(expr, "rangeList")?
                .as_array()
                .ok_or_else(|| "inside: `rangeList` is not an array".to_string())?;
            if members.is_empty() {
                return Err("inside: empty set list".to_string());
            }
            // Cap the enumerated width (each value member is a cube dimension the
            // abstraction tracks) — ranges are cheap (2 comparisons) so they don't count.
            let value_members = members
                .iter()
                .filter(|m| m.get("kind").and_then(Value::as_str) != Some("ValueRange"))
                .count();
            if value_members > 64 {
                return Err(format!(
                    "inside: {value_members} value members — the enumerated set is too large \
                     (>64) to expand into cube predicates"
                ));
            }
            let mut terms: Vec<String> = Vec::with_capacity(members.len());
            for m in members {
                if m.get("kind").and_then(Value::as_str) == Some("ValueRange") {
                    let rk = m
                        .get("rangeKind")
                        .and_then(Value::as_str)
                        .unwrap_or("Simple");
                    if rk != "Simple" {
                        return Err(format!(
                            "inside: only simple `[lo:hi]` ranges are supported (got `{rk}`)"
                        ));
                    }
                    let lo = atom(child(m, "left")?).ok_or_else(|| {
                        "inside: range low bound is not a signal/literal".to_string()
                    })?;
                    let hi = atom(child(m, "right")?).ok_or_else(|| {
                        "inside: range high bound is not a signal/literal".to_string()
                    })?;
                    terms.push(format!("(({lhs} >= {lo}) && ({lhs} <= {hi}))"));
                } else {
                    let v = atom(m).ok_or_else(|| {
                        format!(
                            "inside: set member is not a signal/literal atom (kind={:?})",
                            m.get("kind").and_then(Value::as_str)
                        )
                    })?;
                    terms.push(format!("({lhs} == {v})"));
                }
            }
            Ok(format!("({})", terms.join(" || ")))
        }
        other => Err(format!("unsupported expression kind: {other}")),
    }
}

/// Restructured comparison handler (XL.1c). Accepts, in order:
/// 1. `signal OP literal` → `(sig OP lit)`;
/// 2. `literal OP signal` → `(sig OP' lit)` with the operator flipped;
/// 3. `bool-expr ==/!= 0`  → `!bexpr` / `bexpr` (exact, since `bool != 0 ≡ bool`).
fn compare(expr: &Value, op: &str) -> Result<String, String> {
    let left = unwrap(child(expr, "left")?);
    let right = unwrap(child(expr, "right")?);

    // (1) signal OP literal
    if let (Ok(sig), Some(lit)) = (signal_name(left), sv_integer(right)) {
        return Ok(format!("({sig} {op} {lit})"));
    }
    // (2) literal OP signal  → flip to signal OP' literal
    if let (Some(lit), Ok(sig)) = (sv_integer(left), signal_name(right)) {
        return Ok(format!("({sig} {} {lit})", flip_op(op)));
    }
    // (3) boolean-expr ==/!= 0
    if matches!(op, "==" | "!=") {
        let other = if sv_integer(right) == Some(0) {
            Some(left)
        } else if sv_integer(left) == Some(0) {
            Some(right)
        } else {
            None
        };
        if let Some(other) = other {
            let b = bool_expr(other)?;
            return Ok(if op == "!=" { b } else { format!("(!({b}))") });
        }
    }
    // (4) H.D — relational `signal OP signal` (both genuine signals, incl the
    // XL.3 `$past` history form). The enum/parameter pre-pass has already folded
    // every named *constant* to a literal (caught by (1)/(2)), so a surviving
    // `NamedValue` here is a real register / input / wire — emit a relational
    // predicate (it lowers to `PredicateExpr::CmpReg`). Widening the *translation*
    // here is sound regardless of what the operands resolve to: binding is the
    // cube path's job — REL + H.A bind a state↔state relation; a free-input or
    // combinational operand is SKIPPED there (never mis-verdicted). (Pre-H.D this
    // was gated on a `$past` side, which left e.g. `cnt_q >= cfg_detect_timer_i`
    // and `data_o == data_i` untranslatable.)
    if let (Some(l), Some(r)) = (cmp_signal_atom(left), cmp_signal_atom(right)) {
        return Ok(format!("({l} {op} {r})"));
    }
    // (4b) H.G — arithmetic relational `signal OP (signal + literal)` — the sole
    // arithmetic form (`cnt_q == $past(cnt_q) + 1`, sysrst `CntIncr_A`). Lowers to
    // `PredicateExpr::CmpRegAddend`, whose SMT encoding is BV `bvadd` at the
    // register width (wraps exactly as the RTL `+`). The addend base may be a
    // `$past` shadow; the addend itself is a non-negative literal. Binding is the
    // cube path's job (routed SMT-only there); the translation is sound
    // regardless of what the operands resolve to.
    if let Some(l) = cmp_signal_atom(left)
        && let Some((r, addend)) = add_signal_literal(right)
    {
        return Ok(format!("({l} {op} {r} + {addend})"));
    }
    // (5) H.D — negation-peel for 1-bit boolean eq/ne: `(!x) == y` ≡ `x != y`,
    // `(!x) != y` ≡ `x == y` (and the `(!x) ==/!= literal` forms). Peel a 1-bit
    // `!signal` on either side and flip the eq/ne operator, then re-resolve the
    // other side as a signal atom or a literal. Only valid for `==`/`!=` over a
    // 1-bit operand (for a multi-bit `x`, `!x` means `x == 0`, not a simple flip).
    if matches!(op, "==" | "!=") {
        let flipped = if op == "==" { "!=" } else { "==" };
        let resolve = |e: &Value| -> Option<String> {
            cmp_signal_atom(e).or_else(|| sv_integer(e).map(|n| n.to_string()))
        };
        if let Some(inner) = logical_not_signal(left)
            && let Some(other) = resolve(right)
        {
            return Ok(format!("({inner} {flipped} {other})"));
        }
        if let Some(inner) = logical_not_signal(right)
            && let Some(other) = resolve(left)
        {
            return Ok(format!("({inner} {flipped} {other})"));
        }
    }
    // (6) 1-bit boolean equality/inequality where a plain-signal / literal form did
    // not match — e.g. a reduction operand (`(|oh_i) != en_i`, prim_onehot_check's
    // EnableCheck) or a boolean sub-expression. Both operands are 1-bit booleans, so
    // `a != b ≡ a XOR b` and `a == b ≡ a XNOR b`, expanded into the propositional
    // fragment via `bool_expr` (which handles the reduction `|x → (x != 0)`). Guarded
    // to 1-bit both sides: for a multi-bit operand `bool_expr` reduction-ORs it,
    // which would change an `==`'s meaning.
    if matches!(op, "==" | "!=")
        && signal_width(left) == 1
        && signal_width(right) == 1
        && let (Ok(a), Ok(b)) = (bool_expr(left), bool_expr(right))
    {
        return Ok(if op == "!=" {
            format!("(({a} && (!({b}))) || ((!({a})) && {b}))") // a XOR b
        } else {
            format!("(({a} && {b}) || ((!({a})) && (!({b}))))") // a XNOR b
        });
    }
    Err(format!(
        "comparison must be `signal {op} literal`, `literal {op} signal`, a relational \
         `signal {op} signal`, a 1-bit `!signal ==/!= …`, a 1-bit boolean `==/!=` (incl. \
         a reduction operand), or a `boolean-expr ==/!= 0` form; got left={:?} right={:?} \
         (an arithmetic operand like `== x + 1` needs predicate-arithmetic, not yet \
         supported)",
        left.get("kind").and_then(Value::as_str),
        right.get("kind").and_then(Value::as_str),
    ))
}

/// H.D — if `expr` is a 1-bit `!signal` (`UnaryOp{op: LogicalNot, operand:
/// NamedValue}`), return the inner signal's name. The 1-bit gate keeps the
/// peel-and-flip equivalence `(!x) == y ≡ x != y` exact: for a multi-bit operand
/// `!x` means `x == 0`, which is not a plain operator flip.
fn logical_not_signal(expr: &Value) -> Option<String> {
    let expr = unwrap(expr);
    if expr.get("kind").and_then(Value::as_str)? != "UnaryOp"
        || expr.get("op").and_then(Value::as_str)? != "LogicalNot"
    {
        return None;
    }
    let operand = unwrap(expr.get("operand")?);
    if signal_width(operand) != 1 {
        return None;
    }
    signal_name(operand).ok().map(|s| s.to_string())
}

/// A reduction over a single vector signal → an exact comparison atom.
/// `need_all_ones` selects the `&`/`~&` RHS (`2^W - 1` from the operand width)
/// versus the `|`/`~|` RHS (`0`).
fn reduction_to_cmp(operand: &Value, op: &str, need_all_ones: bool) -> Result<String, String> {
    let sig = signal_name(operand)
        .map_err(|_| format!("reduction `{op}` over a non-signal operand (only `OP signal`)"))?;
    let rhs: i64 = if need_all_ones {
        let w = signal_width(operand);
        if w == 0 || w >= 63 {
            return Err(format!(
                "reduction-and/nand width {w} unusable for an all-ones i64 literal"
            ));
        }
        (1i64 << w) - 1
    } else {
        0
    };
    Ok(format!("({sig} {op} {rhs})"))
}

/// The `__past` shadow-register name for a base signal at history `depth` — the
/// XL.3 naming contract shared with the BTOR2 model-augmentation step (XL.3b
/// synthesises a `depth`-stage shift chain `next(<base>__past) = <base>`,
/// `next(<base>__past2) = <base>__past`, … so the atom binds).
///
/// Depth 1 keeps the historical bare `<base>__past` (backward compatible with
/// `$past`/`$stable`/`$rose`/`$fell` and every existing model); depth `k ≥ 2`
/// (from `$past(x, k)`) is `<base>__past{k}`. `shadow::shadow_stage_name` MUST
/// mirror this exactly — the two are the two halves of one naming contract.
fn past_shadow_name(base: &str, depth: u32) -> String {
    if depth <= 1 {
        format!("{base}__past")
    } else {
        format!("{base}__past{depth}")
    }
}

/// True if `expr` (modulo `Conversion` peeling) is a `$past(...)` call.
fn is_past_call(expr: &Value) -> bool {
    let expr = unwrap(expr);
    expr.get("kind").and_then(Value::as_str) == Some("Call")
        && expr.get("subroutine").and_then(Value::as_str) == Some("$past")
}

/// A comparison operand that resolves to a mu-calc atom name: a plain signal, or
/// `$past(signal[, k])` → its depth-`k` `__past` shadow. `None` for anything else.
fn cmp_signal_atom(expr: &Value) -> Option<String> {
    let expr = unwrap(expr);
    if is_past_call(expr) {
        return past_call_arg(expr)
            .ok()
            .map(|(sig, _, depth)| past_shadow_name(sig, depth));
    }
    signal_name(expr).ok().map(|s| s.to_string())
}

/// H.G — if `expr` is `signal + literal` (or `literal + signal`), return the
/// signal atom (a plain name or a `$past` shadow) and the non-negative literal
/// addend. The base of the `CmpRegAddend` arithmetic relational (the sole
/// arithmetic form the translator accepts, `== $past(cnt) + 1`). Slang names the
/// `+` binary operator `"Add"`.
fn add_signal_literal(expr: &Value) -> Option<(String, u64)> {
    let expr = unwrap(expr);
    if expr.get("kind").and_then(Value::as_str) != Some("BinaryOp")
        || expr.get("op").and_then(Value::as_str) != Some("Add")
    {
        return None;
    }
    let l = unwrap(expr.get("left")?);
    let r = unwrap(expr.get("right")?);
    if let (Some(sig), Some(lit)) = (cmp_signal_atom(l), sv_integer(r))
        && lit >= 0
    {
        return Some((sig, lit as u64));
    }
    if let (Some(lit), Some(sig)) = (sv_integer(l), cmp_signal_atom(r))
        && lit >= 0
    {
        return Some((sig, lit as u64));
    }
    None
}

/// A single-signal Tier-2 call (`$stable`/`$changed`/`$rose`/`$fell`, and the
/// one-hot `$onehot`/`$onehot0`) takes exactly one signal argument; return its
/// `(name, width)`. These are inherently previous-cycle / current-cycle forms, so
/// a second (depth) argument is rejected here. `$past` — the only depth-carrying
/// history call — goes through [`past_call_arg`] instead.
fn call_arg_signal(call: &Value) -> Result<(&str, u32), String> {
    let sub = call
        .get("subroutine")
        .and_then(Value::as_str)
        .unwrap_or("$?");
    let args = call
        .get("arguments")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{sub} without an argument list"))?;
    if args.len() != 1 {
        return Err(format!(
            "{sub} with {} arguments; this Tier-2 call takes a single signal \
             (only `$past` carries a depth argument)",
            args.len()
        ));
    }
    let arg = unwrap(&args[0]);
    let sig = signal_name(arg).map_err(|_| format!("{sub} argument is not a plain signal"))?;
    Ok((sig, signal_width(arg)))
}

/// `$past(signal[, depth])` — Tier-2 history with an optional constant depth `k`
/// (default 1). Returns `(name, width, depth)`. A second argument is the history
/// depth: a constant integer literal ≥ 1 (`$past(x, 3)` = "x, three cycles ago").
/// The XL.3b augmentation builds a `depth`-stage shift chain so the shadow atom
/// [`past_shadow_name`]`(name, depth)` binds. Rejects depth 0 / negative, a
/// non-literal depth, more than two arguments, and a non-signal first argument.
fn past_call_arg(call: &Value) -> Result<(&str, u32, u32), String> {
    let sub = call
        .get("subroutine")
        .and_then(Value::as_str)
        .unwrap_or("$past");
    let args = call
        .get("arguments")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{sub} without an argument list"))?;
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "{sub} with {} arguments; expected `$past(signal[, depth])`",
            args.len()
        ));
    }
    let arg = unwrap(&args[0]);
    let sig =
        signal_name(arg).map_err(|_| format!("{sub} first argument is not a plain signal"))?;
    let depth = if args.len() == 2 {
        let d = sv_integer(unwrap(&args[1]))
            .ok_or_else(|| format!("{sub} depth argument must be a constant integer literal"))?;
        if d < 1 {
            return Err(format!(
                "{sub} depth {d} is not >= 1 (a $past depth is a positive number of cycles)"
            ));
        }
        d as u32
    } else {
        1
    };
    Ok((sig, signal_width(arg), depth))
}

/// `~x` in boolean position: logical-not on a 1-bit operand; a vector `~x` is a
/// value transform, not a boolean, so it is rejected.
fn bitwise_not(operand: &Value) -> Result<String, String> {
    if signal_width(operand) == 1 {
        Ok(format!("(!({}))", bool_expr(operand)?))
    } else {
        Err(format!(
            "bitwise-not `~` over a {}-bit vector is a value transform, not a boolean",
            signal_width(operand)
        ))
    }
}

/// Peel slang `Conversion` (width-cast) wrappers to the underlying expression.
fn unwrap(mut e: &Value) -> &Value {
    while e.get("kind").and_then(Value::as_str) == Some("Conversion") {
        match e.get("operand") {
            Some(o) => e = o,
            None => break,
        }
    }
    e
}

/// Bit-width of an expression from its slang `type` string. `logic[7:0]`→8,
/// `logic`/`bit`/`reg`→1, unknown→1 (the safe scalar default).
/// Is this expression's type an UNPACKED array (i.e. a memory), as opposed to a packed vector?
///
/// mununu#514 — the discriminator is slang's own notation, not a heuristic: slang renders an
/// unpacked dimension with a `$` prefix, so `logic [14:0] mem [0:319]` types as
/// `logic[14:0]$[0:319]` while the packed `logic [7:0] vec` types as `logic[7:0]`. A packed
/// multi-dimensional type (`logic[3:0][7:0]`) carries no `$`, so this separates "memory" from
/// "wide vector" — which is the distinction that matters, because BTOR2 arrays come precisely
/// from unpacked dimensions via yosys `memory_collect`.
///
/// Measured against slang's `--ast-json` on a fixture carrying both forms (2026-09-07).
fn is_unpacked_array(expr: &Value) -> bool {
    unwrap(expr)
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("$["))
}

fn signal_width(expr: &Value) -> u32 {
    let Some(ty) = expr.get("type").and_then(Value::as_str) else {
        return 1;
    };
    match (ty.find('['), ty.find(']')) {
        (Some(l), Some(r)) if r > l => {
            let inner = &ty[l + 1..r];
            if let Some((hi, lo)) = inner.split_once(':')
                && let (Ok(hi), Ok(lo)) = (hi.trim().parse::<i64>(), lo.trim().parse::<i64>())
            {
                return (hi - lo).unsigned_abs() as u32 + 1;
            }
            1
        }
        _ => 1,
    }
}

/// Flip a comparison operator (for the `literal OP signal` normalisation).
fn flip_op(op: &str) -> &str {
    match op {
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        other => other,
    }
}

/// Extract the signal name from a `NamedValue` (`"symbol": "<id> name"`),
/// peeling `Conversion` wrappers first.
fn signal_name(expr: &Value) -> Result<&str, String> {
    let expr = unwrap(expr);
    if expr.get("kind").and_then(Value::as_str) != Some("NamedValue") {
        return Err("expected a NamedValue signal reference".to_string());
    }
    let sym = expr
        .get("symbol")
        .and_then(Value::as_str)
        .ok_or_else(|| "NamedValue without a symbol".to_string())?;
    // slang formats the symbol as "<address> <name>"; take the name token.
    Ok(sym.rsplit(' ').next().unwrap_or(sym))
}

/// Parse an integer-literal expression to its value. Handles plain decimal,
/// based (`8'd5`, `4'hF`, `3'o7`, `1'b0`) and unbased-unsized (`'0`/`'1`,
/// serialised by slang as `1'b0`/`1'b1`) forms. `Conversion`-wrapped literals
/// are peeled first.
fn sv_integer(expr: &Value) -> Option<i64> {
    let expr = unwrap(expr);
    let k = expr.get("kind").and_then(Value::as_str)?;
    if !k.contains("IntegerLiteral") {
        return None;
    }
    let raw = expr
        .get("constant")
        .or_else(|| expr.get("value"))
        .and_then(Value::as_str)?;
    parse_sv_literal(raw)
}

/// Parse SystemVerilog integer-literal text. `"8"`→8, `"8'd5"`→5, `"1'b0"`→0,
/// `"4'hff"`→255, `"3'b101"`→5. Returns `None` for X/Z digits or malformed text.
fn parse_sv_literal(raw: &str) -> Option<i64> {
    let s: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    let Some(tick) = s.find('\'') else {
        return s.parse::<i64>().ok();
    };
    // After the tick: optional signedness (`s`/`S`), then a base char, then digits.
    let rest = &s[tick + 1..];
    let rest = rest.strip_prefix(['s', 'S']).unwrap_or(rest);
    let mut chars = rest.chars();
    let base = chars.next()?;
    let digits: String = chars.collect();
    let radix = match base.to_ascii_lowercase() {
        'b' => 2,
        'o' => 8,
        'd' => 10,
        'h' => 16,
        _ => return None,
    };
    i64::from_str_radix(&digits, radix).ok()
}

fn child<'a>(node: &'a Value, key: &str) -> Result<&'a Value, String> {
    node.get(key)
        .ok_or_else(|| format!("missing field `{key}` on {:?}", node.get("kind")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frozen `slang --ast-json` snapshot of a 5-assertion Tier-1 module
    // (assert bool / assert |-> / assert |=> / assume / cover). Regenerate via:
    //   slang --ast-json testdata/tier1.ast.json testdata/tier1.sv --single-unit
    const TIER1_JSON: &str = include_str!("testdata/tier1.ast.json");
    // Frozen `slang --ast-json` snapshot of the 5 Tier-2 history forms
    // ($stable / $changed / $rose / $fell / $past). Regenerate via:
    //   slang --ast-json testdata/tier2.ast.json testdata/tier2.sv --single-unit
    const TIER2_JSON: &str = include_str!("testdata/tier2.ast.json");

    #[test]
    fn translates_the_tier1_fixture() {
        let report = translate_ast_json(TIER1_JSON).expect("valid ast-json");
        assert_eq!(report.total(), 5, "the fixture has 5 concurrent assertions");
        assert_eq!(
            report.unsupported.len(),
            0,
            "all 5 fixture assertions are Tier-1; got unsupported: {:?}",
            report.unsupported
        );
        // Every emitted formula must parse (translate_one already validates, but
        // re-confirm here as the contract).
        for t in &report.translated {
            crate::mu_calculus::parser::parse(&t.formula)
                .unwrap_or_else(|e| panic!("formula for {} failed to parse: {e:?}", t.name));
        }
        let by_kind = |k: SvaKind| -> Vec<&str> {
            report
                .translated
                .iter()
                .filter(|t| t.kind == k)
                .map(|t| t.formula.as_str())
                .collect()
        };
        // 3 asserts (bool, |->, |=>), 1 assume, 1 cover.
        assert_eq!(by_kind(SvaKind::Assert).len(), 3);
        assert_eq!(by_kind(SvaKind::Assume).len(), 1);
        assert_eq!(by_kind(SvaKind::Cover).len(), 1);
        // The cover is an EF (mu / <>); the asserts are AG (nu / []).
        assert!(by_kind(SvaKind::Cover)[0].starts_with("mu X."));
        assert!(
            by_kind(SvaKind::Assert)
                .iter()
                .all(|f| f.starts_with("nu X."))
        );
        // The |-> assert encodes implication as `!a || b`.
        assert!(
            by_kind(SvaKind::Assert)
                .iter()
                .any(|f| f.contains("(!(") && f.contains("||")),
            "an implication assert must encode `a -> b` as `!a || b`; got {:?}",
            by_kind(SvaKind::Assert)
        );
    }

    #[test]
    fn implication_body_rewrites_to_disjunction() {
        // a |-> b  →  nu X. ((!(a) || b) && [] X)
        let spec = serde_json::json!({
            "kind": "Binary",
            "op": "OverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert_eq!(f, "nu X. (((!(a) || b)) && [] X)");
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn nonoverlapped_implication_adds_next() {
        // a |=> b  →  next-cycle b
        let spec = serde_json::json!({
            "kind": "Binary",
            "op": "NonOverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(f.contains("[] b"), "|=> must put b under a next ([]): {f}");
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl4_fixed_cycle_delay_consequent() {
        // a |-> ##2 b  →  b two cycles later ([] [] b). slang shape: right =
        // SequenceConcat { elements: [{ sequence: Simple(b), min:2, max:2 }] }
        // (captured from real slang 11.0.0 --ast-json).
        let spec = serde_json::json!({
            "kind": "Binary",
            "op": "OverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}},
                 "min": 2, "max": 2}
            ]}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(f.contains("(!(a) || [] [] b)"), "##2 → two boxes: {f}");
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl4_bounded_range_delay_consequent() {
        // a |=> ##[1:3] b  →  b somewhere in the window, plus one [] from |=>.
        let spec = serde_json::json!({
            "kind": "Binary",
            "op": "NonOverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}},
                 "min": 1, "max": 3}
            ]}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(
            f.contains("[] [] (b || [] (b || [] b))"),
            "range [1:3] under |=>: {f}"
        );
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl5_multi_element_chain_consequent() {
        // a |-> b ##1 c ##2 d  →  b now, c at +1, d at +3 (cumulative gaps).
        // slang shape: elements [{b,0,0},{c,1,1},{d,2,2}] (captured from slang 11.0.0).
        let spec = serde_json::json!({
            "kind": "Binary", "op": "OverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}, "min":0, "max":0},
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}}, "min":1, "max":1},
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "4 d"}}, "min":2, "max":2}
            ]}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(
            f.contains("(b && [] (c && [] [] d))"),
            "multi-element chain: {f}"
        );
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl5_consecutive_repetition_consequent() {
        // a |-> c[*3]  →  c held 3 consecutive cycles. slang shape: Simple(c) with
        // repetition {Consecutive, min:3, max:3} (captured from slang 11.0.0).
        let spec = serde_json::json!({
            "kind": "Binary", "op": "OverlappedImplication",
            "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"},
                      "repetition": {"kind": "Consecutive", "min": 3, "max": 3}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(
            f.contains("(c && [] (c && [] c))"),
            "c[*3] consecutive: {f}"
        );
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl5_out_of_fragment_sequences_rejected() {
        let mk = |right: serde_json::Value| {
            serde_json::json!({
                "kind": "Binary", "op": "OverlappedImplication",
                "left":  {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
                "right": right
            })
        };
        // Unbounded repetition `c[*1:$]` — slang encodes `$` as a string max.
        let unb = mk(serde_json::json!({
            "kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"},
            "repetition": {"kind": "Consecutive", "min": 1, "max": "$"}
        }));
        let err = property_body(&unb, &TranslateOptions::default(), &mut Vec::new())
            .expect_err("unbounded must reject");
        assert!(err.contains("Tier-3b"), "got: {err}");
        // Range repetition `c[*1:3]` — Tier-3b.
        let rng = mk(serde_json::json!({
            "kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"},
            "repetition": {"kind": "Consecutive", "min": 1, "max": 3}
        }));
        let err = property_body(&rng, &TranslateOptions::default(), &mut Vec::new())
            .expect_err("range repetition must reject");
        assert!(err.contains("Tier-3b"), "got: {err}");
        // A repetition INSIDE a multi-element chain — length composition, Tier-3b.
        let chain_rep = mk(serde_json::json!({
            "kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"},
                              "repetition": {"kind": "Consecutive", "min": 2, "max": 2}}, "min":0, "max":0},
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}}, "min":1, "max":1}
            ]
        }));
        let err = property_body(&chain_rep, &TranslateOptions::default(), &mut Vec::new())
            .expect_err("repetition in a chain must reject");
        assert!(err.contains("Tier-3b"), "got: {err}");
    }

    #[test]
    fn xl5_sequence_antecedent() {
        // (a ##1 b) |-> c  →  a → X(b → c)  →  (!(a) || [] (!(b) || c)).
        // slang shape: left = SequenceConcat[{a,0,0},{b,1,1}] (captured from slang).
        let spec = serde_json::json!({
            "kind": "Binary", "op": "OverlappedImplication",
            "left": {"kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}}, "min":0, "max":0},
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}, "min":1, "max":1}
            ]},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(
            f.contains("(!(a) || [] (!(b) || c))"),
            "seq antecedent: {f}"
        );
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl5_repetition_antecedent_and_nonoverlap() {
        // a[*3] |=> c  →  (a∧Xa∧XXa) → XXX c  →  nested with the |=> extra [].
        let spec = serde_json::json!({
            "kind": "Binary", "op": "NonOverlappedImplication",
            "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"},
                     "repetition": {"kind": "Consecutive", "min": 3, "max": 3}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(
            f.contains("(!(a) || [] (!(a) || [] (!(a) || [] c)))"),
            "a[*3] |=> c: {f}"
        );
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl5_range_antecedent_rejected() {
        // (a ##[1:2] b) |-> c — a range delay in an ANTECEDENT has multiple match
        // ends (Tier-3b), rejected.
        let spec = serde_json::json!({
            "kind": "Binary", "op": "OverlappedImplication",
            "left": {"kind": "SequenceConcat", "elements": [
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}}, "min":0, "max":0},
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}, "min":1, "max":2}
            ]},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}}
        });
        let err = property_body(&spec, &TranslateOptions::default(), &mut Vec::new())
            .expect_err("range antecedent must reject");
        assert!(err.contains("Tier-3b"), "got: {err}");
    }

    #[test]
    fn unsupported_construct_is_recorded_not_dropped() {
        // Reduction-xor `^x` (parity) is out of the fragment — not expressible
        // as a propositional atom, so it must be rejected (never dropped).
        let spec = serde_json::json!({
            "kind": "Simple",
            "expr": {"kind": "UnaryOp", "op": "BitwiseXor",
                     "operand": {"kind": "NamedValue", "symbol": "1 req", "type": "logic[7:0]"}}
        });
        let err = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect_err("must reject");
        assert!(err.contains("parity"), "got: {err}");
    }

    #[test]
    fn xl1c_reduction_or_becomes_nonzero_comparison() {
        // `|gnt_o |-> ready_i` — the reduction-or LHS → `(gnt_o != 0)`.
        let spec = serde_json::json!({
            "kind": "Binary",
            "op": "OverlappedImplication",
            "left": {"kind": "Simple", "expr":
                {"kind": "UnaryOp", "op": "BitwiseOr",
                 "operand": {"kind": "NamedValue", "symbol": "1 gnt_o", "type": "logic[7:0]"}}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 ready_i"}}
        });
        let f = translate_one(
            &spec,
            SvaKind::Assert,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("translates");
        assert!(f.contains("(gnt_o != 0)"), "reduction-or → != 0; got {f}");
        assert!(f.contains("ready_i"));
        crate::mu_calculus::parser::parse(&f).expect("parses");
    }

    #[test]
    fn xl1c_reduction_and_uses_operand_width_all_ones() {
        // `&x` over logic[7:0] → `(x == 255)`.
        let expr = serde_json::json!({
            "kind": "UnaryOp", "op": "BitwiseAnd",
            "operand": {"kind": "NamedValue", "symbol": "1 x", "type": "logic[7:0]"}
        });
        assert_eq!(bool_expr(&expr).unwrap(), "(x == 255)");
        // `~&x` → `(x != 255)`.
        let nand = serde_json::json!({
            "kind": "UnaryOp", "op": "BitwiseNand",
            "operand": {"kind": "NamedValue", "symbol": "1 x", "type": "logic[2:0]"}
        });
        assert_eq!(bool_expr(&nand).unwrap(), "(x != 7)");
    }

    #[test]
    fn xl1c_disable_iff_macro_form_translates() {
        // The OpenTitan `ASSERT` macro encodes `disable iff ((!rst_ni) !== '0)`:
        //   condition = CaseInequality((!rst_ni), '0)  →  `(!(rst_ni))`.
        let spec = serde_json::json!({
            "kind": "DisableIff",
            "condition": {
                "kind": "BinaryOp", "op": "CaseInequality",
                "left": {"kind": "UnaryOp", "op": "LogicalNot",
                         "operand": {"kind": "NamedValue", "symbol": "1 rst_ni", "type": "logic"}},
                "right": {"kind": "UnbasedUnsizedIntegerLiteral", "value": "1'b0"}
            },
            "expr": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 a"}}
        });
        let body = property_body(&spec, &TranslateOptions::default(), &mut Vec::new())
            .expect("translates");
        assert_eq!(body, "((!(rst_ni)) || a)");
    }

    #[test]
    fn xl1c_sv_literal_parsing() {
        assert_eq!(parse_sv_literal("8"), Some(8));
        assert_eq!(parse_sv_literal("1'b0"), Some(0)); // the `'0` form (was the digit-filter bug)
        assert_eq!(parse_sv_literal("8'd5"), Some(5));
        assert_eq!(parse_sv_literal("4'hff"), Some(255));
        assert_eq!(parse_sv_literal("3'b101"), Some(5));
        assert_eq!(parse_sv_literal("16'sd42"), Some(42));
        assert_eq!(parse_sv_literal("garbage"), None);
    }

    #[test]
    fn xl1c_signal_width_from_type() {
        let mk = |t: &str| serde_json::json!({"kind": "NamedValue", "symbol": "1 s", "type": t});
        assert_eq!(signal_width(&mk("logic[7:0]")), 8);
        assert_eq!(signal_width(&mk("logic[2:0]")), 3);
        assert_eq!(signal_width(&mk("logic")), 1);
        assert_eq!(signal_width(&mk("bit")), 1);
    }

    #[test]
    fn xl1c_dynamic_bit_select_is_rejected_with_reason() {
        // `req_i[idx_o]` — selector is a signal → dynamic, not a Tier-1c atom.
        let expr = serde_json::json!({
            "kind": "ElementSelect",
            "selector": {"kind": "NamedValue", "symbol": "1 idx_o", "type": "logic[2:0]"},
            "value": {"kind": "NamedValue", "symbol": "2 req_i", "type": "logic[7:0]"}
        });
        let err = bool_expr(&expr).expect_err("dynamic index must reject");
        assert!(err.contains("dynamic bit-select"), "got: {err}");
    }

    /// mununu#514 — the constant-index bit-select branch had NO test, which is how the array
    /// case came to share its message. Pins the bit-select side so the array split below cannot
    /// swallow it.
    #[test]
    fn xl1c_constant_bit_select_is_rejected_as_a_bit_select() {
        // `vec[3]` on a PACKED vector — a genuine bit-select.
        let expr = serde_json::json!({
            "kind": "ElementSelect",
            "selector": {"kind": "IntegerLiteral", "value": "3"},
            "value": {"kind": "NamedValue", "symbol": "1 vec", "type": "logic[7:0]"}
        });
        let err = bool_expr(&expr).expect_err("constant bit-select must reject");
        assert!(err.contains("constant bit-select"), "got: {err}");
        assert!(
            !err.contains("unpacked array"),
            "a packed vector must NOT be reported as an array: {err}"
        );
    }

    /// mununu#514 — an ARRAY read is not a bit-select. `mem` is `logic [14:0] mem [0:319]`,
    /// which slang types `logic[14:0]$[0:319]` (the `$` marks the unpacked dimension —
    /// measured against `slang --ast-json`, not guessed).
    ///
    /// This used to report "constant bit-select `sig[k]` needs a bit-level model predicate",
    /// pointing the reader at a workaround that cannot apply to array CONTENT. It cost monono
    /// a block: they reduced a quantified clear-on-read property to a single literal index to
    /// make it expressible, and were refused anyway with the wrong reason.
    #[test]
    fn array_element_select_is_refused_as_an_array_not_a_bit_select() {
        let expr = serde_json::json!({
            "kind": "ElementSelect",
            "selector": {"kind": "IntegerLiteral", "value": "0"},
            "value": {"kind": "NamedValue", "symbol": "1 mem", "type": "logic[14:0]$[0:319]"}
        });
        let err = bool_expr(&expr).expect_err("array element-select must reject");
        assert!(err.contains("unpacked array"), "got: {err}");
        assert!(err.contains("mem"), "names the signal: {err}");
        assert!(
            !err.contains("bit-level model predicate"),
            "must NOT offer the bit-select workaround for an array: {err}"
        );
        assert!(
            err.contains("ANY index"),
            "must say the limit is not specific to a constant index: {err}"
        );
    }

    /// The same array refusal for a SIGNAL index — the limit is the array, not the index form.
    #[test]
    fn array_element_select_with_signal_index_is_also_refused_as_an_array() {
        let expr = serde_json::json!({
            "kind": "ElementSelect",
            "selector": {"kind": "NamedValue", "symbol": "2 rd_x", "type": "logic[8:0]"},
            "value": {"kind": "NamedValue", "symbol": "1 mem", "type": "logic[14:0]$[0:319]"}
        });
        let err = bool_expr(&expr).expect_err("array element-select must reject");
        assert!(err.contains("unpacked array"), "got: {err}");
        assert!(
            !err.contains("dynamic bit-select"),
            "an array read is not a dynamic bit-select: {err}"
        );
    }

    /// A PACKED multi-dimensional type carries no `$`, so it must stay a bit-select.
    #[test]
    fn packed_multidim_is_not_treated_as_an_unpacked_array() {
        let ty =
            serde_json::json!({"kind": "NamedValue", "symbol": "1 p", "type": "logic[3:0][7:0]"});
        assert!(
            !is_unpacked_array(&ty),
            "packed 2-D is not an unpacked array"
        );
        let arr =
            serde_json::json!({"kind": "NamedValue", "symbol": "1 m", "type": "logic[7:0]$[0:3]"});
        assert!(
            is_unpacked_array(&arr),
            "unpacked dimension detected via `$[`"
        );
    }

    #[test]
    #[ignore = "requires the slang CLI + the M.0 prim_arbiter fixture; run with --ignored"]
    fn e2e_prim_arbiter_real_corpus() {
        use crate::adapter::slang::{locate_slang, run_ast_json};
        use std::path::PathBuf;
        let bin = locate_slang().expect("slang available");
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/verify/m0_opentitan_prim_arbiter/source");
        let json = run_ast_json(&bin, &[dir.join("prim_arbiter_fixed.sv")], &[dir])
            .expect("slang --ast-json on prim_arbiter");
        let report = translate_ast_json(&json).expect("translate");
        eprintln!(
            "prim_arbiter: {} translated, {} unsupported (of {} concurrent assertions)",
            report.translated.len(),
            report.unsupported.len(),
            report.total()
        );
        for u in &report.unsupported {
            eprintln!("  unsupported {}: {}", u.name, u.reason);
        }
        // Real OpenTitan SVA. XL.1c lifts the reduction-or / disable-iff-macro
        // properties (`|gnt_o |-> ready_i`, etc.) and `$onehot0(gnt_o)` now expands
        // to a value-set predicate; the bit-arithmetic and dynamic bit-select ones
        // (and `$isunknown`) stay honestly unsupported.
        assert!(
            report.total() >= 13,
            "all 13 concurrent assertions should be seen"
        );
        assert!(
            report.translated.len() >= 6,
            "XL.1c should translate the 6 reduction/comparison ASSERTs \
             (GntImpliesReady/Valid, ReqAndReadyImplyGrant, ReqImpliesValid, \
             ReadyAndValidImplyGrant, NoReadyValidNoGrant); got {} translated, \
             unsupported: {:?}",
            report.translated.len(),
            report
                .unsupported
                .iter()
                .map(|u| &u.reason)
                .collect::<Vec<_>>()
        );
        for t in &report.translated {
            crate::mu_calculus::parser::parse(&t.formula).expect("translated formula parses");
        }
    }

    #[test]
    fn disable_iff_gates_the_body() {
        // disable iff (!rst_n) (a)  →  body `((!(rst_n)) || a)`
        let spec = serde_json::json!({
            "kind": "DisableIff",
            "condition": {"kind": "UnaryOp", "op": "LogicalNot",
                          "operand": {"kind": "NamedValue", "symbol": "1 rst_n"}},
            "expr": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 a"}}
        });
        let body = property_body(&spec, &TranslateOptions::default(), &mut Vec::new())
            .expect("translates");
        assert_eq!(body, "((!(rst_n)) || a)");

        // With reset-gating the guard is dropped and the reset is recorded.
        let mut resets = Vec::new();
        let gated = property_body(&spec, &TranslateOptions { gate_reset: true }, &mut resets)
            .expect("translates");
        assert_eq!(gated, "a", "gate_reset drops the disable-iff guard");
        assert_eq!(
            resets,
            vec![ResetSignal {
                signal: "rst_n".to_string(),
                inactive_value: 1,
            }],
            "active-low reset recorded with inactive value 1"
        );
    }

    #[test]
    fn extract_reset_signal_recognizes_common_idioms() {
        // active-high `disable iff (rst)` → inactive value 0.
        let active_high = serde_json::json!({"kind": "NamedValue", "symbol": "1 rst"});
        assert_eq!(
            extract_reset_signal(&active_high),
            Some(ResetSignal {
                signal: "rst".to_string(),
                inactive_value: 0
            })
        );
        // active-low `disable iff (!rst_n)` → inactive value 1.
        let active_low = serde_json::json!({
            "kind": "UnaryOp", "op": "LogicalNot",
            "operand": {"kind": "NamedValue", "symbol": "1 rst_n"}
        });
        assert_eq!(
            extract_reset_signal(&active_low),
            Some(ResetSignal {
                signal: "rst_n".to_string(),
                inactive_value: 1
            })
        );
        // OpenTitan macro `(!rst_ni) !== '0` → same polarity as the LHS (1).
        let macro_form = serde_json::json!({
            "kind": "BinaryOp", "op": "CaseInequality",
            "left": {"kind": "UnaryOp", "op": "LogicalNot",
                     "operand": {"kind": "NamedValue", "symbol": "1 rst_ni", "type": "logic"}},
            "right": {"kind": "UnbasedUnsizedIntegerLiteral", "value": "1'b0"}
        });
        assert_eq!(
            extract_reset_signal(&macro_form),
            Some(ResetSignal {
                signal: "rst_ni".to_string(),
                inactive_value: 1
            })
        );
        // A multi-signal condition is NOT recognized (left as a kept guard).
        let complex = serde_json::json!({
            "kind": "BinaryOp", "op": "LogicalOr",
            "left": {"kind": "NamedValue", "symbol": "1 rst"},
            "right": {"kind": "NamedValue", "symbol": "2 clr"}
        });
        assert_eq!(extract_reset_signal(&complex), None);
    }

    #[test]
    fn xl2_cover_gets_recoverability_companion() {
        // The Tier-1 fixture's `cover property (a && b)` → EF, plus the AG-EF lens.
        let report = translate_ast_json(TIER1_JSON).expect("valid ast-json");
        let cover = report
            .translated
            .iter()
            .find(|t| t.kind == SvaKind::Cover)
            .expect("fixture has a cover");
        let companion = cover
            .recoverability_companion
            .as_ref()
            .expect("a cover carries its recoverability companion");
        // AG EF shape: outer nu over the cover's EF (mu), closed with `[] Y`.
        assert!(companion.starts_with("nu Y. (("), "got {companion}");
        assert!(
            companion.contains(&cover.formula),
            "companion embeds the EF formula"
        );
        assert!(companion.ends_with(") && [] Y)"), "got {companion}");
        crate::mu_calculus::parser::parse(companion).expect("companion parses");
    }

    #[test]
    fn xl2_non_cover_assertions_have_no_companion() {
        // EF→AG-EF only makes sense for covers; asserts/assumes are AG-shaped.
        let report = translate_ast_json(TIER1_JSON).expect("valid ast-json");
        for t in report
            .translated
            .iter()
            .filter(|t| t.kind != SvaKind::Cover)
        {
            assert!(
                t.recoverability_companion.is_none(),
                "{} ({:?}) must not carry a recoverability companion",
                t.name,
                t.kind
            );
        }
    }

    #[test]
    fn xl2_companion_matches_v7c_recoverability_shape() {
        // The V.7-c csrng showcase checks `nu Y. ((mu X. (idle || <> X)) && [] Y)`
        // by hand. XL.2 forms the same AG-EF shape from `cover property (state_q == 55)`.
        let ef = translate_one(
            &serde_json::json!({
                "kind": "Simple",
                "expr": {"kind": "BinaryOp", "op": "Equality",
                         "left":  {"kind": "NamedValue", "symbol": "1 state_q", "type": "logic[5:0]"},
                         "right": {"kind": "IntegerLiteral", "value": "55", "constant": "55"}}
            }),
            SvaKind::Cover,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect("cover translates");
        let companion = recoverability_companion_formula(&ef).expect("companion forms + parses");
        assert_eq!(
            companion,
            "nu Y. ((mu X. (((state_q == 55)) || <> X)) && [] Y)"
        );
    }

    // --- XL.3 (Tier-2 history) -------------------------------------------

    /// Helper: a `$<sub>(<sig>:<width>)` Call expression for the unit tests.
    fn history_call(sub: &str, sig: &str, ty: &str) -> Value {
        serde_json::json!({
            "kind": "Call", "subroutine": sub,
            "arguments": [{"kind": "NamedValue", "symbol": format!("1 {sig}"), "type": ty}]
        })
    }

    #[test]
    fn xl3_stable_and_changed_compare_against_shadow() {
        assert_eq!(
            bool_expr(&history_call("$stable", "state_q", "logic[5:0]")).unwrap(),
            "(state_q == state_q__past)"
        );
        assert_eq!(
            bool_expr(&history_call("$changed", "state_q", "logic[5:0]")).unwrap(),
            "(state_q != state_q__past)"
        );
    }

    #[test]
    fn xl3_rose_fell_one_bit_edges() {
        assert_eq!(
            bool_expr(&history_call("$rose", "v", "logic")).unwrap(),
            "(v && (!(v__past)))"
        );
        assert_eq!(
            bool_expr(&history_call("$fell", "v", "logic")).unwrap(),
            "((!(v)) && v__past)"
        );
    }

    #[test]
    fn xl3_rose_on_vector_is_rejected() {
        let err = bool_expr(&history_call("$rose", "bus", "logic[7:0]")).expect_err("reject");
        assert!(err.contains("1-bit"), "got: {err}");
    }

    #[test]
    fn xl3_past_in_comparison_resolves_to_shadow() {
        // `state_q == $past(state_q)` (the explicit $stable form).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 state_q", "type": "logic[5:0]"},
            "right": history_call("$past", "state_q", "logic[5:0]")
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(state_q == state_q__past)");
    }

    #[test]
    fn hd_relational_signal_ge_signal_translates() {
        // H.D — `cnt_q >= cfg_detect_timer_i` (NamedValue >= NamedValue) was
        // unsupported pre-H.D; now translates to a relational atom (→ CmpReg).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "GreaterThanEqual",
            "left":  {"kind": "NamedValue", "symbol": "1 cnt_q", "type": "logic[15:0]"},
            "right": {"kind": "NamedValue", "symbol": "2 cfg_detect_timer_i", "type": "logic[15:0]"}
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(cnt_q >= cfg_detect_timer_i)");
    }

    #[test]
    fn hg_arithmetic_addend_translates() {
        // H.G — `cnt_q == $past(cnt_q) + 1` (sysrst `CntIncr_A`), previously
        // rejected as "arithmetic operand not supported". Now translates to the
        // arithmetic relational atom (→ `PredicateExpr::CmpRegAddend`, BV `+`).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 cnt_q", "type": "logic[31:0]"},
            "right": {
                "kind": "BinaryOp", "op": "Add",
                "left":  history_call("$past", "cnt_q", "logic[31:0]"),
                "right": {"kind": "IntegerLiteral", "value": "1", "constant": "1"}
            }
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(cnt_q == cnt_q__past + 1)");
    }

    #[test]
    fn hg_arithmetic_addend_plain_register_base() {
        // H.G — the base need not be `$past`: `x == y + 2` also translates.
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 x", "type": "logic[7:0]"},
            "right": {
                "kind": "BinaryOp", "op": "Add",
                "left":  {"kind": "NamedValue", "symbol": "2 y", "type": "logic[7:0]"},
                "right": {"kind": "IntegerLiteral", "value": "2", "constant": "2"}
            }
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(x == y + 2)");
    }

    #[test]
    fn hd_relational_signal_eq_signal_translates() {
        // H.D — general dataflow equality `data_o == data_i` (no `$past` side).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 data_o", "type": "logic[7:0]"},
            "right": {"kind": "NamedValue", "symbol": "2 data_i", "type": "logic[7:0]"}
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(data_o == data_i)");
    }

    #[test]
    fn hd_negation_caseeq_peels_to_inequality() {
        // H.D — 1-bit `(!x) === y` ≡ `x != y` (peel `!`, flip `==`→`!=`).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "CaseEquality",
            "left":  {"kind": "UnaryOp", "op": "LogicalNot",
                      "operand": {"kind": "NamedValue", "symbol": "1 x", "type": "logic"}},
            "right": {"kind": "NamedValue", "symbol": "2 y", "type": "logic"}
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(x != y)");
    }

    #[test]
    fn hd_negation_caseneq_peels_to_equality() {
        // H.D — 1-bit `(!x) !== 1` ≡ `x == 1` (peel `!`, flip `!=`→`==`; literal RHS).
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "CaseInequality",
            "left":  {"kind": "UnaryOp", "op": "LogicalNot",
                      "operand": {"kind": "NamedValue", "symbol": "1 x", "type": "logic"}},
            "right": {"kind": "IntegerLiteral", "value": "1", "constant": "1"}
        });
        assert_eq!(bool_expr(&cmp).unwrap(), "(x == 1)");
    }

    #[test]
    fn hd_negation_peel_multibit_lowers_via_boolexpr_not_unsound_flip() {
        // A multi-bit `!bus` means `bus == 0`, NOT a simple operator flip. The 1-bit
        // negation-peel (case 5) does NOT fire on it; instead the 1-bit boolean
        // comparison (case 6) handles `(!bus) == y` via `bool_expr`, which correctly
        // lowers `!bus → !(bus != 0)` (i.e. `bus == 0`). Crucially it must NOT become
        // the unsound `(bus != y)` peel-flip. (`!bus` is a 1-bit LogicalNot result.)
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "CaseEquality",
            "left":  {"kind": "UnaryOp", "op": "LogicalNot", "type": "logic",
                      "operand": {"kind": "NamedValue", "symbol": "1 bus", "type": "logic[7:0]"}},
            "right": {"kind": "NamedValue", "symbol": "2 y", "type": "logic"}
        });
        let s = bool_expr(&cmp).expect("(!bus) == y lowers via the boolean-comparison case");
        assert!(
            s.contains("bus != 0"),
            "multi-bit !bus lowered as bus==0 (`!(bus != 0)`): {s}"
        );
        assert!(
            !s.contains("bus != y") && !s.contains("bus == y"),
            "no unsound peel-flip to a direct bus↔y comparison: {s}"
        );
    }

    #[test]
    fn hg_nonlinear_arithmetic_still_unsupported_with_clear_reason() {
        // H.G supports the `reg + const` addend (`hg_arithmetic_addend_translates`).
        // NON-linear / non-constant arithmetic stays unsupported: a `reg * const`
        // (Multiply, not Add) and a `reg + reg` (two-register addend, no literal)
        // both fall through to the honest reject — the predicate layer carries
        // only a constant addend, not general arithmetic.
        let mul = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 cnt", "type": "logic[3:0]"},
            "right": {"kind": "BinaryOp", "op": "Multiply",
                      "left":  {"kind": "NamedValue", "symbol": "1 cnt", "type": "logic[3:0]"},
                      "right": {"kind": "IntegerLiteral", "value": "2", "constant": "2"}}
        });
        let err = bool_expr(&mul).expect_err("multiply RHS must reject");
        assert!(err.contains("arithmetic"), "got: {err}");

        let two_reg = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left":  {"kind": "NamedValue", "symbol": "1 cnt", "type": "logic[3:0]"},
            "right": {"kind": "BinaryOp", "op": "Add",
                      "left":  {"kind": "NamedValue", "symbol": "1 a", "type": "logic[3:0]"},
                      "right": {"kind": "NamedValue", "symbol": "2 b", "type": "logic[3:0]"}}
        });
        let err = bool_expr(&two_reg).expect_err("two-register addend must reject");
        assert!(err.contains("arithmetic"), "got: {err}");
    }

    #[test]
    fn collect_parameter_names_finds_declared_params() {
        // slang serialises `parameter`/`localparam` as a `Parameter` node with a
        // `name`; --param validation checks override names against these.
        let ast = serde_json::json!({
            "members": [
                {"kind": "Parameter", "name": "INIT_WAIT", "value": "20000"},
                {"kind": "Parameter", "name": "W", "value": "15"},
                {"kind": "Variable", "name": "cnt"}
            ]
        });
        let mut out = Vec::new();
        collect_parameter_names(&ast, &mut out);
        assert!(out.contains(&"INIT_WAIT".to_string()), "got {out:?}");
        assert!(out.contains(&"W".to_string()), "got {out:?}");
        assert!(
            !out.contains(&"cnt".to_string()),
            "non-Parameter excluded: {out:?}"
        );
    }

    #[test]
    fn xl3_past_depth_k_lowers_to_the_suffixed_shadow() {
        // `$past(x, 2)` — depth 2 now lowers to the stage-2 shadow `x__past2`
        // (Tier-1 k-deep history). Bare `$past(x)` / depth 1 stays `x__past`.
        let two = serde_json::json!({
            "kind": "Call", "subroutine": "$past",
            "arguments": [
                {"kind": "NamedValue", "symbol": "1 x", "type": "logic"},
                {"kind": "IntegerLiteral", "value": "2", "constant": "2"}
            ]
        });
        assert_eq!(bool_expr(&two).expect("depth 2 accepted"), "x__past2");
        let one = serde_json::json!({
            "kind": "Call", "subroutine": "$past",
            "arguments": [{"kind": "NamedValue", "symbol": "1 x", "type": "logic"}]
        });
        assert_eq!(bool_expr(&one).expect("depth 1 accepted"), "x__past");
    }

    #[test]
    fn xl3_past_depth_zero_and_stable_with_depth_are_rejected() {
        // `$past(x, 0)` — a depth must be a positive number of cycles.
        let zero = serde_json::json!({
            "kind": "Call", "subroutine": "$past",
            "arguments": [
                {"kind": "NamedValue", "symbol": "1 x", "type": "logic"},
                {"kind": "IntegerLiteral", "value": "0", "constant": "0"}
            ]
        });
        let err = bool_expr(&zero).expect_err("depth 0 must reject");
        assert!(err.contains(">= 1"), "got: {err}");
        // `$stable(x, 2)` — only `$past` carries a depth argument.
        let stable2 = serde_json::json!({
            "kind": "Call", "subroutine": "$stable",
            "arguments": [
                {"kind": "NamedValue", "symbol": "1 x", "type": "logic"},
                {"kind": "IntegerLiteral", "value": "2", "constant": "2"}
            ]
        });
        let err = bool_expr(&stable2).expect_err("$stable depth must reject");
        assert!(err.contains("single signal"), "got: {err}");
    }

    #[test]
    fn onehot_expands_isunknown_still_rejected() {
        // `$onehot0` / `$onehot` now expand to a value-set predicate; other system
        // calls (`$isunknown`, `$countones`, …) stay rejected.
        let oh0 = serde_json::json!({
            "kind": "Call", "subroutine": "$onehot0",
            "arguments": [{"kind": "NamedValue", "symbol": "1 gnt", "type": "logic[7:0]"}]
        });
        let s0 = bool_expr(&oh0).expect("$onehot0 now translates");
        // at-most-one bit set ≡ 0 plus each power of two up to 2^7.
        assert!(s0.contains("gnt == 0"), "onehot0 includes zero: {s0}");
        assert!(
            s0.contains("gnt == 1") && s0.contains("gnt == 128"),
            "onehot0 spans the powers of two: {s0}"
        );

        let oh = serde_json::json!({
            "kind": "Call", "subroutine": "$onehot",
            "arguments": [{"kind": "NamedValue", "symbol": "1 gnt", "type": "logic[7:0]"}]
        });
        let s1 = bool_expr(&oh).expect("$onehot now translates");
        // exactly-one bit set ≡ the powers of two, NO zero term.
        assert!(!s1.contains("gnt == 0"), "onehot excludes zero: {s1}");
        assert!(
            s1.contains("gnt == 1") && s1.contains("gnt == 128"),
            "onehot spans the powers of two: {s1}"
        );

        let unk = serde_json::json!({
            "kind": "Call", "subroutine": "$isunknown",
            "arguments": [{"kind": "NamedValue", "symbol": "1 gnt", "type": "logic[7:0]"}]
        });
        let err = bool_expr(&unk).expect_err("$isunknown must reject");
        assert!(
            err.contains("$isunknown") || err.contains("not in the"),
            "got: {err}"
        );
    }

    #[test]
    fn inside_set_and_range_expand_to_supported_comparisons() {
        // `x inside {2, 4, [1:3]}` → `((x==2) || (x==4) || ((x>=1) && (x<=3)))` — every
        // atom is in the Tier-1 fragment. The slang AST wraps literals in `Conversion`
        // (stripped by `unwrap`), modelled here with the real `4'bNNN` constant form.
        let lit = |v: &str| {
            serde_json::json!({
                "kind": "Conversion",
                "operand": {"kind": "IntegerLiteral", "type": "bit[3:0]", "constant": v}
            })
        };
        let node = serde_json::json!({
            "kind": "Inside",
            "left": {"kind": "NamedValue", "type": "logic[3:0]", "symbol": "9 x"},
            "rangeList": [
                lit("4'b10"),   // 2
                lit("4'b100"),  // 4
                {"kind": "ValueRange", "rangeKind": "Simple",
                 "left": lit("4'b1"), "right": lit("4'b11")}   // [1:3]
            ]
        });
        let out = bool_expr(&node).expect("inside translates");
        assert_eq!(
            out, "((x == 2) || (x == 4) || ((x >= 1) && (x <= 3)))",
            "got: {out}"
        );
        // A set member that is not a signal/literal atom is rejected honestly.
        let bad = serde_json::json!({
            "kind": "Inside",
            "left": {"kind": "NamedValue", "type": "logic[3:0]", "symbol": "9 x"},
            "rangeList": [{"kind": "BinaryOp", "op": "Add"}]
        });
        assert!(
            bool_expr(&bad).is_err(),
            "a non-atom `inside` member must be rejected, not silently dropped"
        );
    }

    #[test]
    fn reduction_operand_in_1bit_comparison_expands_to_xor() {
        // prim_onehot_check EnableCheck shape: `(|oh_i) != en_i`. A reduction operand
        // in a 1-bit `!=` expands to XOR over the propositional fragment (the
        // reduction itself lowers `|oh_i → (oh_i != 0)`).
        let ne = serde_json::json!({
            "left": {
                "kind": "UnaryOp", "op": "BitwiseOr", "type": "logic",
                "operand": {"kind": "NamedValue", "symbol": "1 oh_i", "type": "logic[3:0]"}
            },
            "right": {"kind": "NamedValue", "symbol": "2 en_i", "type": "logic"}
        });
        let xor = compare(&ne, "!=").expect("(|oh_i) != en_i translates");
        assert!(xor.contains("oh_i != 0"), "reduction lowered: {xor}");
        assert!(
            xor.contains("&&") && xor.contains("||"),
            "!= is an XOR expansion: {xor}"
        );
        // `==` is the XNOR (same terms, opposite polarity structure).
        let xnor = compare(&ne, "==").expect("(|oh_i) == en_i translates");
        assert!(
            xnor.contains("oh_i != 0") && xnor.contains("&&") && xnor.contains("||"),
            "== is an XNOR expansion: {xnor}"
        );
        assert_ne!(xor, xnor, "XOR and XNOR expansions differ");
    }

    /// mununu#544 — every labelled assertion recovers the label its author wrote.
    ///
    /// monono's measurement: `--expect "a_reseed_only_at_a_span_edge=holds"` exited 4 while
    /// `--expect "affine_spr_sva_sva_0=holds"` exited 0 on the same run — the index was the only
    /// name that worked, so inserting a property mid-file silently re-pointed every later pin.
    ///
    /// The label was believed unavailable: it is not a field on the `ConcurrentAssertion` node.
    /// It is on the enclosing `Block`, as `"<address> <label>"`.
    #[test]
    fn an_assertions_sv_label_is_recovered_from_the_enclosing_block() {
        for (json, want) in [
            (
                TIER1_JSON,
                vec!["ap_bool", "ap_impl", "ap_nimpl", "ap_assume", "cp_cover"],
            ),
            (
                TIER2_JSON,
                vec!["ap_stable", "ap_changed", "ap_rose", "ap_fell", "ap_past"],
            ),
        ] {
            let report = translate_ast_json(json).expect("valid ast-json");
            let got: Vec<String> = report
                .translated
                .iter()
                .map(|t| {
                    t.label
                        .clone()
                        .unwrap_or_else(|| panic!("{} recovered no label", t.name))
                })
                .collect();
            assert_eq!(got, want, "labels must come back in source order");

            // The positional name still exists alongside it — that is what keeps an upgrade
            // migration-safe for a consumer holding index-based pins.
            for (idx, t) in report.translated.iter().enumerate() {
                assert!(
                    t.name.ends_with(&format!("_sva_{idx}")),
                    "positional name must survive: {}",
                    t.name
                );
            }
        }
    }

    /// ...and the extractor does not invent one. An UNLABELLED assertion's `Block` carries the
    /// bare address, whose tail is all digits — and an SV identifier cannot start with a digit,
    /// so that is a sound discriminator. Fabricating a label here would be worse than having
    /// none: a consumer would pin to an address that changes every elaboration.
    #[test]
    fn an_unlabelled_assertion_gets_no_label_rather_than_an_address() {
        assert_eq!(
            super::block_label("6338700059712 ap_bool").as_deref(),
            Some("ap_bool")
        );
        assert_eq!(
            super::block_label("6338700059712"),
            None,
            "bare address is not a label"
        );
        assert_eq!(super::block_label(""), None);
        assert_eq!(
            super::block_label("6338700059712 9bad"),
            None,
            "cannot start with a digit"
        );
        assert_eq!(
            super::block_label("6338700059712 a_reseed_only_at_a_span_edge").as_deref(),
            Some("a_reseed_only_at_a_span_edge"),
            "monono's actual label shape"
        );
    }

    #[test]
    fn xl3_tier2_fixture_translates_all_five_and_records_shadows() {
        let report = translate_ast_json(TIER2_JSON).expect("valid ast-json");
        assert_eq!(report.total(), 5, "fixture has 5 Tier-2 assertions");
        assert_eq!(
            report.unsupported.len(),
            0,
            "all 5 should translate; unsupported: {:?}",
            report.unsupported
        );
        for t in &report.translated {
            crate::mu_calculus::parser::parse(&t.formula)
                .unwrap_or_else(|e| panic!("{} failed to parse: {e:?}", t.name));
        }
        // Shadows: state_q (width 6, from $stable/$changed/$past) + v (width 1,
        // from $rose/$fell), deduped.
        let mut shadows = report.required_shadows.clone();
        shadows.sort_by(|a, b| a.base.cmp(&b.base));
        assert_eq!(
            shadows,
            vec![
                ShadowSignal {
                    base: "state_q".into(),
                    width: 6,
                    depth: 1
                },
                ShadowSignal {
                    base: "v".into(),
                    width: 1,
                    depth: 1
                },
            ],
            "required_shadows must dedup by base + carry width + depth"
        );
    }

    #[test]
    fn enum_member_comparison_resolves_to_literal() {
        // `state_q == MainSmError` where MainSmError is an enum member with value
        // 6'b101001 (=41). The package's EnumValue node carries the value; the
        // pre-pass folds the NamedValue reference to `41` so it translates.
        let doc = serde_json::json!({
            "members": [
                {"kind": "EnumValue", "name": "MainSmError", "value": "6'b101001"}
            ],
            "body": {
                "kind": "ConcurrentAssertion",
                "assertionKind": "Assert",
                "propertySpec": {
                    "kind": "Simple",
                    "expr": {
                        "kind": "BinaryOp", "op": "Equality",
                        "left":  {"kind": "NamedValue", "symbol": "1 state_q", "type": "csrng_pkg::main_sm_state_e"},
                        "right": {"kind": "NamedValue", "symbol": "2 MainSmError", "type": "csrng_pkg::main_sm_state_e"}
                    }
                }
            }
        });
        let report = translate_ast_json(&doc.to_string()).expect("valid");
        assert_eq!(
            report.unsupported.len(),
            0,
            "unsupported: {:?}",
            report.unsupported
        );
        assert_eq!(report.translated.len(), 1);
        assert!(
            report.translated[0].formula.contains("state_q == 41"),
            "enum member must fold to its value (41); got {}",
            report.translated[0].formula
        );
    }

    #[test]
    fn xl3_no_shadows_when_no_history() {
        // The Tier-1 fixture uses no history calls → empty shadow set.
        let report = translate_ast_json(TIER1_JSON).expect("valid ast-json");
        assert!(report.required_shadows.is_empty());
    }
}
