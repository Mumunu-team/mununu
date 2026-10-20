//! BTOR2 emitter — serialise a [`Btor2File`] AST back to BTOR2 text.
//!
//! The inverse of [`super::parser::parse`]. It renders each [`Line`] from its
//! typed [`Node`] (NOT from the retained `source_line`), so it faithfully emits
//! a model that has been **transformed** — a cone-of-influence reduction, an
//! input pinned to a constant, a `constraint` folded in — not just the text that
//! was read. That is what makes it the L2-seam hand-off (`Btor2Emit`) to the
//! subprocess safety engines (btormc / Pono / ABC / AVR): mununu reduces a model
//! internally, emits it, and an external engine decides the reduced query.
//!
//! Fidelity: `parse ∘ emit ∘ parse` is a fixed point — emitting a parsed file
//! and re-parsing yields an identical AST (NIDs, sorts, ops, immediates, and
//! properties all preserved). Optional symbols are emitted when present. The
//! output is a valid BTOR2 file consumable by any conforming tool.

use super::ast::{Btor2File, ConstValue, Line, Nid, Node, Operand, Sort};

/// Serialise `file` to BTOR2 text (one node per line, declaration order, trailing newline).
pub fn emit_btor2(file: &Btor2File) -> String {
    let mut out = String::new();
    for line in &file.lines {
        emit_line(line, &mut out);
        out.push('\n');
    }
    out
}

/// Plan 4 / PR D2 (mununu#736) — the BTOR2 handed to a SUBPROCESS member (btormc, pono, rIC3),
/// renumbered so every constant precedes every state. btor2tools' parser — shared by btormc and
/// pono — requires an `init` value's id to be smaller than the state's, and mununu's automatic
/// reset-value `init` (mununu#578) appends its constant after the yosys output: both members
/// rejected every `sv verify-auto` design (`state id must be greater than id of second operand`)
/// and abstained in ~40 ms, unnoticed because mununu's own parser and rIC3 are lenient.
///
/// Order: sorts, then constants (they reference only sorts), then everything else in declaration
/// order; ids are reassigned 1..n in that order and every reference — a sort, a state, an operand
/// with its sign — is rewritten. A renumbering: the design's semantics, symbols and immediates are
/// unchanged, so a verdict on this text IS a verdict on the design. An `init` whose value is an
/// expression rather than a constant keeps its relative order and may still be rejected; no yosys
/// or mununu path emits one.
pub fn emit_btor2_for_subprocess(file: &Btor2File) -> String {
    use std::collections::HashMap;
    let rank = |l: &Line| match l.node {
        Node::Sort { .. } => 0,
        Node::Const { .. } => 1,
        _ => 2,
    };
    let mut order: Vec<usize> = Vec::with_capacity(file.lines.len());
    for pass in 0..3 {
        order.extend(
            file.lines
                .iter()
                .enumerate()
                .filter(|(_, l)| rank(l) == pass)
                .map(|(i, _)| i),
        );
    }
    let mut map: HashMap<Nid, Nid> = HashMap::with_capacity(order.len());
    for (new, &i) in order.iter().enumerate() {
        map.insert(file.lines[i].nid, new as Nid + 1);
    }
    let re = |n: Nid| map.get(&n).copied().unwrap_or(n);
    let rop = |o: Operand| Operand(if o.0 < 0 { -re(-o.0) } else { re(o.0) });
    let mut out = String::new();
    for &i in &order {
        let l = &file.lines[i];
        let node = match &l.node {
            Node::Sort { sort } => Node::Sort {
                sort: match sort {
                    Sort::BitVec { width } => Sort::BitVec { width: *width },
                    Sort::Array { index, element } => Sort::Array {
                        index: re(*index),
                        element: re(*element),
                    },
                },
            },
            Node::Input { sort, symbol } => Node::Input {
                sort: re(*sort),
                symbol: symbol.clone(),
            },
            Node::State { sort, symbol } => Node::State {
                sort: re(*sort),
                symbol: symbol.clone(),
            },
            Node::Const { sort, value } => Node::Const {
                sort: re(*sort),
                value: value.clone(),
            },
            Node::Op {
                sort,
                op,
                args,
                symbol,
            } => Node::Op {
                sort: re(*sort),
                op: *op,
                args: args.iter().map(|a| rop(*a)).collect(),
                symbol: symbol.clone(),
            },
            Node::Init { sort, state, value } => Node::Init {
                sort: re(*sort),
                state: re(*state),
                value: rop(*value),
            },
            Node::Next { sort, state, value } => Node::Next {
                sort: re(*sort),
                state: re(*state),
                value: rop(*value),
            },
            Node::Bad { signal } => Node::Bad {
                signal: rop(*signal),
            },
            Node::Constraint { signal } => Node::Constraint {
                signal: rop(*signal),
            },
            Node::Fair { signal } => Node::Fair {
                signal: rop(*signal),
            },
            Node::Output { signal, symbol } => Node::Output {
                signal: rop(*signal),
                symbol: symbol.clone(),
            },
            Node::Justice { signals } => Node::Justice {
                signals: signals.iter().map(|s| rop(*s)).collect(),
            },
        };
        let mut line = l.clone();
        line.nid = re(l.nid);
        line.node = node;
        emit_line(&line, &mut out);
        out.push('\n');
    }
    out
}

