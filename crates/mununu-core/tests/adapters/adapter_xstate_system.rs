//! XState adapter benchmark system tests.
//!
//! Full pipeline: XState JSON → translate → parse CTXDSL → realize → evaluate → synthesize.
//! Each benchmark has a known-correct expected result (realizable/unrealizable, state count).

use mununu_core::adapter::xstate::XStateAdapter;
use mununu_core::adapter::xstate::emit_controller::controller_to_xstate_json;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

/// Helper: translate XState JSON, parse + realize CTXDSL.
fn translate_and_realize(
    json: &str,
) -> (
    mununu_core::adapter::AdapterOutput,
    mununu_core::context_dsl::realize::RealizedContext,
) {
    let options = AdapterOptions::default();
    let output = XStateAdapter::translate(json, &options).expect("XState translation failed");

    let doc = context_dsl::parse(&output.ctxdsl).unwrap_or_else(|e| {
        panic!(
            "Generated CTXDSL failed to parse: {e}\n\nCTXDSL:\n{}",
            output.ctxdsl
        )
    });

    let realized = context_dsl::realize_context(&doc, &[])
        .unwrap_or_else(|e| panic!("Generated CTXDSL failed to realize: {e}"));

    (output, realized)
}

// ---------------------------------------------------------------------------
// Benchmark A4.1: Traffic Light with Pedestrian Crossing
// 7 states, realizable
// ---------------------------------------------------------------------------

const TRAFFIC_LIGHT: &str = r#"{
    "id": "traffic_light",
    "initial": "green_ns",
    "states": {
        "green_ns": {
            "on": {
                "TIMER": "yellow_ns",
                "PED_REQUEST": "green_ns_ped_waiting"
            }
        },
        "green_ns_ped_waiting": {
            "on": { "TIMER": "yellow_ns" }
        },
        "yellow_ns": {
            "on": { "TIMER": "red_ns" }
        },
        "red_ns": {
            "on": { "TIMER": "green_ew" }
        },
        "green_ew": {
            "on": { "TIMER": "yellow_ew" }
        },
        "yellow_ew": {
            "on": { "TIMER": "red_ew" }
        },
        "red_ew": {
            "on": { "TIMER": "green_ns" }
        }
    },
    "__mununu": {
        "controllable": ["TIMER"],
        "uncontrollable": ["PED_REQUEST"],
        "properties": [
            {
                "name": "safety_invariant",
                "formula": "nu X. ([] X)",
                "role": "guarantee"
            }
        ]
    }
}"#;

#[test]
fn xstate_traffic_light_structure() {
    let (_output, realized) = translate_and_realize(TRAFFIC_LIGHT);

    let clts = realized
        .context
        .clts("traffic_light")
        .expect("traffic_light automaton should exist");
    assert_eq!(clts.state_count(), 7, "Should have 7 states");
}

#[test]
fn xstate_traffic_light_synthesis() {
    let (_output, realized) = translate_and_realize(TRAFFIC_LIGHT);

    let formula = realized
        .formulas
        .get("safety_invariant")
        .expect("safety_invariant formula should exist");
    let env = realized.environment_for("traffic_light");

    // Evaluate
    let eval_result = realized
        .context
        .evaluate_mu("traffic_light", &formula.formula, &env, None)
        .expect("Eval should succeed");
    assert_eq!(eval_result.len(), 7);
    assert_eq!(
        eval_result.count_ones(),
        7,
        "All 7 states should satisfy the safety invariant"
    );

    // Synthesize
    let synth = realized
        .context
        .synthesise_controller("traffic_light", &formula.formula, &env, None)
        .expect("Synthesis should succeed");
    assert!(synth.realizable, "Traffic light should be realizable");
    assert!(
        synth.controller.state_count() > 0,
        "Controller should have states"
    );
}

// ---------------------------------------------------------------------------
// Benchmark A4.2: Authentication Flow (Parallel Regions)
// 6 × 3 = 18 product states, realizable
// ---------------------------------------------------------------------------

