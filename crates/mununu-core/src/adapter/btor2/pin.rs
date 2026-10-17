//! Reset-gating — pin BTOR2 input signals to a constant value.
//!
//! The slang translator (PR B) recognizes `disable iff (reset)` guards, drops
//! them from the property body, and reports the reset signal + the value that
//! makes the disable condition false (reset *inactive*). This module is the
//! model half of that contract: it rewrites the named input(s) to a constant so
//! the design is verified only while not in reset — the general, in-process form
//! of V.7-c's `connect -set rst_ni 1'b1`.
//!
//! A free reset input would otherwise let the model explore reset-asserted
//! transitions, where a property body that assumes "not in reset" can falsely
//! fail; pinning the reset inactive matches SVA `disable iff` semantics.
//!
//! It works at the BTOR2 **text** level — a `<nid> input <sid> <name>` line for
//! a pinned signal becomes `<nid> one <sid> <name>` (value 1) or
//! `<nid> zero <sid> <name>` (value 0). The nid is preserved, so every existing
//! reference to the input now sees the constant; the input simply ceases to be a
//! free variable. This is **exact** (a real constant tie, not an abstraction)
//! and composes with [`crate::adapter::btor2::kmts_lift::predicate_cube_lift`] /
//! [`crate::adapter::btor2::bit_blast`], both of which re-parse the text.
//!
//! Reset signals are 1-bit → `zero`/`one`; a MULTI-BIT pin value (≥2, e.g. a `mode`
//! select for the config-partition path) becomes a `constd <sid> <value>` at the
//! input's sort. So the same helper concretizes a reset OR an arbitrary config input.

use std::collections::HashMap;

