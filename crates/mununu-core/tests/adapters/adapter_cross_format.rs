//! Cross-format regression tests.
//!
//! Verifies that all adapters produce valid CTXDSL that:
//! 1. Parses without error
//! 2. Realizes without error
//! 3. Has expected structural properties (automata, formulas)
//!
//! Also tests auto_translate() format detection.

use mununu_core::adapter::{AdapterOptions, auto_translate};
use mununu_core::context_dsl;

/// Helper: translate content via auto-detection, then parse and realize.
fn auto_translate_and_realize(content: &str) -> mununu_core::context_dsl::realize::RealizedContext {
    let options = AdapterOptions::default();
    let output = auto_translate(content, &options).expect("auto_translate should succeed");

    let doc = context_dsl::parse(&output.ctxdsl).unwrap_or_else(|e| {
        panic!(
            "Generated CTXDSL failed to parse: {e}\n\nCTXDSL:\n{}",
            output.ctxdsl
        )
    });

    context_dsl::realize_context(&doc, &[])
        .unwrap_or_else(|e| panic!("Generated CTXDSL failed to realize: {e}"))
}

// --- Auto-detection tests ---

#[test]
fn auto_detect_tlsf() {
    let content = r#"
INFO { TITLE: "Test" }
MAIN {
    INPUTS { a; }
    OUTPUTS { b; }
    GUARANTEES { G (!a || b); }
}
"#;
    let options = AdapterOptions::default();
    let output = auto_translate(content, &options).expect("should detect TLSF");
    assert_eq!(
        output.source_info.format,
        mununu_core::adapter::SourceFormat::Tlsf
    );
}

#[test]
fn auto_detect_aiger() {
    // Minimal AAG: 1 input, 0 latches, 1 output, 0 gates, 1 bad
    let content = "aag 1 1 0 0 0 1\n2\n2\n";
    let options = AdapterOptions::default();
    let output = auto_translate(content, &options).expect("should detect AIGER");
    assert_eq!(
        output.source_info.format,
        mununu_core::adapter::SourceFormat::Aiger
    );
}

#[test]
fn auto_detect_promela() {
    let content = r#"
bool flag = false;
active proctype P() {
    do :: true -> flag = true; flag = false; od
}
ltl safety { [] !flag }
"#;
    let options = AdapterOptions::default();
    let output = auto_translate(content, &options).expect("should detect Promela");
    assert_eq!(
        output.source_info.format,
        mununu_core::adapter::SourceFormat::Promela
    );
}

#[test]
fn auto_detect_unknown_fails() {
    let content = "this is not a valid format";
    let options = AdapterOptions::default();
    assert!(auto_translate(content, &options).is_err());
}

// --- TLSF round-trip: translate → parse → realize ---

#[test]
fn tlsf_cross_format_roundtrip() {
    let content = r#"
INFO { TITLE: "CrossTest" }
MAIN {
    INPUTS { req; }
    OUTPUTS { grant; }
    ASSUMPTIONS { G (req -> X req); }
    GUARANTEES { G (req -> F grant); }
}
"#;
    let realized = auto_translate_and_realize(content);
    assert!(realized.context.clts("Signals").is_some());
    assert!(realized.formulas.contains_key("syntcomp_prop"));
}

// --- Promela round-trip: translate → parse → realize ---

#[test]
fn promela_cross_format_roundtrip() {
    let content = r#"
bool flag = false;
active proctype Toggle() {
    do :: true -> flag = true; flag = false; od
}
ltl safety { [] !flag }
"#;
    let realized = auto_translate_and_realize(content);
    // Should have at least the CFG automaton
    assert!(
        realized.context.clts("Toggle_cfg").is_some() || realized.context.clts("System").is_some(),
        "Should have process automaton or composed system"
    );
}

// --- Extension-based detection ---

#[test]
fn detect_format_by_extension() {
    use mununu_core::adapter::detect_format_by_extension;
    use std::path::Path;

    assert_eq!(
        detect_format_by_extension(Path::new("spec.tlsf")),
        Some("tlsf")
    );
    assert_eq!(
        detect_format_by_extension(Path::new("circuit.aag")),
        Some("aiger")
    );
    assert_eq!(
        detect_format_by_extension(Path::new("circuit.aig")),
        Some("aiger")
    );
    assert_eq!(
        detect_format_by_extension(Path::new("model.pml")),
        Some("promela")
    );
    assert_eq!(
        detect_format_by_extension(Path::new("model.promela")),
        Some("promela")
    );
    assert_eq!(detect_format_by_extension(Path::new("model.ctxdsl")), None);
    assert_eq!(detect_format_by_extension(Path::new("readme.md")), None);
}
