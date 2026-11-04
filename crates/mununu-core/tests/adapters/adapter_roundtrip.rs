//! Round-trip tests: TLSF text → parse → IR → emit CTXDSL → parse → realize → eval → synth.
//!
//! Validates that the adapter pipeline produces valid CTXDSL that
//! can be parsed, realized, evaluated, AND synthesized — not just loaded.

use mununu_core::adapter::tlsf::TlsfAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

const LILYDEMO03: &str = r#"
INFO {
  TITLE:       "Lily Demo V3"
  DESCRIPTION: "One of the Lily demo files"
  SEMANTICS:   Mealy
  TARGET:      Mealy
}

MAIN {
  INPUTS {
    req;
    cancel;
    go;
  }

  OUTPUTS {
    grant;
  }

  ASSUMPTIONS {
    G (cancel -> X go);
  }

  INVARIANTS {
    req -> X (grant || X (grant || X grant));
    grant -> X !grant;
    cancel -> X (!grant U go);
  }
}
"#;

/// Helper: translate TLSF, parse + realize CTXDSL, return realized context.
fn translate_and_realize(
    tlsf: &str,
) -> (
    mununu_core::adapter::AdapterOutput,
    mununu_core::context_dsl::realize::RealizedContext,
) {
    let options = AdapterOptions::default();
    let output = TlsfAdapter::translate(tlsf, &options).expect("TLSF translation failed");

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

#[test]
fn tlsf_lilydemo03_full_verification() {
    let (output, realized) = translate_and_realize(LILYDEMO03);

    // Structural checks
    assert_eq!(output.source_info.signal_count, 4);
    assert_eq!(output.source_info.state_count, 32); // 2^(4+1) with turn bit

    let clts = realized
        .context
        .clts("Signals")
        .expect("Signals automaton should exist");
    assert_eq!(
        clts.state_count(),
        32,
        "Should have 2^(4+1) = 32 states (with turn bit)"
    );

    // Formula must exist
    let formula = realized
        .formulas
        .get("syntcomp_prop")
        .expect("syntcomp_prop formula should exist");

    // Environment for the Signals automaton
    let env = realized.environment_for("Signals");

    // EVALUATE: the formula should produce a BitVec result (not panic)
    let eval_result = realized
        .context
        .evaluate_mu("Signals", &formula.formula, &env, None)
        .expect("Formula evaluation should not fail");

    // The eval result is a BitVec of 32 states (with turn bit).
    assert_eq!(
        eval_result.len(),
        32,
        "Eval result should cover all 32 states"
    );

    // SYNTHESIZE: run controller synthesis
    let synth = realized
        .context
        .synthesise_controller("Signals", &formula.formula, &env, None)
        .expect("Controller synthesis should not fail");

    // lilydemo03 is realizable (REF_SIZE: 1 in SYNTCOMP)
    assert!(
        synth.realizable,
        "lilydemo03 should be realizable (SYNTCOMP reference)"
    );

    // Controller should have at least 1 state
    assert!(
        synth.controller.state_count() > 0,
        "Realizable controller should have states"
    );

    // Diagnostics should confirm realizability
    assert!(
        synth
            .diagnostics
            .messages
            .iter()
            .any(|msg| msg.contains("realizable") || msg.contains("Realizable")),
        "Diagnostics should mention realizability: {:?}",
        synth.diagnostics.messages
    );
}

#[test]
fn tlsf_simple_guarantee_eval_and_synth() {
    let input = r#"
INFO {
  TITLE: "SimpleGuarantee"
}
MAIN {
  INPUTS { a; }
  OUTPUTS { b; }
  GUARANTEES {
    G (!a || b);
  }
}
"#;

    let (_output, realized) = translate_and_realize(input);

    let clts = realized.context.clts("Signals").expect("Signals automaton");
    assert_eq!(clts.state_count(), 8, "2 signals + turn bit = 8 states");

    let formula = realized
        .formulas
        .get("syntcomp_prop")
        .expect("syntcomp_prop formula");
    let env = realized.environment_for("Signals");

    // Evaluate
    let eval_result = realized
        .context
        .evaluate_mu("Signals", &formula.formula, &env, None)
        .expect("Eval should succeed");
    assert_eq!(eval_result.len(), 8);

    // Synthesize
    let synth = realized
        .context
        .synthesise_controller("Signals", &formula.formula, &env, None)
        .expect("Synth should succeed");

    // G(!a || b) is realizable: controller sets b whenever a is set
    assert!(
        synth.realizable,
        "G(!a || b) should be realizable — controller just sets b when a is true"
    );
}

#[test]
fn tlsf_unrealizable_spec() {
    // G(a && !a) is trivially unrealizable
    let input = r#"
INFO {
  TITLE: "Unrealizable"
}
MAIN {
  INPUTS { a; }
  OUTPUTS { b; }
  GUARANTEES {
    G (a && !a);
  }
}
"#;

    let (_output, realized) = translate_and_realize(input);
    let formula = realized
        .formulas
        .get("syntcomp_prop")
        .expect("syntcomp_prop");
    let env = realized.environment_for("Signals");

    let eval_result = realized
        .context
        .evaluate_mu("Signals", &formula.formula, &env, None)
        .expect("Eval should succeed even for unsatisfiable formula");

    // G(a && !a) = G(false) — no state satisfies this
    assert_eq!(
        eval_result.count_ones(),
        0,
        "G(a && !a) should be satisfied by 0 states"
    );

    let synth = realized
        .context
        .synthesise_controller("Signals", &formula.formula, &env, None)
        .expect("Synth should succeed");

    assert!(!synth.realizable, "G(a && !a) should be unrealizable");
}
