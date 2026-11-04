//! AIGER adapter round-trip tests.
//!
//! Tests: AAG source → parse → IR → emit CTXDSL → parse CTXDSL → realize → eval → synth.

use mununu_core::adapter::aiger::AigerAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

/// Alarm circuit: sensor input, alarm latch, bad = alarm_on.
/// next_alarm = sensor OR alarm (latching alarm).
const ALARM_AAG: &str = "\
aag 3 1 1 0 1 1
2
4 7
4
6 3 5
i0 sensor
l0 alarm
b0 alarm_on
";

#[test]
fn aiger_alarm_roundtrip_eval_synth() {
    // Detect
    assert!(AigerAdapter::detect(ALARM_AAG));

    // Translate
    let options = AdapterOptions::default();
    let output = AigerAdapter::translate(ALARM_AAG, &options).expect("AIGER translation failed");

    assert_eq!(output.source_info.state_count, 2); // 1 latch = 2 states
    assert!(output.source_info.property_count > 0);

    // Parse CTXDSL
    let doc = context_dsl::parse(&output.ctxdsl)
        .unwrap_or_else(|e| panic!("CTXDSL parse error: {e}\n\nCTXDSL:\n{}", output.ctxdsl));

    // Realize
    let realized =
        context_dsl::realize_context(&doc, &[]).unwrap_or_else(|e| panic!("Realize error: {e}"));

    let clts = realized.context.clts("Circuit").expect("Circuit automaton");
    assert_eq!(clts.state_count(), 2, "1 latch = 2 states");

    // Check safety formula exists
    let formula = realized
        .formulas
        .get("safety_alarm_on")
        .expect("safety_alarm_on formula");

    let env = realized.environment_for("Circuit");

    // Evaluate: the alarm eventually turns on (bad state reachable)
    let eval_result = realized
        .context
        .evaluate_mu("Circuit", &formula.formula, &env, None)
        .expect("Eval should succeed");

    assert_eq!(eval_result.len(), 2);

    // Synthesize: should be unrealizable (sensor is uncontrollable,
    // alarm will eventually turn on)
    let synth = realized
        .context
        .synthesise_controller("Circuit", &formula.formula, &env, None)
        .expect("Synth should succeed");

    // The alarm is reachable: once sensor fires, alarm latches on.
    // Safety property (alarm never on) should be unrealizable with
    // uncontrollable sensor.
    assert!(
        !synth.realizable,
        "Alarm safety should be unrealizable — sensor is uncontrollable"
    );
}

/// Trivial circuit: no inputs, no latches, no gates.
/// Just tests the adapter handles edge cases.
#[test]
fn aiger_empty_circuit() {
    let aag = "aag 0 0 0 0 0\n";
    assert!(AigerAdapter::detect(aag));

    let options = AdapterOptions::default();
    let output = AigerAdapter::translate(aag, &options).expect("Empty circuit should translate");
    assert_eq!(output.source_info.state_count, 1); // 0 latches = 2^0 = 1 state
}
