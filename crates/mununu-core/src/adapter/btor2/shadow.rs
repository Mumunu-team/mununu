//! XL.3b — BTOR2 `__past` shadow shift-register synthesis (Tier-2 SVA history).
//!
//! The slang translator (XL.3a) lowers `$past`/`$stable`/`$changed`/`$rose`/
//! `$fell` to atoms over shadow signals `<base>__past`, `<base>__past2`, … and
//! reports the needed base signals — with the deepest history depth each — in
//! [`crate::adapter::slang::translate::TranslationReport::required_shadows`].
//! This module is the model half of that contract: it augments the BTOR2 design
//! with a `depth`-stage shift register per base so those atoms bind.
//!
//! For each base `b` at depth `k`, resolving to a state cell **or a primary
//! input** of sort `S`, the augmentation appends a `k`-stage chain (with fresh
//! NIDs above the current max):
//!
//! ```text
//! <s1> state <S> b__past                ; b, one cycle ago
//! <n1> next  <S> <s1> <b_nid>           ; next(b__past)  = b
//! <s2> state <S> b__past2               ; b, two cycles ago
//! <n2> next  <S> <s2> <s1>              ; next(b__past2) = b__past
//! …                                     ; up to b__past{k}
//! <i>  init  <S> <sj> <b_init_value>    ; each stage — see below
//! ```
//!
//! `next(b__pastⱼ) = b__past(ⱼ₋₁)` (stage 1 samples `b`) makes `b__pastⱼ` hold
//! `b`'s value `j` cycles ago at every cycle ≥ `j`. Each stage's `init` mirrors
//! `b`'s own reset value, so before the chain has filled (cycles `< j` for stage
//! `j`) `$past(b, j)` reads that reset value — the standard "no history before
//! the first clock edges" convention, generalised from 1 cycle to `k`. Depth 1
//! keeps the historical bare `b__past` name (backward compatible).
//!
//! A **primary input** base has no reset value to mirror, so every stage is
//! pinned to an explicit zero (see the SOUNDNESS note in
//! [`augment_with_past_shadows`] for why zero, not free). It matters in practice
//! — `$past` of an input is what every data-integrity property is written over:
//!
//! ```systemverilog
//! (push && !pop && cnt_q == 0) |=> (d0_q == $past(din))
//! ```
//!
//! Restricting the base to state cells made that whole class unverifiable: the
//! translator declared `required __past shadow registers: din(8)`, nothing built
//! `din__past`, and every engine then correctly reported
//! `predicate references unknown register/signal`. The restriction was
//! conservative rather than semantic — `next` sourced from an `input` node is
//! well-formed BTOR2 and the engines consume it unchanged.
//! This is **exact** (a real added flop, not an abstraction): the augmented
//! design *is* the concrete system the verdict transfers to.
//!
//! This works at the BTOR2 **text** level (append lines) so it composes with
//! [`crate::adapter::btor2::kmts_lift::predicate_cube_lift`] /
//! [`crate::adapter::btor2::bit_blast`], both of which re-parse the text.

use crate::adapter::btor2::ast::{Btor2File, Nid, Node, Operand};
use crate::adapter::btor2::parser;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// The stage-`stage` shadow name for a base — the XL.3 naming contract shared
/// with the translator's `slang::translate::past_shadow_name`. Stage 1 keeps the
/// historical bare `<base>__past` (backward compatible); stage `k ≥ 2` is
/// `<base>__past{k}`. The two functions MUST agree — they are the two halves of
/// one contract (`$past(x, k)` lowers to `x__pastk`, which this chain provides).
fn shadow_stage_name(base: &str, stage: u32) -> String {
    if stage <= 1 {
        format!("{base}__past")
    } else {
        format!("{base}__past{stage}")
    }
}

