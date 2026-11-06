//! mununu#848 — reproducers for the netlist evaluator's `u128` carrier: one test per site that
//! shifted by a bit index or a width with no bound. Each fails on the parent commit (an overflow
//! panic in a debug build, or a wrong value with no panic) and passes on the fix.
//!
//! Its own top-level integration test (target `netlist_wide_nets_848`), outside `netlist.rs`
//! where the fix lives, so `make verify-repro` builds the parent evaluator without the fix; it
//! uses only API the parent already has, so it compiles there.

use mununu_core::adapter::yosys::netlist::{Evaluation, Netlist};
use serde_json::{Value, json};
use std::collections::HashMap;

fn ids(from: u64, n: u64) -> Value {
    Value::from((from..from + n).collect::<Vec<u64>>())
}

fn m(pairs: &[(&str, u128)]) -> HashMap<String, u128> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn top(ports: Value, cells: Value, netnames: Value) -> Netlist {
    let j = json!({"modules": {"m": {
        "attributes": {"top": 1}, "ports": ports, "cells": cells, "netnames": netnames}}});
    Netlist::parse(&j.to_string()).expect("parses")
}

fn bits(ev: &Evaluation, from: u64, n: u64) -> Vec<Option<bool>> {
    (from..from + n)
        .map(|id| ev.bits.get(&id).copied())
        .collect()
}

const MUX_DIRS: &str = r#"{"A": "input", "B": "input", "S": "input", "Y": "output"}"#;
const BINOP_DIRS: &str = r#"{"A": "input", "B": "input", "Y": "output"}"#;
const DFF_DIRS: &str = r#"{"CLK": "input", "D": "input", "Q": "output"}"#;

fn dirs(s: &str) -> Value {
    serde_json::from_str(s).expect("port directions")
}

mod wide_nets_848 {
    use super::*;

    /// A 135-bit register given a value: the evaluator seeds no bit of it (a `u128` cannot carry
    /// it) instead of shifting the value by 128…134.
    #[test]
    fn a_register_wider_than_128_bits_is_left_unknown_not_shifted_past_bit_127() {
        let n = top(
            json!({"y": {"direction": "output", "bits": [4]}}),
            json!({
                "q": {"type": "$dff", "parameters": {}, "port_directions": dirs(DFF_DIRS),
                      "connections": {"CLK": [2], "D": ids(10, 135), "Q": ids(10, 135)}},
                "s": {"type": "$mux", "parameters": {}, "port_directions": dirs(MUX_DIRS),
                      "connections": {"A": ["0"], "B": ["1"], "S": [10], "Y": [4]}}}),
            json!({"r": {"bits": ids(10, 135)}, "y": {"bits": [4]}}),
        );
        let ev = n.evaluate(&m(&[("r", 1)]), &m(&[]));
        assert_eq!(bits(&ev, 10, 135), vec![None; 135], "a u128 cannot carry r");
        assert_eq!(ev.net(&n, "y"), None, "y's selector is r[0]");
    }

    /// An 8-bit sum into a 135-bit result: zero-extended, every bit above 127 included.
    #[test]
    fn an_add_into_a_135_bit_result_extends_the_sum_past_bit_127() {
        let n = top(
            json!({"a": {"direction": "input", "bits": ids(20, 8)},
                   "b": {"direction": "input", "bits": ids(30, 8)},
                   "y": {"direction": "output", "bits": ids(100, 135)}}),
            json!({"add": {"type": "$add", "parameters": {}, "port_directions": dirs(BINOP_DIRS),
                           "connections": {"A": ids(20, 8), "B": ids(30, 8), "Y": ids(100, 135)}}}),
            json!({"a": {"bits": ids(20, 8)}, "b": {"bits": ids(30, 8)},
                   "y": {"bits": ids(100, 135)}}),
        );
        let ev = n.evaluate(&m(&[]), &m(&[("a", 3), ("b", 4)]));
        let mut want = vec![Some(false); 135];
        want[..3].fill(Some(true));
        assert_eq!(bits(&ev, 100, 135), want, "3 + 4 = 7, zero-extended");
    }

    /// A signed 127-bit operand whose top bit is set is negative; sign-extending it must not
    /// subtract `1 << 127` (which overflows).
    #[test]
    fn a_127_bit_signed_operand_with_its_top_bit_set_is_read_as_negative() {
        let n = top(
            json!({"a": {"direction": "input", "bits": ids(1000, 127)},
                   "b": {"direction": "input", "bits": ids(2000, 127)},
                   "y": {"direction": "output", "bits": ids(3000, 127)}}),
            json!({"add": {"type": "$add", "parameters": {"A_SIGNED": 1, "B_SIGNED": 1},
                           "port_directions": dirs(BINOP_DIRS),
                           "connections": {"A": ids(1000, 127), "B": ids(2000, 127),
                                           "Y": ids(3000, 127)}}}),
            json!({"a": {"bits": ids(1000, 127)}, "b": {"bits": ids(2000, 127)},
                   "y": {"bits": ids(3000, 127)}}),
        );
        let ev = n.evaluate(&m(&[]), &m(&[("a", 1 << 126), ("b", 0)]));
        assert_eq!(ev.net(&n, "y"), Some(1 << 126), "-2^126 + 0, as 127 bits");
    }

