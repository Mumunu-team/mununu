//! Verifies that the CTXDSL emitter writes `valuations { … }` blocks for states
//! whose `StateSpec.valuations` is set (as adapters like BTOR2 do from
//! cross-product enumeration), and that the resulting CTXDSL re-parses and
//! re-realizes with the valuations preserved on the CLTS.
//!
//! This is the "round-trip" property promised by Phase C/4: a state that
//! carries valuations on the IR side must come back parseable + queryable on
//! the CLTS side after going through emit → parse → realize.

use mununu_core::adapter::SourceFormat;
use mununu_core::adapter::emit::emit;
use mununu_core::adapter::ir::*;
use mununu_core::context_dsl::{self, realize::realize};
use std::collections::BTreeMap;

fn val(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn fixture_ir() -> AdapterIR {
    AdapterIR {
        metadata: Metadata {
            title: "Demo".to_string(),
            source_format: SourceFormat::Btor2,
            description: None,
            game_semantics: None,
            known_status: None,
        },
        signals: vec![],
        automata: vec![AutomatonSpec {
            name: "Demo".to_string(),
            controllable_labels: vec![],
            internal_labels: vec![],
            states: vec![
                StateSpec {
                    name: "s0".to_string(),
                    is_initial: true,
                    valuations: Some(val(&[("empty", "1"), ("full", "0")])),
                    three_valued: None,
                },
                StateSpec {
                    name: "s1".to_string(),
                    is_initial: false,
                    valuations: Some(val(&[("empty", "0"), ("full", "0")])),
                    three_valued: None,
                },
                StateSpec {
                    name: "s2".to_string(),
                    is_initial: false,
                    valuations: Some(val(&[("empty", "0"), ("full", "1"), ("phase", "writing")])),
                    three_valued: None,
                },
            ],
            transitions: vec![
                TransitionSpec {
                    source: "s0".to_string(),
                    target: "s1".to_string(),
                    labels: vec!["wr_en".to_string()],
                    modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
                TransitionSpec {
                    source: "s1".to_string(),
                    target: "s2".to_string(),
                    labels: vec!["wr_en".to_string()],
                    modality: mununu_core::context_dsl::ast::TransitionModalitySpec::Sharp,

                    additional_targets: Vec::new(),
                },
            ],
        }],
        compositions: vec![],
        properties: vec![],
        controller: None,
    }
}

#[test]
fn emit_writes_valuations_block_in_ctxdsl() {
    let ir = fixture_ir();
    let result = emit(&ir).expect("emit succeeds");
    let text = &result.ctxdsl;

    assert!(
        text.contains("valuations {"),
        "emitted CTXDSL must contain a `valuations {{` block. Output was:\n{text}"
    );
    assert!(
        text.contains("empty = 1;"),
        "emitted CTXDSL must contain `empty = 1;`. Output:\n{text}"
    );
    assert!(
        text.contains("full = 1;"),
        "emitted CTXDSL must contain `full = 1;`. Output:\n{text}"
    );
    assert!(
        text.contains("phase = writing;"),
        "emitted CTXDSL must contain `phase = writing;` (identifier value). Output:\n{text}"
    );
}

#[test]
fn emit_parse_realize_roundtrip_preserves_valuations() {
    let ir = fixture_ir();
    let emit_result = emit(&ir).expect("emit succeeds");
    let ctxdsl = &emit_result.ctxdsl;

    // Re-parse the emitted CTXDSL — the parser must accept the
    // `valuations { … }` blocks the emitter produced.
    let doc = context_dsl::parse(ctxdsl).unwrap_or_else(|e| {
        panic!("re-parse of emitted CTXDSL failed: {e:?}\nCTXDSL was:\n{ctxdsl}")
    });

    // Realize and assert the CLTS carries the valuations.
    let realized = realize(&doc, &[]).expect("realize succeeds");
    let clts = realized
        .context
        .clts("Demo")
        .expect("Demo automaton in realized context");

    let s0_id = clts.state_id("s0").expect("s0 state");
    let s0_val = clts
        .state_valuation(s0_id)
        .expect("s0 must have a valuation registered after roundtrip");
    assert_eq!(s0_val.get("empty").map(String::as_str), Some("1"));
    assert_eq!(s0_val.get("full").map(String::as_str), Some("0"));

    let s2_id = clts.state_id("s2").expect("s2 state");
    let s2_val = clts
        .state_valuation(s2_id)
        .expect("s2 must have a valuation registered after roundtrip");
    assert_eq!(s2_val.get("phase").map(String::as_str), Some("writing"));
}
