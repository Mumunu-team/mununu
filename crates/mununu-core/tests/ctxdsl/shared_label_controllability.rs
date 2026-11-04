//! Verifies controllable/internal ownership semantics.

use mununu_core::context_dsl;
use mununu_core::context_dsl::realize::realize;

// With explicit controllable declared on only one automaton and none on the other,
// realization should succeed.
#[test]
fn shared_controllable_label_no_conflict_when_single_owner() {
    const SRC: &str = r#"
context shared_label_conflict {
    alphabet { label x; }
    automata {
        automaton A {
            states { state s0 initial; state s1; }
            transitions { transition s0 -> s1 on label x; }
        }
        automaton B {
            controllable { }
            states { state t0 initial; state t1; }
            transitions { transition t0 -> t1 on label x; }
        }
    }
    composition { asynchronous Shared { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    realize(&doc, &[]).expect("realization should succeed with single controllable owner");
}

#[test]
fn duplicate_controllable_ownership_rejected() {
    const SRC: &str = r#"
context duplicate_ctrl {
    alphabet { label x; }
    automata {
        automaton A {
            controllable { label x; }
            states { state s0 initial; state s1; }
            transitions { transition s0 -> s1 on label x; }
        }
        automaton B {
            controllable { label x; }
            states { state t0 initial; state t1; }
            transitions { transition t0 -> t1 on label x; }
        }
    }
    composition { asynchronous C { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    let err = realize(&doc, &[]).expect_err("realization should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("controllable label"),
        "expected controllable label conflict, got: {}",
        msg
    );
}

#[test]
fn single_owner_shared_label_allows_realization() {
    const SRC: &str = r#"
context single_owner_ok {
    alphabet { label x; }
    automata {
        automaton A {
            controllable { label x; }
            states { state s0 initial; state s1; }
            transitions { transition s0 -> s1 on label x; }
        }
        automaton B {
            controllable { }
            states { state t0 initial; state t1; }
            transitions { transition t0 -> t1 on label x; }
        }
    }
    composition { asynchronous C { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    realize(&doc, &[]).expect("realization should succeed with single controllable owner");
}
