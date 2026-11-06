//! mununu#857 — a register or input on the slice's walk that the trace gives no value stays
//! unknown (which keeps every arm it feeds), so the slice is wider than the values would make
//! it; `slice_status` must say so (`widened`, the name in the reason) instead of `exact`.
//!
//! Its own top-level integration test (target `slice_missing_value_857`), outside the files the
//! fix touches, and it reads only the report surface the parent already has
//! (`TraceNames::attach_slice` → `slice_status`), so `make verify-repro` builds the parent and
//! sees it fail there.

use mununu_core::adapter::yosys::netlist::Netlist;
use mununu_core::trace::{CexCellView, Trace, TraceKind, TraceNames, TraceStep};
use serde_json::{Value, json};
use std::sync::Arc;

/// A 4-bit register `r` and a 1-bit input `i`; `y = r[0] ? 1 : 0` and `z = i ? 1 : 0`, each
/// through a `$mux` with a source line, so the slice has lines to show.
fn netlist() -> Netlist {
    let dirs = |s: &str| -> Value { serde_json::from_str(s).expect("dirs") };
    let mux = r#"{"A": "input", "B": "input", "S": "input", "Y": "output"}"#;
    let dff = r#"{"CLK": "input", "D": "input", "Q": "output"}"#;
    let j = json!({"modules": {"m": {
        "attributes": {"top": 1},
        "ports": {"i": {"direction": "input", "bits": [20]},
                  "y": {"direction": "output", "bits": [4]},
                  "z": {"direction": "output", "bits": [5]}},
        "cells": {
            "q": {"type": "$dff", "parameters": {}, "port_directions": dirs(dff),
                  "connections": {"CLK": [2], "D": [10, 11, 12, 13], "Q": [10, 11, 12, 13]}},
            "sy": {"type": "$mux", "parameters": {}, "attributes": {"src": "m.sv:5.3-5.20"},
                   "port_directions": dirs(mux),
                   "connections": {"A": ["0"], "B": ["1"], "S": [10], "Y": [4]}},
            "sz": {"type": "$mux", "parameters": {}, "attributes": {"src": "m.sv:6.3-6.20"},
                   "port_directions": dirs(mux),
                   "connections": {"A": ["0"], "B": ["1"], "S": [20], "Y": [5]}}},
        "netnames": {"r": {"bits": [10, 11, 12, 13]}, "i": {"bits": [20]},
                     "y": {"bits": [4]}, "z": {"bits": [5]}}}}});
    Netlist::parse(&j.to_string()).expect("parses")
}

fn cell(register: &str, value: u64, bits: u32) -> CexCellView {
    CexCellView {
        register: register.into(),
        value: Some(value),
        hex: format!("0x{value:x}"),
        bits,
    }
}

/// The status `attach_slice` gives a one-cycle trace with these state and input cells.
fn status(state: Vec<CexCellView>, inputs: Vec<CexCellView>, seed: &str) -> (String, String) {
    let names = TraceNames {
        netlist: Some(Arc::new(netlist())),
        ..TraceNames::default()
    };
    let mut trace = Trace {
        kind: TraceKind::Attack,
        steps: vec![TraceStep {
            cycle: 0,
            state,
            state_name: None,
            inputs,
            faults: Vec::new(),
            labels: Vec::new(),
            monitor: Vec::new(),
        }],
        loop_start: None,
        unreachable_target: Vec::new(),
        replay: None,
        sources: Vec::new(),
        slice: Vec::new(),
        slice_status: None,
        repair_candidates: Vec::new(),
        localization: None,
    };
    names.attach_slice(&mut trace, &[seed.to_string()]);
    let s = trace.slice_status.expect("a status is always attached");
    (s.status, s.reason.unwrap_or_default())
}

mod slice_missing_value_857 {
    use super::*;

    /// The register the selector reads has no cell in the trace: the slice keeps both arms,
    /// and the status says why instead of `exact`.
    #[test]
    fn a_register_with_no_value_on_the_walk_widens_the_slice_and_is_named() {
        let (st, reason) = status(Vec::new(), Vec::new(), "y");
        assert_eq!(st, "widened", "{reason}");
        assert!(
            reason.contains("no value") && reason.ends_with(": r"),
            "{reason}"
        );
    }

    /// The same for an input port the trace does not assign.
    #[test]
    fn an_input_with_no_value_on_the_walk_widens_the_slice_and_is_named() {
        let (st, reason) = status(vec![cell("r", 1, 4)], Vec::new(), "z");
        assert_eq!(st, "widened", "{reason}");
        assert!(
            reason.contains("no value") && reason.ends_with(": i"),
            "{reason}"
        );
    }

    /// Control: every value the walk needs is there — the slice is exact, and a register the
    /// trace omits but the walk never reaches (`r`, for seed `z`) is not reported.
    #[test]
    fn a_value_the_walk_never_reads_does_not_make_the_slice_widened() {
        assert_eq!(status(Vec::new(), vec![cell("i", 1, 1)], "z").0, "exact");
        assert_eq!(status(vec![cell("r", 1, 4)], Vec::new(), "y").0, "exact");
    }
}