/// An operand renders as its signed NID: a negated operand (`-N`, BTOR2's
/// bit-not shorthand) keeps its sign.
fn operand(op: Operand) -> String {
    op.0.to_string()
}

fn emit_line(line: &Line, out: &mut String) {
    use std::fmt::Write;
    let nid = line.nid;
    match &line.node {
        Node::Sort { sort } => match sort {
            Sort::BitVec { width } => {
                let _ = write!(out, "{nid} sort bitvec {width}");
            }
            Sort::Array { index, element } => {
                let _ = write!(out, "{nid} sort array {index} {element}");
            }
        },
        Node::Input { sort, symbol } => {
            let _ = write!(out, "{nid} input {sort}");
            emit_symbol(symbol, out);
        }
        Node::State { sort, symbol } => {
            let _ = write!(out, "{nid} state {sort}");
            emit_symbol(symbol, out);
        }
        Node::Const { sort, value } => match value {
            ConstValue::Zero => {
                let _ = write!(out, "{nid} zero {sort}");
            }
            ConstValue::One => {
                let _ = write!(out, "{nid} one {sort}");
            }
            ConstValue::Ones => {
                let _ = write!(out, "{nid} ones {sort}");
            }
            ConstValue::Bin(s) => {
                let _ = write!(out, "{nid} const {sort} {s}");
            }
            ConstValue::Dec(d) => {
                let _ = write!(out, "{nid} constd {sort} {d}");
            }
            ConstValue::Hex(s) => {
                let _ = write!(out, "{nid} consth {sort} {s}");
            }
        },
        Node::Op {
            sort,
            op,
            args,
            symbol,
        } => {
            let _ = write!(out, "{nid} {} {sort}", op.keyword());
            for a in args {
                let _ = write!(out, " {}", operand(*a));
            }
            // Operator immediates follow the operand list (`slice … <upper> <lower>`,
            // `uext / sext … <amount>`).
            for imm in &line.immediates {
                let _ = write!(out, " {imm}");
            }
            emit_symbol(symbol, out);
        }
        Node::Init { sort, state, value } => {
            let _ = write!(out, "{nid} init {sort} {state} {}", operand(*value));
        }
        Node::Next { sort, state, value } => {
            let _ = write!(out, "{nid} next {sort} {state} {}", operand(*value));
        }
        Node::Bad { signal } => {
            let _ = write!(out, "{nid} bad {}", operand(*signal));
        }
        Node::Constraint { signal } => {
            let _ = write!(out, "{nid} constraint {}", operand(*signal));
        }
        Node::Fair { signal } => {
            let _ = write!(out, "{nid} fair {}", operand(*signal));
        }
        Node::Output { signal, symbol } => {
            let _ = write!(out, "{nid} output {}", operand(*signal));
            emit_symbol(symbol, out);
        }
        Node::Justice { signals } => {
            let _ = write!(out, "{nid} justice {}", signals.len());
            for s in signals {
                let _ = write!(out, " {}", operand(*s));
            }
        }
    }
}