    /// A shift amount of 2^32 shifts every bit out; truncating it to `u32` made it a shift by 0.
    #[test]
    fn a_shift_by_2_pow_32_shifts_every_bit_out_rather_than_none() {
        let n = top(
            json!({"a": {"direction": "input", "bits": ids(40, 4)},
                   "b": {"direction": "input", "bits": ids(50, 40)},
                   "y": {"direction": "output", "bits": ids(60, 8)}}),
            json!({"sh": {"type": "$shl", "parameters": {}, "port_directions": dirs(BINOP_DIRS),
                          "connections": {"A": ids(40, 4), "B": ids(50, 40), "Y": ids(60, 8)}}}),
            json!({"a": {"bits": ids(40, 4)}, "b": {"bits": ids(50, 40)},
                   "y": {"bits": ids(60, 8)}}),
        );
        let ev = n.evaluate(&m(&[]), &m(&[("a", 1), ("b", 1 << 32)]));
        assert_eq!(ev.net(&n, "y"), Some(0), "1 << 2^32 in 8 bits");
    }

    /// Reading a net wider than 128 bits as one value is refused, not shifted past bit 127.
    #[test]
    fn reading_a_135_bit_net_as_one_value_is_refused_not_a_panic() {
        let n = top(
            json!({}),
            json!({}),
            json!({"c": {"bits": Value::from(vec!["1"; 135])}}),
        );
        let ev = n.evaluate(&m(&[]), &m(&[]));
        assert_eq!(ev.net(&n, "c"), None);
    }

    /// The next value of a register wider than 128 bits is refused the same way.
    #[test]
    fn the_next_value_of_a_135_bit_register_is_refused_not_a_panic() {
        let n = top(
            json!({}),
            json!({"q": {"type": "$dff", "parameters": {}, "port_directions": dirs(DFF_DIRS),
                         "connections": {"CLK": [2], "D": Value::from(vec!["1"; 135]),
                                         "Q": ids(10, 135)}}}),
            json!({"r": {"bits": ids(10, 135)}}),
        );
        let ev = n.evaluate(&m(&[]), &m(&[]));
        assert_eq!(ev.next_of(&n, "r"), None);
    }

    /// Two registers that each fit the carrier, compared as one 140-bit word: the `$eq` leaves
    /// its output unknown (an `i128` word cannot hold the operand), the selector it drives keeps
    /// both arms, and the slice names the cell — not the registers, which were carried fine.
    /// (The sibling the failure-path audit found; it uses `SliceReport::too_wide`, so it compiles
    /// from the commit that introduced the field.)
    #[test]
    fn a_word_operand_wider_than_the_carrier_names_its_cell_in_the_slice() {
        let wide: Vec<u64> = (100..170).chain(200..270).collect();
        let n = top(
            json!({"y": {"direction": "output", "bits": [4]}}),
            json!({
                "q1": {"type": "$dff", "parameters": {}, "port_directions": dirs(DFF_DIRS),
                       "connections": {"CLK": [2], "D": ids(100, 70), "Q": ids(100, 70)}},
                "q2": {"type": "$dff", "parameters": {}, "port_directions": dirs(DFF_DIRS),
                       "connections": {"CLK": [2], "D": ids(200, 70), "Q": ids(200, 70)}},
                "e": {"type": "$eq", "parameters": {}, "port_directions": dirs(BINOP_DIRS),
                      "connections": {"A": wide.clone(), "B": wide, "Y": [5]}},
                "s": {"type": "$mux", "parameters": {}, "port_directions": dirs(MUX_DIRS),
                      "attributes": {"src": "m.sv:5.3-5.20"},
                      "connections": {"A": ["0"], "B": ["1"], "S": [5], "Y": [4]}}}),
            json!({"r1": {"bits": ids(100, 70)}, "r2": {"bits": ids(200, 70)},
                   "y": {"bits": [4]}}),
        );
        let s = n.slice(&[(m(&[("r1", 0), ("r2", 0)]), m(&[]))], &["y".to_string()]);
        assert_eq!(s.too_wide.iter().collect::<Vec<_>>(), vec!["e"]);
    }
}
