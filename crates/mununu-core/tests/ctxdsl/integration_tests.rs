//! Integration tests for state unrolling with JSON model input.

use mununu_core::abstraction::integration::{JsonToUnrollingConverter, JsonUnrollingOptions};
use mununu_core::abstraction::unrolling::{UnrollingOptions, unroll_states};
use serde_json::json;

#[test]
fn test_json_to_unrolling_conversion() {
    let states = vec![
        json!({"name": "Start", "initial": true}),
        json!({"name": "End", "initial": false}),
    ];

    let transitions = vec![json!({
        "from": "Start",
        "to": "End",
        "label": "complete"
    })];

    let variables = vec![json!({
        "name": "x",
        "type": "i64",
        "initial": 0
    })];

    let original_states = JsonToUnrollingConverter::convert_states_from_json(&states).unwrap();
    let original_transitions =
        JsonToUnrollingConverter::convert_transitions_from_json(&transitions).unwrap();
    let vars = JsonToUnrollingConverter::convert_variables_from_json(&variables).unwrap();

    assert_eq!(original_states.len(), 2);
    assert_eq!(original_transitions.len(), 1);
    assert_eq!(vars.len(), 1);

    // Test unrolling
    let options = UnrollingOptions::default();
    let result = unroll_states(original_states, original_transitions, vars, options).unwrap();
    assert!(!result.states.is_empty());
    assert!(!result.transitions.is_empty());
}

#[test]
fn test_json_unrolling_options() {
    let opts = JsonUnrollingOptions {
        enabled: true,
        max_states_per_location: Some(10),
        use_interval_abstraction: true,
    };

    let unrolling_opts: UnrollingOptions = opts.into();
    assert_eq!(unrolling_opts.max_states_per_location, Some(10));
    assert!(unrolling_opts.use_interval_abstraction);
}