fn emit_symbol(symbol: &Option<String>, out: &mut String) {
    use std::fmt::Write;
    if let Some(s) = symbol {
        let _ = write!(out, " {s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    /// mununu#736 — the subprocess emission puts every constant before every state, so an `init`
    /// appended after the yosys output (mununu's reset-value establishment) parses under btor2tools'
    /// rule (value id < state id); the renumbering keeps the design: same verdict on the native
    /// engine, symbols and negated operands preserved, and it re-parses.
    #[test]
    fn subprocess_emission_hoists_constants_before_states() {
        // a yosys-shaped file with the reset init appended last (constant AFTER the state)
        const APPENDED_INIT: &str = "1 sort bitvec 1\n2 input 1 en\n3 state 1 q\n4 not 1 3\n\
                                     5 ite 1 2 4 3\n6 next 1 3 5\n7 one 1\n8 eq 1 3 7\n9 bad 8\n\
                                     10 constd 1 0\n11 init 1 3 10\n";
        let file = crate::adapter::btor2::parser::parse(APPENDED_INIT).expect("parse");
        let text = emit_btor2_for_subprocess(&file);
        let re = crate::adapter::btor2::parser::parse(&text).expect("the renumbered text parses");
        for l in &re.lines {
            if let Node::Init { state, value, .. } = &l.node {
                assert!(
                    value.nid() < *state,
                    "init value {} must precede state {} in:\n{text}",
                    value.nid(),
                    state
                );
            }
        }
        assert!(text.contains(" state 1 q"), "symbols survive: {text}");
        assert!(text.contains(" input 1 en"));
        // ids are dense 1..n in emission order, sorts first, then constants
        let nids: Vec<i64> = re.lines.iter().map(|l| l.nid).collect();
        assert_eq!(nids, (1..=nids.len() as i64).collect::<Vec<_>>());
        assert!(matches!(re.lines[0].node, Node::Sort { .. }));
        assert!(matches!(re.lines[1].node, Node::Const { .. }));
        assert!(matches!(re.lines[2].node, Node::Const { .. }));
        // same design: the native engine's verdict is unchanged (q toggles while en, starts 0 → q==1 reachable)
        use crate::adapter::btor2::native_bmc;
        let before = native_bmc::decide_bad_safety_cancellable(&file, 20, Some(5_000), None)
            .expect("native");
        let after =
            native_bmc::decide_bad_safety_cancellable(&re, 20, Some(5_000), None).expect("native");
        assert!(
            matches!(
                before,
                crate::adapter::btor2::native_bmc::SafetyVerdict::Violated { .. }
            ),
            "{before:?}"
        );
        assert!(
            matches!(
                after,
                crate::adapter::btor2::native_bmc::SafetyVerdict::Violated { .. }
            ),
            "{after:?}"
        );
    }

    /// A negated operand (`-N`, BTOR2's bit-not shorthand) keeps its sign through the renumbering.
    #[test]
    fn subprocess_emission_keeps_negated_operands() {
        const NEG: &str =
            "1 sort bitvec 1\n2 state 1 s\n3 zero 1\n4 init 1 2 3\n5 next 1 2 -2\n6 bad -2\n";
        let file = crate::adapter::btor2::parser::parse(NEG).expect("parse");
        let text = emit_btor2_for_subprocess(&file);
        let re = crate::adapter::btor2::parser::parse(&text).expect("re-parse");
        let negs = re
            .lines
            .iter()
            .filter(|l| match &l.node {
                Node::Next { value, .. } => value.is_negated(),
                Node::Bad { signal } => signal.is_negated(),
                _ => false,
            })
            .count();
        assert_eq!(negs, 2, "{text}");
    }

    /// `parse ∘ emit ∘ parse` is a fixed point: emitting a parsed file and
    /// re-parsing yields an AST that re-emits identically.
    fn assert_roundtrip(src: &str) {
        let file = parser::parse(src).expect("parse original");
        let emitted = emit_btor2(&file);
        let reparsed = parser::parse(&emitted).expect("re-parse emitted");
        let re_emitted = emit_btor2(&reparsed);
        assert_eq!(
            emitted, re_emitted,
            "emit is not idempotent under re-parse\n--- emitted ---\n{emitted}\n--- re-emitted ---\n{re_emitted}"
        );
        assert_eq!(
            file.lines.len(),
            reparsed.lines.len(),
            "re-parse changed the line count"
        );
    }

    #[test]
    fn roundtrip_counter_with_bad() {
        assert_roundtrip(
            "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n\
             7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n",
        );
    }

    #[test]
    fn roundtrip_slice_ext_immediates_and_symbols() {
        // Exercises slice (2 immediates), uext/sext (1 immediate), a named
        // input/state, an output symbol, and a negated operand.
        assert_roundtrip(
            "1 sort bitvec 8\n2 sort bitvec 4\n3 sort bitvec 1\n\
             4 input 1 din\n5 state 1 acc\n6 slice 2 5 3 0\n7 uext 1 6 4\n\
             8 sext 1 6 4\n9 not 1 4\n10 and 1 4 9\n11 eq 3 5 4 is_eq\n\
             12 output 5 acc_o\n13 bad 11\n",
        );
    }

    #[test]
    fn roundtrip_constraint_fair_justice_consts() {
        assert_roundtrip(
            "1 sort bitvec 4\n2 input 1 c\n3 state 1 s\n4 const 1 1010\n\
             5 constd 1 -3\n6 consth 1 a\n7 sort bitvec 1\n8 input 7 f\n\
             9 state 7 g\n10 fair 8\n11 constraint 8\n12 justice 2 8 9\n",
        );
    }

    #[test]
    fn emitted_preserves_bad_reachability_verdict() {
        use crate::adapter::btor2::symbolic_bitblast::exact_bad_reachable;
        // A reachable-bad counter: emit → re-check that the emitted model decides
        // the SAME reachability verdict (a semantic, not just syntactic, round-trip).
        const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n\
             7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(COUNTER).expect("parse");
        let emitted = emit_btor2(&file);
        assert_eq!(
            exact_bad_reachable(&emitted),
            Ok(true),
            "the emitted model must preserve the reachable-bad verdict"
        );
    }
}
