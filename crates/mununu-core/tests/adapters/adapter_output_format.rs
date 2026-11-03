//! Integration tests for controller output in native formats.
//!
//! Tests the full round-trip: source format → translate → realize → synthesize
//! → emit controller in native format (XState JSON, SystemVerilog module).

use mununu_core::adapter::xstate::XStateAdapter;
use mununu_core::adapter::xstate::emit_controller::controller_to_xstate_json;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

use mununu_core::adapter::systemverilog::emit_controller::controller_to_systemverilog;

// ---------------------------------------------------------------------------
// XState round-trip: JSON → synthesize → XState JSON controller
// ---------------------------------------------------------------------------

const XSTATE_MACHINE: &str = r#"{
    "id": "light",
    "initial": "green",
    "states": {
        "green": { "on": { "TIMER": "yellow" } },
        "yellow": { "on": { "TIMER": "red" } },
        "red": { "on": { "TIMER": "green" } }
    },
    "__mununu": {
        "controllable": ["TIMER"],
        "properties": [
            { "name": "safety", "formula": "nu X. ([] X)", "role": "guarantee" }
        ]
    }
}"#;

#[test]
fn xstate_round_trip_json_output() {
    let options = AdapterOptions::default();
    let output = XStateAdapter::translate(XSTATE_MACHINE, &options).unwrap();
    let doc = context_dsl::parse(&output.ctxdsl).unwrap();
    let realized = context_dsl::realize_context(&doc, &[]).unwrap();

    let formula = realized.formulas.get("safety").unwrap();
    let env = realized.environment_for("light");
    let synth = realized
        .context
        .synthesise_controller("light", &formula.formula, &env, None)
        .unwrap();

    assert!(synth.realizable);

    // Emit as XState JSON
    let json_output = controller_to_xstate_json(&synth.controller, "light", true);

    // Verify valid JSON structure
    let parsed: serde_json::Value = serde_json::from_str(&json_output)
        .unwrap_or_else(|e| panic!("Invalid XState JSON output: {e}\n\n{json_output}"));

    assert_eq!(parsed["id"], "light_controller");
    assert_eq!(parsed["__mununu"]["synthesis_result"], "realizable");
    assert!(!parsed["states"].as_object().unwrap().is_empty());
    assert!(!parsed["initial"].as_str().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// SystemVerilog round-trip: SV → synthesize → SV module controller
// ---------------------------------------------------------------------------

// CTXDSL equivalent of a 4-state handshake FSM (IDLE/WAIT/ACTIVE/DONE).
// S.2b removed the native SV input parser; this test exercises the
// `controller_to_systemverilog` *emitter* (a keep-feature), which is
// input-agnostic — it renders a synthesised `Clts` controller, so any
// CTXDSL source that yields a multi-state controller drives it.
const HANDSHAKE_CTXDSL: &str = r#"
    context handshake {
        alphabet { label req; label noreq; }
        automata {
            automaton handshake {
                controllable { label req; label noreq; }
                states { state IDLE initial; state WAIT; state ACTIVE; state DONE; }
                transitions {
                    transition IDLE -> WAIT on label req;
                    transition IDLE -> IDLE on label noreq;
                    transition WAIT -> ACTIVE on label req;
                    transition WAIT -> ACTIVE on label noreq;
                    transition ACTIVE -> ACTIVE on label req;
                    transition ACTIVE -> DONE on label noreq;
                    transition DONE -> IDLE on label req;
                    transition DONE -> IDLE on label noreq;
                }
            }
        }
        mu_formulas { formula safety { over handshake; body = nu X. ([] X); } }
        controllers { controller c { source handshake; satisfying safety; } }
    }
"#;

#[test]
fn systemverilog_round_trip_sv_output() {
    let doc = context_dsl::parse(HANDSHAKE_CTXDSL).unwrap();
    let realized = context_dsl::realize_context(&doc, &[]).unwrap();

    let formula = realized.formulas.get("safety").unwrap();
    let env = realized.environment_for("handshake");
    let synth = realized
        .context
        .synthesise_controller("handshake", &formula.formula, &env, None)
        .unwrap();

    assert!(synth.realizable);

    // Emit as SystemVerilog module
    let sv_output = controller_to_systemverilog(&synth.controller, "handshake", true);

    // Verify valid SV module structure
    assert!(
        sv_output.contains("module handshake_controller"),
        "Should contain module declaration"
    );
    assert!(
        sv_output.contains("typedef enum"),
        "Should contain enum typedef"
    );
    assert!(
        sv_output.contains("always_ff"),
        "Should contain always_ff block"
    );
    assert!(
        sv_output.contains("case (state)"),
        "Should contain case statement"
    );
    assert!(sv_output.contains("endmodule"), "Should end with endmodule");
    assert!(sv_output.contains("if (rst)"), "Should contain reset logic");
}

// ---------------------------------------------------------------------------
// Cross-format round-trip: same system, different output formats
// ---------------------------------------------------------------------------

#[test]
fn cross_format_same_system_different_outputs() {
    // Use a simple CTXDSL system and export to both formats
    let ctxdsl = r#"
    context test {
        alphabet { label go; label stop; }
        automata {
            automaton FSM {
                controllable { label go; label stop; }
                states { state A initial; state B; }
                transitions {
                    transition A -> B on label go;
                    transition B -> A on label stop;
                }
            }
        }
        mu_formulas {
            formula safety { over FSM; body = nu X. ([] X); }
        }
        controllers { controller c { source FSM; satisfying safety; } }
    }
    "#;

    let doc = context_dsl::parse(ctxdsl).unwrap();
    let realized = context_dsl::realize_context(&doc, &[]).unwrap();
    let formula = realized.formulas.get("safety").unwrap();
    let env = realized.environment_for("FSM");
    let synth = realized
        .context
        .synthesise_controller("FSM", &formula.formula, &env, None)
        .unwrap();

    assert!(synth.realizable);

    // XState JSON output
    let xstate_json = controller_to_xstate_json(&synth.controller, "FSM", true);
    let parsed: serde_json::Value = serde_json::from_str(&xstate_json).unwrap();
    assert_eq!(parsed["__mununu"]["synthesis_result"], "realizable");
    let xstate_states = parsed["states"].as_object().unwrap().len();

    // SystemVerilog output
    let sv = controller_to_systemverilog(&synth.controller, "FSM", true);
    assert!(sv.contains("module FSM_controller"));
    assert!(sv.contains("endmodule"));

    // Both outputs should represent the same number of states
    assert!(xstate_states > 0, "XState output should have states");
    assert!(
        sv.contains("typedef enum"),
        "SV output should have states via enum"
    );
}

// ---------------------------------------------------------------------------
// Unrealizable: both output formats should handle gracefully
// ---------------------------------------------------------------------------

#[test]
fn unrealizable_output_xstate() {
    let json = controller_to_xstate_json(
        &mununu_core::clts::Clts::builder().build().unwrap(),
        "empty",
        false,
    );
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["__mununu"]["synthesis_result"], "unrealizable");
}

#[test]
fn unrealizable_output_systemverilog() {
    let sv = controller_to_systemverilog(
        &mununu_core::clts::Clts::builder().build().unwrap(),
        "empty",
        false,
    );
    assert!(sv.contains("unrealizable"));
    assert!(!sv.contains("module empty_controller"));
}