/// Rewrite each `input` line whose symbol matches a `(name, value)` pin into a
/// constant of that value. Returns the rewritten BTOR2 text and the list of
/// inputs that were actually found and pinned (as `"<name>=<value>"`), so the
/// caller can report only the resets it genuinely pinned (a recognized reset
/// signal that does not appear as a BTOR2 input is silently not pinned — the
/// honest signal that it was optimized/renamed away).
pub fn pin_inputs_to_constants(content: &str, pins: &[(String, u64)]) -> (String, Vec<String>) {
    if pins.is_empty() {
        return (content.to_string(), Vec::new());
    }
    let want: HashMap<&str, u64> = pins.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let mut pinned: Vec<String> = Vec::new();
    let mut out = String::with_capacity(content.len() + 16);

    for line in content.lines() {
        if let Some(rewritten) = try_pin_line(line, &want) {
            // `<name>=<value>` for the diagnostic; recover the matched name/value
            // from the original line's symbol token.
            if let Some(sym) = line.split_whitespace().nth(3)
                && let Some(val) = want.get(sym)
            {
                pinned.push(format!("{sym}={val}"));
            }
            out.push_str(&rewritten);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    (out, pinned)
}

/// The primary input `name` denotes: the input itself, or a yosys no-op alias of one
/// (`<nid> uext <sort> <arg> 0 <name>`, possibly chained). A port of a sub-instance is such an
/// alias of the net connected to it — `u_rx.rst_ni` of the top-level `rst_ni` — so a reset a
/// sub-instance's `disable iff` names (mununu#709) pins the input that drives it. `None` when
/// the name is not an input and not a pure alias of one (an internal net: it cannot be pinned).
pub fn input_behind(file: &crate::adapter::btor2::ast::Btor2File, name: &str) -> Option<String> {
    use crate::adapter::btor2::ast::{Node, Op};
    let by_nid: HashMap<i64, &crate::adapter::btor2::ast::Line> =
        file.lines.iter().map(|l| (l.nid, l)).collect();
    let mut line = file.lines.iter().find(|l| match &l.node {
        Node::Input { symbol, .. } | Node::Op { symbol, .. } => symbol.as_deref() == Some(name),
        _ => false,
    })?;
    // Bounded: an alias chain is short, and a malformed cycle must not hang.
    for _ in 0..64 {
        match &line.node {
            Node::Input { symbol, .. } => return symbol.clone(),
            Node::Op {
                op: Op::Uext, args, ..
            } if line.immediates.first() == Some(&0)
                && args.len() == 1
                && !args[0].is_negated() =>
            {
                line = by_nid.get(&args[0].nid())?;
            }
            _ => return None,
        }
    }
    None
}

/// If `line` is `<nid> input <sid> <symbol> …` and `symbol` is a wanted pin,
/// return the rewritten constant line; otherwise `None`.
fn try_pin_line(line: &str, want: &HashMap<&str, u64>) -> Option<String> {
    let mut toks = line.split_whitespace();
    let nid = toks.next()?;
    if toks.next()? != "input" {
        return None;
    }
    let sid = toks.next()?;
    let sym = toks.next()?;
    // A bare `<nid> input <sid>` (no symbol) cannot be matched by name; `;`
    // means the next token is a comment, not a symbol.
    if sym == ";" {
        return None;
    }
    let value = *want.get(sym)?;
    // 0/1 keep the reset-gating `zero`/`one` spelling; a MULTI-BIT config value (≥2, e.g. a `mode`
    // select for the config-partition path) becomes a `constd` of that value at the input's sort.
    // `constd sid 0`/`constd sid 1` would be equivalent — the special cases just preserve the existing
    // reset-gating output the tests pin on.
    let line = match value {
        0 => format!("{nid} zero {sid} {sym}"),
        1 => format!("{nid} one {sid} {sym}"),
        v => format!("{nid} constd {sid} {v}"),
    };
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mununu#709 — a sub-instance's port is a no-op `uext … 0` alias (possibly chained) of the
    /// input connected to it; an internal net is no input's alias.
    #[test]
    fn a_ports_alias_resolves_to_the_input_behind_it() {
        let file = crate::adapter::btor2::parser::parse(
            "1 sort bitvec 1\n2 input 1 rst_ni\n3 uext 1 2 0 u_rx.rst_ni\n\
             4 uext 1 3 0 u_rx.u_dec.rst_ni\n5 not 1 2\n6 uext 1 5 0 u_rx.rst_q\n",
        )
        .expect("parses");
        assert_eq!(input_behind(&file, "rst_ni").as_deref(), Some("rst_ni"));
        assert_eq!(
            input_behind(&file, "u_rx.rst_ni").as_deref(),
            Some("rst_ni")
        );
        assert_eq!(
            input_behind(&file, "u_rx.u_dec.rst_ni").as_deref(),
            Some("rst_ni")
        );
        assert_eq!(input_behind(&file, "u_rx.rst_q"), None);
        assert_eq!(input_behind(&file, "absent"), None);
    }

    #[test]
    fn pins_active_low_reset_to_one_keeps_state() {
        // A minimal FSM BTOR2 with a free `rst_n` input driving a reset mux.
        let btor2 = "1 sort bitvec 1\n\
                     2 input 1 clk\n\
                     3 input 1 rst_n\n\
                     4 sort bitvec 2\n\
                     5 state 4 state\n\
                     6 const 4 00\n\
                     12 ite 4 3 11 6\n\
                     13 next 4 5 12\n";
        let (out, pinned) = pin_inputs_to_constants(btor2, &[("rst_n".into(), 1)]);
        assert!(out.contains("3 one 1 rst_n"), "rst_n pinned to one: {out}");
        assert!(!out.contains("3 input 1 rst_n"), "no longer a free input");
        assert!(out.contains("5 state 4 state"), "state register preserved");
        assert!(out.contains("2 input 1 clk"), "other inputs untouched");
        assert_eq!(pinned, vec!["rst_n=1".to_string()]);
    }

    #[test]
    fn pins_active_high_reset_to_zero() {
        let btor2 = "1 sort bitvec 1\n2 input 1 rst\n";
        let (out, pinned) = pin_inputs_to_constants(btor2, &[("rst".into(), 0)]);
        assert!(
            out.contains("2 zero 1 rst"),
            "active-high reset → zero: {out}"
        );
        assert_eq!(pinned, vec!["rst=0".to_string()]);
    }

    #[test]
    fn pins_multi_bit_config_input_to_constd() {
        // A 2-bit `mode` config input pinned to 3 → `constd` of 3 (not `one`, which would be 1).
        let btor2 = "1 sort bitvec 2\n2 input 1 mode\n";
        let (out, pinned) = pin_inputs_to_constants(btor2, &[("mode".into(), 3)]);
        assert!(
            out.contains("2 constd 1 3"),
            "mode pinned to constd 3: {out}"
        );
        assert!(!out.contains("2 input 1 mode"));
        assert_eq!(pinned, vec!["mode=3".to_string()]);
    }

    #[test]
    fn unmatched_pin_is_reported_as_not_pinned() {
        // A recognized reset that is not a BTOR2 input (renamed/optimized away)
        // is not pinned, and the returned list reflects that.
        let btor2 = "1 sort bitvec 1\n2 input 1 clk\n";
        let (out, pinned) = pin_inputs_to_constants(btor2, &[("rst_n".into(), 1)]);
        assert_eq!(out, "1 sort bitvec 1\n2 input 1 clk\n");
        assert!(pinned.is_empty(), "nothing matched: {pinned:?}");
    }

    #[test]
    fn empty_pins_is_identity() {
        let btor2 = "1 sort bitvec 1\n2 input 1 clk\n".to_string();
        let (out, pinned) = pin_inputs_to_constants(&btor2, &[]);
        assert_eq!(out, btor2);
        assert!(pinned.is_empty());
    }

    #[test]
    fn input_without_symbol_is_left_alone() {
        // `3 input 1` (no symbol) can't be matched by name.
        let btor2 = "1 sort bitvec 1\n3 input 1\n".to_string();
        let (out, pinned) = pin_inputs_to_constants(&btor2, &[("rst_n".into(), 1)]);
        assert!(out.contains("3 input 1\n"));
        assert!(pinned.is_empty());
    }
}