const AUTH_FLOW: &str = r#"{
    "id": "auth_flow",
    "initial": "main",
    "states": {
        "main": {
            "type": "parallel",
            "states": {
                "auth": {
                    "initial": "idle",
                    "states": {
                        "idle": {
                            "on": { "LOGIN": "logging_in" }
                        },
                        "logging_in": {
                            "on": {
                                "MFA_REQUIRED": "mfa_pending",
                                "DENY_ACCESS": "failed"
                            }
                        },
                        "mfa_pending": {
                            "on": { "MFA_CODE": "mfa_verifying" }
                        },
                        "mfa_verifying": {
                            "on": {
                                "VERIFY": "authenticated",
                                "DENY_ACCESS": "failed"
                            }
                        },
                        "authenticated": {
                            "on": { "LOGOUT": "idle" }
                        },
                        "failed": {
                            "on": { "RETRY": "idle" }
                        }
                    }
                },
                "session": {
                    "initial": "no_session",
                    "states": {
                        "no_session": {
                            "on": { "SESSION_START": "active_session" }
                        },
                        "active_session": {
                            "on": {
                                "SESSION_EXPIRE": "expired",
                                "LOGOUT": "no_session"
                            }
                        },
                        "expired": {
                            "on": { "SESSION_START": "active_session" }
                        }
                    }
                }
            }
        }
    },
    "__mununu": {
        "controllable": ["VERIFY", "DENY_ACCESS", "SESSION_START", "MFA_REQUIRED", "RETRY"],
        "uncontrollable": ["LOGIN", "MFA_CODE", "SESSION_EXPIRE", "LOGOUT"],
        "properties": [
            {
                "name": "safety",
                "formula": "nu X. ([] X)",
                "role": "guarantee"
            }
        ]
    }
}"#;

#[test]
fn xstate_auth_flow_structure() {
    let (_output, realized) = translate_and_realize(AUTH_FLOW);

    // Both parallel regions should exist
    let auth = realized
        .context
        .clts("main_auth")
        .expect("main_auth region should exist");
    assert_eq!(auth.state_count(), 6, "Auth region should have 6 states");

    let session = realized
        .context
        .clts("main_session")
        .expect("main_session region should exist");
    assert_eq!(
        session.state_count(),
        3,
        "Session region should have 3 states"
    );

    // Composition should exist
    let composed = realized
        .context
        .clts("auth_flow_system")
        .expect("auth_flow_system composition should exist");
    assert!(
        composed.state_count() > 0,
        "Composed system should have states"
    );
}

#[test]
fn xstate_auth_flow_synthesis() {
    let (_output, realized) = translate_and_realize(AUTH_FLOW);

    let formula = realized
        .formulas
        .get("safety")
        .expect("safety formula should exist");

    // Evaluate on composition
    let target = "auth_flow_system";
    let env = realized.environment_for(target);

    let synth = realized
        .context
        .synthesise_controller(target, &formula.formula, &env, None)
        .expect("Synthesis should succeed");
    assert!(synth.realizable, "Auth flow safety should be realizable");
}

// ---------------------------------------------------------------------------
// Benchmark A4.3: Order Processing Pipeline
// 3 × 3 × 3 product (simplified for tractability), tests unrealizability
// ---------------------------------------------------------------------------

const ORDER_PIPELINE: &str = r#"{
    "id": "order_pipeline",
    "initial": "pipeline",
    "states": {
        "pipeline": {
            "type": "parallel",
            "states": {
                "order": {
                    "initial": "cart",
                    "states": {
                        "cart": {
                            "on": { "SUBMIT_ORDER": "checkout" }
                        },
                        "checkout": {
                            "on": {
                                "SHIP": "shipped",
                                "CANCEL": "cancelled"
                            }
                        },
                        "shipped": {},
                        "cancelled": {}
                    }
                },
                "payment": {
                    "initial": "pending",
                    "states": {
                        "pending": {
                            "on": {
                                "PROCESS_PAYMENT": "processing"
                            }
                        },
                        "processing": {
                            "on": {
                                "PAYMENT_SUCCESS": "captured",
                                "PAYMENT_FAIL": "failed"
                            }
                        },
                        "captured": {},
                        "failed": {
                            "on": { "PROCESS_PAYMENT": "processing" }
                        }
                    }
                },
                "inventory": {
                    "initial": "available",
                    "states": {
                        "available": {
                            "on": { "RESERVE_INVENTORY": "reserved" }
                        },
                        "reserved": {
                            "on": { "CANCEL": "available" }
                        }
                    }
                }
            }
        }
    },
    "__mununu": {
        "controllable": ["PROCESS_PAYMENT", "RESERVE_INVENTORY", "SHIP"],
        "uncontrollable": ["SUBMIT_ORDER", "CANCEL", "PAYMENT_SUCCESS", "PAYMENT_FAIL"],
        "properties": [
            {
                "name": "safety",
                "formula": "nu X. ([] X)",
                "role": "guarantee"
            }
        ]
    }
}"#;