/// Augment BTOR2 `content` with a `depth`-stage `<base>__past` shadow shift chain
/// for each `(base, depth)` in `bases`. Returns the augmented BTOR2 text.
///
/// - A base that already has a stage-1 `<base>__past` state is skipped
///   (idempotent, so re-augmenting the same base is a no-op); this assumes the
///   prior augmentation used a depth ≥ this one, which the caller guarantees by
///   passing each base once with its deepest required depth.
/// - A base resolves to a state cell or a primary input. Every stage mirrors the
///   state cell's `init`; an input has none to mirror, so every stage is pinned
///   to zero (SVA leaves `$past` undefined before the first `k` clock edges).
/// - A base that resolves to neither is an error (the `$past`/`$stable` atom
///   would never bind) — never silently dropped.
/// - `bases` is deduplicated by name internally; order of appended chains follows
///   first appearance.
pub fn augment_with_past_shadows(
    content: &str,
    bases: &[(&str, u32)],
) -> Result<String, AdapterError> {
    if bases.is_empty() {
        return Ok(content.to_string());
    }

    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/shadow: {}", e.message);
        e
    })?;
    let symbols = parser::collect_symbols(&file);
    let existing_state_symbols: std::collections::HashSet<&str> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::State { .. }))
        .filter_map(|l| symbols.get(&l.nid).map(String::as_str))
        .collect();

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for &(base, depth) in bases {
        if !seen.insert(base) {
            continue; // dedup repeated bases
        }
        let depth = depth.max(1);
        let stage1_symbol = shadow_stage_name(base, 1);
        if existing_state_symbols.contains(stage1_symbol.as_str()) {
            continue; // already augmented — idempotent
        }

        // A base is either a state cell or a primary input. Both are legitimate
        // `$past` sources and both produce the same `next(shadow) = source` shift;
        // they differ only in where each shadow stage's cycle-0 value comes from.
        let source = resolve_shadow_source(&file, base).ok_or_else(|| AdapterError {
            kind: AdapterErrorKind::UnsupportedConstruct,
            message: format!(
                "adapter/btor2/shadow: base signal `{base}` resolves to neither a BTOR2 state \
                 cell nor a primary input, so its `{stage1_symbol}` shadow chain cannot be \
                 synthesised (the Tier-2 history atom would never bind). Check the SVA signal \
                 name matches a register or a module input."
            ),
            location: None,
        })?;
        let sort_nid = source.sort();

        // btormc's parser requires an `init` value's NID to be BELOW the state's,
        // so the zero an input shadow inits from is allocated FIRST — everything
        // appended here sits above every pre-existing NID, and allocating it after
        // the state would emit a model the external oracle rejects while mununu's
        // own order-agnostic parser read it happily. One zero is shared by every
        // stage. A state cell's init needs no such care: it reuses the source's
        // own value node, already far below.
        let mut zero_nid = None;
        if matches!(source, ShadowSource::PrimaryInput { .. }) {
            zero_nid = Some(next_nid);
            appended.push(format!("{next_nid} zero {sort_nid}"));
            next_nid += 1;
        }

        // The init value every stage mirrors: the state cell's own init (or none);
        // computed once, reused across all stages of the chain.
        let source_init = match source {
            ShadowSource::StateCell { nid, .. } => file.lines.iter().find_map(|l| match &l.node {
                Node::Init { state, value, .. } if *state == nid => Some(*value),
                _ => None,
            }),
            ShadowSource::PrimaryInput { .. } => None,
        };

        // Build the shift chain: stage 1 samples the source; stage j (j ≥ 2)
        // samples stage j-1, so stage j holds the source's value j cycles ago.
        let mut prev_value_nid = source.nid();
        for stage in 1..=depth {
            let symbol = shadow_stage_name(base, stage);
            let shadow_state = next_nid;
            next_nid += 1;
            let shadow_next = next_nid;
            next_nid += 1;
            appended.push(format!("{shadow_state} state {sort_nid} {symbol}"));
            // next(stageⱼ) = stage(ⱼ₋₁)'s current value (stage 1 = the source).
            appended.push(format!(
                "{shadow_next} next {sort_nid} {shadow_state} {prev_value_nid}"
            ));

            match source {
                // Mirror the state cell's init onto EVERY stage, so before the
                // chain fills (cycles < j for stage j) `$past(b, j)` reads b's
                // reset value — the "no history before the first clock edges"
                // convention, generalised from 1 cycle to k. A source with no
                // init leaves the stage init-less too, keeping the pair consistent
                // under whatever a given engine does with an init-less cell.
                ShadowSource::StateCell { .. } => {
                    if let Some(init) = source_init {
                        let shadow_init = next_nid;
                        next_nid += 1;
                        appended.push(format!(
                            "{shadow_init} init {sort_nid} {shadow_state} {}",
                            operand_text(init)
                        ));
                    }
                }

                // SOUNDNESS: an input has no init to mirror, so each stage's
                // pre-fill value is CHOSEN — pinned to zero rather than left free.
                //
                // Free would be the more faithful reading of SVA, where `$past` is
                // undefined before the first clock edges. It is not a choice this
                // adapter can make: an init-less state cell means different things
                // to different engines. The cube and exact engines default it to 0
                // (`state_cell_init_values` / `initial_state_bdd`, per the
                // `setundef -zero` power-up); the reachability portfolio leaves it
                // FREE, per BTOR2's nondeterministic-init semantics. That is
                // exactly the verdict DISAGREEMENT `reset_init::inject_zero_init`
                // exists to close — and it cannot close this one, because it runs
                // on the pre-augmentation BTOR2, before this appends the shadow. A
                // free shadow would escape the mitigation by construction.
                //
                // What is given up is bounded, and observable in only one shape.
                // The lift puts a `|=>` consequent under a `[]`, so it is evaluated
                // only at states with a predecessor. For depth 1 that fully hides
                // the invented value; for depth k the chain takes k cycles to
                // fill, so a `$past(input, k)` under a single `[]` still reads the
                // invented zero for the first k-1 cycles after reset (an over-
                // approximation bounded to those cycles). A `|->` consequent reads
                // it at the initial state too. Every later cycle is exact — a real
                // added flop, not an abstraction. Pinning matches the power-up the
                // rest of the adapter already assumes, so every engine decides one
                // model.
                //
                // Full argument, per-engine ledger, and why no engine may assume
                // stutter equivalence here: docs/design/past-shadow-soundness.md
                ShadowSource::PrimaryInput { .. } => {
                    let zero = zero_nid.expect("an input source allocates its zero above");
                    let shadow_init = next_nid;
                    next_nid += 1;
                    appended.push(format!(
                        "{shadow_init} init {sort_nid} {shadow_state} {zero}"
                    ));
                }
            }

            prev_value_nid = shadow_state; // the next stage samples this stage
        }
    }

    if appended.is_empty() {
        return Ok(content.to_string());
    }
    Ok(format!("{}\n{}\n", content.trim_end(), appended.join("\n")))
}

