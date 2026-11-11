//! Track V — the k-liveness `bad` monitor for a justice set (Claessen–Sörensson 2012).
//!
//! A BTOR2 `justice n l₁ … lₙ` (plus the global `fair` literals) is violated iff an infinite
//! path exists on which every literal holds infinitely often. k-liveness proves the property
//! by bounding how often that can happen: if no reachable path completes more than `k` **rounds**
//! — a round ends once every literal has been seen at least once since the previous round — then
//! some literal occurs finitely often on every path, so no fair path exists and the property
//! HOLDS. The bound is proved as plain `bad`-reachability, which is what IC3 is good at: a small
//! inductive invariant over the design plus a few monitor bits, instead of the full fair-cycle
//! fixpoint (the exact rung) or an invariant that relates two copies of the state (l2s).
//!
//! # The construction (the degeneralised round counter)
//!
//! For literals `l₁ … lₘ` (the justice set's own, then every `fair` literal):
//!
//! - `seen_i` (latch, init 0): `occ_i = seen_i ∨ l_i`; `round = ⋀_i occ_i`;
//!   `next(seen_i) = round ? 0 : occ_i`.
//! - `cnt` (latch of `k_bits`, init 0): `next(cnt) = round ? cnt + 1 : cnt`.
//! - `bad = (cnt == 2^k_bits − 1)`.
//!
//! `bad` unreachable ⇒ no path completes 2^k_bits − 1 rounds ⇒ HOLDS (sound on the concrete
//! design — no abstraction is involved). `bad` reachable says only that this many rounds can
//! happen; it is **not** a violation (a finite count says nothing about an infinite one), so a
//! caller moves to a wider counter. Complete on finite state: a reachable cycle that completes a
//! round gives infinitely many, so `k ≤ |S|` always suffices — the caller's schedule of widths is
//! the only incompleteness, and it says which width it reached.
//!
//! Why rounds and not "all literals at the same step": a justice SET's literals need not hold
//! simultaneously (AIGER 1.9: each infinitely often), so counting steps on which all hold at once
//! would under-count and could "prove" a violated property. The round counter is the standard
//! generalised-Büchi degeneralisation.
//!
//! Text-level, like [`crate::adapter::btor2::l2s_monitor`]: the original lines are kept verbatim
//! except the `justice` / `fair` lines, which are dropped (a safety member must see exactly one
//! `bad` and no justice section); the monitor is appended with fresh NIDs above the current max.
//! The literals are taken from the `justice` line itself, so no output naming is required.

use crate::adapter::btor2::ast::{Node, Sort};
use crate::adapter::btor2::parser;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// Name of the counter latch in the emitted text (also the `bad` line's provenance).
pub const COUNTER_SYMBOL: &str = "mununu_kliveness_cnt";

/// Append the k-liveness monitor for the `justice` line at `justice_index` (0-based, in file
/// order) to `content`, with a `k_bits`-wide round counter (`1 ≤ k_bits ≤ 32`). Every `fair`
/// literal joins the set. Returns the augmented BTOR2 text with the `justice` / `fair` lines
/// removed and exactly one `bad` appended.
pub fn emit_kliveness_monitor(
    content: &str,
    justice_index: usize,
    k_bits: u32,
) -> Result<String, AdapterError> {
    if !(1..=32).contains(&k_bits) {
        return Err(err(format!(
            "counter width {k_bits} is outside 1..=32 (k = 2^width − 1 rounds)"
        )));
    }
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/kliveness_monitor: {}", e.message);
        e
    })?;

    // The literals: the chosen justice line's, then every fair line's.
    let mut justice_lines = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::Justice { .. }));
    let justice = justice_lines
        .nth(justice_index)
        .ok_or_else(|| err(format!("the design has no justice line #{justice_index}")))?;
    let mut lits: Vec<i64> = match &justice.node {
        Node::Justice { signals } => signals.iter().map(|o| o.0).collect(),
        _ => Vec::new(),
    };
    for l in &file.lines {
        if let Node::Fair { signal } = &l.node {
            lits.push(signal.0);
        }
    }
    if lits.is_empty() {
        return Err(err(format!(
            "justice line #{justice_index} has no literals"
        )));
    }
    let dropped: std::collections::HashSet<i64> = file
        .lines
        .iter()
        .filter(|l| matches!(l.node, Node::Justice { .. } | Node::Fair { .. }))
        .map(|l| l.nid)
        .collect();

    // The 1-bit sort, reused if the design declares one.
    let bool_sort = file.lines.iter().find_map(|l| match &l.node {
        Node::Sort {
            sort: Sort::BitVec { width: 1 },
        } => Some(l.nid),
        _ => None,
    });

    let mut next = file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1;
    let mut out: Vec<String> = Vec::new();
    let mut push = |body: String| -> i64 {
        let nid = next;
        out.push(format!("{nid} {body}"));
        next += 1;
        nid
    };
    let b = match bool_sort {
        Some(s) => s,
        None => push("sort bitvec 1".to_string()),
    };
    let zero1 = push(format!("zero {b}"));
    let csort = push(format!("sort bitvec {k_bits}"));
    let czero = push(format!("zero {csort}"));
    let cones = push(format!("ones {csort}"));

    // Literals as positive 1-bit nodes (a negative operand is `not` of its nid).
    let pos: Vec<i64> = lits
        .iter()
        .map(|&l| {
            if l < 0 {
                push(format!("not {b} {}", -l))
            } else {
                l
            }
        })
        .collect();
    let seen: Vec<i64> = (0..pos.len())
        .map(|i| push(format!("state {b} mununu_kliveness_seen{i}")))
        .collect();
    for &s in &seen {
        push(format!("init {b} {s} {zero1}"));
    }
    let occ: Vec<i64> = seen
        .iter()
        .zip(&pos)
        .map(|(&s, &l)| push(format!("or {b} {s} {l}")))
        .collect();
    let mut round = occ[0];
    for &o in &occ[1..] {
        round = push(format!("and {b} {round} {o}"));
    }
    for (&s, &o) in seen.iter().zip(&occ) {
        let nx = push(format!("ite {b} {round} {zero1} {o}"));
        push(format!("next {b} {s} {nx}"));
    }
    let cnt = push(format!("state {csort} {COUNTER_SYMBOL}"));
    push(format!("init {csort} {cnt} {czero}"));
    let inc = push(format!("inc {csort} {cnt}"));
    let nx = push(format!("ite {csort} {round} {inc} {cnt}"));
    push(format!("next {csort} {cnt} {nx}"));
    let bad = push(format!("eq {b} {cnt} {cones}"));
    push(format!("bad {bad}"));

    // Original text minus the justice / fair lines, then the monitor.
    let mut text = String::with_capacity(content.len() + out.len() * 24);
    for line in content.lines() {
        let nid = line
            .split_whitespace()
            .next()
            .and_then(|t| t.parse::<i64>().ok());
        if nid.is_some_and(|n| dropped.contains(&n)) {
            continue;
        }
        text.push_str(line);
        text.push('\n');
    }
    for l in out {
        text.push_str(&l);
        text.push('\n');
    }
    Ok(text)
}