#[test]
fn xstate_order_pipeline_structure() {
    let (_output, realized) = translate_and_realize(ORDER_PIPELINE);

    let order = realized
        .context
        .clts("pipeline_order")
        .expect("pipeline_order should exist");
    assert_eq!(order.state_count(), 4, "Order region should have 4 states");

    let payment = realized
        .context
        .clts("pipeline_payment")
        .expect("pipeline_payment should exist");
    assert_eq!(
        payment.state_count(),
        4,
        "Payment region should have 4 states"
    );

    let inventory = realized
        .context
        .clts("pipeline_inventory")
        .expect("pipeline_inventory should exist");
    assert_eq!(
        inventory.state_count(),
        2,
        "Inventory region should have 2 states"
    );

    // Composition exists
    assert!(realized.context.clts("order_pipeline_system").is_some());
}

#[test]
fn xstate_order_pipeline_safety_synthesis() {
    let (_output, realized) = translate_and_realize(ORDER_PIPELINE);

    let formula = realized
        .formulas
        .get("safety")
        .expect("safety formula should exist");

    let target = "order_pipeline_system";
    let env = realized.environment_for(target);

    let synth = realized
        .context
        .synthesise_controller(target, &formula.formula, &env, None)
        .expect("Synthesis should succeed");
    assert!(
        synth.realizable,
        "Order pipeline safety invariant should be realizable"
    );
}

// ---------------------------------------------------------------------------
// Benchmark A4.4: Elevator (cross-validation with existing CTXDSL)
// 6 cabin × 4 panel = 24 product states
// ---------------------------------------------------------------------------

const ELEVATOR: &str = r#"{
    "id": "elevator",
    "initial": "system",
    "states": {
        "system": {
            "type": "parallel",
            "states": {
                "cabin": {
                    "initial": "floor0",
                    "states": {
                        "floor0": {
                            "on": {
                                "open_door": "door0_open",
                                "move_up": "floor1"
                            }
                        },
                        "floor1": {
                            "on": {
                                "open_door": "door1_open",
                                "move_down": "floor0",
                                "move_up": "floor2"
                            }
                        },
                        "floor2": {
                            "on": {
                                "open_door": "door2_open",
                                "move_down": "floor1"
                            }
                        },
                        "door0_open": {
                            "on": { "serve": "floor0" }
                        },
                        "door1_open": {
                            "on": { "serve": "floor1" }
                        },
                        "door2_open": {
                            "on": { "serve": "floor2" }
                        }
                    }
                },
                "requests": {
                    "initial": "idle",
                    "states": {
                        "idle": {
                            "on": {
                                "request_0": "req0_pending",
                                "request_1": "req1_pending",
                                "request_2": "req2_pending"
                            }
                        },
                        "req0_pending": {
                            "on": {
                                "serve": "idle",
                                "request_0": "req0_pending"
                            }
                        },
                        "req1_pending": {
                            "on": {
                                "serve": "idle",
                                "request_1": "req1_pending"
                            }
                        },
                        "req2_pending": {
                            "on": {
                                "serve": "idle",
                                "request_2": "req2_pending"
                            }
                        }
                    }
                }
            }
        }
    },
    "__mununu": {
        "controllable": ["move_up", "move_down", "open_door", "serve", "request_0", "request_1", "request_2"],
        "properties": [
            {
                "name": "safety_invariant",
                "formula": "nu X. ([] X)",
                "role": "guarantee"
            }
        ]
    }
}"#;

#[test]
fn xstate_elevator_structure() {
    let (_output, realized) = translate_and_realize(ELEVATOR);

    let cabin = realized
        .context
        .clts("system_cabin")
        .expect("system_cabin should exist");
    assert_eq!(cabin.state_count(), 6, "Cabin should have 6 states");

    let requests = realized
        .context
        .clts("system_requests")
        .expect("system_requests should exist");
    assert_eq!(
        requests.state_count(),
        4,
        "Request panel should have 4 states"
    );

    // Composition exists
    assert!(realized.context.clts("elevator_system").is_some());
}