/// Can `base` be a `$past` shadow source in this model?
///
/// mununu#565 — the minted symbol for `base[msb:lsb]`.
///
/// **One naming contract with the translator's `slice_atom_name`.** Both halves must agree or the
/// atom never binds — the same pairing `shadow_stage_name` has with `past_shadow_name`, and the
/// same reason it is a named function rather than an inline `format!`.
fn slice_signal_name(base: &str, msb: u32, lsb: u32) -> String {
    format!("{base}__bits{msb}_{lsb}")
}

/// The sort NID of a node that has one.
fn sort_nid_of(node: &Node) -> Option<Nid> {
    match node {
        Node::Input { sort, .. } | Node::State { sort, .. } | Node::Const { sort, .. } => {
            Some(*sort)
        }
        Node::Op { sort, .. } => Some(*sort),
        _ => None,
    }
}

/// mununu#565 — append a named `slice` node per `(base, msb, lsb)`, so an SVA bit-slice atom
/// (`wdata[7:0] == 8'hA5`) binds to a plain BTOR2 symbol.
///
/// **Why a derived signal rather than a slice-aware atom.** `[` already means a Box modality to
/// the mu-calculus lexer and an array index to `PredicateExpr`, so a bracketed atom would MISPARSE
/// rather than fail. Minting the signal here leaves every layer below the translator untouched.
///
/// A slice is combinational, so this is strictly simpler than the `__past` chain: one node, no
/// `state`/`next`/`init`, and no btormc NID-ordering constraint.
///
/// **Call AFTER [`augment_with_past_shadows`]** — `$past(sig[7:0])` slices the shadow, so
/// `sig__past` must already be a symbol.
///
/// Idempotent. A base that does not resolve, or a slice outside its base's width, is SKIPPED: the
/// property then declines to bind downstream with a named message, which beats aborting a whole
/// run over one assertion.
pub fn augment_with_slices(
    content: &str,
    slices: &[(&str, u32, u32)],
) -> Result<String, AdapterError> {
    if slices.is_empty() {
        return Ok(content.to_string());
    }
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/shadow: {}", e.message);
        e
    })?;
    // `collect_symbols` deliberately maps only Input/State nids to names (its pass 2 attaches an
    // Op's symbol to the STATE it aliases, behind a width filter, so a narrow alias cannot
    // false-positive onto a wide cell). A minted slice is an Op whose symbol names ITSELF, so it
    // is invisible there — which is why this scans the lines directly for the idempotency check.
    let symbols = parser::collect_symbols(&file);
    let mut by_symbol: std::collections::HashMap<&str, Nid> =
        symbols.iter().map(|(nid, s)| (s.as_str(), *nid)).collect();
    for l in &file.lines {
        if let Node::Op {
            symbol: Some(sym), ..
        } = &l.node
        {
            by_symbol.entry(sym.as_str()).or_insert(l.nid);
        }
    }

    let mut sort_of_width: std::collections::HashMap<u32, Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: crate::adapter::btor2::ast::Sort::BitVec { width },
            } => Some((*width, l.nid)),
            _ => None,
        })
        .collect();

    let mut next_nid: Nid = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut appended: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for &(base, msb, lsb) in slices {
        let symbol = slice_signal_name(base, msb, lsb);
        if !seen.insert(symbol.clone()) || by_symbol.contains_key(symbol.as_str()) {
            continue;
        }
        let Some(&base_nid) = by_symbol.get(base) else {
            continue;
        };
        let base_width = file
            .lines
            .iter()
            .find(|l| l.nid == base_nid)
            .and_then(|l| sort_nid_of(&l.node))
            .and_then(|sn| parser::bv_width(&file, sn))
            .unwrap_or(0);
        if base_width == 0 || msb >= base_width || lsb > msb {
            continue;
        }
        let width = msb - lsb + 1;
        let sort_nid = match sort_of_width.get(&width) {
            Some(&n) => n,
            None => {
                let n = next_nid;
                next_nid += 1;
                appended.push(format!("{n} sort bitvec {width}"));
                sort_of_width.insert(width, n);
                n
            }
        };
        appended.push(format!(
            "{next_nid} slice {sort_nid} {base_nid} {msb} {lsb} {symbol}"
        ));
        next_nid += 1;
    }

    if appended.is_empty() {
        return Ok(content.to_string());
    }
    let mut out = content.trim_end().to_string();
    out.push('\n');
    for line in appended {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

/// Exported so a caller that pre-filters bases uses the SAME rule as the
/// augmentation itself. Two independent answers to "what is a valid base" is
/// precisely how the input case stayed broken: the augmentation was taught to
/// accept an input while `verify_auto`'s filter still asked
/// `resolve_state_by_symbol`, so input bases were dropped before they ever
/// reached here and the fix was inert on the only path users take.
pub fn can_shadow(file: &Btor2File, base: &str) -> bool {
    resolve_shadow_source(file, base).is_some()
}

/// Resolve a `$past` base to its source line: `(nid, sort, is_input)`.
///
/// A state cell is tried first, matching the historical behaviour and keeping
/// the symbol-distance heuristic in `resolve_state_by_symbol` authoritative
/// when a name could mean either. A primary input is the fallback.
/// Where a `__past` shadow takes its value from, and hence how its cycle 0 is set.
#[derive(Clone, Copy)]
enum ShadowSource {
    /// A BTOR2 state cell — the shadow mirrors this cell's `init`, or its absence.
    StateCell { nid: Nid, sort: Nid },
    /// A primary input — there is no `init` to mirror, so the shadow's cycle-0
    /// value is chosen. See the SOUNDNESS note in `augment_with_past_shadows`.
    PrimaryInput { nid: Nid, sort: Nid },
}

impl ShadowSource {
    /// The NID the shadow's `next` reads from.
    fn nid(self) -> Nid {
        match self {
            Self::StateCell { nid, .. } | Self::PrimaryInput { nid, .. } => nid,
        }
    }

    /// The sort both the shadow flop and its `next` carry.
    fn sort(self) -> Nid {
        match self {
            Self::StateCell { sort, .. } | Self::PrimaryInput { sort, .. } => sort,
        }
    }
}

/// Resolve `base` to the signal a `{base}__past` shadow should sample.
///
/// Precedence is EXACT BEFORE FUZZY, and that ordering is the point.
/// `parser::resolve_state_by_symbol` also matches an `Op`/`Output` line named
/// `base` and walks its cone to the nearest state — a match at distance N. Asking
/// it first would let that distant state outrank an `input` line that carries the
/// name exactly, silently shadowing the wrong signal. So: exact state, then exact
/// input, then the cone walk.
///
/// Tier 1 requires a UNIQUE exact state match; two states sharing a symbol fall
/// through to the walk, which rejects the ambiguity itself (`tied` → `None`)
/// rather than picking one arbitrarily.
fn resolve_shadow_source(file: &Btor2File, base: &str) -> Option<ShadowSource> {
    let mut exact_states = file.lines.iter().filter_map(|l| match &l.node {
        Node::State {
            sort,
            symbol: Some(s),
            ..
        } if s == base => Some(ShadowSource::StateCell {
            nid: l.nid,
            sort: *sort,
        }),
        _ => None,
    });
    if let Some(only) = exact_states.next()
        && exact_states.next().is_none()
    {
        return Some(only);
    }

    let exact_input = file.lines.iter().find_map(|l| match &l.node {
        Node::Input {
            sort,
            symbol: Some(s),
        } if s == base => Some(ShadowSource::PrimaryInput {
            nid: l.nid,
            sort: *sort,
        }),
        _ => None,
    });
    if exact_input.is_some() {
        return exact_input;
    }

    let nid = parser::resolve_state_by_symbol(file, base)?;
    match file.lookup(nid).map(|l| &l.node) {
        Some(Node::State { sort, .. }) => Some(ShadowSource::StateCell { nid, sort: *sort }),
        _ => None,
    }
}

/// Render a BTOR2 operand back to text, preserving the `-N` (bit-not) shorthand.
fn operand_text(op: Operand) -> String {
    if op.is_negated() {
        format!("-{}", op.nid())
    } else {
        op.nid().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::ast::Btor2File;
    use std::collections::HashMap;

    // A 4-bit counter `s` that increments each cycle, with `s` reset to 0.
    //   1 sort bitvec 4
    //   2 state 1 s
    //   3 one 1
    //   4 add 1 2 3        ; s + 1
    //   5 init 1 2 ... (zero)
    //   6 next 1 2 4
    const COUNTER: &str = "\
1 sort bitvec 4
2 state 1 s
3 one 1
4 add 1 2 3
5 zero 1
6 init 1 2 5
7 next 1 2 4
";

    /// mununu#565 — an 8-bit register `w`, wide enough to slice.
    const WIDE: &str = "\
1 sort bitvec 8
2 state 1 w
3 one 1
4 add 1 2 3
5 zero 1
6 init 1 2 5
7 next 1 2 4
";

    /// mununu#565 — a named `slice` node is appended, with the right sort and bounds.
    #[test]
    fn slices_append_a_named_node_of_the_sliced_width() {
        let out = augment_with_slices(WIDE, &[("w", 3, 0)]).expect("augments");
        let file = parser::parse(&out).expect("augmented btor2 parses");
        // NB: `collect_symbols` maps only Input/State nids to names, so a minted Op symbol is
        // looked up on the lines directly. That asymmetry is exactly what mununu#565's e2e has to
        // prove harmless for BINDING — see the e2e in `verify_auto.rs`.
        let nid = file
            .lines
            .iter()
            .find(|l| matches!(&l.node, Node::Op { symbol: Some(sym), .. } if sym == "w__bits3_0"))
            .map(|l| l.nid)
            .expect("the minted symbol exists — the translator's atom names exactly this");
        // It is a 4-bit slice, not an 8-bit alias.
        let line = file.lines.iter().find(|l| l.nid == nid).expect("line");
        let sort = sort_nid_of(&line.node).expect("slice has a sort");
        assert_eq!(
            parser::bv_width(&file, sort),
            Some(4),
            "w[3:0] is 4 bits wide; a wrong sort would bind the atom to the wrong value"
        );
    }

    /// mununu#565 — idempotent, so a re-lift or a repeated base does not duplicate nodes.
    #[test]
    fn slices_are_idempotent_and_deduped() {
        let once = augment_with_slices(WIDE, &[("w", 3, 0)]).expect("augments");
        let twice = augment_with_slices(&once, &[("w", 3, 0)]).expect("augments again");
        assert_eq!(
            once, twice,
            "re-augmenting an already-sliced model must be a no-op"
        );

        let dup = augment_with_slices(WIDE, &[("w", 3, 0), ("w", 3, 0)]).expect("augments");
        assert_eq!(dup, once, "a repeated (base,msb,lsb) mints one node");
    }

    /// mununu#565 — a slice outside its base, or over an unknown base, is SKIPPED rather than
    /// emitting a model the engines would reject.
    ///
    /// Skipping is deliberate: the property then declines to bind downstream with a named
    /// message naming the atom, which beats aborting a whole multi-property run over one
    /// assertion. An emitted out-of-range `slice` would instead be a malformed model.
    #[test]
    fn an_out_of_range_or_unknown_slice_is_skipped_not_emitted() {
        // msb 9 on an 8-bit base.
        let oob = augment_with_slices(WIDE, &[("w", 9, 0)]).expect("does not error");
        assert_eq!(oob, WIDE, "an out-of-range slice must not be emitted");

        // A base that is not in the model at all.
        let unknown = augment_with_slices(WIDE, &[("nosuch", 3, 0)]).expect("does not error");
        assert_eq!(unknown, WIDE, "an unresolvable base must not be emitted");

        // And the empty list is the identity, so a slice-free design is byte-identical.
        let none = augment_with_slices(WIDE, &[]).expect("does not error");
        assert_eq!(none, WIDE, "no slices ⇒ the model is untouched");
    }

    /// mununu#565 — the naming contract with the translator. If these two ever disagree the atom
    /// silently fails to bind, so the contract is asserted rather than assumed.
    #[test]
    fn slice_name_matches_the_translator_half_of_the_contract() {
        assert_eq!(slice_signal_name("wdata", 7, 0), "wdata__bits7_0");
        // `$past(sig[7:0])` slices the SHADOW, so the two contracts compose.
        assert_eq!(
            slice_signal_name(&shadow_stage_name("wdata", 1), 7, 0),
            "wdata__past__bits7_0"
        );
    }

    #[test]
    fn augment_appends_state_next_init_for_a_base() {
        let out = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augments");
        let file = parser::parse(&out).expect("augmented btor2 parses");
        let symbols = parser::collect_symbols(&file);
        // A new state `s__past` of the same sort (1) exists.
        let shadow = file
            .lines
            .iter()
            .find(|l| {
                matches!(l.node, Node::State { .. })
                    && symbols.get(&l.nid).map(String::as_str) == Some("s__past")
            })
            .expect("s__past state was appended");
        let Node::State { sort, .. } = shadow.node else {
            unreachable!()
        };
        assert_eq!(sort, 1, "shadow reuses the source's sort");
        // A `next` line links the shadow to the source state (NID 2).
        let has_next = file.lines.iter().any(|l| {
            matches!(
                l.node,
                Node::Next { state, value, .. } if state == shadow.nid && value.nid() == 2
            )
        });
        assert!(has_next, "next(s__past) = s must be present");
        // An `init` line mirrors the source's init (value NID 5, the `zero`).
        let has_init = file.lines.iter().any(|l| {
            matches!(
                l.node,
                Node::Init { state, value, .. } if state == shadow.nid && value.nid() == 5
            )
        });
        assert!(has_init, "init(s__past) must mirror s's init");
    }

    #[test]
    fn shadow_captures_previous_cycle_value() {
        // The soundness proof: after one step from s = 5, the shadow holds 5
        // (s's *previous* value) while s advances to 6.
        let out = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augments");
        let file = parser::parse(&out).expect("parse");
        let regs = HashMap::from([("s".to_string(), 5u128)]);
        let next =
            crate::adapter::btor2::bit_blast::simulate_one_step(&file, &regs, &HashMap::new())
                .expect("simulate");
        assert_eq!(
            next.get("s__past").copied(),
            Some(5),
            "shadow took s's value"
        );
        assert_eq!(next.get("s").copied(), Some(6), "s advanced (5 + 1)");
    }

    #[test]
    fn cycle0_shadow_equals_source_init() {
        // init mirroring makes $stable true at cycle 0: with s reset to 0, the
        // shadow also resets to 0, so `s == s__past` holds at the first cycle.
        let out = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augments");
        let file = parser::parse(&out).expect("parse");
        // honor_init: read the init values both cells reset to.
        let s_init = init_value(&file, "s");
        let shadow_init = init_value(&file, "s__past");
        assert_eq!(s_init, shadow_init, "shadow init mirrors source init");
        assert_eq!(s_init, Some(0));
    }

    #[test]
    fn augment_builds_a_k_stage_chain_for_depth_gt_1() {
        // `$past(s, 3)` needs a 3-stage shift chain: s__past ← s, s__past2 ←
        // s__past, s__past3 ← s__past2. Verify the stage names, the chain wiring,
        // and that each stage mirrors the source's init (so `$past(s, j)` reads
        // the reset value before the chain fills).
        let out = augment_with_past_shadows(COUNTER, &[("s", 3)]).expect("augments depth 3");
        let file = parser::parse(&out).expect("augmented btor2 parses");
        let symbols = parser::collect_symbols(&file);
        let state_nid = |sym: &str| -> Option<Nid> {
            file.lines.iter().find_map(|l| match &l.node {
                Node::State { .. } if symbols.get(&l.nid).map(String::as_str) == Some(sym) => {
                    Some(l.nid)
                }
                _ => None,
            })
        };
        let src = state_nid("s").expect("source s");
        let s1 = state_nid("s__past").expect("stage 1 = s__past");
        let s2 = state_nid("s__past2").expect("stage 2 = s__past2");
        let s3 = state_nid("s__past3").expect("stage 3 = s__past3");
        // Chain wiring: next(s__past)=s, next(s__past2)=s__past, next(s__past3)=s__past2.
        let next_of = |state: Nid| -> Option<Nid> {
            file.lines.iter().find_map(|l| match &l.node {
                Node::Next {
                    state: st, value, ..
                } if *st == state => Some(value.nid()),
                _ => None,
            })
        };
        assert_eq!(next_of(s1), Some(src), "next(s__past) = s");
        assert_eq!(next_of(s2), Some(s1), "next(s__past2) = s__past");
        assert_eq!(next_of(s3), Some(s2), "next(s__past3) = s__past2");
        // Every stage mirrors the source's init (s resets to 0), so each reads the
        // reset value before its own history has accrued.
        for stage in ["s__past", "s__past2", "s__past3"] {
            assert_eq!(
                init_value(&file, stage),
                Some(0),
                "{stage} must mirror the source init"
            );
        }
        // Stage 1 captures the source after one step (deeper stages fill later).
        use std::collections::HashMap;
        let regs = HashMap::from([("s".to_string(), 5u128)]);
        let nxt =
            crate::adapter::btor2::bit_blast::simulate_one_step(&file, &regs, &HashMap::new())
                .expect("simulate");
        assert_eq!(nxt.get("s__past").copied(), Some(5), "stage 1 took s");
    }

    #[test]
    fn depth_1_is_backward_compatible_with_the_bare_name() {
        // Depth 1 must still emit exactly `s__past` (no suffix) — the name every
        // pre-existing model and `$stable`/`$rose`/`$fell` lowering depends on.
        let out = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augments");
        let file = parser::parse(&out).expect("parses");
        let symbols = parser::collect_symbols(&file);
        let names: std::collections::HashSet<&str> = file
            .lines
            .iter()
            .filter(|l| matches!(l.node, Node::State { .. }))
            .filter_map(|l| symbols.get(&l.nid).map(String::as_str))
            .collect();
        assert!(names.contains("s__past"), "depth 1 = bare s__past");
        assert!(
            !names.contains("s__past1"),
            "depth 1 must NOT emit a suffixed s__past1"
        );
    }

    // A design whose only `$past` source is a primary INPUT:
    //   1 sort bitvec 8
    //   2 input 1 din
    //   3 state 1 q          ; q <= din
    //   4 next 1 3 2
    const INPUT_SOURCE: &str = "1 sort bitvec 8\n2 input 1 din\n3 state 1 q\n4 next 1 3 2\n";

    #[test]
    fn a_primary_input_gets_a_shadow_flop() {
        // The case that made every data-integrity property unverifiable: the
        // translator asks for `din__past`, and before this the augmentation
        // refused because `din` is an input rather than a state cell.
        let out =
            augment_with_past_shadows(INPUT_SOURCE, &[("din", 1)]).expect("input base accepted");
        let file = parser::parse(&out).expect("augmented model parses");
        let symbols = parser::collect_symbols(&file);

        let shadow = file
            .lines
            .iter()
            .find(|l| {
                matches!(l.node, Node::State { .. })
                    && symbols.get(&l.nid).map(String::as_str) == Some("din__past")
            })
            .expect("din__past is a state cell in the augmented model");

        // next(din__past) = din, so the flop holds the input's previous value.
        let din_nid = file
            .lines
            .iter()
            .find(|l| symbols.get(&l.nid).map(String::as_str) == Some("din"))
            .expect("din")
            .nid;
        assert!(
            file.lines.iter().any(|l| matches!(&l.node,
                Node::Next { state, value, .. } if *state == shadow.nid && value.nid() == din_nid)),
            "next(din__past) must be sourced from the input"
        );
    }

    #[test]
    fn an_input_shadow_is_pinned_to_zero_so_every_engine_agrees() {
        // A state cell's shadow mirrors the source's init. An input has none, and
        // leaving the shadow init-less is NOT the safe default here: the cube and
        // exact engines read an init-less cell as 0 while the reach portfolio
        // leaves it free, which is the verdict disagreement `inject_zero_init`
        // exists to prevent — and that pass runs before this one, so it cannot
        // cover a shadow appended afterwards. Pinning here is what keeps the
        // engines deciding the same model.
        let out = augment_with_past_shadows(INPUT_SOURCE, &[("din", 1)]).expect("augment");
        let file = parser::parse(&out).expect("parse");
        assert_eq!(
            init_value(&file, "din__past"),
            Some(0),
            "an input-sourced shadow must carry an EXPLICIT zero init"
        );
    }

    #[test]
    fn an_input_shadows_zero_is_declared_before_the_state_it_inits() {
        // btormc's parser requires an `init` value's NID to be BELOW the state's.
        // mununu's own parser is order-agnostic, so getting this wrong produces a
        // model that reads fine in-process and is rejected by the external oracle
        // — a disagreement that would surface as a portfolio failure, not as a
        // parse error here.
        let out = augment_with_past_shadows(INPUT_SOURCE, &[("din", 1)]).expect("augment");
        let file = parser::parse(&out).expect("parse");
        let symbols = parser::collect_symbols(&file);
        let shadow_nid = file
            .lines
            .iter()
            .find(|l| symbols.get(&l.nid).map(String::as_str) == Some("din__past"))
            .expect("din__past")
            .nid;
        let init_value_nid = file
            .lines
            .iter()
            .find_map(|l| match &l.node {
                Node::Init { state, value, .. } if *state == shadow_nid => Some(value.nid()),
                _ => None,
            })
            .expect("the shadow carries an init");
        assert!(
            init_value_nid < shadow_nid,
            "init value {init_value_nid} must precede state {shadow_nid}"
        );
    }

    #[test]
    fn an_exact_input_outranks_a_like_named_alias_over_a_distant_state() {
        // `resolve_state_by_symbol` also matches an `output`/op line named `din`
        // and walks its cone to the nearest state — a hit at distance N. If the
        // walk were tried first, that distant `q` would shadow the wrong signal
        // while an `input` line carries the name exactly. Exact beats fuzzy.
        let aliased = "1 sort bitvec 8\n                       2 input 1 din\n                       3 state 1 q\n                       4 next 1 3 2\n                       5 output 3 din\n";
        let out = augment_with_past_shadows(aliased, &[("din", 1)]).expect("augment");
        let file = parser::parse(&out).expect("parse");
        let symbols = parser::collect_symbols(&file);
        let shadow_nid = file
            .lines
            .iter()
            .find(|l| symbols.get(&l.nid).map(String::as_str) == Some("din__past"))
            .expect("din__past")
            .nid;
        assert!(
            file.lines.iter().any(|l| matches!(&l.node,
                Node::Next { state, value, .. } if *state == shadow_nid && value.nid() == 2)),
            "next(din__past) must read the INPUT (nid 2), not the aliased state"
        );
    }

    #[test]
    fn a_state_cell_still_wins_over_a_like_named_input() {
        // Resolution order matters: a state cell is tried first, so the
        // symbol-distance heuristic stays authoritative when a name could
        // mean either.
        let both = "1 sort bitvec 8\n2 input 1 s\n3 state 1 s\n4 next 1 3 2\n";
        let out = augment_with_past_shadows(both, &[("s", 1)]).expect("augment");
        let file = parser::parse(&out).expect("parse");
        let symbols = parser::collect_symbols(&file);
        let state_nid = file
            .lines
            .iter()
            .find(|l| {
                matches!(l.node, Node::State { .. })
                    && symbols.get(&l.nid).map(String::as_str) == Some("s")
            })
            .expect("state s")
            .nid;
        let shadow_nid = file
            .lines
            .iter()
            .find(|l| symbols.get(&l.nid).map(String::as_str) == Some("s__past"))
            .expect("s__past")
            .nid;
        assert!(
            file.lines.iter().any(|l| matches!(&l.node,
                Node::Next { state, value, .. } if *state == shadow_nid && value.nid() == state_nid)),
            "the STATE cell must win when a name resolves to both"
        );
    }

    #[test]
    fn missing_base_is_an_error_not_silent() {
        let err =
            augment_with_past_shadows(COUNTER, &[("nonexistent", 1)]).expect_err("must error");
        assert_eq!(err.kind, AdapterErrorKind::UnsupportedConstruct);
        // Asserts the INTENT — the message names the signal and says it did
        // not resolve — rather than an exact phrase. The wording changed when
        // inputs became valid bases, and a test that pins prose fails for a
        // reason that has nothing to do with behaviour.
        assert!(
            err.message.contains("nonexistent") && err.message.contains("resolve"),
            "error must name the unresolved signal; got: {}",
            err.message
        );
    }

    #[test]
    fn augmentation_is_idempotent() {
        let once = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augment 1");
        let twice = augment_with_past_shadows(&once, &[("s", 1)]).expect("augment 2");
        assert_eq!(once, twice, "re-augmenting an existing shadow is a no-op");
    }

    #[test]
    fn empty_bases_returns_input_unchanged() {
        assert_eq!(augment_with_past_shadows(COUNTER, &[]).unwrap(), COUNTER);
    }

    #[test]
    fn augmented_model_lifts_with_shadow_in_valuations() {
        // The augmentation composes with the explicit-state bit-blast lift: the
        // shadow `s__past` appears as a state-valuation variable, so the
        // evaluator's on-demand `s == s__past` ($stable) comparison atom binds
        // against the lifted Clts. (8 state bits — well under MAX_STATE_BITS.)
        use crate::adapter::AdapterOptions;
        let out = augment_with_past_shadows(COUNTER, &[("s", 1)]).expect("augment");
        let lifted = crate::adapter::btor2::bit_blast::translate(&out, &AdapterOptions::default())
            .expect("bit-blast lift of the augmented model");
        let has_shadow = lifted
            .state_valuations
            .values()
            .any(|states| states.values().any(|vars| vars.contains_key("s__past")));
        assert!(
            has_shadow,
            "s__past must appear in the lifted state valuations so $stable binds; got: {:?}",
            lifted.state_valuations
        );
    }

    /// Read the value a state cell initialises to (its `Init` line's value).
    fn init_value(file: &Btor2File, symbol: &str) -> Option<u128> {
        let symbols = parser::collect_symbols(file);
        let state_nid = file.lines.iter().find_map(|l| match &l.node {
            Node::State { .. } if symbols.get(&l.nid).map(String::as_str) == Some(symbol) => {
                Some(l.nid)
            }
            _ => None,
        })?;
        // Read the init operand's constant directly (simulate_one_step seeds
        // states from the caller's map, not from the BTOR2 init lines).
        let init_op = file.lines.iter().find_map(|l| match &l.node {
            Node::Init { state, value, .. } if *state == state_nid => Some(*value),
            _ => None,
        })?;
        match &file.lookup(init_op.nid())?.node {
            Node::Const { value, .. } => Some(const_to_u128(value)),
            _ => None,
        }
    }

    fn const_to_u128(v: &crate::adapter::btor2::ast::ConstValue) -> u128 {
        use crate::adapter::btor2::ast::ConstValue::*;
        match v {
            Zero => 0,
            One => 1,
            Ones => u128::MAX,
            Dec(d) => *d as u128,
            Bin(b) => u128::from_str_radix(b, 2).unwrap_or(0),
            Hex(h) => u128::from_str_radix(h, 16).unwrap_or(0),
        }
    }
}
