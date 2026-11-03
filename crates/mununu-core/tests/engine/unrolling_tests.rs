//! Comprehensive tests for state unrolling.

use mununu_core::abstraction::unrolling::{
    Effect, OriginalState, OriginalTransition, UnrollingOptions, VariableDecl, unroll_states,
};

#[test]
fn test_simple_unrolling_no_variables() {
    let states = vec![OriginalState {
        name: "Start".to_string(),
        initial: true,
    }];
    let transitions = vec![OriginalTransition {
        from: "Start".to_string(),
        to: "End".to_string(),
        label: "complete".to_string(),
        guard: None,
        effects: vec![],

        modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

        additional_targets: Vec::new(),
    }];
    let variables = vec![];
    let options = UnrollingOptions::default();

    let result = unroll_states(states, transitions, variables, options).unwrap();
    // Should have at least Start and End states
    assert!(!result.states.is_empty());
    assert!(!result.transitions.is_empty());
}

#[test]
fn test_unrolling_with_bool_variable() {
    let states = vec![
        OriginalState {
            name: "Review".to_string(),
            initial: true,
        },
        OriginalState {
            name: "Approved".to_string(),
            initial: false,
        },
    ];
    let transitions = vec![OriginalTransition {
        from: "Review".to_string(),
        to: "Review".to_string(),
        label: "approve".to_string(),
        guard: None,
        effects: vec![Effect {
            target: "approved".to_string(),
            value_expr: "true".to_string(),
        }],

        modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

        additional_targets: Vec::new(),
    }];
    let variables = vec![VariableDecl {
        name: "approved".to_string(),
        ty: "bool".to_string(),
        initial: Some("false".to_string()),
    }];
    let options = UnrollingOptions::default();

    let result = unroll_states(states, transitions, variables, options).unwrap();
    // Should have Review_approved_false and Review_approved_true
    assert!(result.states.len() >= 2);
    assert!(!result.transitions.is_empty());
}

#[test]
fn test_unrolling_with_integer_variable() {
    let states = vec![OriginalState {
        name: "Processing".to_string(),
        initial: true,
    }];
    let transitions = vec![OriginalTransition {
        from: "Processing".to_string(),
        to: "Processing".to_string(),
        label: "increment".to_string(),
        guard: None,
        effects: vec![Effect {
            target: "count".to_string(),
            value_expr: "count + 1".to_string(),
        }],

        modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

        additional_targets: Vec::new(),
    }];
    let variables = vec![VariableDecl {
        name: "count".to_string(),
        ty: "i64".to_string(),
        initial: Some("0".to_string()),
    }];
    let options = UnrollingOptions {
        max_states_per_location: Some(20), // Limit to prevent infinite unrolling
        ..Default::default()
    };

    // A self-looping incrementing automaton will always exhaust any per-location
    // limit. The unroller must return StateSpaceExplosion, not silently succeed.
    use mununu_core::abstraction::unrolling::UnrollingError;
    let err = unroll_states(states, transitions, variables, options)
        .expect_err("infinite self-loop must trigger the state-limit error");
    assert!(
        matches!(err, UnrollingError::StateSpaceExplosion { .. }),
        "expected StateSpaceExplosion, got: {err:?}"
    );
}

#[test]
fn test_unrolling_with_guard() {
    let states = vec![
        OriginalState {
            name: "Processing".to_string(),
            initial: true,
        },
        OriginalState {
            name: "Complete".to_string(),
            initial: false,
        },
    ];
    let transitions = vec![OriginalTransition {
        from: "Processing".to_string(),
        to: "Complete".to_string(),
        label: "finish".to_string(),
        guard: Some("count >= 5".to_string()),
        effects: vec![],

        modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

        additional_targets: Vec::new(),
    }];
    let variables = vec![VariableDecl {
        name: "count".to_string(),
        ty: "i64".to_string(),
        initial: Some("0".to_string()),
    }];
    let options = UnrollingOptions {
        max_states_per_location: Some(10),
        ..Default::default()
    };

    let result = unroll_states(states, transitions, variables, options)
        .expect("guard-gated unrolling should succeed");
    // The guard `count >= 5` prevents the Processing→Complete transition until
    // count is high enough.  With limit=10 we should see unrolled states.
    assert!(
        !result.states.is_empty(),
        "expected at least one unrolled state"
    );
    // Verify that any transition to Complete only originates from a state
    // where count >= 5 (guard was evaluated, not unconditionally added).
    let complete_transitions: Vec<_> = result
        .transitions
        .iter()
        .filter(|t| t.to.location == "Complete")
        .collect();
    for t in &complete_transitions {
        // The AbstractState display is "Processing_count_<N>"; extract N.
        let count_val: i64 = t
            .from
            .to_string()
            .rsplit('_')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1);
        assert!(
            count_val >= 5,
            "transition to Complete from '{}' violates guard count >= 5",
            t.from
        );
    }
}

#[test]
fn test_unrolling_state_limit() {
    let states = vec![OriginalState {
        name: "Loop".to_string(),
        initial: true,
    }];
    let transitions = vec![OriginalTransition {
        from: "Loop".to_string(),
        to: "Loop".to_string(),
        label: "increment".to_string(),
        guard: None,
        effects: vec![Effect {
            target: "x".to_string(),
            value_expr: "x + 1".to_string(),
        }],

        modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

        additional_targets: Vec::new(),
    }];
    let variables = vec![VariableDecl {
        name: "x".to_string(),
        ty: "i64".to_string(),
        initial: Some("0".to_string()),
    }];
    let options = UnrollingOptions {
        max_states_per_location: Some(3), // Very low limit
        ..Default::default()
    };

    let result = unroll_states(states, transitions, variables, options);
    // Should hit the limit
    assert!(result.is_err());
}