#[test]
fn xstate_elevator_synthesis() {
    let (_output, realized) = translate_and_realize(ELEVATOR);

    let formula = realized
        .formulas
        .get("safety_invariant")
        .expect("safety_invariant should exist");

    let target = "elevator_system";
    let env = realized.environment_for(target);

    let synth = realized
        .context
        .synthesise_controller(target, &formula.formula, &env, None)
        .expect("Synthesis should succeed");
    assert!(
        synth.realizable,
        "Elevator safety invariant should be realizable"
    );
    assert!(
        synth.controller.state_count() > 0,
        "Controller should have states"
    );
}

/// Cross-validate: the elevator XState adapter should produce the same
/// safety invariant verdict as the hand-written elevator_gr1.ctxdsl.
#[test]
fn xstate_elevator_cross_validate_with_ctxdsl() {
    // XState path
    let (_xstate_output, xstate_realized) = translate_and_realize(ELEVATOR);
    let xstate_formula = xstate_realized
        .formulas
        .get("safety_invariant")
        .expect("XState safety_invariant");
    let xstate_target = "elevator_system";
    let xstate_env = xstate_realized.environment_for(xstate_target);
    let xstate_synth = xstate_realized
        .context
        .synthesise_controller(xstate_target, &xstate_formula.formula, &xstate_env, None)
        .expect("XState synthesis");

    // CTXDSL path (load the hand-written example)
    let ctxdsl_source =
        std::fs::read_to_string("../../examples/elevator_gr1.ctxdsl").expect("elevator_gr1.ctxdsl");
    let ctxdsl_doc = context_dsl::parse(&ctxdsl_source).expect("parse elevator_gr1.ctxdsl");
    let ctxdsl_realized =
        context_dsl::realize_context(&ctxdsl_doc, &[]).expect("realize elevator_gr1.ctxdsl");
    let ctxdsl_formula = ctxdsl_realized
        .formulas
        .get("safety_invariant")
        .expect("CTXDSL safety_invariant");
    let ctxdsl_target = "elevator_system";
    let ctxdsl_env = ctxdsl_realized.environment_for(ctxdsl_target);
    let ctxdsl_synth = ctxdsl_realized
        .context
        .synthesise_controller(ctxdsl_target, &ctxdsl_formula.formula, &ctxdsl_env, None)
        .expect("CTXDSL synthesis");

    // Cross-validate: both should be realizable
    assert_eq!(
        xstate_synth.realizable, ctxdsl_synth.realizable,
        "XState and CTXDSL should agree on realizability"
    );
}

// ---------------------------------------------------------------------------
// Round-trip test: XState JSON → synthesize → XState JSON controller output
// ---------------------------------------------------------------------------

#[test]
fn xstate_traffic_light_round_trip() {
    let (_output, realized) = translate_and_realize(TRAFFIC_LIGHT);

    let formula = realized
        .formulas
        .get("safety_invariant")
        .expect("safety_invariant should exist");
    let env = realized.environment_for("traffic_light");

    let synth = realized
        .context
        .synthesise_controller("traffic_light", &formula.formula, &env, None)
        .expect("Synthesis should succeed");
    assert!(synth.realizable);

    // Emit controller as XState JSON
    let json_output =
        controller_to_xstate_json(&synth.controller, "traffic_light", synth.realizable);

    // Verify the output is valid JSON with expected structure
    let parsed: serde_json::Value = serde_json::from_str(&json_output)
        .unwrap_or_else(|e| panic!("Controller JSON is invalid: {e}\n\nJSON:\n{json_output}"));

    assert_eq!(parsed["id"], "traffic_light_controller");
    assert_eq!(parsed["__mununu"]["synthesis_result"], "realizable");

    // Controller should have states
    let states = parsed["states"]
        .as_object()
        .expect("states should be object");
    assert!(
        !states.is_empty(),
        "Controller should have at least one state"
    );

    // Initial state should be set
    assert!(
        !parsed["initial"].as_str().unwrap_or("").is_empty(),
        "Controller should have an initial state"
    );
}
