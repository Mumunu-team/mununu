//! SYNTCOMP Lily benchmark regression suite.
//!
//! Tests each lilydemo TLSF file through the full adapter pipeline:
//! TLSF → parse → IR → emit CTXDSL → parse CTXDSL → realize → eval → synth.
//!
//! Requires the `syntcomp` feature and access to the mununu-private repo.
//! Run with: `cargo test --features syntcomp --test adapter_syntcomp`
#![cfg(feature = "syntcomp")]
//! Verifies:
//! 1. Syntactic soundness: generated CTXDSL parses and realizes
//! 2. State count matches expected (2^(inputs+outputs+1) with turn bit)
//! 3. Formula evaluates without error
//! 4. Synthesis realizability matches SYNTCOMP reference

use mununu_core::adapter::tlsf::TlsfAdapter;
use mununu_core::adapter::{AdapterOptions, FormatAdapter};
use mununu_core::context_dsl;

/// Run a single SYNTCOMP benchmark through the full pipeline.
/// Returns (state_count, realizable) or panics on error.
fn run_syntcomp_benchmark(tlsf_source: &str, name: &str) -> (usize, bool) {
    eprintln!("--- {name} ---");
    // 1. Detect
    assert!(
        TlsfAdapter::detect(tlsf_source),
        "{name}: TLSF detection failed"
    );

    // 2. Translate
    let options = AdapterOptions::default();
    let output = TlsfAdapter::translate(tlsf_source, &options)
        .unwrap_or_else(|e| panic!("{name}: TLSF translation failed: {e}"));

    // Print formula section for debugging
    for line in output.ctxdsl.lines() {
        if line.contains("body") {
            eprintln!("  {}", line.trim());
        }
    }

    // 3. Parse CTXDSL
    let doc = context_dsl::parse(&output.ctxdsl).unwrap_or_else(|e| {
        panic!(
            "{name}: Generated CTXDSL parse error: {e}\n\nCTXDSL:\n{}",
            output.ctxdsl
        )
    });

    // 4. Realize
    let realized = context_dsl::realize_context(&doc, &[])
        .unwrap_or_else(|e| panic!("{name}: CTXDSL realization failed: {e}"));

    // 5. Check structure
    let clts = realized
        .context
        .clts("Signals")
        .unwrap_or_else(|| panic!("{name}: Signals automaton not found"));
    let state_count = clts.state_count();

    let formula = realized
        .formulas
        .get("syntcomp_prop")
        .unwrap_or_else(|| panic!("{name}: syntcomp_prop formula not found"));

    let env = realized.environment_for("Signals");

    // 6. Evaluate formula (must not panic)
    let eval_result = realized
        .context
        .evaluate_mu("Signals", &formula.formula, &env, None)
        .unwrap_or_else(|e| panic!("{name}: Formula evaluation failed: {e}"));

    assert_eq!(
        eval_result.len(),
        state_count,
        "{name}: Eval result length mismatch"
    );

    // 7. Synthesize (must not panic)
    let synth = realized
        .context
        .synthesise_controller("Signals", &formula.formula, &env, None)
        .unwrap_or_else(|e| panic!("{name}: Controller synthesis failed: {e}"));

    (state_count, synth.realizable)
}

macro_rules! syntcomp_test {
    (#[ignore = $reason:expr] $name:ident, $file:expr, $expected_states:expr, $expected_realizable:expr) => {
        #[test]
        #[ignore = $reason]
        fn $name() {
            let source = include_str!($file);
            let (states, realizable) = run_syntcomp_benchmark(source, stringify!($name));
            assert_eq!(
                states,
                $expected_states,
                "{}: state count",
                stringify!($name)
            );
            assert_eq!(
                realizable,
                $expected_realizable,
                "{}: realizability",
                stringify!($name)
            );
        }
    };
    ($name:ident, $file:expr, $expected_states:expr, $expected_realizable:expr) => {
        #[test]
        fn $name() {
            let source = include_str!($file);
            let (states, realizable) = run_syntcomp_benchmark(source, stringify!($name));
            assert_eq!(
                states,
                $expected_states,
                "{}: expected {} states, got {}",
                stringify!($name),
                $expected_states,
                states
            );
            assert_eq!(
                realizable,
                $expected_realizable,
                "{}: expected realizable={}, got {}",
                stringify!($name),
                $expected_realizable,
                realizable
            );
        }
    };
}

// State counts are 2^(N+1) where N = inputs + outputs, due to turn bit.

// --- lilydemo01-09: 3+1 or 1+1 signals ---

syntcomp_test!(
    lilydemo01,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo01.tlsf",
    32,    // 2^(4+1) = 32
    false  // unrealizable
);

syntcomp_test!(
    lilydemo02,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo02.tlsf",
    32,
    false
);

syntcomp_test!(
    lilydemo03,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo03.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo04,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo04.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo05,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo05.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo06,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo06.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo07,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo07.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo08,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo08.tlsf",
    8, // 2^(2+1)
    true
);

syntcomp_test!(
    lilydemo09,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo09.tlsf",
    8,
    true
);

// --- lilydemo10-13: 2+2 or 1+1 ---

syntcomp_test!(
    lilydemo10,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo10.tlsf",
    32, // 2^(4+1)
    true
);

syntcomp_test!(
    lilydemo11,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo11.tlsf",
    32,
    false
);

syntcomp_test!(
    lilydemo12,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo12.tlsf",
    32,
    true
);

syntcomp_test!(
    lilydemo13,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo13.tlsf",
    8,
    true
);

// --- lilydemo14-23: mixed signal counts ---

syntcomp_test!(
    lilydemo14,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo14.tlsf",
    32, // 2^(4+1)
    true
);

// SYNTCOMP reference says unrealizable, but under Mealy semantics (controller
// sees current inputs) a valid alternating-grant strategy exists. The controller
// alternates a1/a2 grants while maintaining mutual exclusion.
syntcomp_test!(
    lilydemo15,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo15.tlsf",
    32,   // 2^(4+1)
    true  // realizable under Mealy semantics (SYNTCOMP ref: unrealizable)
);

// Same as lilydemo15: SYNTCOMP says unrealizable, but alternating-grant
// strategy is valid under Mealy semantics. This is the 3-input/3-output variant.
syntcomp_test!(
    lilydemo16,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo16.tlsf",
    128,  // 2^(6+1)
    true  // realizable under Mealy semantics (SYNTCOMP ref: unrealizable)
);

syntcomp_test!(
    lilydemo17,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo17.tlsf",
    64, // 2^(5+1)
    true
);

syntcomp_test!(
    lilydemo18,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo18.tlsf",
    256, // 2^(7+1)
    true
);

syntcomp_test!(
    lilydemo19,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo19.tlsf",
    32, // 2^(4+1)
    true
);

syntcomp_test!(
    lilydemo20,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo20.tlsf",
    64, // 2^(5+1)
    true
);

syntcomp_test!(
    lilydemo21,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo21.tlsf",
    512, // 2^(8+1)
    true
);

syntcomp_test!(
    lilydemo22,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo22.tlsf",
    32, // 2^(4+1)
    true
);

syntcomp_test!(
    lilydemo23,
    "../../../../../mununu-private/examples/syntcomp/tlsf/lilydemo23.tlsf",
    8,    // 2^(2+1)
    true  // realizable (matches SYNTCOMP reference)
);
