use mununu_core::context_dsl;
use mununu_core::context_dsl::realize::realize;

#[test]
fn parses_explicit_controllable_and_internal() {
    const SRC: &str = r#"
context demo {
    alphabet { label a; label b; label tau; }
    automata {
        automaton A {
            controllable { label a; }
            internal { label tau; }
            states { state s0 initial; state s1; }
            transitions {
                transition s0 -> s1 on label a;
                transition s1 -> s1 on label tau;
            }
        }
        automaton B {
            controllable { }
            internal { }
            states { state t0 initial; state t1; }
            transitions { transition t0 -> t1 on label a; }
        }
    }
    composition { asynchronous C { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    realize(&doc, &[]).expect("realization succeeds with explicit controllable/internal");
}

#[test]
fn duplicate_controllable_declaration_fails() {
    const SRC: &str = r#"
context dup_ctrl {
    alphabet { label x; }
    automata {
        automaton A { controllable { label x; } states { state s0 initial; state s1; } transitions { transition s0 -> s1 on label x; } }
        automaton B { controllable { label x; } states { state t0 initial; state t1; } transitions { transition t0 -> t1 on label x; } }
    }
    composition { asynchronous C { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    let err = realize(&doc, &[]).expect_err("realization should fail for duplicate controllable");
    assert!(
        err.to_string().contains("controllable label"),
        "expected controllable label conflict, got: {}",
        err
    );
}

#[test]
fn duplicate_internal_declaration_fails() {
    const SRC: &str = r#"
context dup_internal {
    alphabet { label tau; }
    automata {
        automaton A { internal { label tau; } states { state s0 initial; state s1; } transitions { transition s0 -> s1 on label tau; } }
        automaton B { internal { label tau; } states { state t0 initial; state t1; } transitions { transition t0 -> t1 on label tau; } }
    }
    composition { asynchronous C { members [A, B]; } }
}
"#;
    let doc = context_dsl::parse(SRC).expect("context parses");
    let err = realize(&doc, &[]).expect_err("realization should fail for duplicate internal");
    assert!(
        err.to_string().contains("internal label"),
        "expected internal label conflict, got: {}",
        err
    );
}
