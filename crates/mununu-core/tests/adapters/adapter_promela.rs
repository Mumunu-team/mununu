//! Promela adapter round-trip tests.
//!
//! Tests: Promela → parse → CFG → IR → emit CTXDSL → parse CTXDSL → realize.

use mununu_core::adapter::promela::PromelaAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

const PETERSON: &str = r#"
byte turn = 0;
bool flag0 = false;
bool flag1 = false;
bool cs0 = false;
bool cs1 = false;

active proctype P0() {
    do
    :: true ->
        flag0 = true;
        turn = 1;
        (flag1 == false || turn == 0);
        cs0 = true;
        cs0 = false;
        flag0 = false;
    od
}

active proctype P1() {
    do
    :: true ->
        flag1 = true;
        turn = 0;
        (flag0 == false || turn == 1);
        cs1 = true;
        cs1 = false;
        flag1 = false;
    od
}

ltl mutex { [] !(cs0 && cs1) }
"#;

#[test]
fn promela_detect() {
    assert!(PromelaAdapter::detect(PETERSON));
    assert!(!PromelaAdapter::detect("aag 3 1 1 0 1 1\n"));
    assert!(!PromelaAdapter::detect("INFO {\n  TITLE: \"test\"\n}"));
}

#[test]
fn promela_peterson_roundtrip() {
    let options = AdapterOptions::default();
    let output = PromelaAdapter::translate(PETERSON, &options).expect("Promela translation failed");

    // Should have translated variables and processes
    assert!(output.source_info.signal_count > 0);
    assert!(output.source_info.property_count > 0);

    // Parse the generated CTXDSL
    let doc = context_dsl::parse(&output.ctxdsl).unwrap_or_else(|e| {
        panic!(
            "Generated CTXDSL failed to parse: {e}\n\nCTXDSL:\n{}",
            output.ctxdsl
        )
    });

    // Realize
    let realized = context_dsl::realize_context(&doc, &[])
        .unwrap_or_else(|e| panic!("Generated CTXDSL failed to realize: {e}"));

    // Should have automata for P0_cfg, P1_cfg, Var_turn, Var_flag0, etc.
    let p0 = realized.context.clts("P0_cfg");
    assert!(p0.is_some(), "P0_cfg automaton should exist");

    let p1 = realized.context.clts("P1_cfg");
    assert!(p1.is_some(), "P1_cfg automaton should exist");
}

#[test]
fn promela_simple_process() {
    let input = r#"
bool x = false;

active proctype Toggle() {
    do
    :: true ->
        x = true;
        x = false;
    od
}
"#;

    let options = AdapterOptions::default();
    let output = PromelaAdapter::translate(input, &options).expect("Translation failed");

    let doc = context_dsl::parse(&output.ctxdsl)
        .unwrap_or_else(|e| panic!("Parse failed: {e}\n\nCTXDSL:\n{}", output.ctxdsl));

    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize failed: {e}"));

    let toggle = realized
        .context
        .clts("Toggle_cfg")
        .expect("Toggle_cfg automaton");
    assert!(toggle.state_count() > 0, "Toggle CFG should have states");
}
