//! XL.1b/XL.1c — Tier-1 SVA → mu-calculus translation over slang's `--ast-json`.
//!
//! Walks the JSON [`crate::adapter::slang::run_ast_json`] produces, finds every
//! `ConcurrentAssertion`, and translates the Tier-1 fragment to a mu-calculus
//! formula string that [`crate::mu_calculus::parser::parse`] accepts and the
//! cube/CEGAR evaluator can check.
//!
//! **The shipped fragment** — this table is EXECUTABLE: `the_shipped_fragment_truth_table` (in
//! `mod tests`) carries one row per line below, supported and rejected alike, so the two cannot
//! drift apart without a red test. The slang shapes it uses are the real ones
//! (`testdata/sva_fragment_forms.{sv,ast.json}`, slang 11.0.448, 2026-10-14).
//!
//! | SVA | mu-calculus | since |
//! |---|---|---|
//! | `assert property (b)` | `nu X. (b && [] X)` (AG b) | XL.1b |
//! | `a \|-> b` | `nu X. ((!a \|\| b) && [] X)` (AG(a→b)) | XL.1b |
//! | `a \|=> b` | `nu X. ((!a \|\| [] b) && [] X)` (AG(a→AX b)) | XL.1b |
//! | `a \|-> ##k b` | k boxes: `(!a \|\| []…[] b)` | XL.4 |
//! | `a \|-> ##[m:n] b` | `[]ᵐ(b \|\| [](b \|\| … \|\| []b))` — b within [m,n] | XL.4 |
//! | `a \|-> b[*n]` | `(b && [](b && … && []b))` — n consecutive | XL.5 |
//! | `a \|-> b ##1 c ##2 d` | `(b && [](c && [][]d))` — a bounded chain of booleans | XL.5 |
//! | `(a ##1 b) \|-> c`, `a[*n] \|-> c` | `(!a \|\| [](!b \|\| c))` — FIXED-delay sequence antecedents | XL.5 |
//! | `(a ##[m:n] b) \|-> c`, `a[*m:n] \|-> c`, `a[*2] ##1 b \|-> c` | the consequent at EVERY match end, as a conjunction over the alternatives in NNF (`antecedent_implies`); ≤ 64 alternatives, ≤ 64 cycles | 2026-10-15 |
//! | `cover property (b)` | `mu X. (b \|\| <> X)` (EF b), plus the recoverability lens `nu Y. ((mu X. …) && [] Y)` | XL.2 |
//! | `disable iff (r) P` | gate the body `(r \|\| body)`, or drop it and record the reset when `gate_reset` | XL.1c |
//! | `$stable(x)` / `$changed(x)` | `(x == x__past)` / `(x != x__past)` | XL.3 |
//! | `$rose(x)` / `$fell(x)` (1-bit) | `(x && !x__past)` / `(!x && x__past)` | XL.3 |
//! | `$past(x[, k])`, any k ≥ 1 | the shadow atom `x__past` / `x__pastk` (a value, used inside a comparison) | XL.3, k-deep later |
//! | `$onehot(x)` / `$onehot0(x)` (≤ 32-bit) | the value set `x ∈ {1,2,4,…}` / `{0,1,2,4,…}` — one Or-of-Cmp atom | #204 |
//! | `x inside {v, [lo:hi], …}` (≤ 64 values) | `(x == v) \|\| ((x >= lo) && (x <= hi)) \|\| …` | XL.6 |
//! | `sig[hi:lo] OP k`, `$past(sig[hi:lo])` | the minted slice signal `sig__bitsM_L` (`[hi:lo]` only); `M`/`L` are the BIT POSITIONS yosys gives the indices (`index_positions`), so `x[11:8]` of `logic [15:8] x` is `x__bits3_0` | mununu#565, positions 2026-10-15 |
//! | `sig[k]` constant bit-select (packed, one dimension, typedef / struct resolved) | the 1-bit slice `sig__bitsP_P`; bare in boolean position | 2026-10-15 |
//! | `$rose` / `$fell` / `$stable` / `$changed` over `sig[k]` or `sig[hi:lo]` | the slice now against the same slice of `sig__past` | 2026-10-15 |
//! | `e throughout S`, `S` bounded | `S` with `e` conjoined at every cycle of the match, gap cycles included (`throughout_consequent`) | 2026-10-15 |
//! | `$countones(x) OP k`, `$countones(x[hi:lo])`, `$countones(x)` in boolean position | the minted combinational count `x__popcount` (`btor2::shadow::augment_with_popcounts`), ≤ 64 bits | 2026-10-15 |
//! | `$isunknown(x)` | `false` — the model is 2-state; recorded per assertion (`two_state_folds` → a `two-state-fold` note) | 2026-10-15 |
//! | `s_eventually b`, `a \|-> s_eventually b` | `AF b` = `mu Y. (b \|\| [] Y)`, the response shape the l2s rescue reduces | 2026-10-15 |
//! | `##[m:$] b` (last gap, boolean operand), `strong(…)` of it | `[]ᵐ AF b`; an unmarked one is read strongly and recorded (`strong_readings`) | 2026-10-15 |
//! | `b[*n:$]` as the whole consequent, n ≥ 1 | `b[*n]` (its first match; exact) | 2026-10-15 |
//! | `s_eventually [m:n] b`, `eventually [m:n] b`, `nexttime p`, `always p` | `b` within `[m,n]`; `[] p`; `nu W. (p && [] W)` | 2026-10-15 |
//! | `\|x`, `~\|x`, `&x`, `~&x` | `(x != 0)`, `(x == 0)`, `(x == 2^W-1)`, `(x != 2^W-1)` | XL.1c |
//! | `reg OP reg`, `reg == $past(reg) + k` | relational / addend atoms (`CmpReg`, `CmpRegAddend`) | H.D / H.G |
//! | `state_q == S_IDLE` (enum, `parameter`, `localparam`) | the elaborated integer | XL.6b, mununu#632 |
//! | packed-struct member `s.f` / `s.f.g`, element `cnt_q[1]` of `logic [1:0][W-1:0]` | the member's / element's slice (`packed_sub`: first member most significant, right-most dimension fastest); bare when 1-bit, `!= 0` otherwise in boolean position | OpenTitan Phase 1 |
//! | `a -> b`, `a <-> b`; `& \| ^ ~^ ~` with every operand and the result 1-bit | `(!a \|\| b)`, XNOR; the boolean connectives | OpenTitan Phase 1 |
//! | a constant slang folded (`constant` on the node: `{W{1'b1}}`, `W - 1`, a converted literal) | the folded value; X/Z refused | OpenTitan Phase 1 |
//! | a bare sequence as the property (`##1 a == b`) | `consequent_expr` — it holds at a start iff it matches there | OpenTitan Phase 1 |
//! | any expression the rows above refuse: arithmetic (`*` ≤ 32 bits), vector `& \| ^ ~`, shifts, `?:`, concatenation, signed comparisons, `^x`, `$past(expr)`, edge / `$countones` / `$onehot` calls over expressions, an in-range dynamic bit-select | `bool_expr`'s fallback: `expr_ir::build_expr` mints the expression as `mununu_x_<hash>` (`btor2::minted_expr`); bare when 1-bit, `!= 0` otherwise, `(mununu_x_… OP k)` against a constant | OpenTitan Phase 2 |
//! | a call to a user function whose body is one `return` (`min` / `max`) | inlined by the expression compiler | OpenTitan Phase 3 |
//!
//! Implication `a → b` is emitted as `!a || b` — the mu-calculus parser has no
//! `->` operator, and the rewrite is exact. Every rewrite above is exact
//! (semantics-preserving): added shadow flops and minted slice signals, no abstraction.
//!
//! **Rejected — with a named reason, never dropped** (claims-integrity), each a row of the same
//! test. Every `Err` reaches the user as `[unsupported] name: reason` / `UnsupportedAssertionView`:
//!
//! | SVA | reason, in short |
//! |---|---|
//! | `##[m:$]` before anything but the LAST element, `[*0:$]`, `##[m:$]` / `[*n:$]` in an antecedent | needs a sequence automaton — Tier-3b (an unbounded antecedent has unboundedly many match ends) |
//! | `weak(…)` of an unbounded sequence | satisfied by every infinite trace — checks nothing |
//! | `s_eventually` / `##[m:$]` over a sequence, `cover` of an eventuality | operand must be a boolean / `EF` of an `AF` is not a reachability question |
//! | `nexttime [k]`, `always [m:n]`, `s_nexttime` with a range | ranged forms not captured |
//! | `[->n]` goto, `[=n]` non-consecutive repetition | needs a sequence automaton — Tier-3b |
//! | `[*m:n]` as a consequent, repetition inside a CONSEQUENT chain element, a nested sub-sequence in an antecedent, `[*0…]`, an antecedent past 64 alternatives | length composition / the size guard |
//! | `e throughout S` with `S` unbounded or a repetition inside its chain; a non-boolean `e` | needs a sequence automaton / length composition |
//! | `sig[idx]` dynamic, `mem[i]` array content, an enum- or otherwise opaque-typed base | distinct refusals (mununu#514; the index cannot be mapped to a bit) |
//! | `/ % **`, `*` wider than 32 bits, X/Z constants, a dynamic index that can leave the range, `>>>` of a signed operand in an unsigned context, a function body other than one `return` | the expression compiler's refusals (`expr_ir`) |
//! | `x[i +: w]`, `x[i -: w]` | indexed part-select: slang gives base-and-WIDTH, declined rather than mis-sliced (mununu#565) |
//! | `$countones` over more than 64 bits; other system calls | the count cap / not supported |
//! | `$past(x, 0)`, `$stable(x, k)` | depth ≥ 1 / single argument |
//!
//! The first widening (2026-10-15) added the bit-select, the 2-state `$isunknown` fold and the
//! liveness forms above; the second adds ranged antecedents (and repetition inside an antecedent
//! chain) by NNF unrolling, `throughout` on bounded consequents and a minted `$countones`.
//! The OpenTitan Phase 1 (2026-10-15) adds struct members, multi-dimensional elements, `->` /
//! `<->`, 1-bit bitwise operators, slang-folded constants and bare sequence properties.
//! Goto / `[=n]` and unbounded antecedents stay out.
//!
//! **Instances (mununu#709).** Every assertion is translated once per instance it sits in, over
//! the signals as the flattened design names them (`u_rx.state_q`), with that instance's
//! parameter values (`scope_refs`, `ConstantValues`); a hierarchical reference from the top
//! resolves when its module has one instance. The contract is
//! `hierarchy_forms_translate_from_real_slang_json` (`testdata/sva_hierarchy_forms.{sv,ast.json}`).
//!
//! **Tier-2 shadow registers (XL.3).** `$past`/`$stable`/`$changed`/`$rose`/
//! `$fell` read a signal's *previous-cycle* value, encoded as an atom over a
//! shadow `<sig>__past` (`<sig>__pastk` at depth k). The translator emits the atom and records
//! the requirement in [`TranslationReport::required_shadows`]; the BTOR2
//! model-augmentation step (XL.3b) synthesises the shadow flop
//! (`next(<sig>__past) = <sig>`) so the atom binds.
//!
//! **Atoms (XL.1b + XL.1c).** A boolean leaf is one of:
//! - a 1-bit signal (`NamedValue` → identifier atom);
//! - a multi-bit signal in boolean position (vector → `(sig != 0)`, the implicit
//!   reduction-or SV applies to a vector condition);
//! - a comparison `sig OP k` (`==`/`!=`/`<`/`>`/`<=`/`>=` against an integer), `sig OP sig`,
//!   or `sig == sig + k`;
//! - a **reduction** (XL.1c): `|x`→`(x != 0)`, `~|x`→`(x == 0)`, `&x`→`(x == 2^W-1)`,
//!   `~&x`→`(x != 2^W-1)` (width `W` from the operand's slang type);
//! - a **compare-to-zero of a boolean expr** (XL.1c): `bexpr !== '0`→`bexpr`,
//!   `bexpr === '0`→`!bexpr` — this is how OpenTitan's `` `ASSERT `` macro encodes
//!   `disable iff ((!rst_ni) !== '0)`.
//!
//! Boolean structure (`!`, `&&`, `||`) recurses. Every emitted formula is validated through the
//! mu-calculus parser as a safety net.
//!
//! [XL.0]: ../../../../.claude/plans/measurements/XL-0-sva-parser-spike-2026-06-26.md

use serde_json::Value;

use crate::adapter::btor2::minted_expr::{CmpOp, ExprIr};

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

/// mununu#565 — a bit-slice a translated formula reads as an atom, e.g. `wdata[7:0]`.
///
/// The BTOR2 augmentation mints one named `slice` node per entry, so the mu-calculus atom is a
/// PLAIN IDENTIFIER (`wdata__bits7_0`). That is the whole design: `[` already means a Box
/// modality to the mu-calculus lexer and an array index to `PredicateExpr`, so a bracketed atom
/// would MISPARSE rather than fail. Minting a derived signal keeps every layer below the
/// translator untouched — exactly how `$past` is handled ([`ShadowSignal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceSignal {
    /// The signal being sliced, as it appears in the lifted BTOR2 (may itself be a `__past`
    /// shadow, for `$past(sig[7:0])`).
    pub base: String,
    /// Inclusive high bit.
    pub msb: u32,
    /// Inclusive low bit.
    pub lsb: u32,
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
    /// mununu#565: bit-slices the translated formulas reference as atoms (deduped). The BTOR2
    /// augmentation mints a named `slice` node per entry; empty means no slice was used.
    pub required_slices: Vec<SliceSignal>,
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
    /// Constructs folded to a constant because the lifted model is 2-state (`$isunknown(x)` is
    /// `false`: there is no X or Z to observe). One entry per (assertion, construct); verify-auto
    /// turns each into a per-property `two-state-fold` scope note, because a property that guards
    /// against X then holds trivially and must not read as an X-propagation check.
    pub two_state_folds: Vec<TranslationCaveat>,
    /// Unbounded sequences (`##[m:$] b`) read with the STRONG semantics. IEEE 1800-2017 makes a
    /// sequence in an `assert` weak by default, and a weak `##[1:$] b` is satisfied by every
    /// infinite trace — vacuous. Authors who write it mean "eventually", so the translator reads it
    /// as `s_eventually` and verify-auto says so per property (`strong-sequence-reading` note).
    /// An explicit `strong(…)` or `s_eventually` is not recorded: it already says what it means.
    pub strong_readings: Vec<TranslationCaveat>,
    /// Bases of `$countones(base)` the translated formulas read (deduped; a base may be a minted
    /// slice). The BTOR2 augmentation mints `<base>__popcount` per entry.
    pub required_popcounts: Vec<String>,
    /// Expressions the translated formulas read as minted atoms (`mununu_x_<hash>`, deduped):
    /// the expression compiler's output (`expr_ir`). The BTOR2 augmentation
    /// (`btor2::minted_expr::augment_with_expressions`) mints one named node per entry.
    pub required_expressions: Vec<MintedExpression>,
}

/// An expression a formula reads as the atom `name` (= `expr.minted_name()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedExpression {
    pub name: String,
    pub expr: ExprIr,
}