fn err(message: String) -> AdapterError {
    AdapterError {
        kind: AdapterErrorKind::UnsupportedConstruct,
        message: format!("adapter/btor2/kliveness_monitor: {message}"),
        location: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast::simulate_one_step;

    /// A free-running 2-bit counter with justice on `lo ∧ hi` — a fair cycle exists (VIOLATED).
    const COUNTER: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                           6 init 1 4 2\n7 init 1 5 2\n8 not 1 4\n9 next 1 4 8\n10 xor 1 4 5\n\
                           11 next 1 5 10\n12 and 1 4 5\n13 justice 1 12\n";
    /// Justice as a SET: `lo` and `hi` each infinitely often (they never hold together here —
    /// `hi` is `lo`'s complement after reset — so a same-step conjunction would never fire).
    const SET: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 lo\n5 state 1 hi\n\
                       6 init 1 4 2\n7 init 1 5 3\n8 not 1 4\n9 next 1 4 8\n10 not 1 5\n\
                       11 next 1 5 10\n12 justice 2 4 5\n";

    fn parse(t: &str) -> crate::adapter::btor2::ast::Btor2File {
        parser::parse(t).expect("monitor parses")
    }

    /// Simulate `steps` steps from the given register values (the monitor's latches start at 0,
    /// as their `init` lines say) and return the round counter.
    fn cnt_after(text: &str, init: &[(&str, u128)], steps: usize) -> u128 {
        let file = parse(text);
        let mut state: std::collections::HashMap<String, u128> =
            init.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for l in &file.lines {
            if let Node::State {
                symbol: Some(s), ..
            } = &l.node
                && s.starts_with("mununu_kliveness_")
            {
                state.insert(s.clone(), 0);
            }
        }
        let inputs = std::collections::HashMap::new();
        for _ in 0..steps {
            state = simulate_one_step(&file, &state, &inputs).expect("simulates");
        }
        state.get(COUNTER_SYMBOL).copied().unwrap_or(0)
    }

    #[test]
    fn the_monitor_drops_the_justice_lines_and_adds_exactly_one_bad() {
        let m = emit_kliveness_monitor(COUNTER, 0, 2).expect("emits");
        assert!(!m.contains(" justice "), "{m}");
        assert_eq!(m.matches(" bad ").count(), 1, "{m}");
        assert!(m.contains(COUNTER_SYMBOL), "{m}");
        parse(&m);
    }

    /// On the counter `lo ∧ hi` holds every 4th step (3, 7, 11, …): after 12 steps 3 rounds.
    #[test]
    fn the_counter_counts_completed_rounds_of_the_fair_event() {
        let m = emit_kliveness_monitor(COUNTER, 0, 4).expect("emits");
        let init = [("lo", 0), ("hi", 0)];
        assert_eq!(cnt_after(&m, &init, 4), 1);
        assert_eq!(cnt_after(&m, &init, 12), 3);
    }

    /// A SET's literals occur on different steps; a round completes once both have been seen.
    /// `lo` = 0,1,0,1,…, `hi` = 1,0,1,0,…: step 0 sees `hi`, step 1 sees `lo` → round 1 ends at
    /// step 1, and every two steps after.
    #[test]
    fn a_round_needs_every_literal_not_all_at_once() {
        let m = emit_kliveness_monitor(SET, 0, 4).expect("emits");
        let init = [("lo", 0), ("hi", 1)];
        assert_eq!(cnt_after(&m, &init, 2), 1);
        assert_eq!(cnt_after(&m, &init, 6), 3);
    }

    #[test]
    fn a_missing_justice_line_or_a_bad_width_is_an_error() {
        assert!(emit_kliveness_monitor(COUNTER, 1, 4).is_err());
        assert!(emit_kliveness_monitor(COUNTER, 0, 0).is_err());
        assert!(emit_kliveness_monitor(COUNTER, 0, 33).is_err());
    }
}
