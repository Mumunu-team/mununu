//! mununu#1106 item 7 — an atom that reads a variable no state of the model carries a value
//! for is REFUSED, naming the variables that do; it used to resolve to `Maybe` and hold at
//! every state (`docs/ctxdsl-modelling-guide.md` §3 documented the behaviour), so a misspelled
//! name made a safety property vacuous and a reachability property spuriously true.

use mununu_core::context_dsl;

const SRC: &str = r#"
context T {
    automata {
        automaton T {
            variables { var v : i64 = 0; }
            states { state A initial; state B; }
            transitions {
                transition A -> B on label go effects { v = 1; };
                transition B -> B on label idle;
            }
        }
    }
    mu_formulas {
        formula v_init   { over T; body = v == 0; }
        formula typo     { over T; body = nosuchvar == 1; }
        formula typo_mu  { over T; body = mu X. (nosuchvar == 1 || <> X); }
        formula two_typo { over T; body = (nosuchvar == 1) && (also_not == v); }
    }
}
"#;

/// REPRODUCER: compiles on the parent (public API only); there `typo` evaluates `Ok` and holds
/// at the initial state, so the `expect_err` fails.
#[test]
fn x1106_an_atom_over_an_undeclared_variable_is_refused_naming_the_declared_ones() {
    let doc = context_dsl::parse(SRC).expect("parse");
    let realized = context_dsl::realize_context(&doc, &[]).expect("realize");
    let over = realized
        .context
        .clts_names()
        .first()
        .cloned()
        .expect("one realized automaton");
    let env = realized.environment_for(&over);
    let eval = |fname: &str| {
        let rf = realized.formulas.get(fname).expect("formula declared");
        realized.context.evaluate_mu(&over, &rf.formula, &env, None)
    };

    let err = eval("typo").expect_err("an atom over `nosuchvar` is refused");
    let msg = err.to_string();
    assert!(
        msg.contains("atom `nosuchvar == 1`")
            && msg.contains("variable `nosuchvar`")
            && msg.contains("would have held at every state")
            && msg.contains("Variables with per-state values: v")
            && msg.contains("bool"),
        "{msg}"
    );
    // Inside a fixpoint the same atom is refused the same way.
    let msg = eval("typo_mu").expect_err("refused under mu").to_string();
    assert!(msg.contains("variable `nosuchvar`"), "{msg}");
    // Several unknown names in one atom are all named, sorted.
    let msg = eval("two_typo").expect_err("refused").to_string();
    assert!(
        msg.contains("variables `also_not`, `nosuchvar`") || msg.contains("variable `nosuchvar`"),
        "{msg}"
    );
    // Control: the declared variable still binds.
    let result = eval("v_init").expect("`v == 0` evaluates");
    let clts = realized.context.clts(&over).expect("clts");
    assert!(
        clts.initial_states()
            .iter()
            .all(|sid| result.get(sid.index()).map(|b| *b).unwrap_or(false)),
        "`v == 0` holds at the initial state"
    );
}