/// A per-assertion caveat the translator recorded: a 2-state fold
/// ([`TranslationReport::two_state_folds`]) or a strong reading of an unbounded sequence
/// ([`TranslationReport::strong_readings`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationCaveat {
    /// The assertion's report name (`<module>_sva_<n>`), the same key a verdict carries.
    pub assertion: String,
    /// The SVA construct, e.g. `$isunknown` or `##[1:$]`.
    pub construct: String,
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

    // Every assertion with the instance it sits in: one entry PER INSTANCE (slang serialises a
    // body shared by several instances once and refers to it by address), and the declaration
    // scopes its references resolve against.
    let mut scopes = Hierarchy::default();
    let bodies = collect_instance_bodies(&root);
    let mut found: Vec<FoundAssertion<'_>> = Vec::new();
    collect_assertions(
        &root,
        &Walk {
            enclosing: "design",
            label: None,
            body: None,
            prefix: "",
            rel: String::new(),
        },
        &bodies,
        &mut scopes,
        &mut found,
    );

    // XL.6b follow-up — enum-constant resolution. slang keeps an enum-member
    // reference (e.g. `MainSmError`) as a `NamedValue`, NOT folded to its value,
    // so `state_q == MainSmError` reads as signal-vs-signal. Map every enum
    // member to its integer value so the pre-pass can rewrite those references
    // to integer literals (enum-typed FSMs are ubiquitous in real RTL).
    //
    // mununu#632 — the same for `parameter` / `localparam` constants: slang keeps
    // `retry_cnt_q == MAX_RETRIES` as a `NamedValue` of the parameter while its
    // `Parameter` node already carries the ELABORATED value (`"3"`, `"3'b10"`; an
    // overridden `-G` parameter carries the override). Left unfolded, the engine
    // reported "unknown register/signal `MAX_RETRIES`" and SKIPPED the property —
    // on the design that found this, one of the four skipped properties was
    // VIOLATED. `localparam` state encodings are the dominant hand-written and
    // LLM-written FSM style, so this removed most FSM transition properties from
    // the no-sidecar path. An enum member wins over a parameter of the same name.
    let enum_values = ConstantValues::collect(&root);

    let type_aliases = collect_type_aliases(&root);
    // Phase 3: user functions the expression compiler may inline, typedefs resolved like specs.
    let subroutines: std::collections::HashMap<String, Value> =
        super::expr_ir::collect_subroutines(&root)
            .into_iter()
            .map(|(addr, def)| (addr, resolve_type_aliases(&def, &type_aliases)))
            .collect();
    MINT.with(|m| m.borrow_mut().subroutines = subroutines);

    let mut report = TranslationReport::default();
    for (idx, found) in found.iter().enumerate() {
        let FoundAssertion {
            module,
            label,
            node,
            body,
            prefix,
        } = found;
        // An assertion inside a sub-instance names its instance (`u_rx.u_dec.prim_diff_decode_sva_12`):
        // a module instantiated twice yields the same label twice, one per instance.
        let name = format!("{prefix}{module}_sva_{idx}");
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
        // Then name every signal as the flattened design does: `state_q` in instance `u_rx` is
        // `u_rx.state_q` there, and a hierarchical reference `u_rx.state_q` from the top too.
        let spec = match scope_refs(&spec, &scopes, *body, prefix) {
            Ok(spec) => spec,
            Err(reason) => {
                report.unsupported.push(UnsupportedAssertion {
                    name,
                    kind: Some(kind),
                    reason,
                });
                continue;
            }
        };
        // A typedef'd signal carries an opaque `type` (`"<addr> mod.byte_t"`); replace it by the
        // alias target (`logic[7:0]`) so a bit-select or slice can map SV indices to bit
        // positions (`index_positions`).
        let spec = resolve_type_aliases(&spec, &type_aliases);
        let spec = &spec;
        MINT.with(|m| {
            let mut m = m.borrow_mut();
            m.exprs.clear();
            m.shadows.clear();
        });
        match translate_one(spec, kind, opts, &mut report.reset_signals) {
            Ok(formula) => {
                // Phase 2: the expressions (and their `$past` shadows) this formula reads.
                let (exprs, shadows) = MINT.with(|m| {
                    let mut m = m.borrow_mut();
                    (std::mem::take(&mut m.exprs), std::mem::take(&mut m.shadows))
                });
                for e in exprs {
                    if !report.required_expressions.iter().any(|r| r.name == e.name) {
                        report.required_expressions.push(e);
                    }
                }
                for (base, width, depth) in shadows {
                    match report.required_shadows.iter_mut().find(|s| s.base == base) {
                        Some(s) => s.depth = s.depth.max(depth),
                        None => report
                            .required_shadows
                            .push(ShadowSignal { base, width, depth }),
                    }
                }
                // XL.2: a cover's `EF φ` gets its `AG EF φ` recoverability lens.
                // (Omitted if the companion somehow fails to parse — never ship
                // a formula the evaluator can't read.)
                let recoverability_companion = (kind == SvaKind::Cover)
                    .then(|| recoverability_companion_formula(&formula).ok())
                    .flatten();
                // XL.3: record the `__past` shadow flops this assertion needs
                // (only for assertions that actually translated).
                collect_shadow_signals(spec, &mut report.required_shadows);
                collect_slice_signals(spec, &mut report.required_slices);
                for base in collect_popcounts(spec) {
                    if !report.required_popcounts.contains(&base) {
                        report.required_popcounts.push(base);
                    }
                }
                for construct in collect_two_state_folds(spec) {
                    report.two_state_folds.push(TranslationCaveat {
                        assertion: name.clone(),
                        construct,
                    });
                }
                for construct in collect_implicit_strong_readings(spec) {
                    report.strong_readings.push(TranslationCaveat {
                        assertion: name.clone(),
                        construct,
                    });
                }
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

/// One concurrent assertion as found in the AST, with where it sits.
struct FoundAssertion<'a> {
    /// The nearest enclosing named scope (module, generate block) — the positional name's stem.
    module: String,
    label: Option<String>,
    node: &'a Value,
    /// The instance body it sits in (`None` outside any instance).
    body: Option<u64>,
    /// The flattened instance prefix of this occurrence: `""` for the top, `"u_rx.u_dec."` below.
    prefix: String,
}

/// The declaration scopes of the elaborated design, as the flattened design names them.
#[derive(Default)]
struct Hierarchy {
    /// A declaration's address → its instance body and its path inside that body (`""`, or the
    /// enclosing generate blocks: `"gen_no_async."`, `"gen_cnts[1]."`).
    decls: std::collections::HashMap<u64, (u64, String)>,
    /// An instance body's address → the flattened prefix of every instance of it.
    occurrences: std::collections::HashMap<u64, Vec<String>>,
}

/// The walk's position: what [`collect_assertions`] carries down the AST.
struct Walk<'s> {
    enclosing: &'s str,
    label: Option<&'s str>,
    /// The instance body being walked, and its occurrence's flattened prefix.
    body: Option<u64>,
    prefix: &'s str,
    /// The generate-block path inside `body`.
    rel: String,
}

/// Every `InstanceBody` by address — the target of an instance whose `body` is a reference
/// (`"<addr> <module>"`) to a body serialised under an earlier instance.
fn collect_instance_bodies(root: &Value) -> std::collections::HashMap<u64, &Value> {
    fn walk<'a>(n: &'a Value, out: &mut std::collections::HashMap<u64, &'a Value>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("InstanceBody")
                    && let Some(addr) = m.get("addr").and_then(Value::as_u64)
                {
                    out.entry(addr).or_insert(n);
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
    walk(root, &mut out);
    out
}

/// The name a generate block contributes to the flattened path: its own name, or `<array>[<i>]`
/// for an iteration of a `for` generate (slang leaves the iteration unnamed and records the loop
/// variable as a `Parameter` member). `None` for an iteration whose index is not found.
fn generate_block_name(
    block: &serde_json::Map<String, Value>,
    array: Option<(&str, &str)>,
) -> Option<String> {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
    if !name.is_empty() {
        return Some(name.to_string());
    }
    let (array_name, var) = array?;
    let index = block
        .get("members")?
        .as_array()?
        .iter()
        .find(|m| {
            m.get("kind").and_then(Value::as_str) == Some("Parameter")
                && m.get("name").and_then(Value::as_str) == Some(var)
        })?
        .get("value")?
        .as_str()
        .and_then(parse_sv_literal)?;
    Some(format!("{array_name}[{index}]"))
}

/// Collect every `ConcurrentAssertion`, with the scope that encloses it, its SV label (see
/// [`block_label`]) when it has one, and its instance occurrence; record every declaration's
/// scope into `scopes` on the way.
///
/// An `Instance` opens a new occurrence (prefix `<parent prefix><generate path><name>.`, empty
/// for a top-level instance) and is walked through its body, resolved via `bodies` when slang
/// serialised it as a reference. A generate block extends the path; an UNINSTANTIATED one (the
/// branch the elaboration did not take) is skipped — its assertions are not part of the design.
fn collect_assertions<'a>(
    node: &'a Value,
    at: &Walk<'_>,
    bodies: &std::collections::HashMap<u64, &'a Value>,
    scopes: &mut Hierarchy,
    out: &mut Vec<FoundAssertion<'a>>,
) {
    match node {
        Value::Object(map) => {
            let kind = map.get("kind").and_then(Value::as_str);
            match kind {
                Some("ConcurrentAssertion") => out.push(FoundAssertion {
                    module: at.enclosing.to_string(),
                    label: at.label.map(str::to_string),
                    node,
                    body: at.body,
                    prefix: at.prefix.to_string(),
                }),
                Some("Instance") => {
                    let name = map.get("name").and_then(Value::as_str).unwrap_or("");
                    let body = match map.get("body") {
                        Some(b @ Value::Object(_)) => Some(b),
                        Some(Value::String(r)) => r
                            .split(' ')
                            .next()
                            .and_then(|a| a.parse::<u64>().ok())
                            .and_then(|a| bodies.get(&a).copied()),
                        _ => None,
                    };
                    let Some(body) = body else { return };
                    let Some(addr) = body.get("addr").and_then(Value::as_u64) else {
                        return;
                    };
                    let prefix = match at.body {
                        None => String::new(),
                        Some(_) => format!("{}{}{name}.", at.prefix, at.rel),
                    };
                    scopes
                        .occurrences
                        .entry(addr)
                        .or_default()
                        .push(prefix.clone());
                    collect_assertions(
                        body,
                        &Walk {
                            enclosing: if name.is_empty() { at.enclosing } else { name },
                            label: at.label,
                            body: Some(addr),
                            prefix: &prefix,
                            rel: String::new(),
                        },
                        bodies,
                        scopes,
                        out,
                    );
                    return;
                }
                Some("GenerateBlock") | Some("GenerateBlockArray")
                    if map.get("isUninstantiated").and_then(Value::as_bool) == Some(true) =>
                {
                    return;
                }
                Some("GenerateBlockArray") => {
                    let array = map.get("name").and_then(Value::as_str).unwrap_or("");
                    let var = map
                        .get("loopVariable")
                        .and_then(Value::as_str)
                        .and_then(|v| v.rsplit(' ').next())
                        .unwrap_or("");
                    for member in map
                        .get("members")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let Value::Object(block) = member else {
                            continue;
                        };
                        if block.get("kind").and_then(Value::as_str) != Some("GenerateBlock") {
                            continue;
                        }
                        if block.get("isUninstantiated").and_then(Value::as_bool) == Some(true) {
                            continue;
                        }
                        // An iteration whose index cannot be read keeps a path no flattened name
                        // matches (`?`), so its references refuse rather than mis-resolve.
                        let seg = generate_block_name(block, Some((array, var)))
                            .unwrap_or_else(|| format!("{array}[?]"));
                        let inner = Walk {
                            enclosing: if array.is_empty() {
                                at.enclosing
                            } else {
                                array
                            },
                            label: at.label,
                            body: at.body,
                            prefix: at.prefix,
                            rel: format!("{}{seg}.", at.rel),
                        };
                        for v in block.values() {
                            collect_assertions(v, &inner, bodies, scopes, out);
                        }
                    }
                    return;
                }
                Some("Variable") | Some("Net") => {
                    if let (Some(body), Some(addr)) =
                        (at.body, map.get("addr").and_then(Value::as_u64))
                    {
                        scopes.decls.entry(addr).or_insert((body, at.rel.clone()));
                    }
                }
                _ => {}
            }
            // Update the enclosing name for descendants when this node names one.
            let next = map
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(at.enclosing);
            // A `Block` carrying a symbol-table entry re-labels its descendants; the NEAREST
            // enclosing one wins, so a nested block shadows an outer label rather than leaking it.
            let owned = (kind == Some("Block"))
                .then(|| {
                    map.get("block")
                        .and_then(Value::as_str)
                        .and_then(block_label)
                })
                .flatten();
            let rel = match kind {
                Some("GenerateBlock") => match generate_block_name(map, None) {
                    Some(seg) => format!("{}{seg}.", at.rel),
                    None => at.rel.clone(),
                },
                _ => at.rel.clone(),
            };
            let inner = Walk {
                enclosing: next,
                label: owned.as_deref().or(at.label),
                body: at.body,
                prefix: at.prefix,
                rel,
            };
            for v in map.values() {
                collect_assertions(v, &inner, bodies, scopes, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_assertions(v, at, bodies, scopes, out);
            }
        }
        _ => {}
    }
}

/// mununu#709 — name every signal an assertion reads as the flattened design names it.
///
/// - A `NamedValue` declared in the assertion's own instance body becomes
///   `<occurrence prefix><generate path><name>` — `state_q` in `u_rx` is `u_rx.state_q`. Before
///   this, an assertion inside a sub-instance read the TOP's signal of the same name (or none):
///   checked against the wrong net whenever the names collided.
/// - A `HierarchicalValue` (`u_rx.state_q` written in the top) becomes a `NamedValue` of the
///   declaration's flattened name when its body has exactly one instance.
///
/// `Err` for a reference that cannot be named that way: a hierarchical reference into a body
/// several instances share (slang gives the declaration, not the instance), or a path through a
/// `for`-generate iteration (`gen[1].x` — brackets are not part of the atom grammar).
fn scope_refs(
    spec: &Value,
    scopes: &Hierarchy,
    body: Option<u64>,
    prefix: &str,
) -> Result<Value, String> {
    match spec {
        Value::Object(m) => {
            let kind = m.get("kind").and_then(Value::as_str);
            if matches!(kind, Some("NamedValue") | Some("HierarchicalValue"))
                && let Some(sym) = m.get("symbol").and_then(Value::as_str)
                && let Some((addr, name)) = sym.split_once(' ')
                && let Ok(addr) = addr.parse::<u64>()
                && let Some((decl_body, rel)) = scopes.decls.get(&addr)
            {
                let full = if kind == Some("NamedValue") {
                    if Some(*decl_body) != body {
                        return Ok(spec.clone());
                    }
                    format!("{prefix}{rel}{name}")
                } else {
                    match scopes.occurrences.get(decl_body).map(Vec::as_slice) {
                        Some([only]) => format!("{only}{rel}{name}"),
                        Some(many) => {
                            return Err(format!(
                                "hierarchical reference to `{name}` in a module instantiated \
                                 {} times: the reference does not say which instance",
                                many.len()
                            ));
                        }
                        None => return Ok(spec.clone()),
                    }
                };
                if full.contains('[') {
                    return Err(format!(
                        "`{full}` sits in a `for`-generate iteration: a bracketed hierarchical \
                         name is not supported yet"
                    ));
                }
                let mut out = m.clone();
                out.insert("kind".into(), Value::String("NamedValue".into()));
                out.insert("symbol".into(), Value::String(format!("{addr} {full}")));
                return Ok(Value::Object(out));
            }
            let mut out = serde_json::Map::new();
            for (k, v) in m {
                out.insert(k.clone(), scope_refs(v, scopes, body, prefix)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(a) => a
            .iter()
            .map(|v| scope_refs(v, scopes, body, prefix))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// XL.6b follow-up / mununu#632 — the elaborated integer value of every enum member and every
/// `parameter` / `localparam` (slang `EnumValue` / `Parameter` nodes, `value` an SV literal like
/// `6'b101001`), so [`resolve_enum_refs`] can fold a reference to one into a literal: slang keeps
/// `state_q == MainSmError` and `retry_cnt_q == MAX_RETRIES` as `NamedValue`s, and the engine would
/// report the constant as an unknown signal. A value that is not a literal (a string, a type, an
/// unpacked value) is skipped, and a reference to it stays a `NamedValue`.
///
/// mununu#709 — keyed by the declaration's ADDRESS, which a reference's `symbol` carries: names collide across
/// modules (`Width` is 6 in one instance and 2 in another, `Idle` encodes differently in two FSMs),
/// and the former name-keyed map gave every reference the first declaration's value. The by-name
/// map is a fallback for a reference whose declaration was not serialised, holding only the names
/// every declaration agrees on (an enum member wins over a parameter of the same name).
struct ConstantValues {
    by_addr: std::collections::HashMap<u64, i64>,
    by_name: std::collections::HashMap<String, i64>,
}

impl ConstantValues {
    fn collect(root: &Value) -> Self {
        #[derive(Default)]
        struct Seen {
            by_addr: std::collections::HashMap<u64, i64>,
            enums: std::collections::HashMap<String, Option<i64>>,
            params: std::collections::HashMap<String, Option<i64>>,
        }
        fn note(map: &mut std::collections::HashMap<String, Option<i64>>, name: &str, v: i64) {
            map.entry(name.to_string())
                .and_modify(|e| {
                    if *e != Some(v) {
                        *e = None;
                    }
                })
                .or_insert(Some(v));
        }
        fn walk(n: &Value, seen: &mut Seen) {
            match n {
                Value::Object(m) => {
                    let kind = m.get("kind").and_then(Value::as_str);
                    if matches!(kind, Some("EnumValue") | Some("Parameter"))
                        && let Some(name) = m.get("name").and_then(Value::as_str)
                        && !name.is_empty()
                        && let Some(v) = m
                            .get("value")
                            .and_then(Value::as_str)
                            .and_then(parse_sv_literal)
                    {
                        if let Some(addr) = m.get("addr").and_then(Value::as_u64) {
                            seen.by_addr.insert(addr, v);
                        }
                        if kind == Some("EnumValue") {
                            note(&mut seen.enums, name, v);
                        } else {
                            note(&mut seen.params, name, v);
                        }
                    }
                    for v in m.values() {
                        walk(v, seen);
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        walk(v, seen);
                    }
                }
                _ => {}
            }
        }
        let mut seen = Seen::default();
        walk(root, &mut seen);
        let mut by_name = std::collections::HashMap::new();
        for (name, v) in seen.params {
            if let Some(v) = v {
                by_name.insert(name, v);
            }
        }
        for (name, v) in seen.enums {
            match v {
                Some(v) => {
                    by_name.insert(name, v);
                }
                None => {
                    by_name.remove(&name);
                }
            }
        }
        ConstantValues {
            by_addr: seen.by_addr,
            by_name,
        }
    }

    /// The value a reference's `symbol` (`"<addr> <name>"`) denotes, if it is a constant.
    fn of(&self, symbol: &str) -> Option<i64> {
        let (addr, name) = symbol.split_once(' ').unwrap_or(("", symbol));
        match addr.parse::<u64>().ok().and_then(|a| self.by_addr.get(&a)) {
            Some(v) => Some(*v),
            None => self.by_name.get(name).copied(),
        }
    }
}

/// XL.6b follow-up — deep-clone an expression tree, replacing every `NamedValue` (or
/// `HierarchicalValue`) referencing an enum member — or, since mununu#632, a `parameter` /
/// `localparam` constant — with an `IntegerLiteral` of its value ([`ConstantValues::of`]). A
/// signal *of* an enum type (e.g. `state_q`) is not a constant, so it is left untouched.
fn resolve_enum_refs(node: &Value, enums: &ConstantValues) -> Value {
    match node {
        Value::Object(m) => {
            if matches!(
                m.get("kind").and_then(Value::as_str),
                Some("NamedValue") | Some("HierarchicalValue")
            ) && let Some(sym) = m.get("symbol").and_then(Value::as_str)
                && let Some(v) = enums.of(sym)
            {
                // Keep slang's `type`: the expression compiler sizes the literal by it.
                let mut lit = serde_json::json!({
                    "kind": "IntegerLiteral",
                    "constant": v.to_string(),
                    "value": v.to_string(),
                });
                if let Some(ty) = m.get("type") {
                    lit["type"] = ty.clone();
                }
                return lit;
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

/// Every `TypeAlias` in the AST, `addr → target type string` (`"logic[7:0]"`,
/// `"struct packed{…}…"`). slang writes a typedef'd signal's `type` as `"<addr> <scope>.<name>"`,
/// which says nothing about its declared range; the alias node, found by address, does. Chains
/// (an alias of an alias) are followed up to a fixed depth.
fn collect_type_aliases(root: &Value) -> std::collections::HashMap<String, String> {
    fn walk(n: &Value, out: &mut std::collections::HashMap<String, String>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("TypeAlias")
                    && let (Some(addr), Some(target)) = (
                        m.get("addr").and_then(Value::as_u64),
                        m.get("target").and_then(Value::as_str),
                    )
                {
                    out.insert(addr.to_string(), target.to_string());
                }
                for v in m.values() {
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = std::collections::HashMap::new();
    walk(root, &mut out);
    out
}

/// The alias address a slang type string names, when it names one: `"9382… t.byte_t"` → `"9382…"`.
fn alias_addr(ty: &str) -> Option<&str> {
    let (addr, rest) = ty.split_once(' ')?;
    (!rest.is_empty() && addr.chars().all(|c| c.is_ascii_digit())).then_some(addr)
}

/// Expand the alias references EMBEDDED in a type string — a packed struct's member typed by
/// another typedef serialises as `struct packed{93825836944992 p::esc_t tx;logic[2:0] cnt;}p::s$3`
/// — so the member's width is readable. A reference is a run of digits at the start of a member
/// (after `{` or `;`), a space, and the alias name up to the next space. Up to 8 rounds (nested
/// typedefs); an unknown address is left as it is (its width then stays unreadable — a refusal).
fn expand_embedded_aliases(
    ty: &str,
    aliases: &std::collections::HashMap<String, String>,
) -> String {
    let mut t = ty.to_string();
    for _ in 0..8 {
        let mut out = String::with_capacity(t.len());
        let mut changed = false;
        let mut rest = t.as_str();
        while let Some(i) = rest.find(['{', ';']) {
            out.push_str(&rest[..=i]);
            rest = &rest[i + 1..];
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 0
                && rest[digits..].starts_with(' ')
                && let Some(target) = aliases.get(&rest[..digits])
            {
                let after = &rest[digits + 1..];
                let name_len = after.find([' ', ';', '}']).unwrap_or(after.len());
                out.push_str(target);
                rest = &after[name_len..];
                changed = true;
            }
        }
        out.push_str(rest);
        t = out;
        if !changed {
            break;
        }
    }
    t
}

/// Deep-clone `node`, replacing every alias-named `type` by the alias target (followed through
/// up to 8 links). Any other field is untouched.
fn resolve_type_aliases(
    node: &Value,
    aliases: &std::collections::HashMap<String, String>,
) -> Value {
    match node {
        Value::Object(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                if k == "type"
                    && let Some(ty) = v.as_str()
                {
                    let mut t = ty.to_string();
                    for _ in 0..8 {
                        match alias_addr(&t).and_then(|a| aliases.get(a)) {
                            Some(target) => t = target.clone(),
                            None => break,
                        }
                    }
                    obj.insert(
                        k.clone(),
                        Value::String(expand_embedded_aliases(&t, aliases)),
                    );
                } else {
                    obj.insert(k.clone(), resolve_type_aliases(v, aliases));
                }
            }
            Value::Object(obj)
        }
        Value::Array(a) => {
            Value::Array(a.iter().map(|v| resolve_type_aliases(v, aliases)).collect())
        }
        other => other.clone(),
    }
}

/// XL.3: walk a (translated) assertion's spec and record every base signal a
/// Tier-2 history call (`$past`/`$stable`/`$changed`/`$rose`/`$fell`) reads, so
/// the BTOR2 augmentation knows which `__past` shadow flops to synthesise.
/// Deduped by base name. Only call on specs that translated successfully.
/// mununu#565 — collect every bit-slice the translated formulas will read as an atom.
///
/// Mirrors [`collect_shadow_signals`]: a separate walk over the spec, so `compare` stays a pure
/// `Result<String, String>` with no side channel. Deduped by (base, msb, lsb).
///
/// A slice under `$past` is recorded against the SHADOW name, because that is the signal the
/// minted `slice` node must read — `$past(sig[7:0])` is `slice(sig__past, 7, 0)`.
fn collect_slice_signals(node: &Value, out: &mut Vec<SliceSignal>) {
    match node {
        Value::Object(map) => {
            if is_past_call(node) {
                if let Some(arg) = past_call_arg_node(node)
                    && let Some(Ok((base, msb, lsb))) = slice_ref(arg)
                {
                    let depth = past_call_depth(node).unwrap_or(1);
                    push_slice(out, past_shadow_name(&base, depth), msb, lsb);
                }
            } else if is_edge_or_stability_call(node) {
                // `$rose(sig[3])`, `$stable(sig[7:4])` read the slice NOW and the same slice of
                // the depth-1 shadow; the walk below records the current-cycle one.
                if let Some(arg) = past_call_arg_node(node)
                    && let Some(Ok((base, msb, lsb))) = slice_ref(arg)
                {
                    push_slice(out, past_shadow_name(&base, 1), msb, lsb);
                }
            } else if let Some(Ok((base, msb, lsb))) = slice_ref(node) {
                push_slice(out, base, msb, lsb);
            }
            for v in map.values() {
                collect_slice_signals(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_slice_signals(v, out);
            }
        }
        _ => {}
    }
}

/// The 2-state folds an assertion's spec uses (deduped): today `$isunknown`.
fn collect_two_state_folds(node: &Value) -> Vec<String> {
    fn walk(n: &Value, out: &mut Vec<String>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("Call")
                    && let Some(sub @ "$isunknown") = m.get("subroutine").and_then(Value::as_str)
                    && !out.iter().any(|c| c == sub)
                {
                    out.push(sub.to_string());
                }
                m.values().for_each(|v| walk(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// The unbounded delays (`##[m:$]`) an assertion reads with the strong semantics WITHOUT the author
/// having said `strong(…)` (deduped, spelled as written). Unbounded repetition `[*n:$]` is not
/// recorded: as a consequent it collapses exactly to `[*n]` under either reading.
fn collect_implicit_strong_readings(node: &Value) -> Vec<String> {
    fn walk(n: &Value, under_strong: bool, out: &mut Vec<String>) {
        match n {
            Value::Object(m) => {
                let kind = m.get("kind").and_then(Value::as_str);
                let strong_here = under_strong
                    || (kind == Some("StrongWeak")
                        && m.get("strength").and_then(Value::as_str) == Some("strong"));
                if kind == Some("SequenceConcat")
                    && !strong_here
                    && let Some(els) = m.get("elements").and_then(Value::as_array)
                {
                    for el in els {
                        if el.get("max").and_then(Value::as_str) == Some("$") {
                            let min = el.get("min").and_then(Value::as_u64).unwrap_or(0);
                            let c = format!("##[{min}:$]");
                            if !out.contains(&c) {
                                out.push(c);
                            }
                        }
                    }
                }
                m.values().for_each(|v| walk(v, strong_here, out));
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, under_strong, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(node, false, &mut out);
    out
}

/// Does the spec use a liveness construct (an unbounded eventuality)? A `cover` of one is
/// rejected: `cover` is `EF`, and `EF` of an `AF` is not what a cover author means.
fn uses_eventuality(node: &Value) -> bool {
    match node {
        Value::Object(m) => {
            let unary_ev = m.get("kind").and_then(Value::as_str) == Some("Unary")
                && m.get("op").and_then(Value::as_str) == Some("SEventually");
            unary_ev
                || m.get("max").and_then(Value::as_str) == Some("$")
                || m.values().any(uses_eventuality)
        }
        Value::Array(a) => a.iter().any(uses_eventuality),
        _ => false,
    }
}

/// SVA-fragment widening (2026-10-15) — the minted atom name for `$countones(base)`. One naming
/// contract with `btor2::shadow::popcount_signal_name`.
fn popcount_atom_name(base: &str) -> String {
    format!("{base}__popcount")
}

/// `$countones(x)` → the base its count is minted over (`x`, or a slice `x__bitsM_L`). `None` when
/// `expr` is not a `$countones` call; `Some(Err)` for one the translator refuses, with the reason.
fn countones_base(expr: &Value) -> Option<Result<String, String>> {
    let expr = unwrap(expr);
    if expr.get("kind").and_then(Value::as_str) != Some("Call")
        || expr.get("subroutine").and_then(Value::as_str) != Some("$countones")
    {
        return None;
    }
    let args = expr.get("arguments").and_then(Value::as_array);
    let Some([arg]) = args.map(Vec::as_slice) else {
        return Some(Err("$countones takes exactly one argument".into()));
    };
    let arg = unwrap(arg);
    let (base, width) = match slice_ref(arg) {
        Some(Ok((b, msb, lsb))) => (slice_atom_name(&b, msb, lsb), msb - lsb + 1),
        Some(Err(e)) => return Some(Err(e)),
        None => match signal_name(arg) {
            Ok(b) if !is_unpacked_array(arg) => (b.to_string(), signal_width(arg)),
            _ => {
                return Some(Err(
                    "$countones over anything but a packed signal or a `[hi:lo]` slice is not \
                     supported"
                        .into(),
                ));
            }
        },
    };
    if width > crate::adapter::btor2::shadow::MAX_POPCOUNT_WIDTH {
        return Some(Err(format!(
            "$countones over a {width}-bit operand — the minted count is capped at {} bits",
            crate::adapter::btor2::shadow::MAX_POPCOUNT_WIDTH
        )));
    }
    Some(Ok(base))
}

/// Every `$countones` base an assertion reads (deduped).
fn collect_popcounts(node: &Value) -> Vec<String> {
    fn walk(n: &Value, out: &mut Vec<String>) {
        match n {
            Value::Object(m) => {
                if let Some(Ok(base)) = countones_base(n)
                    && !out.contains(&base)
                {
                    out.push(base);
                }
                m.values().for_each(|v| walk(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// `$stable` / `$changed` / `$rose` / `$fell` — the depth-1 history calls.
fn is_edge_or_stability_call(expr: &Value) -> bool {
    let expr = unwrap(expr);
    expr.get("kind").and_then(Value::as_str) == Some("Call")
        && matches!(
            expr.get("subroutine").and_then(Value::as_str),
            Some("$stable" | "$changed" | "$rose" | "$fell")
        )
}

/// Dedupe helper for [`collect_slice_signals`].
fn push_slice(out: &mut Vec<SliceSignal>, base: String, msb: u32, lsb: u32) {
    if !out
        .iter()
        .any(|s| s.base == base && s.msb == msb && s.lsb == lsb)
    {
        out.push(SliceSignal { base, msb, lsb });
    }
}

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
                    past_call_arg(node)
                        .ok()
                        .map(|(b, w, d)| (b.to_string(), w, d))
                } else {
                    call_arg_signal(node)
                        .ok()
                        .map(|(b, w)| (b.to_string(), w, 1))
                };
                // A history call over a SLICE or a BIT (`$past(sig[7:0])`, `$rose(sig[3])`) reads
                // `slice(<sig>__past…)`, so the WHOLE base needs its shadow. Without this the
                // shadow was never requested, the minted slice had no base, and the atom never
                // bound — the property skipped (found 2026-10-15).
                let resolved = resolved.or_else(|| {
                    let arg = past_call_arg_node(node)?;
                    let Some(Ok((base, _, _))) = slice_ref(arg) else {
                        return None;
                    };
                    let mut root = arg;
                    while let Some(v) = root.get("value") {
                        root = unwrap(v);
                    }
                    let width = signal_width(root);
                    let depth = if sub == Some("$past") {
                        past_call_depth(node)?
                    } else {
                        1
                    };
                    Some((base, width, depth))
                });
                if let Some((base, width, depth)) = resolved {
                    let base: &str = &base;
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
    if kind == SvaKind::Cover && uses_eventuality(spec) {
        return Err(
            "a `cover` of a liveness sequence (`s_eventually`, `##[m:$]`) is not \
                    supported: `cover` reads as `EF`, and `EF` of an eventuality is not a \
                    reachability question"
                .to_string(),
        );
    }
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
            // mununu#705 — the disable guards every boolean block at the cycle it is evaluated,
            // not only the attempt's first (`assume_guarantee::guard_with_disable`). A body this
            // cannot guard keeps the first-cycle form: stronger than SVA, never weaker.
            Ok(
                super::assume_guarantee::guard_with_disable(&inner, &cond, false)
                    .unwrap_or_else(|_| format!("({cond} || {inner})")),
            )
        }
        // `a |-> <conseq>` / `a |=> <conseq>` — the antecedent is a boolean, a fixed-delay
        // `##` chain of booleans or a fixed `[*n]` repetition (XL.5, `antecedent_implies`);
        // the consequent is a boolean or a bounded sequence (XL.4, `consequent_expr`). Ranged
        // or unbounded antecedents are rejected there with their reason.
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
        // `assert property (s_eventually b)`, `(always b)`, `(nexttime b)`, `strong(…)`: a
        // temporal property with no antecedent — lowered exactly as a consequent would be, then
        // checked at every cycle by the `nu X` wrapper (`s_eventually b` → `AG AF b`).
        "Unary" | "StrongWeak" => consequent_expr(spec),
        // A bare sequence as the whole property (`##1 a == b`, `a ##2 b`): IEEE 1800 16.12.2 —
        // it holds at a start iff the sequence matches from there, which is how a consequent
        // reads it. Bounded sequences only (`consequent_expr` refuses the rest).
        "SequenceConcat" => consequent_expr(spec),
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
        // `b[*n:$]` as the WHOLE consequent: its first match is `b[*n]`, and an implication's
        // consequent holds iff some match exists, so it is exactly `b[*n]` (n ≥ 1). Inside a
        // chain it is not, and `seq_leaf` keeps rejecting it there.
        Some("Simple")
            if spec
                .get("repetition")
                .and_then(|r| r.get("max"))
                .and_then(Value::as_str)
                == Some("$") =>
        {
            let rep = spec.get("repetition").expect("guarded");
            let min = rep.get("min").and_then(Value::as_u64).unwrap_or(0);
            if rep.get("kind").and_then(Value::as_str) != Some("Consecutive") || min == 0 {
                return Err(format!(
                    "unbounded repetition `[*{min}:$]` other than a consecutive `[*n:$]` with \
                     n >= 1 needs a sequence automaton — Tier-3b"
                ));
            }
            let fixed = serde_json::json!({"kind": "Consecutive", "min": min, "max": min});
            lower_repetition(&bool_expr(child(spec, "expr")?)?, &fixed)
        }
        // A boolean leaf, possibly with a consecutive repetition (`b[*n]`).
        Some("Simple") => seq_leaf(spec),
        Some("Unary") => unary_temporal(spec),
        // `strong(S)` / `weak(S)`. Over a BOUNDED sequence the two readings coincide on infinite
        // traces. Over an unbounded one, `strong` is the eventuality; `weak` is satisfied by every
        // infinite trace, and lowering it to `true` would be exact but would read as a proof.
        // `e throughout S` (2026-10-15): S matches AND `e` holds at every cycle of that match,
        // gap cycles included. Over a BOUNDED S only (`throughout_consequent`).
        Some("Binary") if spec.get("op").and_then(Value::as_str) == Some("Throughout") => {
            let e_spec = child(spec, "left")?;
            if e_spec.get("kind").and_then(Value::as_str) != Some("Simple")
                || e_spec.get("repetition").is_some()
            {
                return Err("the left operand of `throughout` must be a boolean".into());
            }
            let e = bool_expr(child(e_spec, "expr")?)?;
            throughout_consequent(child(spec, "right")?, &e)
        }
        Some("StrongWeak") => {
            let inner = child(spec, "expr")?;
            let weak = spec.get("strength").and_then(Value::as_str) == Some("weak");
            if weak && uses_eventuality(inner) {
                return Err(
                    "`weak(…)` of an unbounded sequence is satisfied by every infinite \
                            trace, so it checks nothing; write `strong(…)` or `s_eventually` \
                            for the eventuality"
                        .to_string(),
                );
            }
            consequent_expr(inner)
        }
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
                // `##[m:$] b` — b at some cycle ≥ m: `[]^m AF b` (the strong reading; see
                // `TranslationReport::strong_readings`). Only over a boolean leaf.
                if el.get("max").and_then(Value::as_str) == Some("$") {
                    return unbounded_delay(el, seq);
                }
                let leaf = seq_leaf(seq)?;
                let (min, max) = elem_delay(el)?;
                Ok(delayed(&leaf, min, max))
            } else {
                // A multi-element `##` chain. To keep every element's cycle offset
                // unambiguous, each element must be a length-1 boolean (no
                // repetition / nested sub-sequence — those compose lengths, Tier-3b).
                let mut leaves: Vec<String> = Vec::with_capacity(elements.len());
                let mut delays: Vec<(u32, u32)> = Vec::with_capacity(elements.len());
                // An unbounded gap is lowered only before the LAST element: `b ##[1:$] c` is
                // `b && [] AF c`. Anywhere else (`##[1:$] b ##1 c`) it is LTL `F(b ∧ X c)`, which
                // `AF(b ∧ AX c)` would strengthen — rejected.
                let last = elements.len() - 1;
                let mut unbounded_tail: Option<String> = None;
                for (i, el) in elements.iter().enumerate() {
                    if el.get("max").and_then(Value::as_str) == Some("$") {
                        if i != last || i == 0 {
                            return Err("an unbounded `##[m:$]` gap is supported only before \
                                 the last element of a chain (`b ##[1:$] c`); elsewhere it \
                                 needs a sequence automaton — Tier-3b"
                                .into());
                        }
                        let seq = el
                            .get("sequence")
                            .ok_or("sequence element without a `sequence`")?;
                        unbounded_tail = Some(unbounded_delay(el, seq)?);
                        continue;
                    }
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
                // then prefix the first element's own gap. An unbounded tail is already
                // `[]^m AF c` and is conjoined to the element before it.
                let n = leaves.len();
                let mut s = match unbounded_tail {
                    Some(tail) => format!("({} && {tail})", leaves[n - 1]),
                    None => leaves[n - 1].clone(),
                };
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

/// `e throughout S` as a consequent: `S` lowered as [`consequent_expr`] would, with `e` conjoined
/// at EVERY cycle the match spans (IEEE 1800-2017 §16.9.9) — each leaf becomes `(e && b)`, a
/// repetition `(e && b)[*n]`, and each delay window ([`delayed_with`]) requires `e` on the cycles
/// it waits through. Unbounded sequences and nested sub-sequences are refused.
fn throughout_consequent(spec: &Value, e: &str) -> Result<String, String> {
    if uses_eventuality(spec) {
        return Err(
            "`throughout` over an unbounded sequence (`##[m:$]`, `[*n:$]`) needs a \
                    sequence automaton — Tier-3b"
                .into(),
        );
    }
    let leaf = |seq: &Value| -> Result<String, String> {
        if seq.get("kind").and_then(Value::as_str) != Some("Simple") {
            return Err(
                "a nested sub-sequence under `throughout` needs length composition — \
                        Tier-3b"
                    .into(),
            );
        }
        let b = format!("({e} && {})", bool_expr(child(seq, "expr")?)?);
        match seq.get("repetition") {
            None => Ok(b),
            Some(rep) => lower_repetition(&b, rep),
        }
    };
    match spec.get("kind").and_then(Value::as_str) {
        Some("Simple") => leaf(spec),
        Some("SequenceConcat") => {
            let elements = spec
                .get("elements")
                .and_then(Value::as_array)
                .ok_or("SequenceConcat without an `elements` array")?;
            if elements.is_empty() {
                return Err("empty SequenceConcat".into());
            }
            let mut leaves = Vec::with_capacity(elements.len());
            let mut delays = Vec::with_capacity(elements.len());
            for el in elements {
                let seq = el
                    .get("sequence")
                    .ok_or("sequence element without a `sequence`")?;
                if elements.len() > 1 && seq.get("repetition").is_some() {
                    return Err("a repetition inside a `##` chain under `throughout` needs \
                                length composition — Tier-3b"
                        .into());
                }
                leaves.push(leaf(seq)?);
                delays.push(elem_delay(el)?);
            }
            let n = leaves.len();
            let mut s = leaves[n - 1].clone();
            for i in (1..n).rev() {
                let (min, max) = delays[i];
                s = format!("({} && {})", leaves[i - 1], delayed_with(&s, min, max, e));
            }
            let (min0, max0) = delays[0];
            Ok(delayed_with(&s, min0, max0, e))
        }
        other => Err(format!(
            "unsupported operand of `throughout` (expected a bounded sequence of booleans, got \
             {other:?})"
        )),
    }
}

/// [`delayed`] with an invariant: `f` at some cycle in `[min, max]` from now, and `e` at every
/// cycle waited through before it. `f` carries `e` at its own first cycle (its leaves are
/// `(e && b)`). Offset k: `W(max) = f`; `W(k) = f || (e && [] W(k+1))` for `min ≤ k < max`;
/// `W(k) = e && [] W(k+1)` for `k < min`.
fn delayed_with(f: &str, min: u32, max: u32, e: &str) -> String {
    let mut w = f.to_string();
    for _ in min..max {
        w = format!("({f} || ({e} && [] {w}))");
    }
    for _ in 0..min {
        w = format!("({e} && [] {w})");
    }
    w
}

/// `μ Y. (f ∨ [] Y)` — `AF f`, "f on every path, eventually". The box recursion is the
/// universal reading the l2s rescue reduces (`liveness_rescue::reduce_response_af`); the
/// diamond would be `EF`, recoverability, a different property.
fn eventually(f: &str) -> String {
    format!("(mu Y. ({f} || [] Y))")
}

/// `##[m:$] b` over a boolean leaf → `[]^m AF b`.
fn unbounded_delay(el: &Value, seq: &Value) -> Result<String, String> {
    if seq.get("kind").and_then(Value::as_str) != Some("Simple") || seq.get("repetition").is_some()
    {
        return Err(
            "`##[m:$]` is supported only before a boolean (`##[1:$] b`); a sequence \
                    after an unbounded gap needs a sequence automaton — Tier-3b"
                .into(),
        );
    }
    let min = el.get("min").and_then(Value::as_u64).unwrap_or(0);
    if min > MAX_SEQ_UNROLL {
        return Err(format!(
            "sequence delay {min} exceeds the unroll cap {MAX_SEQ_UNROLL}"
        ));
    }
    Ok(nest_boxes(
        min as u32,
        &eventually(&bool_expr(child(seq, "expr")?)?),
    ))
}

/// The unary temporal property operators slang serialises as `Unary{op}` (shapes captured from
/// slang 11.0.448, `testdata/sva_fragment_forms.ast.json`):
///
/// - `s_eventually b` → `AF b`; `s_eventually [m:$] b` → `[]^m AF b`; `s_eventually [m:n] b` and
///   the weak `eventually [m:n] b` → `b` within `[m, n]` (the two coincide on infinite traces).
///   The operand must be a boolean.
/// - `nexttime p` → `[] p`; `always p` → `ν W. (p ∧ [] W)`, `p` any consequent. Their ranged
///   forms (`nexttime [k]`, `always [m:n]`) are not lowered — their shapes were not captured.
fn unary_temporal(spec: &Value) -> Result<String, String> {
    let op = spec.get("op").and_then(Value::as_str).unwrap_or("?");
    let inner = child(spec, "expr")?;
    let ranged = spec.get("min").is_some() || spec.get("max").is_some();
    let boolean_operand = || -> Result<String, String> {
        if inner.get("kind").and_then(Value::as_str) != Some("Simple")
            || inner.get("repetition").is_some()
        {
            return Err(format!(
                "`{op}` over a sequence or a property is not supported; its operand must be a \
                 boolean"
            ));
        }
        bool_expr(child(inner, "expr")?)
    };
    match op {
        "SEventually" | "Eventually" if ranged => {
            let min = spec.get("min").and_then(Value::as_u64).unwrap_or(0);
            let b = boolean_operand()?;
            match spec.get("max") {
                Some(Value::String(d)) if d == "$" => {
                    if min > MAX_SEQ_UNROLL {
                        return Err(format!("eventuality delay {min} exceeds the unroll cap"));
                    }
                    Ok(nest_boxes(min as u32, &eventually(&b)))
                }
                Some(m) => {
                    let max = m
                        .as_u64()
                        .ok_or_else(|| format!("`{op}` range bound is not a constant"))?;
                    if max < min || max > MAX_SEQ_UNROLL {
                        return Err(format!("`{op} [{min}:{max}]` range is out of bounds"));
                    }
                    Ok(delayed(&b, min as u32, max as u32))
                }
                None => Err(format!("`{op}` with a lower bound and no upper bound")),
            }
        }
        "SEventually" => Ok(eventually(&boolean_operand()?)),
        "NextTime" | "SNextTime" if !ranged => Ok(format!("[] {}", consequent_expr(inner)?)),
        "Always" if !ranged => Ok(format!("(nu W. ({} && [] W))", consequent_expr(inner)?)),
        "Eventually" => Err("unranged weak `eventually` is not legal SVA".to_string()),
        other => Err(format!(
            "unsupported temporal property operator `{other}`{}",
            if ranged { " with a range" } else { "" }
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

/// XL.5 / ranged antecedents (2026-10-15) — an implication whose *antecedent* is a bounded
/// sequence: `<antecedent> |-> <conseq>`, where `conseq` is the already-lowered consequent
/// (aligned to the antecedent's match end; the caller adds the `|=>` extra `[]`).
///
/// IEEE 1800-2017 §16.12.7: the consequent must hold at EVERY match end of the antecedent. Each
/// link of the antecedent is a boolean `b` preceded by a gap `##[lo:hi]` and repeated `[*rlo:rhi]`
/// consecutive times (`##k` / `[*n]` are the degenerate ranges); every alternative is a separate
/// match, so each range lowers to a CONJUNCTION over its alternatives, in negation-normal form —
/// never a `!` over a modality (the cube evaluator would turn `¬[]` into a must-diamond):
///
/// - gap `[lo:hi]` before `f`: `[]^lo (f && [] (f && … [] f))` — `f` at each of the hi−lo+1
///   cycles ([`boxes_all`], the dual of [`delayed`]);
/// - `b[*rlo:rhi]` then `next`: `!b || T(1)` with
///   `T(j) = (next, if j ≥ rlo) && ([] (!b || T(j+1)), if j < rhi)` — after the j-th consecutive
///   `b`, the match ends here if `j` is in range, and continues if one more `b` comes.
///
/// For fixed gaps and repetitions this is exactly the XL.5 nested implication
/// `!e₀ || []^δ₁(!e₁ || []^δ₂(… || conseq))`. SOUNDNESS: the antecedent atoms sit in negative
/// position under boxes evaluated ∀-may; the whole is an ordinary mu-calculus formula the KMTS
/// evaluator decides soundly. Size is the PRODUCT of the alternatives across links, guarded at
/// [`MAX_ANTECEDENT_ALTERNATIVES`], and the total span at [`MAX_SEQ_UNROLL`].
fn antecedent_implies(left: &Value, conseq: &str) -> Result<String, String> {
    let links = antecedent_links(left)?;
    let mut acc = conseq.to_string();
    for l in links.iter().rev() {
        let elem = repeated_implies(&l.b, l.rep.0, l.rep.1, &acc);
        acc = boxes_all(&elem, l.gap.0, l.gap.1);
    }
    Ok(acc)
}

/// The most match-end alternatives a ranged antecedent may expand to (the product over its links
/// of gap width × repetition width). Each alternative copies the consequent once.
const MAX_ANTECEDENT_ALTERNATIVES: u64 = 64;

/// One link of a bounded antecedent: boolean `b`, the gap `##[lo:hi]` before it, and its
/// consecutive repetition `[*rlo:rhi]` (`(1, 1)` when unrepeated).
struct AntecedentLink {
    b: String,
    gap: (u32, u32),
    rep: (u32, u32),
}

/// `f` at every cycle in `[lo, hi]`: `[]^lo (f && [] (f && … [] f))`.
fn boxes_all(f: &str, lo: u32, hi: u32) -> String {
    let mut out = f.to_string();
    for _ in 0..(hi - lo) {
        out = format!("({f} && [] {out})");
    }
    nest_boxes(lo, &out)
}

/// `b[*rlo:rhi]` starting now implies `next` at each of its match ends: `!b || T(1)`.
fn repeated_implies(b: &str, rlo: u32, rhi: u32, next: &str) -> String {
    let mut t = next.to_string(); // T(rhi): the run cannot continue, so it must end here.
    for j in (1..rhi).rev() {
        let cont = format!("[] (!({b}) || {t})");
        t = if j >= rlo {
            format!("({next} && {cont})")
        } else {
            cont
        };
    }
    format!("(!({b}) || {t})")
}

/// A consecutive repetition range `[*m:n]`, `1 ≤ m ≤ n`, bounded. Goto / non-consecutive /
/// unbounded / empty (`[*0…]`) repetition is rejected.
fn consecutive_range(rep: &Value) -> Result<(u32, u32), String> {
    let kind = rep.get("kind").and_then(Value::as_str).unwrap_or("?");
    if kind != "Consecutive" {
        return Err(format!(
            "`{kind}` repetition (goto `[->n]` / non-consecutive `[=n]`) needs a \
             sequence automaton — Tier-3b"
        ));
    }
    let min = rep.get("min").and_then(Value::as_u64);
    let Some(max) = rep.get("max").and_then(Value::as_u64) else {
        return Err(
            "unbounded repetition `[*n:$]` in an ANTECEDENT has unboundedly many match \
                    ends; it needs a sequence automaton — Tier-3b"
                .into(),
        );
    };
    let min = min.unwrap_or(0);
    if min == 0 {
        return Err("a repetition that can match empty (`[*0…]`) is out of fragment".into());
    }
    if max < min || max > MAX_SEQ_UNROLL {
        return Err(format!(
            "repetition range `[*{min}:{max}]` is out of bounds"
        ));
    }
    Ok((min as u32, max as u32))
}

/// The links of a bounded antecedent: a boolean, a repeated boolean `b[*m:n]`, or a `##` chain
/// whose elements are booleans or repeated booleans with fixed or ranged gaps. Rejects
/// unbounded gaps / repetition, goto / non-consecutive repetition, nested sub-sequences, and an
/// expansion past the size guards.
fn antecedent_links(left: &Value) -> Result<Vec<AntecedentLink>, String> {
    let leaf = |seq: &Value, gap: (u32, u32)| -> Result<AntecedentLink, String> {
        if seq.get("kind").and_then(Value::as_str) != Some("Simple") {
            return Err(format!(
                "a nested sub-sequence in an antecedent (got {:?}) needs length composition — \
                 Tier-3b",
                seq.get("kind").and_then(Value::as_str)
            ));
        }
        let rep = match seq.get("repetition") {
            None => (1, 1),
            Some(r) => consecutive_range(r)?,
        };
        Ok(AntecedentLink {
            b: bool_expr(child(seq, "expr")?)?,
            gap,
            rep,
        })
    };
    let links = match left.get("kind").and_then(Value::as_str) {
        Some("Simple") => vec![leaf(left, (0, 0))?],
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
                if el.get("max").and_then(Value::as_str) == Some("$") {
                    return Err(
                        "an unbounded `##[m:$]` gap in an ANTECEDENT has unboundedly \
                                many match ends; it needs a sequence automaton — Tier-3b"
                            .into(),
                    );
                }
                v.push(leaf(seq, elem_delay(el)?)?);
            }
            if v.is_empty() {
                return Err("empty SequenceConcat antecedent".into());
            }
            v
        }
        other => {
            return Err(format!(
                "unsupported implication antecedent (expected a boolean, a repeated boolean or \
                 a bounded `##` chain of them, got {other:?})"
            ));
        }
    };
    let alternatives: u64 = links
        .iter()
        .map(|l| u64::from(l.gap.1 - l.gap.0 + 1) * u64::from(l.rep.1 - l.rep.0 + 1))
        .product();
    if alternatives > MAX_ANTECEDENT_ALTERNATIVES {
        return Err(format!(
            "the antecedent's ranges expand to {alternatives} match-end alternatives (> \
             {MAX_ANTECEDENT_ALTERNATIVES}); each copies the consequent — narrow the ranges"
        ));
    }
    let span: u64 = links
        .iter()
        .map(|l| u64::from(l.gap.1) + u64::from(l.rep.1))
        .sum();
    if span > MAX_SEQ_UNROLL {
        return Err(format!(
            "the antecedent spans up to {span} cycles, past the unroll cap {MAX_SEQ_UNROLL}"
        ));
    }
    Ok(links)
}

/// Translate a boolean `Expression` into mu-calculus atom/connective text.
/// A boolean-position expression: the atom lowering first ([`bool_expr_atoms`]); where it
/// refuses, the expression compiler (`expr_ir::build_expr`) mints the expression as one named
/// combinational node and the formula reads that node — bare when 1 bit, `!= 0` otherwise, and
/// `(node OP k)` for an unsigned comparison against a constant, so the predicate cube keeps a
/// dimension. A form both refuse keeps the atom lowering's reason, with the compiler's appended.
fn bool_expr(expr: &Value) -> Result<String, String> {
    match bool_expr_atoms(expr) {
        Ok(f) => Ok(f),
        Err(reason) => {
            mint_boolean(expr).map_err(|why| format!("{reason} (expression compiler: {why})"))
        }
    }
}

/// Translation-time state of the expression compiler for the assertion being translated.
#[derive(Default)]
struct MintState {
    /// User subroutines of the design by slang address (for inlining).
    subroutines: std::collections::HashMap<String, Value>,
    /// Expressions the current assertion's formula reads.
    exprs: Vec<MintedExpression>,
    /// `(base, width, depth)` shadows those expressions read.
    shadows: Vec<(String, u32, u32)>,
}

thread_local! {
    static MINT: std::cell::RefCell<MintState> = std::cell::RefCell::default();
}

/// [`bool_expr`]'s compiler fallback. An unsigned comparison with one constant side mints only
/// the other side and keeps the literal, so the predicate cube gets a dimension.
fn mint_boolean(expr: &Value) -> Result<String, String> {
    let subs = MINT.with(|m| m.borrow().subroutines.clone());
    let mut probe = super::expr_ir::BuildCtx::with_subroutines(subs);
    let ir = super::expr_ir::build_expr(expr, &mut probe)?;
    if let ExprIr::Cmp { op, signed, a, b } = &ir
        && (!*signed || matches!(op, CmpOp::Eq | CmpOp::Ne))
    {
        let lit = |e: &ExprIr| match e {
            ExprIr::Const { bits, .. } if *bits <= i64::MAX as u128 => Some(*bits),
            _ => None,
        };
        let sym = |op: CmpOp| match op {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        };
        let side = match (lit(a), lit(b)) {
            (None, Some(k)) => Some(((**a).clone(), sym(*op), k)),
            (Some(k), None) => Some(((**b).clone(), flip_op(sym(*op)), k)),
            _ => None,
        };
        if let Some((value, op, k)) = side {
            let name = register_ir(value, probe.shadows);
            return Ok(format!("({name} {op} {k})"));
        }
    }
    let w = ir.width();
    let name = register_ir(ir, probe.shadows);
    Ok(if w == 1 {
        name
    } else {
        format!("({name} != 0)")
    })
}

/// Record an already-built expression (and the shadows its build needed); its atom name.
fn register_ir(ir: ExprIr, shadows: Vec<(String, u32, u32)>) -> String {
    let name = match &ir {
        ExprIr::Leaf { name, .. } => name.clone(),
        _ => ir.minted_name(),
    };
    MINT.with(|m| {
        let mut m = m.borrow_mut();
        if !matches!(ir, ExprIr::Leaf { .. }) && !m.exprs.iter().any(|e| e.name == name) {
            m.exprs.push(MintedExpression {
                name: name.clone(),
                expr: ir,
            });
        }
        for (b, w, d) in shadows {
            match m.shadows.iter_mut().find(|s| s.0 == b) {
                Some(s) => s.2 = s.2.max(d),
                None => m.shadows.push((b, w, d)),
            }
        }
    });
    name
}

/// The atom lowering of a boolean-position expression (the translator's own fragment).
fn bool_expr_atoms(expr: &Value) -> Result<String, String> {
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
                // `a -> b` and `a <-> b` (SV 11.4.7): logical operators over the operands' truth
                // values, exactly as `||` / `&&` read them.
                "LogicalImplication" => {
                    let l = bool_expr(child(expr, "left")?)?;
                    let r = bool_expr(child(expr, "right")?)?;
                    Ok(format!("((!({l})) || {r})"))
                }
                "LogicalEquivalence" => {
                    let l = bool_expr(child(expr, "left")?)?;
                    let r = bool_expr(child(expr, "right")?)?;
                    Ok(xnor(&l, &r))
                }
                // `&`, `|`, `^`, `~^` are bitwise, i.e. value transforms; on 1-bit operands with a
                // 1-bit result they coincide with the boolean connectives. Anything wider is a
                // vector the expression compiler must mint, so it is refused here.
                "BinaryAnd" | "BinaryOr" | "BinaryXor" | "BinaryXnor" => {
                    let (l, r) = (child(expr, "left")?, child(expr, "right")?);
                    let one = |n: &Value| {
                        n.get("type").and_then(Value::as_str).and_then(type_width) == Some(1)
                    };
                    if !(one(expr) && one(unwrap(l)) && one(unwrap(r))) {
                        return Err(format!(
                            "bitwise `{op}` over multi-bit operands is a value transform, not a \
                             boolean; only its 1-bit form is supported"
                        ));
                    }
                    let (a, b) = (bool_expr(l)?, bool_expr(r)?);
                    Ok(match op {
                        "BinaryAnd" => format!("({a} && {b})"),
                        "BinaryOr" => format!("({a} || {b})"),
                        "BinaryXor" => xor(&a, &b),
                        _ => xnor(&a, &b),
                    })
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
                // A constant bit-select is the 1-bit slice `[k:k]` (SVA-fragment widening,
                // 2026-10-15): the same minted signal mununu#565 built for `[hi:lo]`, with the
                // SV index mapped to the bit position yosys gives it (`index_positions`). An
                // element of a multi-dimensional packed array (`cnt_q[1]`) is the W-bit slice of
                // that element. In boolean position a bit is the bare atom, a vector `!= 0`.
                packed_bool(expr)
            } else {
                Err("dynamic bit-select `sig[idx]` (index is a signal) is not \
                     expressible as a propositional atom"
                    .to_string())
            }
        }
        // A packed-struct member (`esc_tx_i.esc_p`) or a part-select in boolean position.
        "MemberAccess" | "RangeSelect" => packed_bool(expr),
        // XL.3 (Tier-2 history). `$stable`/`$changed`/`$rose`/`$fell`/`$past`
        // lower to atoms over a 1-step shadow register `<sig>__past` (the XL.3b
        // BTOR2 model-augmentation step synthesises that flop). All exact.
        "Call" => {
            let sub = expr
                .get("subroutine")
                .and_then(Value::as_str)
                .unwrap_or("?");
            match sub {
                // Over a slice or a bit (`$stable(sig[7:4])`, `$rose(sig[3])`): the slice now
                // against the same slice of the depth-1 shadow (`collect_shadow_signals` requests
                // the base shadow, `collect_slice_signals` both slices).
                "$stable" | "$changed" | "$rose" | "$fell"
                    if past_call_arg_node(expr).and_then(slice_ref).is_some() =>
                {
                    let arg = past_call_arg_node(expr).expect("guarded");
                    if expr
                        .get("arguments")
                        .and_then(Value::as_array)
                        .is_some_and(|a| a.len() != 1)
                    {
                        return Err(format!(
                            "{sub} with a depth argument; only `$past` carries a depth"
                        ));
                    }
                    let (base, msb, lsb) = slice_ref(arg).expect("guarded")?;
                    let now = slice_atom_name(&base, msb, lsb);
                    let then = slice_atom_name(&past_shadow_name(&base, 1), msb, lsb);
                    match sub {
                        "$stable" => Ok(format!("({now} == {then})")),
                        "$changed" => Ok(format!("({now} != {then})")),
                        _ if msb != lsb => Err(format!(
                            "{sub}(<{}-bit slice>) — Tier-2 supports {sub} on a single bit only",
                            msb - lsb + 1
                        )),
                        "$rose" => Ok(format!("({now} && (!({then})))")),
                        _ => Ok(format!("((!({now})) && {then})")),
                    }
                }
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
                "$past" if past_call_arg_node(expr).and_then(slice_ref).is_some() => {
                    let atom = cmp_signal_atom(expr).ok_or_else(|| {
                        let arg = past_call_arg_node(expr).expect("guarded");
                        match slice_ref(arg) {
                            Some(Err(e)) => e,
                            _ => "$past over a selection with a non-constant depth".to_string(),
                        }
                    })?;
                    let (_, msb, lsb) =
                        slice_ref(past_call_arg_node(expr).expect("guarded")).expect("guarded")?;
                    Ok(if msb > lsb {
                        format!("({atom} != 0)")
                    } else {
                        atom
                    })
                }
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
                // SOUNDNESS: exact for the model, which is 2-state — the lift has no X or Z, so no
                // value is unknown. The fold is recorded (`collect_two_state_folds`) and surfaced
                // per property as a `two-state-fold` scope note: `!$isunknown(x)` then HOLDS
                // trivially and says nothing about X-propagation in the RTL, which needs a 4-state
                // simulator or an X-aware formal tool.
                "$isunknown" => Ok("false".to_string()),
                // `$countones(x)` in boolean position: "some bit is set".
                "$countones" => match countones_base(expr) {
                    Some(Ok(base)) => Ok(format!("({} != 0)", popcount_atom_name(&base))),
                    Some(Err(e)) => Err(e),
                    None => unreachable!("guarded by the subroutine match"),
                },
                other => Err(format!(
                    "system/subroutine call `{other}` (e.g. `$countones`) \
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
            // SOUNDNESS — `inside` ranges lower to `>=` / `<=` atoms, which are unsigned; a signed
            // left operand would invert them (see `signed_comparison_guard`).
            if typed_signed(child(expr, "left")?) || typed_signed(unwrap(child(expr, "left")?)) {
                return Err(
                    "`inside` over a signed operand: its ranges would be compared \
                            unsigned; not supported yet"
                        .to_string(),
                );
            }
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
    signed_comparison_guard(child(expr, "left")?, child(expr, "right")?, op)?;
    // SOUNDNESS: the atom forms below compare the UNCONVERTED operands (`unwrap` strips every
    // `Conversion`). That is exact only when each conversion preserves the value: a zero-extension
    // or a same-width re-typing. A sign extension (`8'(s)` of a signed `s`, a signed `s` against a
    // wider signed operand) or a truncation changes it, so the comparison goes to the expression
    // compiler, which applies the conversions (`bool_expr`'s fallback). Before 2026-10-15 the
    // sign extension was dropped: `r == 8'(s)` compared the raw 4-bit `s`.
    for side in ["left", "right"] {
        if let Some(why) = value_changing_conversion(child(expr, side)?) {
            return Err(format!(
                "the comparison's {side} operand is {why}; the atom lowering compares unconverted \
                 values"
            ));
        }
    }
    let left = unwrap(child(expr, "left")?);
    let right = unwrap(child(expr, "right")?);
    // A literal operand is read BEFORE its conversions are stripped: slang folds the converted
    // value onto the outer node (`-8'sd11` cast into an unsigned comparison is `8'd245`).
    let (lit_l, lit_r) = (
        sv_integer(child(expr, "left")?),
        sv_integer(child(expr, "right")?),
    );

    // mununu#565 — a slice this translator refuses reports WHY, before the generic shape message
    // below can blame the operand kind. An indexed part-select mis-sliced silently would be a
    // soundness bug, not a narrower reach, so it declines loudly.
    for side in [left, right] {
        if let Some(Err(reason)) = slice_ref(side) {
            return Err(reason);
        }
        if let Some(Err(reason)) = countones_base(side) {
            return Err(reason);
        }
    }

    // (1) signal OP literal — `signal` may be a bit-slice (mununu#565), which resolves to its
    // minted derived signal so the atom stays a plain identifier.
    if let (Some(sig), Some(lit)) = (cmp_signal_atom(left), lit_r) {
        return Ok(format!("({sig} {op} {lit})"));
    }
    // (2) literal OP signal  → flip to signal OP' literal
    if let (Some(lit), Some(sig)) = (lit_l, cmp_signal_atom(right)) {
        return Ok(format!("({sig} {} {lit})", flip_op(op)));
    }
    // (3) boolean-expr ==/!= 0
    if matches!(op, "==" | "!=") {
        let other = if lit_r == Some(0) {
            Some(left)
        } else if lit_l == Some(0) {
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
            xor(&a, &b)
        } else {
            xnor(&a, &b)
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

/// `Some(reason)` when a chain of slang `Conversion`s around `node` changes its value — a sign
/// extension (a widening into a signed type: SV extends by the expression's signedness) or a
/// truncation. A folded constant (`constant` present) is exempt: its value is read after the
/// conversions.
fn value_changing_conversion(node: &Value) -> Option<&'static str> {
    let mut n = node;
    while n.get("kind").and_then(Value::as_str) == Some("Conversion") {
        if n.get("constant").is_some() {
            return None;
        }
        let op = n.get("operand")?;
        let width = |x: &Value| x.get("type").and_then(Value::as_str).and_then(type_width);
        if let (Some(to), Some(from)) = (width(n), width(op)) {
            if to < from {
                return Some("truncated by a conversion");
            }
            if to > from && typed_signed(n) {
                return Some("sign-extended by a conversion");
            }
        }
        n = op;
    }
    None
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
pub(super) fn past_shadow_name(base: &str, depth: u32) -> String {
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

/// mununu#565 — the minted atom name for `base[msb:lsb]`.
///
/// One naming contract with the BTOR2 side's `slice_stage_name`, exactly as
/// [`past_shadow_name`] pairs with `shadow_stage_name`. Both halves must agree or the atom never
/// binds.
fn slice_atom_name(base: &str, msb: u32, lsb: u32) -> String {
    format!("{base}__bits{msb}_{lsb}")
}

/// mununu#565 — recognise a bit-slice operand.
///
/// `None` when `expr` is not a `RangeSelect` at all. `Some(Err(..))` for a slice this translator
/// deliberately refuses, so the caller reports a REASON rather than a generic shape complaint.
///
/// **Only `selectionKind == "Simple"` is accepted, and that limit is measured, not cautious.**
/// Against slang 11.0.448, `wdata[7 -: 8]` serialises as `selectionKind: "IndexedDown"` with
/// `left: 7, right: 8` — that is the base index and the **WIDTH**, not a bit range. Reading
/// `left`/`right` as bounds there yields bits `[7:8]`: inverted and wrong, with no error. Refusing
/// is the honest option until the indexed arithmetic is written and tested.
fn slice_ref(expr: &Value) -> Option<Result<(String, u32, u32), String>> {
    let expr = unwrap(expr);
    let kind = expr.get("kind").and_then(Value::as_str)?;
    if !matches!(kind, "ElementSelect" | "RangeSelect" | "MemberAccess") {
        return None;
    }
    match packed_sub(expr)? {
        Ok(sub) => Some(Ok((sub.base, sub.lsb + sub.width - 1, sub.lsb))),
        Err(reason) => Some(Err(reason)),
    }
}

/// A packed sub-range of one signal, in BTOR2 bit positions: `base[lsb + width - 1 : lsb]`,
/// with slang's type string for the sub-range (to select further into it).
pub(super) struct PackedSub {
    pub(super) base: String,
    pub(super) lsb: u32,
    pub(super) width: u32,
    ty: String,
}

/// Resolve a constant selection chain over ONE packed signal to its bit range: the signal itself,
/// a packed-struct member (`s.f`, `s.f.g`), a constant element or bit (`v[k]`, `cnt_q[1]` of a
/// `logic [1:0][3:0]`) and a simple part-select (`v[hi:lo]`), in any nesting.
///
/// `None` when `expr` is not such a chain at all (an unpacked array, a dynamic index, an
/// expression base) so the caller gives its own reason; `Some(Err)` for a chain this function
/// recognises but refuses — an indexed part-select, an unreadable type, an out-of-range index.
///
/// The layout facts it relies on (SV 7.4, 7.2.1): a packed array's RIGHT-most dimension varies
/// fastest and bit 0 is the right-most declared index of the last dimension; a packed struct's
/// FIRST member is the most significant. slang serialises a struct type as
/// `struct packed{logic[3:0] hi;logic[1:0] mid;logic lo;}p::s$2` once the alias pre-pass has
/// resolved the typedef (captured from slang 11.0.448, 2026-10-15).
pub(super) fn packed_sub(expr: &Value) -> Option<Result<PackedSub, String>> {
    let expr = unwrap(expr);
    let ty_of = |n: &Value| {
        n.get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match expr.get("kind").and_then(Value::as_str)? {
        "NamedValue" => {
            if is_unpacked_array(expr) {
                return None;
            }
            let name = signal_name(expr).ok()?.to_string();
            let ty = ty_of(expr);
            Some(match type_width(&ty) {
                Some(width) => Ok(PackedSub {
                    base: name,
                    lsb: 0,
                    width,
                    ty,
                }),
                None => Err(format!(
                    "cannot map a selection of `{name}` to bit positions: its type `{ty}` is not a \
                     plain packed vector (`logic [hi:lo]`), a packed array or a packed struct"
                )),
            })
        }
        "MemberAccess" => {
            let parent = match packed_sub(expr.get("value")?) {
                Some(Ok(p)) => p,
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    return Some(Err(
                        "member access over an expression that is not a packed-struct signal"
                            .to_string(),
                    ));
                }
            };
            let member = expr
                .get("member")
                .and_then(Value::as_str)
                .map(|m| m.rsplit(' ').next().unwrap_or(m))
                .unwrap_or("?");
            let Some(fields) = struct_fields(&parent.ty) else {
                return Some(Err(format!(
                    "member `.{member}` of `{}`: its type `{}` is not a packed struct",
                    parent.base, parent.ty
                )));
            };
            let mut lsb = 0u32;
            for (name, fty) in fields.iter().rev() {
                let Some(w) = type_width(fty) else {
                    return Some(Err(format!(
                        "packed struct of `{}`: the width of member `{name}` (`{fty}`) is not \
                         readable, so `.{member}`'s bit offset is unknown",
                        parent.base
                    )));
                };
                if name == member {
                    let ty = ty_of(expr);
                    if type_width(&ty).is_some_and(|tw| tw != w) {
                        return Some(Err(format!(
                            "member `.{member}`: slang types it `{ty}` but the struct declares \
                             `{fty}`; refusing to guess its bits"
                        )));
                    }
                    return Some(Ok(PackedSub {
                        base: parent.base,
                        lsb: parent.lsb + lsb,
                        width: w,
                        ty: fty.clone(),
                    }));
                }
                lsb += w;
            }
            Some(Err(format!(
                "member `.{member}` not found in `{}`'s struct type `{}`",
                parent.base, parent.ty
            )))
        }
        "ElementSelect" | "RangeSelect" => {
            let is_range = expr.get("kind").and_then(Value::as_str) == Some("RangeSelect");
            let base_node = unwrap(expr.get("value")?);
            if is_unpacked_array(base_node) {
                return None;
            }
            if is_range {
                let kind = expr
                    .get("selectionKind")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                if kind != "Simple" {
                    return Some(Err(format!(
                        "bit-slice with selectionKind `{kind}` (an indexed part-select such as `x[i +: w]` / \
                         `x[i -: w]`) is not supported; slang reports its bounds as base-and-WIDTH rather than \
                         high-and-low, so treating them as a range would silently slice the wrong bits. Use an \
                         explicit `x[hi:lo]` range."
                    )));
                }
            }
            let (hi_idx, lo_idx) = if is_range {
                match (
                    expr.get("left").and_then(sv_integer),
                    expr.get("right").and_then(sv_integer),
                ) {
                    (Some(l), Some(r)) => (l, r),
                    _ => {
                        return Some(Err(
                            "bit-slice bounds must be constant integer literals".to_string()
                        ));
                    }
                }
            } else {
                // A signal index is not a constant selection — `None`, so `bool_expr` reports it.
                let k = expr.get("selector").map(unwrap).and_then(sv_integer)?;
                (k, k)
            };
            let parent = match packed_sub(base_node) {
                Some(Ok(p)) => p,
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    return Some(Err(
                        "bit-select base is not a plain signal reference; only `signal[k]`, \
                         `signal[hi:lo]` and selections of packed-struct members are supported"
                            .to_string(),
                    ));
                }
            };
            let name = parent.base.clone();
            let (msb_pos, lsb_pos, elem_w, elem_ty) =
                match index_positions(&parent.ty, &name, hi_idx, lo_idx) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            if msb_pos < lsb_pos {
                return Some(Err(format!(
                    "bit-slice `{name}[{hi_idx}:{lo_idx}]` runs against the declared range of `{name}`"
                )));
            }
            let width = (msb_pos - lsb_pos + 1) * elem_w;
            let ty = if is_range || elem_w == 1 {
                format!("logic[{}:0]", width - 1)
            } else {
                elem_ty
            };
            Some(Ok(PackedSub {
                base: parent.base,
                lsb: parent.lsb + lsb_pos * elem_w,
                width,
                ty,
            }))
        }
        _ => None,
    }
}

/// For a selection `[hi_idx:lo_idx]` (equal for an element) of a value of slang type `ty`: the
/// element positions of the two bounds counted from the least significant element, the element
/// width, and the element's type. A packed struct is indexed as `[W-1:0]` bits.
///
/// Position 0 is the RIGHT-most declared index, whatever the range: `logic [15:8] x` → `x[9]` is
/// bit 1; `logic [0:7] y` → `y[1]` is bit 6. Before 2026-10-15 the slice path passed SV indices
/// through as bit positions — the identity for `[N-1:0]`, wrong for any other range.
fn index_positions(
    ty: &str,
    name: &str,
    hi_idx: i64,
    lo_idx: i64,
) -> Result<(u32, u32, u32, String), String> {
    let ty = ty.trim();
    let (dims, head) = if ty.starts_with("struct packed") || ty.starts_with("union packed") {
        let w =
            type_width(ty).ok_or_else(|| format!("cannot read the width of `{name}` (`{ty}`)"))?;
        (vec![(i64::from(w) - 1, 0)], "logic".to_string())
    } else {
        let (head, dims) = packed_dims(ty).ok_or_else(|| {
            format!(
                "cannot map an index of `{name}` to a bit position: its type `{ty}` is not a plain \
                 packed vector (`logic [hi:lo]`), a packed array or a packed struct"
            )
        })?;
        (dims, head)
    };
    let (first, rest) = dims
        .split_first()
        .ok_or_else(|| format!("`{name}` (`{ty}`) is a scalar; it has no bits to select"))?;
    let elem_w: u32 = rest
        .iter()
        .map(|(a, b)| (a - b).unsigned_abs() as u32 + 1)
        .product();
    let (left, right) = *first;
    let (hi, lo) = (left.max(right), left.min(right));
    let pos = |idx: i64| -> Result<u32, String> {
        if idx < lo || idx > hi {
            return Err(format!(
                "index {idx} is outside `{name}`'s declared range [{left}:{right}]"
            ));
        }
        Ok((if left >= right {
            idx - right
        } else {
            right - idx
        }) as u32)
    };
    let elem_ty = if rest.is_empty() {
        head.clone()
    } else {
        let mut t = head.clone();
        for (a, b) in rest {
            t.push_str(&format!("[{a}:{b}]"));
        }
        t
    };
    Ok((pos(hi_idx)?, pos(lo_idx)?, elem_w, elem_ty))
}

/// The scalar kinds a packed vector / array is built from.
fn scalar_head(head: &str) -> bool {
    matches!(
        head,
        "logic" | "bit" | "reg" | "wire" | "logic signed" | "bit signed" | "reg signed"
    )
}

/// `logic[1:0][3:0]` → `("logic", [(1,0), (3,0)])`. `None` unless the type is a scalar kind with
/// only constant packed dimensions (an unpacked `$[…]` suffix is not part of the packed type).
pub(super) fn packed_dims(ty: &str) -> Option<(String, Vec<(i64, i64)>)> {
    let ty = ty.split('$').next().unwrap_or("").trim();
    let head = ty.split('[').next().unwrap_or("").trim().to_string();
    if !scalar_head(&head) {
        return None;
    }
    let mut dims = Vec::new();
    let mut rest = &ty[head.len()..];
    while let Some(open) = rest.find('[') {
        if !rest[..open].trim().is_empty() {
            return None;
        }
        let close = rest[open..].find(']')? + open;
        let (a, b) = rest[open + 1..close].split_once(':')?;
        dims.push((a.trim().parse().ok()?, b.trim().parse().ok()?));
        rest = &rest[close + 1..];
    }
    rest.trim().is_empty().then_some((head, dims))
}

/// Width in bits of a slang type string: a scalar (`logic` = 1), a packed vector or array (the
/// product of every packed dimension — `logic[1:0][3:0]` is 8), the 2-state integer types, or a
/// packed struct / union (sum / widest of its members). `None` for anything unreadable.
pub(super) fn type_width(ty: &str) -> Option<u32> {
    let ty = ty.trim();
    if ty.starts_with("struct packed") || ty.starts_with("union packed") {
        let widths: Option<Vec<u32>> = struct_fields(ty)?
            .iter()
            .map(|(_, t)| type_width(t))
            .collect();
        let widths = widths?;
        return if ty.starts_with("struct") {
            Some(widths.iter().sum())
        } else {
            widths.iter().copied().max()
        };
    }
    let base = ty.split('$').next().unwrap_or("").trim();
    match base.strip_suffix(" signed").unwrap_or(base) {
        "byte" => return Some(8),
        "shortint" => return Some(16),
        "int" | "integer" => return Some(32),
        "longint" => return Some(64),
        _ => {}
    }
    let (_, dims) = packed_dims(ty)?;
    Some(
        dims.iter()
            .map(|(a, b)| (a - b).unsigned_abs() as u32 + 1)
            .product(),
    )
}

/// The members of a packed struct / union type string, in declaration order (first = most
/// significant): `struct packed{logic[3:0] hi;logic lo;}p::s$2` → `[("hi", "logic[3:0]"),
/// ("lo", "logic")]`. Nested struct members keep their braces. `None` when the string is not
/// a packed struct / union or does not parse.
fn struct_fields(ty: &str) -> Option<Vec<(String, String)>> {
    let ty = ty.trim();
    if !(ty.starts_with("struct packed") || ty.starts_with("union packed")) {
        return None;
    }
    let open = ty.find('{')?;
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in ty[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &ty[open + 1..close?];
    let mut fields = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ';' if depth == 0 => {
                let f = body[start..i].trim();
                start = i + 1;
                if f.is_empty() {
                    continue;
                }
                // The name is the last space-separated token at brace depth 0.
                let mut d = 0i32;
                let mut split = None;
                for (k, ch) in f.char_indices() {
                    match ch {
                        '{' => d += 1,
                        '}' => d -= 1,
                        ' ' if d == 0 => split = Some(k),
                        _ => {}
                    }
                }
                let k = split?;
                fields.push((f[k + 1..].trim().to_string(), f[..k].trim().to_string()));
            }
            _ => {}
        }
    }
    (!fields.is_empty()).then_some(fields)
}

/// A comparison operand that resolves to a mu-calc atom name: a plain signal, or
/// `$past(signal[, k])` → its depth-`k` `__past` shadow. `None` for anything else.
fn cmp_signal_atom(expr: &Value) -> Option<String> {
    let expr = unwrap(expr);
    if is_past_call(expr) {
        // mununu#565 — `$past(sig[hi:lo])`. The shadow is over the WHOLE signal (shadow.rs keys on
        // a BTOR2 symbol and copies its sort, so it has no notion of a bit range); the slice is
        // then taken OF the shadow. So the atom is a slice of `<sig>__past`, and `shadow.rs` needs
        // no change at all.
        if let Some(arg) = past_call_arg_node(expr)
            && let Some(Ok((base, msb, lsb))) = slice_ref(arg)
        {
            let depth = past_call_depth(expr).unwrap_or(1);
            let shadow = past_shadow_name(&base, depth);
            return Some(slice_atom_name(&shadow, msb, lsb));
        }
        return past_call_arg(expr)
            .ok()
            .map(|(sig, _, depth)| past_shadow_name(sig, depth));
    }
    // `$countones(x)` (2026-10-15) — its minted count signal.
    if let Some(Ok(base)) = countones_base(expr) {
        return Some(popcount_atom_name(&base));
    }
    // mununu#565 — a bare `sig[hi:lo]` operand becomes its minted derived signal.
    if let Some(Ok((base, msb, lsb))) = slice_ref(expr) {
        return Some(slice_atom_name(&base, msb, lsb));
    }
    signal_name(expr).ok().map(|s| s.to_string())
}

/// mununu#565 — the first argument node of a `$past` call, unwrapped, without requiring it to be
/// a plain signal (which [`past_call_arg`] does). Lets a slice argument be recognised.
fn past_call_arg_node(call: &Value) -> Option<&Value> {
    call.get("arguments")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty() && a.len() <= 2)
        .map(|a| unwrap(&a[0]))
}

/// mununu#565 — the depth of a `$past` call (`1` when omitted).
fn past_call_depth(call: &Value) -> Option<u32> {
    let args = call.get("arguments").and_then(Value::as_array)?;
    if args.len() < 2 {
        return Some(1);
    }
    let d = sv_integer(unwrap(&args[1]))?;
    (d >= 1).then_some(d as u32)
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

/// `a XOR b` in the propositional fragment.
fn xor(a: &str, b: &str) -> String {
    format!("(({a} && (!({b}))) || ((!({a})) && {b}))")
}

/// `a XNOR b` in the propositional fragment.
fn xnor(a: &str, b: &str) -> String {
    format!("(({a} && {b}) || ((!({a})) && (!({b}))))")
}

/// A constant selection (bit, element, member, part-select) in boolean position: a 1-bit
/// selection is the bare minted atom; a wider one is SV's implicit `!= 0`.
fn packed_bool(expr: &Value) -> Result<String, String> {
    match slice_ref(expr) {
        Some(Ok((b, msb, lsb))) => {
            let atom = slice_atom_name(&b, msb, lsb);
            Ok(if msb > lsb {
                format!("({atom} != 0)")
            } else {
                atom
            })
        }
        Some(Err(reason)) => Err(reason),
        None => Err(
            "selection over an expression that is not a packed signal, struct member or \
             constant index"
                .to_string(),
        ),
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
pub(super) fn is_unpacked_array(expr: &Value) -> bool {
    unwrap(expr)
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("$["))
}

fn signal_width(expr: &Value) -> u32 {
    expr.get("type")
        .and_then(Value::as_str)
        .and_then(type_width)
        .unwrap_or(1)
}

/// Does slang type this node (as the comparison sees it — the `Conversion` target, before
/// `unwrap` strips it) as signed?
pub(super) fn typed_signed(node: &Value) -> bool {
    node.get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t.contains("signed") && !t.contains("unsigned"))
}

/// SOUNDNESS (2026-10-15) — a comparison slang evaluates SIGNED must not reach an atom, because
/// every atom compares unsigned (`bad_monitor::btor2_cmp_keyword`: `<` → `ult`). slang converts
/// both operands of a signed comparison to a signed type (`x >= 0` with `logic signed [3:0] x`
/// becomes `logic signed[31:0]` on both sides); `unwrap` used to strip that and emit `(x >= 0)`,
/// which the engines decided unsigned — a false HOLDS on an always-negative `x`
/// (`e2e_signed_relational_comparison_is_not_decided_unsigned`). Until signed comparisons are
/// minted (the expression compiler), refuse:
/// - a relational `< <= > >=` on signed operands;
/// - an equality on signed operands whose literal lies outside the signal's signed range — there
///   sign extension makes SV's answer differ from the bit-pattern atom (`x == 15` on a 4-bit signed
///   `x` is never true in SV, but `x == 15` as an atom matches `-1`).
///
/// A mixed signed/unsigned comparison is converted to UNSIGNED by slang, so it is unaffected.
fn signed_comparison_guard(left: &Value, right: &Value, op: &str) -> Result<(), String> {
    if !(typed_signed(left) || typed_signed(right)) {
        return Ok(());
    }
    if matches!(op, "<" | "<=" | ">" | ">=") {
        return Err(format!(
            "signed relational comparison `{op}`: the operands are signed, and mununu's comparison \
             atoms are unsigned — deciding it would invert the answer for negative values; not \
             supported yet"
        ));
    }
    for (sig, lit) in [(left, right), (right, left)] {
        let (Some(name), Some(v)) = (cmp_signal_atom(sig), sv_integer(lit)) else {
            continue;
        };
        let w = signal_width(unwrap(sig));
        let half = 1i64.checked_shl(w.saturating_sub(1)).unwrap_or(i64::MAX);
        if w < 63 && (v >= half || v < -half) {
            return Err(format!(
                "signed equality `{name} {op} {v}`: {v} is outside the signed range of the \
                 {w}-bit `{name}`, where sign extension makes SV's answer differ from the bit \
                 pattern; not supported yet"
            ));
        }
    }
    Ok(())
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
pub(super) fn signal_name(expr: &Value) -> Result<&str, String> {
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
    // slang annotates every constant expression it folded with `constant` — a replication
    // `{4{1'b1}}`, `W - 1`, a `Conversion` of a literal (holding the CONVERTED value). The
    // outermost annotation wins; X/Z digits do not parse, so a 4-state constant stays refused.
    let mut e = expr;
    loop {
        let k = e.get("kind").and_then(Value::as_str).unwrap_or("");
        if k != "NamedValue"
            && let Some(c) = e.get("constant").and_then(Value::as_str)
        {
            return parse_sv_literal(c);
        }
        match (k, e.get("operand")) {
            ("Conversion", Some(o)) => e = o,
            _ => break,
        }
    }
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
    // A negative signed constant (`-4'sd8`, as slang folds one) is the negated magnitude.
    if let Some(rest) = s.strip_prefix('-')
        && rest.contains('\'')
    {
        return parse_sv_literal(rest).map(|v| -v);
    }
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
        // Unbounded repetition `c[*0:$]` — slang encodes `$` as a string max. (`c[*n:$]` with
        // n >= 1 as the WHOLE consequent collapses exactly to `c[*n]` since 2026-10-15; the empty
        // lower bound does not.)
        let unb = mk(serde_json::json!({
            "kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"},
            "repetition": {"kind": "Consecutive", "min": 0, "max": "$"}
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

    /// The JSON of a bounded antecedent element: boolean `name`, gap `[lo:hi]`, optional
    /// consecutive repetition `[rlo:rhi]` (shapes as slang 11.0.448 serialises them).
    fn ante_el(name: &str, lo: u64, hi: u64, rep: Option<(u64, u64)>) -> Value {
        let mut seq = serde_json::json!({"kind": "Simple",
            "expr": {"kind": "NamedValue", "symbol": format!("1 {name}"), "type": "logic"}});
        if let Some((rlo, rhi)) = rep {
            seq["repetition"] = serde_json::json!({"kind": "Consecutive", "min": rlo, "max": rhi});
        }
        serde_json::json!({"sequence": seq, "min": lo, "max": hi})
    }

    fn implication(op: &str, elements: Vec<Value>, cons: &str) -> Value {
        serde_json::json!({"kind": "Binary", "op": op,
            "left": {"kind": "SequenceConcat", "elements": elements},
            "right": {"kind": "Simple",
                      "expr": {"kind": "NamedValue", "symbol": format!("9 {cons}"), "type": "logic"}}})
    }

    /// `(a ##[1:2] b) |-> c` — two match ends, the consequent at BOTH (IEEE 1800-2017 §16.12.7).
    #[test]
    fn xl5_range_delay_antecedent_holds_at_every_match_end() {
        let spec = implication(
            "OverlappedImplication",
            vec![ante_el("a", 0, 0, None), ante_el("b", 1, 2, None)],
            "c",
        );
        let f = property_body(&spec, &TranslateOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(f, "(!(a) || [] ((!(b) || c) && [] (!(b) || c)))");
        crate::mu_calculus::parser::parse(&format!("nu X. ({f} && [] X)")).expect("parses");
    }

    /// `a[*1:3] |-> c` — a match ends after each of the first three consecutive `a`s; the
    /// lowering nests in NNF and never puts `!` over a modality.
    #[test]
    fn xl5_range_repetition_antecedent_nests_without_negated_boxes() {
        let spec = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
            "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"},
                     "repetition": {"kind": "Consecutive", "min": 1, "max": 3}},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 c"}}});
        let f = property_body(&spec, &TranslateOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(f, "(!(a) || (c && [] (!(a) || (c && [] (!(a) || c)))))");
        assert!(
            !f.contains("!([]") && !f.contains("!(<>"),
            "no negated modality: {f}"
        );
    }

    /// `|=>` adds its one box before the antecedent is lowered — once, at each match end.
    #[test]
    fn xl5_range_antecedent_nonoverlap_adds_one_box() {
        let spec = implication(
            "NonOverlappedImplication",
            vec![ante_el("a", 0, 0, None), ante_el("b", 1, 2, None)],
            "c",
        );
        let f = property_body(&spec, &TranslateOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(f, "(!(a) || [] ((!(b) || [] c) && [] (!(b) || [] c)))");
    }

    /// `a[*2] ##1 b |-> c` — a repetition inside a chain composes (was Tier-3b).
    #[test]
    fn xl5_repetition_inside_antecedent_chain_composes() {
        let spec = implication(
            "OverlappedImplication",
            vec![ante_el("a", 0, 0, Some((2, 2))), ante_el("b", 1, 1, None)],
            "c",
        );
        let f = property_body(&spec, &TranslateOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(f, "(!(a) || [] (!(a) || [] (!(b) || c)))");
    }

    /// The size guards: the product of alternatives and the total span; unbounded and empty
    /// repetition stay rejected.
    #[test]
    fn xl5_range_antecedent_product_guard() {
        let reject = |elements: Vec<Value>, needle: &str| {
            let spec = implication("OverlappedImplication", elements, "c");
            let err = property_body(&spec, &TranslateOptions::default(), &mut Vec::new())
                .expect_err("must reject");
            assert!(err.contains(needle), "want `{needle}` in: {err}");
        };
        // 9 × 9 = 81 alternatives > 64.
        reject(
            vec![ante_el("a", 0, 8, None), ante_el("b", 1, 9, None)],
            "alternatives",
        );
        // 8 × 8 = 64 is fine.
        let ok = implication(
            "OverlappedImplication",
            vec![ante_el("a", 0, 7, None), ante_el("b", 1, 8, None)],
            "c",
        );
        assert!(property_body(&ok, &TranslateOptions::default(), &mut Vec::new()).is_ok());
        reject(vec![ante_el("a", 0, 0, Some((0, 2)))], "empty");
        let unb = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
            "left": {"kind": "SequenceConcat", "elements": [
                ante_el("a", 0, 0, None),
                {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}},
                 "min": 1, "max": "$"}]},
            "right": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}}});
        let err = property_body(&unb, &TranslateOptions::default(), &mut Vec::new()).unwrap_err();
        assert!(err.contains("unboundedly"), "{err}");
    }

    /// SEMANTIC differential for the ranged-antecedent lowering. `a`, `b`, `c` are registers
    /// driven by three 8-cycle periodic waveforms off a 3-bit counter, so the design has ONE
    /// path. The exact engine decides the lowered formula; a brute-force matcher enumerates every
    /// antecedent match on the same waveform and checks the consequent at each match end. They
    /// must agree on every shape × waveform — the check that the NNF unrolling means what IEEE
    /// 1800-2017 §16.12.7 says, not merely that its text looks right.
    #[test]
    fn ranged_antecedent_lowering_agrees_with_a_brute_force_matcher() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        // (gap lo, gap hi, rep lo, rep hi, signal index 0=a 1=b 2=c) per link; consequent signal;
        // `|=>`?
        type Link = (u32, u32, u32, u32, usize);
        let shapes: Vec<(Vec<Link>, usize, bool)> = vec![
            (vec![(0, 0, 1, 1, 0), (1, 2, 1, 1, 1)], 2, false), // a ##[1:2] b |-> c
            (vec![(0, 0, 1, 3, 0)], 2, false),                  // a[*1:3] |-> c
            (vec![(0, 0, 2, 2, 0), (1, 1, 1, 1, 1)], 2, false), // a[*2] ##1 b |-> c
            (vec![(0, 0, 1, 1, 0), (0, 2, 1, 2, 1)], 2, true),  // a ##[0:2] b[*1:2] |=> c
            (
                vec![(0, 0, 1, 1, 0), (1, 1, 1, 1, 1), (1, 3, 1, 1, 2)],
                0,
                false,
            ), // a ##1 b ##[1:3] c |-> a
        ];
        let names = ["a", "b", "c"];
        let mut seed: u32 = 0x2026_1015;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        let mut disagreements = Vec::new();
        let mut holds_seen = 0;
        let mut violated_seen = 0;
        for (shape, cons, nonoverlap) in &shapes {
            let elements: Vec<Value> = shape
                .iter()
                .map(|&(lo, hi, rlo, rhi, s)| {
                    let rep = (rlo, rhi) != (1, 1);
                    ante_el(
                        names[s],
                        lo.into(),
                        hi.into(),
                        rep.then_some((rlo.into(), rhi.into())),
                    )
                })
                .collect();
            let op = if *nonoverlap {
                "NonOverlappedImplication"
            } else {
                "OverlappedImplication"
            };
            let spec = implication(op, elements, names[*cons]);
            let f = translate_one(
                &spec,
                SvaKind::Assert,
                &TranslateOptions::default(),
                &mut Vec::new(),
            )
            .expect("translates");
            let formula = crate::mu_calculus::parser::parse(&f).expect("parses");
            for _ in 0..40 {
                // Biased toward 1s so antecedents actually match.
                let masks = [rnd() | rnd(), rnd() | rnd(), rnd()];
                let sig = |s: usize, t: usize| masks[s] >> (t % 8) & 1 == 1;
                // Brute force over the periodic word: every start t in one period covers all.
                let mut holds = true;
                for t in 0..8 {
                    let mut ends = vec![];
                    // (link index, earliest cycle the link's gap counts from)
                    fn walk(
                        shape: &[(u32, u32, u32, u32, usize)],
                        i: usize,
                        from: usize,
                        sig: &dyn Fn(usize, usize) -> bool,
                        ends: &mut Vec<usize>,
                    ) {
                        let (lo, hi, rlo, rhi, s) = shape[i];
                        for d in lo..=hi {
                            let start = from + d as usize;
                            for r in rlo..=rhi {
                                if (0..r as usize).all(|k| sig(s, start + k)) {
                                    let end = start + r as usize - 1;
                                    if i + 1 == shape.len() {
                                        ends.push(end);
                                    } else {
                                        walk(shape, i + 1, end, sig, ends);
                                    }
                                }
                            }
                        }
                    }
                    walk(shape, 0, t, &sig, &mut ends);
                    if ends
                        .iter()
                        .any(|&e| !sig(*cons, e + usize::from(*nonoverlap)))
                    {
                        holds = false;
                    }
                }
                let btor2 = periodic_design(&masks);
                let exact = exact_symbolic_verdict(&btor2, &formula).expect("exact decides");
                let want = if holds {
                    ExactVerdict::Holds
                } else {
                    ExactVerdict::Violated
                };
                if holds {
                    holds_seen += 1;
                } else {
                    violated_seen += 1;
                }
                if exact != want {
                    disagreements.push((f.clone(), masks, exact, want));
                }
            }
        }
        assert!(
            disagreements.is_empty(),
            "lowering ≠ SVA semantics: {disagreements:#?}"
        );
        assert!(
            holds_seen > 20 && violated_seen > 20,
            "both verdicts exercised: {holds_seen} / {violated_seen}"
        );
    }

    /// `a |-> (c throughout (b ##[1:2] b))` — `c` at the start, through the waited cycle(s), and
    /// at each `b`; the window is `b` at +1 or +2.
    #[test]
    fn throughout_conjoins_the_invariant_at_every_cycle_of_the_match() {
        let spec = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
            "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
            "right": {"kind": "Binary", "op": "Throughout",
                "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}},
                "right": {"kind": "SequenceConcat", "elements": [
                    ante_el("b", 0, 0, None), ante_el("b", 1, 2, None)]}}});
        let f = property_body(&spec, &TranslateOptions::default(), &mut Vec::new()).unwrap();
        assert_eq!(
            f,
            "(!(a) || ((c && b) && (c && [] ((c && b) || (c && [] (c && b))))))"
        );
        crate::mu_calculus::parser::parse(&format!("nu X. ({f} && [] X)")).expect("parses");
    }

    /// Unbounded and nested operands of `throughout` stay refused, as does a non-boolean
    /// invariant.
    #[test]
    fn throughout_over_an_unbounded_or_nested_sequence_is_refused() {
        let thr = |left: Value, right: Value| {
            serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
                "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a"}},
                "right": {"kind": "Binary", "op": "Throughout", "left": left, "right": right}})
        };
        let c =
            serde_json::json!({"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"}});
        let unb = serde_json::json!({"kind": "SequenceConcat", "elements": [
            {"sequence": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}}, "min": 1, "max": "$"}]});
        let err = property_body(
            &thr(c.clone(), unb),
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.contains("unbounded"), "{err}");
        let rep_e = serde_json::json!({"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c"},
            "repetition": {"kind": "Consecutive", "min": 2, "max": 2}});
        let b =
            serde_json::json!({"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "2 b"}});
        let err = property_body(
            &thr(rep_e, b),
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(err.contains("must be a boolean"), "{err}");
    }

    /// SEMANTIC differential for `throughout` (2026-10-15), the consequent counterpart of
    /// `ranged_antecedent_lowering_agrees_with_a_brute_force_matcher`: `a |-> (c throughout S)`
    /// over random periodic waveforms, the exact engine on the lowered formula vs a brute-force
    /// check that, wherever `a` holds, SOME match of `S` starts there with `c` at every cycle of it.
    #[test]
    fn throughout_lowering_agrees_with_a_brute_force_matcher() {
        use crate::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};
        // S = elements over `b`: (gap lo, gap hi, repetition n).
        type El = (u32, u32, u32);
        let shapes: Vec<Vec<El>> = vec![
            vec![(0, 0, 1), (1, 1, 1)], // b ##1 b
            vec![(1, 2, 1)],            // ##[1:2] b
            vec![(0, 0, 2)],            // b[*2]
            vec![(1, 1, 1), (0, 2, 1)], // ##1 b ##[0:2] b
        ];
        let mut seed: u32 = 0x0715_2026;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        let mut disagreements = Vec::new();
        let (mut holds_seen, mut violated_seen) = (0, 0);
        for shape in &shapes {
            let elements: Vec<Value> = shape
                .iter()
                .map(|&(lo, hi, n)| {
                    ante_el(
                        "b",
                        lo.into(),
                        hi.into(),
                        (n != 1).then_some((n.into(), n.into())),
                    )
                })
                .collect();
            let spec = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
                "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "1 a", "type": "logic"}},
                "right": {"kind": "Binary", "op": "Throughout",
                    "left": {"kind": "Simple", "expr": {"kind": "NamedValue", "symbol": "3 c", "type": "logic"}},
                    "right": {"kind": "SequenceConcat", "elements": elements}}});
            let f = translate_one(
                &spec,
                SvaKind::Assert,
                &TranslateOptions::default(),
                &mut Vec::new(),
            )
            .expect("translates");
            let formula = crate::mu_calculus::parser::parse(&f).expect("parses");
            for _ in 0..40 {
                let masks = [rnd(), rnd() | rnd(), rnd() | rnd() | rnd()];
                let sig = |s: usize, t: usize| masks[s] >> (t % 8) & 1 == 1;
                // Does a match of S start at `t` with c on every cycle t..=end?
                fn matches(
                    shape: &[(u32, u32, u32)],
                    i: usize,
                    cur: usize,
                    start: usize,
                    sig: &dyn Fn(usize, usize) -> bool,
                ) -> bool {
                    let (lo, hi, n) = shape[i];
                    (lo..=hi).any(|d| {
                        let s0 = cur + d as usize;
                        let end = s0 + n as usize - 1;
                        (s0..=end).all(|k| sig(1, k))
                            && (start..=end).all(|k| sig(2, k))
                            && (i + 1 == shape.len() || matches(shape, i + 1, end, start, sig))
                    })
                }
                let holds = (0..8).all(|t| !sig(0, t) || matches(shape, 0, t, t, &sig));
                let btor2 = periodic_design(&masks);
                let exact = exact_symbolic_verdict(&btor2, &formula).expect("exact decides");
                let want = if holds {
                    ExactVerdict::Holds
                } else {
                    ExactVerdict::Violated
                };
                if holds {
                    holds_seen += 1
                } else {
                    violated_seen += 1
                }
                if exact != want {
                    disagreements.push((f.clone(), masks, exact, want));
                }
            }
        }
        assert!(
            disagreements.is_empty(),
            "lowering ≠ SVA semantics: {disagreements:#?}"
        );
        assert!(
            holds_seen > 10 && violated_seen > 10,
            "both verdicts exercised: {holds_seen} / {violated_seen}"
        );
    }

    /// `$countones` reads the minted `<base>__popcount`, over a signal or a slice (whose minted
    /// name is the base); the collector records each base once.
    #[test]
    fn xl7_countones_mints_popcount_atom_and_records_its_base_once() {
        let lit = |v: &str| serde_json::json!({"kind": "IntegerLiteral", "type": "int", "value": v, "constant": v});
        let cnt = |arg: Value| serde_json::json!({"kind": "Call", "subroutine": "$countones", "arguments": [arg]});
        let gnt =
            serde_json::json!({"kind": "NamedValue", "symbol": "1 gnt", "type": "logic[7:0]"});
        let sl = serde_json::json!({"kind": "RangeSelect", "selectionKind": "Simple",
            "value": {"kind": "NamedValue", "symbol": "2 w", "type": "logic[15:0]"},
            "left": lit("7"), "right": lit("0")});
        let ge = serde_json::json!({"kind": "BinaryOp", "op": "GreaterThanEqual",
            "left": cnt(gnt.clone()), "right": lit("2")});
        assert_eq!(bool_expr(&ge).unwrap(), "(gnt__popcount >= 2)");
        let eq = serde_json::json!({"kind": "BinaryOp", "op": "Equality",
            "left": cnt(sl.clone()), "right": cnt(gnt.clone())});
        assert_eq!(
            bool_expr(&eq).unwrap(),
            "(w__bits7_0__popcount == gnt__popcount)"
        );
        let both =
            serde_json::json!({"kind": "BinaryOp", "op": "LogicalAnd", "left": ge, "right": eq});
        assert_eq!(
            collect_popcounts(&both),
            vec!["gnt".to_string(), "w__bits7_0".to_string()]
        );
        // The slice the count reads is itself collected, so it is minted first.
        let mut slices = Vec::new();
        collect_slice_signals(&both, &mut slices);
        assert!(
            slices
                .iter()
                .any(|s| s.base == "w" && (s.msb, s.lsb) == (7, 0))
        );
        // The naming contract with the BTOR2 side.
        assert_eq!(
            popcount_atom_name("x"),
            crate::adapter::btor2::popcount_signal_name("x")
        );
    }

    /// A 3-bit counter `cnt` (init 0, +1 mod 8) and registers `a`, `b`, `c` with
    /// `x(t) = mask_x bit (t mod 8)`: init = bit 0, next = bit `(cnt + 1) mod 8`.
    fn periodic_design(masks: &[u8; 3]) -> String {
        let mut l = vec![
            "1 sort bitvec 1".to_string(),
            "2 sort bitvec 3".to_string(),
            "3 state 2 cnt".to_string(),
            "4 zero 2".to_string(),
            "5 init 2 3 4".to_string(),
            "6 one 2".to_string(),
            "7 add 2 3 6".to_string(),
            "8 next 2 3 7".to_string(),
            "9 zero 1".to_string(),
            "10 one 1".to_string(),
        ];
        let mut nid = 11;
        // eq_k = (cnt + 1 == k) for k in 0..8, shared by the three registers.
        let mut eq = vec![];
        for k in 0..8 {
            l.push(format!("{nid} constd 2 {k}"));
            l.push(format!("{} eq 1 7 {nid}", nid + 1));
            eq.push(nid + 1);
            nid += 2;
        }
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let bit = |t: usize| if masks[i] >> t & 1 == 1 { 10 } else { 9 };
            let st = nid;
            l.push(format!("{st} state 1 {name}"));
            l.push(format!("{} init 1 {st} {}", st + 1, bit(0)));
            nid += 2;
            let mut acc = bit(7);
            for k in (0..7).rev() {
                l.push(format!("{nid} ite 1 {} {} {acc}", eq[k], bit(k)));
                acc = nid;
                nid += 1;
            }
            l.push(format!("{nid} next 1 {st} {acc}"));
            nid += 1;
        }
        l.join("\n") + "\n"
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
    fn xl1c_dynamic_bit_select_mints_only_while_the_index_stays_in_range() {
        // `req_i[idx_o]` with a 3-bit index into an 8-bit vector — every index is in range, so
        // the expression compiler mints `(req_i >> idx_o)[0]`.
        let sel = |iw: u32| {
            serde_json::json!({
                "kind": "ElementSelect", "type": "logic",
                "selector": {"kind": "NamedValue", "symbol": "1 idx_o",
                             "type": format!("logic[{}:0]", iw - 1)},
                "value": {"kind": "NamedValue", "symbol": "2 req_i", "type": "logic[7:0]"}
            })
        };
        let atom = bool_expr(&sel(3)).expect("in range mints");
        let ir = minted(&atom);
        let env = |n: &str| if n == "req_i" { 0b0010_0100 } else { 5 };
        assert_eq!(ir.eval(&env), 1, "req_i[5] of 0b0010_0100");
        // A 4-bit index can reach 8..15, where SV reads X: refused, both reasons named.
        let err = bool_expr(&sel(4)).expect_err("out of range must refuse");
        assert!(
            err.contains("dynamic bit-select") && err.contains("outside"),
            "got: {err}"
        );
    }

    /// The expression a minted atom name stands for (the most recent registration).
    fn minted(atom: &str) -> ExprIr {
        MINT.with(|m| {
            m.borrow()
                .exprs
                .iter()
                .rev()
                .find(|e| e.name == atom)
                .map(|e| e.expr.clone())
                .unwrap_or_else(|| panic!("`{atom}` was not minted"))
        })
    }

    /// mununu#514 — the constant-index bit-select branch had NO test, which is how the array
    /// case came to share its message. Pins the bit-select side so the array split below cannot
    /// swallow it.
    #[test]
    fn xl1c_constant_bit_select_is_the_one_bit_slice() {
        // `vec[3]` on a PACKED vector — a genuine bit-select, since 2026-10-15 the 1-bit slice
        // `vec[3:3]`, minted as `vec__bits3_3` (and, in boolean position, the bare atom).
        let expr = serde_json::json!({
            "kind": "ElementSelect",
            "selector": {"kind": "IntegerLiteral", "value": "3"},
            "value": {"kind": "NamedValue", "symbol": "1 vec", "type": "logic[7:0]"}
        });
        assert_eq!(bool_expr(&expr).unwrap(), "vec__bits3_3");
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

    /// mununu#565 — a bit-slice against a literal is ACCEPTED and resolves to its minted signal.
    ///
    /// The shape is the one slang 11.0.448 actually emits, measured in `mununu-sva` rather than
    /// guessed: base under `value`, bounds as `left`/`right`, `selectionKind: "Simple"`.
    #[test]
    fn xl565_slice_against_literal_resolves_to_a_minted_signal() {
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left": {
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "type": "int", "value": "7", "constant": "7"},
                "right": {"kind": "IntegerLiteral", "type": "int", "value": "0", "constant": "0"}
            },
            "right": {"kind": "IntegerLiteral", "type": "int", "value": "165", "constant": "165"}
        });
        // A PLAIN identifier: `[` is a Box modality to the mu-calculus lexer, so a bracketed atom
        // would misparse rather than fail.
        assert_eq!(bool_expr(&cmp).unwrap(), "(wdata__bits7_0 == 165)");
    }

    /// mununu#565 — REPRODUCER for the trap the falsifier caught.
    ///
    /// `wdata[7 -: 8]` serialises with `selectionKind: "IndexedDown"` and `left: 7, right: 8` —
    /// base and **WIDTH**, not bounds. Reading them as a range gives bits `[7:8]`: inverted and
    /// wrong, with no error anywhere. So the indexed forms decline with a NAMED reason rather than
    /// being mis-compiled, which is the same call `xl1c_constant_bit_select_is_rejected_…` makes.
    #[test]
    fn xl565_indexed_part_select_declines_with_its_reason_not_a_wrong_slice() {
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left": {
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "IndexedDown",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "type": "int", "value": "7", "constant": "7"},
                "right": {"kind": "IntegerLiteral", "type": "int", "value": "8", "constant": "8"}
            },
            "right": {"kind": "IntegerLiteral", "type": "int", "value": "165", "constant": "165"}
        });
        let err = bool_expr(&cmp).expect_err("an indexed part-select must decline");
        assert!(err.contains("IndexedDown"), "must name what it saw: {err}");
        assert!(
            err.contains("WIDTH") || err.contains("width"),
            "must say WHY — bounds are base-and-width there: {err}"
        );
        // It must NOT silently produce a slice atom.
        assert!(
            !err.contains("__bits"),
            "must not have minted anything: {err}"
        );
    }

    /// mununu#565 — `$past(sig[hi:lo])` slices the SHADOW, so `shadow.rs` needs no change.
    #[test]
    fn xl565_past_over_a_slice_slices_the_shadow() {
        let cmp = serde_json::json!({
            "kind": "BinaryOp", "op": "Equality",
            "left": {"kind": "NamedValue", "type": "logic[7:0]", "symbol": "1 shadow_q"},
            "right": {
                "kind": "Call", "subroutine": "$past",
                "arguments": [{
                    "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "Simple",
                    "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "2 wdata"},
                    "left":  {"kind": "IntegerLiteral", "type": "int", "constant": "7"},
                    "right": {"kind": "IntegerLiteral", "type": "int", "constant": "0"}
                }]
            }
        });
        assert_eq!(
            bool_expr(&cmp).unwrap(),
            "(shadow_q == wdata__past__bits7_0)",
            "the slice is taken OF the one-cycle-old whole signal"
        );
    }

    /// mununu#565 — the collector records what the BTOR2 pass must mint, deduped.
    #[test]
    fn xl565_collector_records_slices_once_each() {
        let spec = serde_json::json!({
            "a": {
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "constant": "7"},
                "right": {"kind": "IntegerLiteral", "constant": "0"}
            },
            "b": {
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "constant": "7"},
                "right": {"kind": "IntegerLiteral", "constant": "0"}
            },
            "c": {
                "kind": "RangeSelect", "type": "logic[15:0]", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "constant": "31"},
                "right": {"kind": "IntegerLiteral", "constant": "16"}
            },
            "indexed_is_not_collected": {
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "IndexedUp",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left":  {"kind": "IntegerLiteral", "constant": "0"},
                "right": {"kind": "IntegerLiteral", "constant": "8"}
            }
        });
        let mut out = Vec::new();
        collect_slice_signals(&spec, &mut out);
        out.sort_by_key(|s| (s.msb, s.lsb));
        assert_eq!(
            out.len(),
            2,
            "deduped, and the indexed form is not collected: {out:?}"
        );
        assert_eq!(
            (out[0].base.as_str(), out[0].msb, out[0].lsb),
            ("wdata", 7, 0)
        );
        assert_eq!(
            (out[1].base.as_str(), out[1].msb, out[1].lsb),
            ("wdata", 31, 16)
        );
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
    fn onehot_expands_and_countones_mints_a_count() {
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

        let cnt = serde_json::json!({
            "kind": "Call", "subroutine": "$countones",
            "arguments": [{"kind": "NamedValue", "symbol": "1 gnt", "type": "logic[7:0]"}]
        });
        // `$countones(x)` in boolean position (2026-10-15): "some bit is set".
        assert_eq!(bool_expr(&cnt).unwrap(), "(gnt__popcount != 0)");
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

    /// mununu#632 — a `localparam` / `parameter` reference in an atom folds to the
    /// elaborated value slang carries on the `Parameter` node, the way an enum member
    /// does; a parameter whose value is not a literal stays unresolved. The fixture
    /// mirrors `examples/verify/v12_link_ctrl_llm_fsm/link_ctrl.sv`'s
    /// `a_err_implies_retry_limit` (`err_q |-> retry_cnt_q == MAX_RETRIES`).
    #[test]
    fn x632_parameter_reference_resolves_to_its_elaborated_value() {
        let doc = serde_json::json!({
            "members": [
                {"kind": "Parameter", "name": "MAX_RETRIES", "type": "int", "value": "3", "isLocal": true},
                {"kind": "Parameter", "name": "S_XFER", "type": "logic[2:0]", "value": "3'b10", "isLocal": true},
                {"kind": "Parameter", "name": "NAME", "type": "string", "value": "\"link\"", "isLocal": true}
            ],
            "body": [
                {
                    "kind": "ConcurrentAssertion", "assertionKind": "Assert",
                    "propertySpec": { "kind": "Simple", "expr": {
                        "kind": "BinaryOp", "op": "Equality",
                        "left":  {"kind": "NamedValue", "symbol": "1 retry_cnt_q", "type": "logic[1:0]"},
                        "right": {"kind": "NamedValue", "symbol": "2 MAX_RETRIES", "type": "int"}
                    }}
                },
                {
                    "kind": "ConcurrentAssertion", "assertionKind": "Assert",
                    "propertySpec": { "kind": "Simple", "expr": {
                        "kind": "BinaryOp", "op": "Equality",
                        "left":  {"kind": "NamedValue", "symbol": "3 state_q", "type": "logic[2:0]"},
                        "right": {"kind": "NamedValue", "symbol": "4 S_XFER", "type": "logic[2:0]"}
                    }}
                },
                {
                    "kind": "ConcurrentAssertion", "assertionKind": "Assert",
                    "propertySpec": { "kind": "Simple", "expr": {
                        "kind": "BinaryOp", "op": "Equality",
                        "left":  {"kind": "NamedValue", "symbol": "5 tag_q", "type": "string"},
                        "right": {"kind": "NamedValue", "symbol": "6 NAME", "type": "string"}
                    }}
                }
            ]
        });
        let report = translate_ast_json(&doc.to_string()).expect("valid");
        let formulas: Vec<&str> = report
            .translated
            .iter()
            .map(|t| t.formula.as_str())
            .collect();
        assert!(
            formulas.iter().any(|f| f.contains("retry_cnt_q == 3")),
            "`MAX_RETRIES` folds to 3: {formulas:?}"
        );
        assert!(
            formulas.iter().any(|f| f.contains("state_q == 2")),
            "`S_XFER` (3'b10) folds to 2: {formulas:?}"
        );
        // The string parameter cannot fold: its reference stays a name (the engine's
        // "unknown register/signal" skip, as before), never a wrong literal.
        let all: String = formulas.join(" ")
            + &report
                .unsupported
                .iter()
                .map(|u| u.reason.clone())
                .collect::<Vec<_>>()
                .join(" ");
        assert!(
            !all.contains("tag_q == 0") && (all.contains("NAME") || !all.contains("tag_q")),
            "a non-literal parameter must not fold to a number: {formulas:?} / {:?}",
            report.unsupported
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

    /// REPRODUCER (2026-10-15) — a slice of a NON-zero-based vector read the wrong bits.
    ///
    /// yosys numbers a vector's bits from 0 at the right-most declared index whatever the range, so
    /// `off[11:8]` of `logic [15:8] off` is BTOR2 bits 3..0. The slice path passed SV indices through
    /// as bit positions, minting `slice … 11 8` — past the end of an 8-bit vector, so the slice was
    /// never minted and the property skipped; on `logic [11:4]` the same mistake reads the WRONG bits
    /// silently. Fails on the parent commit (`off__bits11_8`).
    #[test]
    fn slice_of_an_offset_vector_reads_the_yosys_bit_positions() {
        let lit = |v: &str| serde_json::json!({"kind": "IntegerLiteral", "type": "int", "value": v, "constant": v});
        let slice = |ty: &str, l: &str, r: &str| {
            serde_json::json!({"kind": "RangeSelect", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "symbol": "1 off", "type": ty},
                "left": lit(l), "right": lit(r)})
        };
        let cmp = |sl: Value| serde_json::json!({"kind": "BinaryOp", "op": "Equality", "left": sl, "right": lit("3")});
        assert_eq!(
            bool_expr(&cmp(slice("logic[15:8]", "11", "8"))).unwrap(),
            "(off__bits3_0 == 3)"
        );
        assert_eq!(
            bool_expr(&cmp(slice("logic[11:4]", "7", "4"))).unwrap(),
            "(off__bits3_0 == 3)"
        );
        // An ascending vector: `asc[0:3]` of `logic [0:7]` is its four MOST significant bits.
        assert_eq!(
            bool_expr(&cmp(slice("logic[0:7]", "0", "3"))).unwrap(),
            "(off__bits7_4 == 3)"
        );
        // The zero-based case is unchanged — the identity, which is why this went unnoticed.
        assert_eq!(
            bool_expr(&cmp(slice("logic[31:0]", "7", "0"))).unwrap(),
            "(off__bits7_0 == 3)"
        );
    }

    /// REPRODUCER (2026-10-15) — `$past(sig[7:0])` never requested `sig`'s shadow.
    ///
    /// The atom is a slice OF the shadow (`sig__past__bits7_0`), but `collect_shadow_signals` only
    /// resolved a plain-signal argument, so `sig__past` was never built, the slice had no base and
    /// was never minted, and the property skipped. (This is the mechanism behind the
    /// `q == wdata__past__bits7_0` discrepancy the verify-auto seam test records.) Fails on the
    /// parent commit (no shadow recorded).
    #[test]
    fn past_over_a_slice_requests_the_base_shadow() {
        let lit = |v: &str| serde_json::json!({"kind": "IntegerLiteral", "type": "int", "value": v, "constant": v});
        let sl = serde_json::json!({"kind": "RangeSelect", "selectionKind": "Simple",
            "value": {"kind": "NamedValue", "symbol": "1 sig", "type": "logic[15:0]"},
            "left": lit("7"), "right": lit("0")});
        let bit = serde_json::json!({"kind": "ElementSelect", "selector": lit("3"),
            "value": {"kind": "NamedValue", "symbol": "2 flag", "type": "logic[7:0]"}});
        let spec = serde_json::json!({"kind": "Simple", "expr": {"kind": "BinaryOp", "op": "LogicalAnd",
            "left": {"kind": "BinaryOp", "op": "Equality",
                     "left": {"kind": "NamedValue", "symbol": "3 q", "type": "logic[7:0]"},
                     "right": {"kind": "Call", "subroutine": "$past", "arguments": [sl, lit("2")]}},
            "right": {"kind": "Call", "subroutine": "$rose", "arguments": [bit]}}});
        let mut shadows = Vec::new();
        collect_shadow_signals(&spec, &mut shadows);
        let sig = shadows
            .iter()
            .find(|s| s.base == "sig")
            .expect("`sig` needs its shadow");
        assert_eq!((sig.width, sig.depth), (16, 2));
        let flag = shadows
            .iter()
            .find(|s| s.base == "flag")
            .expect("`flag` needs its shadow");
        assert_eq!((flag.width, flag.depth), (8, 1));
        let mut slices = Vec::new();
        collect_slice_signals(&spec, &mut slices);
        let has = |b: &str, m: u32, l: u32| {
            slices
                .iter()
                .any(|s| s.base == b && s.msb == m && s.lsb == l)
        };
        assert!(
            has("sig__past2", 7, 0),
            "the slice of the depth-2 shadow: {slices:?}"
        );
        assert!(
            has("flag", 3, 3) && has("flag__past", 3, 3),
            "both halves of $rose: {slices:?}"
        );
    }

    /// The real slang 11.0.448 shapes (`testdata/sva_bit_select_forms.{sv,ast.json}`, captured in
    /// the `mununu-sva` image): every bit-select and slice maps to the bit position yosys gives it,
    /// a typedef resolves through its `TypeAlias`, a packed struct is zero-based, a
    /// multi-dimensional element is its W-bit slice, and history calls over a bit request both
    /// halves.
    #[test]
    fn bit_selects_and_slices_map_sv_indices_to_yosys_bit_positions() {
        let report = translate_ast_json(include_str!("testdata/sva_bit_select_forms.ast.json"))
            .expect("the fixture parses");
        let formulas: Vec<&str> = report
            .translated
            .iter()
            .map(|t| t.formula.as_str())
            .collect();
        let has = |needle: &str| formulas.iter().any(|f| f.contains(needle));
        for needle in [
            "off__bits1_1",                          // off[9]       of logic[15:8]
            "(off__bits3_0 == 3)",                   // off[11:8]    of logic[15:8]
            "asc__bits6_6",                          // asc[1]       of logic[0:7]
            "(asc__bits7_4 == 3)",                   // asc[0:3]     of logic[0:7]
            "tb__bits2_2",                           // tb[2], typedef logic[7:0] byte_t
            "ps__bits5_5",                           // ps[5], packed struct
            "(v__bits3_3 && (!(v__past__bits3_3)))", // $rose(v[3])
            "(v__bits7_4 == v__past__bits7_4)",      // $stable(v[7:4])
            "(v__bits2_2 == v__past__bits2_2)",      // v[2] == $past(v[2])
            "(!(false))",                            // !$isunknown(v)
            "(md__bits15_8 == 2)",                   // md[1] of logic[3:0][7:0] (element 1)
        ] {
            assert!(has(needle), "no formula contains `{needle}`: {formulas:#?}");
        }
        assert!(
            report.unsupported.is_empty(),
            "every form translates: {:?}",
            report.unsupported
        );
        assert!(
            report
                .required_shadows
                .iter()
                .any(|s| s.base == "v" && s.width == 8 && s.depth == 1)
        );
        assert_eq!(report.two_state_folds.len(), 1);
        assert_eq!(report.two_state_folds[0].construct, "$isunknown");
        assert!(
            report
                .translated
                .iter()
                .any(|t| t.name == report.two_state_folds[0].assertion)
        );
    }

    /// OpenTitan coverage, Phase 1 — the real slang 11.0.448 shapes
    /// (`testdata/sva_phase1_forms.{sv,ast.json}`, captured in the `mununu-sva` image). Struct
    /// members map first-member-most-significant, a member typed by another typedef expands
    /// through the embedded alias, a `logic [1:0][W-1:0]` element is its W-bit slice, `->` / `<->`
    /// and 1-bit `& | ^ ~` are boolean, slang's folded constants are literals and a bare `##1`
    /// sequence is a consequent. A vector `&` stays refused (a value, for the expression compiler).
    #[test]
    fn phase1_forms_translate_from_real_slang_json() {
        let report = translate_ast_json(include_str!("testdata/sva_phase1_forms.ast.json"))
            .expect("the fixture parses");
        let by_label = |l: &str| {
            report
                .translated
                .iter()
                .find(|t| t.label.as_deref() == Some(l))
                .map(|t| t.formula.clone())
                .unwrap_or_else(|| panic!("`{l}` did not translate: {:?}", report.unsupported))
        };
        for (label, needle) in [
            ("p_mem", "(e__bits1_1 == e__bits0_0)"),
            ("p_memw", "(w__bits2_1 == 1)"),
            ("p_nest", "n__bits3_3"),
            ("p_nest", "(n__bits2_0 == 5)"),
            (
                "p_xor",
                "((e__bits1_1 && (!(e__bits0_0))) || ((!(e__bits1_1)) && e__bits0_0))",
            ),
            ("p_rose", "(e__bits1_1 && (!(e__past__bits1_1)))"),
            ("p_pastm", "(w__past__bits6_3 == 3)"),
            ("p_md", "(cnt__bits7_4 == 3)"),
            ("p_mdst", "(cnt__bits7_4 == cnt__past__bits7_4)"),
            ("p_band", "((!(a)) || b)"),
            ("p_impl", "((!(a)) || (!(b)))"),
            ("p_equiv", "((a && b) || ((!(a)) && (!(b))))"),
            ("p_seq", "[] (a == b)"),
            ("p_rep", "(cnt__bits3_0 == 15)"),
        ] {
            let f = by_label(label);
            assert!(f.contains(needle), "{label}: `{f}` lacks `{needle}`");
        }
        // `(cnt[0] & cnt[1]) == 0` — a vector `&` — is the expression compiler's: the value is
        // minted and compared against the literal.
        assert!(report.unsupported.is_empty(), "{:?}", report.unsupported);
        assert!(
            by_label("p_wide").contains("mununu_x_"),
            "{}",
            by_label("p_wide")
        );
        // History over a member / element requests the WHOLE base's shadow.
        for base in ["e", "w", "cnt"] {
            assert!(
                report.required_shadows.iter().any(|s| s.base == base),
                "no `{base}` shadow: {:?}",
                report.required_shadows
            );
        }
        let w = report
            .required_shadows
            .iter()
            .find(|s| s.base == "w")
            .expect("w");
        assert_eq!(w.width, 7, "the shadow is over the whole 7-bit struct");
    }

    /// OpenTitan coverage, Phases 2 + 3 — the real slang 11.0.448 shapes
    /// (`testdata/sva_expr_forms.{sv,ast.json}`): every form translates, and the minted IR of
    /// prim_count's counter checks computes what the SVA means (`$past` pushed to the leaves,
    /// the signed cast, `min` / `max` inlined from their single-`return` bodies).
    #[test]
    fn expression_forms_translate_from_real_slang_json() {
        let report = translate_ast_json(include_str!("testdata/sva_expr_forms.ast.json"))
            .expect("the fixture parses");
        assert!(report.unsupported.is_empty(), "{:#?}", report.unsupported);
        let formula = |l: &str| {
            report
                .translated
                .iter()
                .find(|t| t.label.as_deref() == Some(l))
                .map(|t| t.formula.clone())
                .unwrap_or_else(|| panic!("no `{l}`"))
        };
        // The one minted expression a label's formula reads (its consequent's).
        let minted_in = |l: &str| -> Vec<ExprIr> {
            let f = formula(l);
            report
                .required_expressions
                .iter()
                .filter(|m| f.contains(&m.name))
                .map(|m| m.expr.clone())
                .collect()
        };
        let env = |vals: &'static [(&'static str, u128)]| {
            move |n: &str| {
                vals.iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| *v)
                    .unwrap_or_else(|| panic!("leaf `{n}` not in the environment"))
            }
        };
        // e_min: cnt_o == min($past(cnt_o) + $past({2'b0, step_i}), 15) — saturating increment.
        let e_min_all = minted_in("e_min");
        let [e_min] = &e_min_all[..] else {
            panic!("{}", formula("e_min"))
        };
        let at = |cnt: u128, past: u128, step: u128| {
            let vals: Vec<(&str, u128)> = vec![
                ("cnt_o", cnt),
                ("cnt_o__past", past),
                ("step_i__past", step),
            ];
            e_min.eval(&move |n| vals.iter().find(|(k, _)| *k == n).map(|(_, v)| *v).unwrap())
        };
        assert_eq!(at(12, 9, 3), 1, "9 + 3 = 12");
        assert_eq!(at(11, 9, 3), 0);
        assert_eq!(at(15, 14, 3), 1, "14 + 3 saturates at 15");
        assert_eq!(at(1, 14, 3), 0, "no 4-bit wrap: the sum is 6 bits wide");
        // e_max: cnt_q[1] == max($past(signed'({2'b0, cnt_q[1]})) - $past({2'b0, step_i}), '0)
        // — a saturating decrement over the SIGNED difference.
        let e_max_all = minted_in("e_max");
        let [e_max] = &e_max_all[..] else {
            panic!("{}", formula("e_max"))
        };
        let dec = |now: u128, past: u128, step: u128| {
            // cnt_q is logic[1:0][3:0]: element 1 is bits 7..4.
            let vals: Vec<(&str, u128)> = vec![
                ("cnt_q", now << 4),
                ("cnt_q__past", past << 4),
                ("step_i__past", step),
            ];
            e_max.eval(&move |n| vals.iter().find(|(k, _)| *k == n).map(|(_, v)| *v).unwrap())
        };
        assert_eq!(dec(7, 9, 2), 1, "9 - 2 = 7");
        assert_eq!(dec(0, 1, 3), 1, "1 - 3 saturates at 0 (signed max)");
        assert_eq!(dec(14, 1, 3), 0, "an unsigned max would read 1 - 3 as 62");
        // e_sgn: sx < 0 |-> sx >= -8 — a signed comparison, true for every 4-bit sx.
        let sgn = minted_in("e_sgn");
        assert!(!sgn.is_empty(), "{}", formula("e_sgn"));
        for e in &sgn {
            if let ExprIr::Cmp { signed, .. } = e {
                assert!(*signed, "{e:?}");
            }
        }
        // e_dyn: gnt_o[idx_i] && req_i[idx_i] — a 2-bit index into 4 bits, always in range.
        let dyn_ = minted_in("e_dyn");
        assert_eq!(dyn_.len(), 2, "{}", formula("e_dyn"));
        let e = env(&[("gnt_o", 0b0100), ("req_i", 0b0100), ("idx_i", 2)]);
        assert!(dyn_.iter().all(|x| x.eval(&e) == 1));
        // Every `$past` leaf requested its shadow.
        for base in ["cnt_o", "step_i", "cnt_q", "idx_i"] {
            assert!(
                report.required_shadows.iter().any(|s| s.base == base),
                "no `{base}` shadow: {:?}",
                report.required_shadows
            );
        }
    }

    /// **The contract between the SVA translator's liveness lowerings and the l2s rescue**
    /// (`liveness_rescue::reduce_response_af`) — the seam the plan named, in the cheap tier.
    ///
    /// The exact engine decides every one of these directly when it can; the rescue is what
    /// carries them past it (and past a primary-input atom, which the exact engine refuses). One
    /// row per liveness shape the translator can emit: it must classify as `Liveness` and reduce
    /// to `(has antecedent, delay)` — the delay being the boxes `|=>` and `##[m≥1:$]` put between
    /// the implication and the `AF`, which the monitor reads through a request delay line. The
    /// last row is the recorded gap: a relational eventuality the monitor's boolean compiler cannot bind.
    #[test]
    fn every_translator_liveness_shape_is_recognised_by_the_rescue() {
        use crate::adapter::liveness_rescue::reduce_response_shape;
        use crate::mu_calculus::PropertyClass;
        let sig = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic"});
        let simple = |e: Value| serde_json::json!({"kind": "Simple", "expr": e});
        let imp = |op: &str, l: Value, r: Value| serde_json::json!({"kind": "Binary", "op": op, "left": l, "right": r});
        let sev = |e: Value| serde_json::json!({"kind": "Unary", "op": "SEventually", "expr": e});
        let unb = |min: u64| {
            serde_json::json!({"kind": "SequenceConcat",
            "elements": [{"sequence": simple(sig("b")), "min": min, "max": "$"}]})
        };
        let rel = serde_json::json!({"kind": "BinaryOp", "op": "Equality",
            "left": {"kind": "NamedValue", "symbol": "1 x", "type": "logic[3:0]"},
            "right": {"kind": "NamedValue", "symbol": "2 y", "type": "logic[3:0]"}});
        let ov = "OverlappedImplication";
        // (what, spec, Some((has_antecedent, delay)) if it must reduce, None for the recorded gap)
        type Row<'a> = (&'a str, Value, Option<(bool, u32)>);
        let rows: Vec<Row> = vec![
            ("s_eventually b", sev(simple(sig("b"))), Some((false, 0))),
            (
                "a |-> s_eventually b",
                imp(ov, simple(sig("a")), sev(simple(sig("b")))),
                Some((true, 0)),
            ),
            (
                "a |-> ##[0:$] b",
                imp(ov, simple(sig("a")), unb(0)),
                Some((true, 0)),
            ),
            (
                "a |=> s_eventually b",
                imp(
                    "NonOverlappedImplication",
                    simple(sig("a")),
                    sev(simple(sig("b"))),
                ),
                Some((true, 1)),
            ),
            (
                "a |-> ##[1:$] b",
                imp(ov, simple(sig("a")), unb(1)),
                Some((true, 1)),
            ),
            (
                "a |-> ##[3:$] b",
                imp(ov, simple(sig("a")), unb(3)),
                Some((true, 3)),
            ),
            (
                "(a && b) |-> s_eventually !c — compound request and grant",
                imp(
                    ov,
                    simple(serde_json::json!({"kind": "BinaryOp", "op": "LogicalAnd",
                        "left": sig("a"), "right": sig("b")})),
                    sev(simple(
                        serde_json::json!({"kind": "UnaryOp", "op": "LogicalNot",
                        "operand": sig("c")}),
                    )),
                ),
                Some((true, 0)),
            ),
            // The recorded gap: the monitor compiles register comparisons, not `reg == reg`.
            (
                "a |-> s_eventually (x == y)",
                imp(ov, simple(sig("a")), sev(simple(rel))),
                None,
            ),
        ];
        for (what, spec, want) in rows {
            let f = translate_one(
                &spec,
                SvaKind::Assert,
                &TranslateOptions::default(),
                &mut Vec::new(),
            )
            .unwrap_or_else(|e| panic!("{what}: {e}"));
            let parsed = crate::mu_calculus::parser::parse(&f).expect("parses");
            assert_eq!(
                parsed.property_class(),
                PropertyClass::Liveness,
                "{what}: {f}"
            );
            let got = reduce_response_shape(&parsed).map(|r| (r.ante.is_some(), r.delay));
            eprintln!("[liveness contract] {what} => {f} => {got:?}");
            assert_eq!(got, want, "{what}: the rescue's view of `{f}` changed");
        }
    }

    /// The caveat collectors: an unmarked `##[m:$]` is recorded as a strong reading, an explicit
    /// `strong(…)` and `s_eventually` are not, and a `cover` of an eventuality is refused.
    #[test]
    fn unbounded_sequences_record_their_strong_reading_and_cover_refuses_them() {
        let sig = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic"});
        let simple = |e: Value| serde_json::json!({"kind": "Simple", "expr": e});
        let unb = serde_json::json!({"kind": "SequenceConcat",
            "elements": [{"sequence": simple(sig("b")), "min": 1, "max": "$"}]});
        let plain = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
            "left": simple(sig("a")), "right": unb.clone()});
        assert_eq!(
            collect_implicit_strong_readings(&plain),
            vec!["##[1:$]".to_string()]
        );
        let strong = serde_json::json!({"kind": "Binary", "op": "OverlappedImplication",
            "left": simple(sig("a")), "right": {"kind": "StrongWeak", "strength": "strong", "expr": unb}});
        assert!(collect_implicit_strong_readings(&strong).is_empty());
        let sev =
            serde_json::json!({"kind": "Unary", "op": "SEventually", "expr": simple(sig("b"))});
        assert!(collect_implicit_strong_readings(&sev).is_empty());
        let err = translate_one(
            &sev,
            SvaKind::Cover,
            &TranslateOptions::default(),
            &mut Vec::new(),
        )
        .expect_err("a cover of an eventuality is refused");
        assert!(err.contains("cover"), "{err}");
    }

    /// REPRODUCER (2026-10-15) — a comparison slang evaluates SIGNED was emitted as an unsigned atom.
    /// JSON captured from slang 11.0.448 for `logic signed [3:0] x; logic [3:0] u;`. On the parent
    /// commit `x >= 0` translated to `(x >= 0)`, which the engines decide unsigned — a false HOLDS on
    /// an always-negative `x` (`e2e_signed_relational_comparison_is_not_decided_unsigned`).
    #[test]
    fn signed_comparisons_mint_a_signed_comparison_never_an_unsigned_atom() {
        let conv = |ty: &str, operand: Value| serde_json::json!({"kind": "Conversion", "type": ty, "operand": operand});
        let x =
            serde_json::json!({"kind": "NamedValue", "type": "logic signed[3:0]", "symbol": "1 x"});
        let u = serde_json::json!({"kind": "NamedValue", "type": "logic[3:0]", "symbol": "2 u"});
        let int = |v: &str| serde_json::json!({"kind": "IntegerLiteral", "type": "int", "value": v, "constant": v});
        let cmp = |op: &str, l: Value, r: Value| serde_json::json!({"kind": "BinaryOp", "type": "logic", "op": op, "left": l, "right": r});
        // `x >= 0` and `x < 0` — both operands converted to `logic signed[31:0]`. The atom
        // lowering refuses (its `<` is unsigned); the expression compiler mints a SIGNED
        // comparison over the sign-extended operands, which reads x = 4'b1111 as -1.
        for (op, at_minus_one, at_three) in [("GreaterThanEqual", 0, 1), ("LessThan", 1, 0)] {
            let e = cmp(
                op,
                conv("logic signed[31:0]", x.clone()),
                conv("logic signed[31:0]", int("0")),
            );
            let atom = bool_expr(&e).expect("minted");
            let ir = minted(&atom);
            assert!(matches!(ir, ExprIr::Cmp { signed: true, .. }), "{ir:?}");
            assert_eq!(ir.eval(&|_| 0b1111), at_minus_one, "{op} at x = -1");
            assert_eq!(ir.eval(&|_| 3), at_three, "{op} at x = 3");
        }
        // Signed equality: in range decides as the bit pattern; out of range mints a comparison
        // of the sign-extended value, which is never 15 (the old refusal is now exact).
        let eq3 = cmp(
            "Equality",
            conv("logic signed[31:0]", x.clone()),
            conv("logic signed[31:0]", int("3")),
        );
        // The sign extension makes it a minted value against the literal (exact either way here,
        // since 3 is non-negative, but the conversion is applied, not assumed away).
        let atom = bool_expr(&eq3).unwrap();
        assert!(
            atom.starts_with("(mununu_x_") && atom.ends_with(" == 3)"),
            "{atom}"
        );
        let eq15 = cmp(
            "Equality",
            conv("logic signed[31:0]", x.clone()),
            conv("logic signed[31:0]", int("15")),
        );
        let atom = bool_expr(&eq15).expect("minted");
        let name = atom
            .trim_start_matches('(')
            .split(' ')
            .next()
            .expect("an atom name");
        assert!(atom.ends_with(" == 15)"), "{atom}");
        let value = minted(name);
        assert!(
            (0..16).all(|v| value.eval(&|_| v) != 15),
            "the sign-extended x is never 15: {value:?}"
        );
        // Mixed signed/unsigned: slang converts to UNSIGNED, so it translates.
        let xu = cmp("LessThan", conv("logic[3:0]", x.clone()), u.clone());
        assert_eq!(bool_expr(&xu).unwrap(), "(x < u)");
        // Plain unsigned comparison unaffected.
        let u13 = cmp(
            "GreaterThan",
            conv("logic[31:0]", u),
            conv("logic[31:0]", int("13")),
        );
        assert_eq!(bool_expr(&u13).unwrap(), "(u > 13)");
    }

    /// A sign extension slang put on a comparison operand must be APPLIED, not stripped.
    /// `r == 8'(s)` with `logic [7:0] r` and `logic signed [3:0] s` (slang 11.0.448:
    /// `conv<logic[7:0]>(conv<logic signed[7:0]>(s))`) used to lower to the atom `(r == s)`, which
    /// compares the raw 4-bit `s`: with RTL `r = {4'b0, s}` the assertion is false for a negative
    /// `s`, yet the atom holds — a false HOLDS.
    #[test]
    fn a_sign_extended_comparison_operand_is_not_compared_unconverted() {
        let e = serde_json::json!({"kind": "BinaryOp", "op": "Equality", "type": "logic",
            "left": {"kind": "NamedValue", "symbol": "1 r", "type": "logic[7:0]"},
            "right": {"kind": "Conversion", "type": "logic[7:0]",
                "operand": {"kind": "Conversion", "type": "logic signed[7:0]",
                    "operand": {"kind": "NamedValue", "symbol": "2 s", "type": "logic signed[3:0]"}}}});
        let atom = bool_expr(&e).expect("translates");
        assert_ne!(atom, "(r == s)", "the sign extension was dropped");
        let ir = minted(&atom);
        let at = |r: u128, s: u128| ir.eval(&move |n| if n == "r" { r } else { s });
        assert_eq!(at(0xFF, 0xF), 1, "8'(-1) is 8'hFF");
        assert_eq!(
            at(0x0F, 0xF),
            0,
            "{{4'b0, s}} is not 8'(s) for a negative s"
        );
        assert_eq!(at(0x05, 0x5), 1);
    }

    /// mununu#705 — a kept `disable iff (D)` guards EVERY cycle of the attempt, not only its
    /// first: `disable iff (d) a |=> c` must excuse the consequent when `d` holds at ITS cycle.
    /// The former lowering `(d || (!(a) || [] c))` demanded `c` even when `d` rose mid-attempt.
    #[test]
    fn a_kept_disable_guards_every_cycle_of_the_attempt() {
        let sig = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic"});
        // `d` is not a recognisable single reset (a plain input), so the guard is kept.
        let spec = serde_json::json!({"kind": "DisableIff", "condition": sig("d"),
            "expr": {"kind": "Binary", "op": "NonOverlappedImplication",
                     "left": {"kind": "Simple", "expr": sig("a")},
                     "right": {"kind": "Simple", "expr": sig("c")}}});
        let opts = TranslateOptions { gate_reset: false };
        let body = property_body(&spec, &opts, &mut Vec::new()).unwrap();
        crate::mu_calculus::parser::parse(&body).unwrap();
        assert!(
            body.contains("[] (d || c)"),
            "the consequent is excused by `d`: {body}"
        );
    }

    /// mununu#708 — a kept `disable iff (D)` also guards the attempt's FIRST cycle when nothing is evaluated
    /// there: `disable iff (d) ##1 a |-> c` has no boolean block at its start, so guarding blocks
    /// alone never looks at `d` there and an attempt disabled at its start still had to complete
    /// (a spurious VIOLATED on OpenTitan's `prim_esc_rxtx` `EscRespCheck_A`).
    #[test]
    fn a_kept_disable_also_guards_the_attempt_start() {
        let sig = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic"});
        let spec = serde_json::json!({"kind": "DisableIff", "condition": sig("d"),
            "expr": {"kind": "Binary", "op": "OverlappedImplication",
                     "left": {"kind": "SequenceConcat", "elements": [
                         {"sequence": {"kind": "Simple", "expr": sig("a")}, "min": 1, "max": 1}]},
                     "right": {"kind": "Simple", "expr": sig("c")}}});
        let opts = TranslateOptions { gate_reset: false };
        let body = property_body(&spec, &opts, &mut Vec::new()).unwrap();
        let f = crate::mu_calculus::parser::parse(&body).unwrap();
        // The root is `(d || …)`: the attempt is excused when `d` holds where it starts.
        let crate::mu_calculus::Node::Or(l, _) = f.node(f.root()) else {
            panic!("the root is not a disjunction: {body}");
        };
        assert_eq!(
            super::super::assume_guarantee::print(&f, *l).unwrap(),
            "d",
            "the root disjunct is the disable: {body}"
        );
    }

    /// mununu#709 — assertions in sub-instances and hierarchical references, from real slang JSON
    /// (`testdata/sva_hierarchy_forms.{sv,ast.json}`): every instance of a module gets its own
    /// assertions, reading ITS signals by their flattened names and ITS parameter values.
    #[test]
    fn hierarchy_forms_translate_from_real_slang_json() {
        let report = translate_ast_json(include_str!("testdata/sva_hierarchy_forms.ast.json"))
            .expect("translates");
        let formula = |name: &str| {
            report
                .translated
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` not translated: {:?}", report.unsupported))
                .formula
                .clone()
        };
        let refused = |name: &str| {
            report
                .unsupported
                .iter()
                .find(|u| u.name == name)
                .unwrap_or_else(|| panic!("`{name}` not refused"))
                .reason
                .clone()
        };
        // `u_a` and `u_b` share one serialised body; each gets its assertions, over its signals.
        // `W` is the instance's (2), not the top's `W = 7` the former first-name-wins map gave.
        assert!(formula("u_a.leaf_sva_0").contains("(u_a.q_r != 2)"));
        assert!(formula("u_b.leaf_sva_2").contains("(u_b.q_r != 2)"));
        // A generate block extends the path; the branch not elaborated contributes nothing.
        assert!(formula("u_a.g_two_sva_1").contains("u_a.g_two.t == u_a.q_r__bits0_0"));
        // `u_d` overrides `W` to 3 and so elaborates the other branch.
        assert!(formula("u_d.leaf_sva_4").contains("(u_d.q_r != 3)"));
        assert!(formula("u_d.g_other_sva_5").contains("(u_d.q_r != 0)"));
        assert!(
            !report
                .translated
                .iter()
                .any(|t| t.name.starts_with("u_d.g_two"))
        );
        // The top's own `q_r` keeps its name — and no sub-instance assertion reads it.
        assert!(formula("top_sva_12").contains("(q_r == 0)"));
        // Hierarchical references: one instance resolves, a shared body is ambiguous.
        assert!(formula("top_sva_10").contains("(u_d.q_r != 7)"));
        assert!(refused("top_sva_11").contains("instantiated 4 times"));
        // A `for`-generate iteration's names carry brackets the atom grammar cannot spell.
        assert!(refused("g_arr[0].u_c.leaf_sva_6").contains("`for`-generate"));
        assert_eq!(report.translated.len() + report.unsupported.len(), 13);
    }

    /// A literal operand is read AFTER slang's conversions. `x4 != 4'(20)` — a size cast slang
    /// folds to `4'd4` on the `Conversion` — used to read the inner literal `20`: `(x4 != 20)` is
    /// always true for a 4-bit `x4`, so the assertion held even where `x4` is 4.
    #[test]
    fn a_narrowing_cast_on_a_literal_operand_is_applied() {
        let e = serde_json::json!({"kind": "BinaryOp", "op": "Inequality", "type": "logic",
            "left": {"kind": "NamedValue", "symbol": "1 x4", "type": "logic[3:0]"},
            "right": {"kind": "Conversion", "type": "logic[3:0]", "constant": "4'd4",
                "operand": {"kind": "IntegerLiteral", "type": "int", "value": "20", "constant": "20"}}});
        assert_eq!(bool_expr(&e).unwrap(), "(x4 != 4)");
    }

    /// The shipped fragment as an EXECUTABLE table (SVA-extraction-gaps plan, step 0, 2026-10-14):
    /// one row per form the module doc at the top of this file lists as supported or rejected, so
    /// that table cannot drift from the code without a red test. Property-level rows go through
    /// `property_body` (the entry `translate_one` uses); atom-level rows through `bool_expr`.
    ///
    /// `Translates(needle)`: the lowering contains `needle` and parses. `Rejected(needle)`: the
    /// reason contains `needle` — a rejection is a contract too (claims-integrity: named, never
    /// dropped).
    ///
    /// The JSON shapes are the ones slang 11.0.448 emits (`--ast-json`, captured 2026-10-14 in the
    /// `mununu-sva` image from `testdata/sva_fragment_forms.sv`): `s_eventually` / `eventually` /
    /// `nexttime` / `always` are `Unary{op}` (a ranged one carries `min`/`max`), `strong(…)` /
    /// `weak(…)` are `StrongWeak{strength}`, `throughout` is `Binary{Throughout}`, an unbounded `$`
    /// is the STRING `"$"` in `max`, goto / non-consecutive repetition are `GoTo` / `Nonconsecutive`.
    #[test]
    fn the_shipped_fragment_truth_table() {
        enum Want {
            Translates(&'static str),
            Rejected(&'static str),
        }
        use Want::*;
        let sig = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic"});
        let vec8 = |n: &str| serde_json::json!({"kind": "NamedValue", "symbol": format!("1 {n}"), "type": "logic[7:0]"});
        let lit = |v: &str| serde_json::json!({"kind": "IntegerLiteral", "type": "int", "value": v, "constant": v});
        let simple = |e: Value| serde_json::json!({"kind": "Simple", "expr": e});
        let rep = |e: Value, kind: &str, min: Value, max: Value| serde_json::json!({"kind": "Simple", "expr": e, "repetition": {"kind": kind, "min": min, "max": max}});
        let concat = |els: Vec<(Value, Value, Value)>| {
            serde_json::json!({"kind": "SequenceConcat", "elements": els.into_iter()
                .map(|(seq, min, max)| serde_json::json!({"sequence": seq, "min": min, "max": max}))
                .collect::<Vec<_>>()})
        };
        let imp = |op: &str, l: Value, r: Value| serde_json::json!({"kind": "Binary", "op": op, "left": l, "right": r});
        let unary = |op: &str, e: Value| serde_json::json!({"kind": "Unary", "op": op, "expr": e});
        let call = |sub: &str, args: Vec<Value>| serde_json::json!({"kind": "Call", "subroutine": sub, "arguments": args});
        let cmp = |op: &str, l: Value, r: Value| serde_json::json!({"kind": "BinaryOp", "op": op, "left": l, "right": r});
        let n = |v: u64| serde_json::json!(v);
        let dollar = || serde_json::json!("$");
        let ov = "OverlappedImplication";
        let nov = "NonOverlappedImplication";

        // ---- property level: (what it is in SVA, the slang shape, the contract)
        let props: Vec<(&str, Value, Want)> = vec![
            ("assert property (b)", simple(sig("b")), Translates("b")),
            (
                "assert property (##1 b) — a bare sequence (holds iff it matches from each start)",
                concat(vec![(simple(sig("b")), n(1), n(1))]),
                Translates("[] b"),
            ),
            (
                "a |-> b",
                imp(ov, simple(sig("a")), simple(sig("b"))),
                Translates("(!(a) || b)"),
            ),
            (
                "a |=> b",
                imp(nov, simple(sig("a")), simple(sig("b"))),
                Translates("(!(a) || [] b)"),
            ),
            (
                "a |-> ##2 b (XL.4)",
                imp(
                    ov,
                    simple(sig("a")),
                    concat(vec![(simple(sig("b")), n(2), n(2))]),
                ),
                Translates("(!(a) || [] [] b)"),
            ),
            (
                "a |=> ##[1:3] b (XL.4)",
                imp(
                    nov,
                    simple(sig("a")),
                    concat(vec![(simple(sig("b")), n(1), n(3))]),
                ),
                Translates("[] [] (b || [] (b || [] b))"),
            ),
            (
                "a |-> b[*3] (XL.5)",
                imp(
                    ov,
                    simple(sig("a")),
                    rep(sig("b"), "Consecutive", n(3), n(3)),
                ),
                Translates("(b && [] (b && [] b))"),
            ),
            (
                "a |-> b ##1 c ##2 d (XL.5)",
                imp(
                    ov,
                    simple(sig("a")),
                    concat(vec![
                        (simple(sig("b")), n(0), n(0)),
                        (simple(sig("c")), n(1), n(1)),
                        (simple(sig("d")), n(2), n(2)),
                    ]),
                ),
                Translates("(b && [] (c && [] [] d))"),
            ),
            (
                "(a ##1 b) |-> c (XL.5 fixed-delay antecedent)",
                imp(
                    ov,
                    concat(vec![
                        (simple(sig("a")), n(0), n(0)),
                        (simple(sig("b")), n(1), n(1)),
                    ]),
                    simple(sig("c")),
                ),
                Translates("(!(a) || [] (!(b) || c))"),
            ),
            (
                "a[*3] |=> c (XL.5 repetition antecedent)",
                imp(
                    nov,
                    rep(sig("a"), "Consecutive", n(3), n(3)),
                    simple(sig("c")),
                ),
                Translates("(!(a) || [] (!(a) || [] (!(a) || [] c)))"),
            ),
            // -- rejected, each with its named reason
            (
                "(a ##[1:2] b) |-> c — ranged antecedent, c at every match end (2026-10-15)",
                imp(
                    ov,
                    concat(vec![
                        (simple(sig("a")), n(0), n(0)),
                        (simple(sig("b")), n(1), n(2)),
                    ]),
                    simple(sig("c")),
                ),
                Translates("(!(a) || [] ((!(b) || c) && [] (!(b) || c)))"),
            ),
            (
                "a[*1:2] |-> c — ranged repetition antecedent (2026-10-15)",
                imp(
                    ov,
                    rep(sig("a"), "Consecutive", n(1), n(2)),
                    simple(sig("c")),
                ),
                Translates("(!(a) || (c && [] (!(a) || c)))"),
            ),
            (
                "(a ##[1:$] b) |-> c — unbounded antecedent",
                imp(
                    ov,
                    concat(vec![
                        (simple(sig("a")), n(0), n(0)),
                        (simple(sig("b")), n(1), dollar()),
                    ]),
                    simple(sig("c")),
                ),
                Rejected("unboundedly many match ends"),
            ),
            (
                "a |-> ##[1:$] b — unbounded delay, read strongly (2026-10-15)",
                imp(
                    ov,
                    simple(sig("a")),
                    concat(vec![(simple(sig("b")), n(1), dollar())]),
                ),
                Translates("(!(a) || [] (mu Y. (b || [] Y)))"),
            ),
            (
                "a |-> ##[1:$] b ##1 c — unbounded gap mid-chain",
                imp(
                    ov,
                    simple(sig("a")),
                    concat(vec![
                        (simple(sig("b")), n(1), dollar()),
                        (simple(sig("c")), n(1), n(1)),
                    ]),
                ),
                Rejected("Tier-3b"),
            ),
            (
                "a |-> b[*2:$] — unbounded repetition as the whole consequent ≡ b[*2] (2026-10-15)",
                imp(
                    ov,
                    simple(sig("a")),
                    rep(sig("b"), "Consecutive", n(2), dollar()),
                ),
                Translates("(!(a) || (b && [] b))"),
            ),
            (
                "a |-> weak(##[1:$] b) — satisfied by every trace",
                imp(
                    ov,
                    simple(sig("a")),
                    serde_json::json!({"kind": "StrongWeak", "strength": "weak",
                "expr": concat(vec![(simple(sig("b")), n(1), dollar())])}),
                ),
                Rejected("every infinite trace"),
            ),
            (
                "a |-> s_eventually (b ##1 c) — eventuality of a sequence",
                imp(
                    ov,
                    simple(sig("a")),
                    unary(
                        "SEventually",
                        concat(vec![
                            (simple(sig("b")), n(0), n(0)),
                            (simple(sig("c")), n(1), n(1)),
                        ]),
                    ),
                ),
                Rejected("must be a boolean"),
            ),
            (
                "a |-> b[*1:3] — range repetition",
                imp(
                    ov,
                    simple(sig("a")),
                    rep(sig("b"), "Consecutive", n(1), n(3)),
                ),
                Rejected("Tier-3b"),
            ),
            (
                "a |-> b[->2] — goto repetition",
                imp(ov, simple(sig("a")), rep(sig("b"), "GoTo", n(2), n(2))),
                Rejected("Tier-3b"),
            ),
            (
                "a |-> b[=2] — non-consecutive repetition",
                imp(
                    ov,
                    simple(sig("a")),
                    rep(sig("b"), "Nonconsecutive", n(2), n(2)),
                ),
                Rejected("Tier-3b"),
            ),
            (
                "s_eventually b — AG AF b (2026-10-15)",
                unary("SEventually", simple(sig("b"))),
                Translates("(mu Y. (b || [] Y))"),
            ),
            (
                "a |-> s_eventually b — AG(a → AF b), the l2s response shape (2026-10-15)",
                imp(ov, simple(sig("a")), unary("SEventually", simple(sig("b")))),
                Translates("(!(a) || (mu Y. (b || [] Y)))"),
            ),
            (
                "a |-> strong(##[1:$] b)",
                imp(
                    ov,
                    simple(sig("a")),
                    serde_json::json!({"kind": "StrongWeak", "strength": "strong",
                "expr": concat(vec![(simple(sig("b")), n(1), dollar())])}),
                ),
                Translates("(!(a) || [] (mu Y. (b || [] Y)))"),
            ),
            (
                "a |-> eventually [1:3] b",
                imp(
                    ov,
                    simple(sig("a")),
                    serde_json::json!({"kind": "Unary", "op": "Eventually",
                "expr": simple(sig("b")), "min": 1, "max": 3}),
                ),
                Translates("(!(a) || [] (b || [] (b || [] b)))"),
            ),
            (
                "a |-> nexttime b",
                imp(ov, simple(sig("a")), unary("NextTime", simple(sig("b")))),
                Translates("(!(a) || [] b)"),
            ),
            (
                "a |-> (c throughout (b ##1 b)) — c at every cycle of the match (2026-10-15)",
                imp(
                    ov,
                    simple(sig("a")),
                    serde_json::json!({"kind": "Binary", "op": "Throughout",
                "left": simple(sig("c")),
                "right": concat(vec![(simple(sig("b")), n(0), n(0)), (simple(sig("b")), n(1), n(1))])}),
                ),
                Translates("((c && b) && (c && [] (c && b)))"),
            ),
            (
                "always b",
                unary("Always", simple(sig("b"))),
                Translates("(nu W. (b && [] W))"),
            ),
        ];
        let mut failures: Vec<String> = Vec::new();
        for (what, spec, want) in &props {
            let got = property_body(spec, &TranslateOptions::default(), &mut Vec::new());
            failures.extend(check(what, got, want));
        }

        // ---- atom level
        let atoms: Vec<(&str, Value, Want)> = vec![
            (
                "$stable(x) (XL.3)",
                call("$stable", vec![sig("x")]),
                Translates("(x == x__past)"),
            ),
            (
                "$changed(x)",
                call("$changed", vec![sig("x")]),
                Translates("(x != x__past)"),
            ),
            (
                "$rose(v) (1-bit)",
                call("$rose", vec![sig("v")]),
                Translates("(v && (!(v__past)))"),
            ),
            (
                "$fell(v) (1-bit)",
                call("$fell", vec![sig("v")]),
                Translates("((!(v)) && v__past)"),
            ),
            (
                "$past(x, 2) — any depth",
                call("$past", vec![sig("x"), lit("2")]),
                Translates("x__past2"),
            ),
            (
                "cnt == $past(cnt) + 1 (H.G addend)",
                cmp(
                    "Equality",
                    vec8("cnt"),
                    cmp("Add", call("$past", vec![vec8("cnt")]), lit("1")),
                ),
                Translates("(cnt == cnt__past + 1)"),
            ),
            (
                "a >= b (relational, signal vs signal)",
                cmp("GreaterThanEqual", vec8("a"), vec8("b")),
                Translates("(a >= b)"),
            ),
            (
                "$onehot(gnt) (8-bit)",
                call("$onehot", vec![vec8("gnt")]),
                Translates("(gnt == 128)"),
            ),
            (
                "x inside {2, 4, [1:3]}",
                serde_json::json!({"kind": "Inside", "left": vec8("x"), "rangeList": [
                lit("2"), lit("4"), {"kind": "ValueRange", "rangeKind": "Simple", "left": lit("1"), "right": lit("3")}]}),
                Translates("((x >= 1) && (x <= 3))"),
            ),
            (
                "wdata[7:0] == 165 (mununu#565 slice)",
                cmp(
                    "Equality",
                    serde_json::json!({
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "Simple",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left": lit("7"), "right": lit("0")}),
                    lit("165"),
                ),
                Translates("(wdata__bits7_0 == 165)"),
            ),
            (
                "|x (XL.1c reduction-or)",
                serde_json::json!({"kind": "UnaryOp", "op": "BitwiseOr", "operand": vec8("x")}),
                Translates("(x != 0)"),
            ),
            (
                "vec[3] — constant bit-select, the 1-bit slice (2026-10-15)",
                serde_json::json!({"kind": "ElementSelect", "selector": lit("3"), "value": vec8("vec")}),
                Translates("vec__bits3_3"),
            ),
            (
                "$countones(v) <= 2 — the minted count (2026-10-15)",
                cmp(
                    "LessThanEqual",
                    call("$countones", vec![vec8("v")]),
                    lit("2"),
                ),
                Translates("(v__popcount <= 2)"),
            ),
            (
                "$isunknown(v) — folded to false in the 2-state model (2026-10-15)",
                call("$isunknown", vec![vec8("v")]),
                Translates("false"),
            ),
            (
                "$rose(v[3]) — an edge of one bit",
                call(
                    "$rose",
                    vec![
                        serde_json::json!({"kind": "ElementSelect", "selector": lit("3"), "value": vec8("v")}),
                    ],
                ),
                Translates("(v__bits3_3 && (!(v__past__bits3_3)))"),
            ),
            // OpenTitan coverage, Phase 1 (2026-10-15) — shapes from slang 11.0.448.
            (
                "md[1] — element of a multi-dimensional packed array (a W-bit slice)",
                serde_json::json!({"kind": "ElementSelect", "selector": lit("1"),
                "value": {"kind": "NamedValue", "symbol": "1 md", "type": "logic[3:0][7:0]"}}),
                Translates("(md__bits15_8 != 0)"),
            ),
            (
                "cnt_q[1] == 3 — element compared",
                cmp(
                    "Equality",
                    serde_json::json!({"kind": "ElementSelect", "type": "logic[3:0]", "selector": lit("1"),
                    "value": {"kind": "NamedValue", "symbol": "1 cnt_q", "type": "logic[1:0][3:0]"}}),
                    lit("3"),
                ),
                Translates("(cnt_q__bits7_4 == 3)"),
            ),
            (
                "e.esc_p — packed-struct member (first member = most significant)",
                serde_json::json!({"kind": "MemberAccess", "type": "logic", "member": "1 esc_p",
                "value": {"kind": "NamedValue", "symbol": "1 e",
                          "type": "struct packed{logic esc_p;logic esc_n;}p::s$1"}}),
                Translates("e__bits1_1"),
            ),
            (
                "w.mid == 1 — a multi-bit member",
                cmp(
                    "Equality",
                    serde_json::json!({"kind": "MemberAccess", "type": "logic[1:0]", "member": "1 mid",
                    "value": {"kind": "NamedValue", "symbol": "1 w",
                              "type": "struct packed{logic[3:0] hi;logic[1:0] mid;logic lo;}p::s$2"}}),
                    lit("1"),
                ),
                Translates("(w__bits2_1 == 1)"),
            ),
            (
                "a -> b — boolean implication",
                cmp("LogicalImplication", sig("a"), sig("b")),
                Translates("((!(a)) || b)"),
            ),
            (
                "a <-> b — boolean equivalence",
                cmp("LogicalEquivalence", sig("a"), sig("b")),
                Translates("((a && b) || ((!(a)) && (!(b))))"),
            ),
            (
                "p ^ n — XOR of 1-bit operands",
                serde_json::json!({"kind": "BinaryOp", "op": "BinaryXor", "type": "logic",
                                   "left": sig("p"), "right": sig("n")}),
                Translates("((p && (!(n))) || ((!(p)) && n))"),
            ),
            (
                "a & b — AND of 1-bit operands",
                serde_json::json!({"kind": "BinaryOp", "op": "BinaryAnd", "type": "logic",
                                   "left": sig("a"), "right": sig("b")}),
                Translates("(a && b)"),
            ),
            (
                "x == {8{1'b1}} — a constant folded by slang (replication)",
                cmp(
                    "Equality",
                    vec8("x"),
                    serde_json::json!({"kind": "Conversion", "type": "logic[7:0]", "constant": "8'b11111111",
                    "operand": {"kind": "Replication", "type": "bit[7:0]", "constant": "8'b11111111",
                                "count": lit("8"), "concat": {"kind": "Concatenation"}}}),
                ),
                Translates("(x == 255)"),
            ),
            (
                "x & y — bitwise AND of vectors in boolean position: a minted value, `!= 0`",
                serde_json::json!({"kind": "BinaryOp", "op": "BinaryAnd", "type": "logic[7:0]",
                                   "left": vec8("x"), "right": vec8("y")}),
                Translates("!= 0)"),
            ),
            // -- rejected
            (
                "off[3] — index outside the declared range [15:8]",
                serde_json::json!({"kind": "ElementSelect", "selector": lit("3"),
                "value": {"kind": "NamedValue", "symbol": "1 off", "type": "logic[15:8]"}}),
                Rejected("outside"),
            ),
            (
                "$rose(v[7:4]) — an edge of a multi-bit slice (its LSB)",
                serde_json::json!({"kind": "Call", "subroutine": "$rose", "type": "bit",
                    "arguments": [{"kind": "RangeSelect", "selectionKind": "Simple",
                    "type": "logic[3:0]", "value": vec8("v"), "left": lit("7"), "right": lit("4")}]}),
                // The expression compiler (Phase 2): IEEE 1800 16.9.3 — the LSB's edge.
                Translates("mununu_x_"),
            ),
            (
                "req[idx] — dynamic bit-select, the 3-bit index always in range (minted)",
                serde_json::json!({"kind": "ElementSelect", "type": "logic",
                "selector": {"kind": "NamedValue", "symbol": "1 idx", "type": "logic[2:0]"}, "value": vec8("req")}),
                Translates("mununu_x_"),
            ),
            (
                "req[idx] — a 4-bit index can leave [7:0], where SV reads X",
                serde_json::json!({"kind": "ElementSelect", "type": "logic",
                "selector": {"kind": "NamedValue", "symbol": "1 idx", "type": "logic[3:0]"}, "value": vec8("req")}),
                Rejected("outside"),
            ),
            (
                "mem[0] — unpacked array content (mununu#514)",
                serde_json::json!({"kind": "ElementSelect", "selector": lit("0"),
                "value": {"kind": "NamedValue", "symbol": "1 mem", "type": "logic[14:0]$[0:319]"}}),
                Rejected("unpacked array"),
            ),
            (
                "wdata[7 -: 8] — indexed part-select (mununu#565)",
                cmp(
                    "Equality",
                    serde_json::json!({
                "kind": "RangeSelect", "type": "logic[7:0]", "selectionKind": "IndexedDown",
                "value": {"kind": "NamedValue", "type": "logic[31:0]", "symbol": "1 wdata"},
                "left": lit("7"), "right": lit("8")}),
                    lit("165"),
                ),
                Rejected("IndexedDown"),
            ),
            (
                "$countones over a 65-bit vector — past the minted-count cap",
                call(
                    "$countones",
                    vec![serde_json::json!({"kind": "NamedValue",
                    "symbol": "1 wide", "type": "logic[64:0]"})],
                ),
                Rejected("capped"),
            ),
            (
                "^x — parity (a minted reduction)",
                serde_json::json!({"kind": "UnaryOp", "op": "BitwiseXor", "type": "logic", "operand": vec8("x")}),
                Translates("mununu_x_"),
            ),
            (
                "$rose(bus) on a vector (its LSB, minted)",
                serde_json::json!({"kind": "Call", "subroutine": "$rose", "type": "bit",
                    "arguments": [vec8("bus")]}),
                Translates("mununu_x_"),
            ),
            (
                "$past(x, 0)",
                call("$past", vec![sig("x"), lit("0")]),
                Rejected(">= 1"),
            ),
            (
                "cnt == cnt * 2 — an 8-bit multiplication (minted)",
                serde_json::json!({"kind": "BinaryOp", "op": "Equality", "type": "logic",
                    "left": vec8("cnt"),
                    "right": {"kind": "BinaryOp", "op": "Multiply", "type": "logic[7:0]",
                        "left": vec8("cnt"),
                        "right": {"kind": "Conversion", "type": "logic[7:0]", "constant": "8'd2",
                                  "operand": lit("2")}}}),
                Translates("mununu_x_"),
            ),
            (
                "w * w over 64 bits — past the minted multiplication cap",
                serde_json::json!({"kind": "BinaryOp", "op": "Inequality", "type": "logic",
                    "left": {"kind": "BinaryOp", "op": "Multiply", "type": "logic[63:0]",
                        "left": {"kind": "NamedValue", "symbol": "1 w", "type": "logic[63:0]"},
                        "right": {"kind": "NamedValue", "symbol": "1 w", "type": "logic[63:0]"}},
                    "right": {"kind": "Conversion", "type": "logic[63:0]", "constant": "64'd0",
                              "operand": lit("0")}}),
                Rejected("multiplication"),
            ),
        ];
        for (what, expr, want) in &atoms {
            failures.extend(check(what, bool_expr(expr), want));
        }

        fn check(what: &str, got: Result<String, String>, want: &Want) -> Option<String> {
            match (want, got) {
                (Translates(needle), Ok(f)) => {
                    if !f.contains(needle) {
                        return Some(format!(
                            "{what}: expected `{needle}` in the lowering, got `{f}`"
                        ));
                    }
                    crate::mu_calculus::parser::parse(&f)
                        .err()
                        .map(|e| format!("{what}: lowering `{f}` does not parse: {e}"))
                }
                (Translates(needle), Err(e)) => Some(format!(
                    "{what}: expected a lowering containing `{needle}`, got rejection: {e}"
                )),
                (Rejected(needle), Err(e)) => (!e.contains(needle))
                    .then(|| format!("{what}: expected the reason to name `{needle}`, got: {e}")),
                (Rejected(_), Ok(f)) => Some(format!(
                    "{what}: expected a rejection, but it translated to `{f}` — update the \
                     module-doc table"
                )),
            }
        }
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }
}
