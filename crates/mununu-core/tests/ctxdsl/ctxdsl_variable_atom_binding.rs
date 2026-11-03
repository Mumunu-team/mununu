//! Regression test for the CTXDSL variable-value atom-binding fix (2026-06-23).
//!
//! Before the fix, a mu-calculus atom of the shape `var == value` over a
//! **hand-authored CTXDSL variable** silently evaluated `false` — the unroll
//! path (`abstraction::unrolling`) incorporated variable values into the state
//! *identity* but never emitted numeric per-state valuations, so the
//! `realize` abstract-states wiring (gated on `clts_valuations_are_numeric`)
//! never fired and the atom fell through the "predicate-not-found → empty
//! bitset" under-approximation. (Demonstrated: even `v == 0` at the initial
//! state where `v = 0` returned false.)
//!
//! The fix has `build_clts_from_unrolled` emit each unrolled `AbstractState`'s
//! `IntConstant` variable bindings via `with_valuation_for_state`, reusing the
//! same numeric-binding machinery the BTOR2 bit-blaster already feeds. This
//! test pins that `var == value` atoms now bind on the realize → evaluate path.

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
        // v == 1 is reachable (after `go`); v == 0 holds at the initial state.
        formula v_reach { over T; body = mu X. (v == 1 || <> X); }
        formula v_init  { over T; body = v == 0; }
    }
}
"#;

#[test]
fn ctxdsl_variable_value_atoms_bind_after_unroll() {
    let doc = context_dsl::parse(SRC).expect("parse");
    let realized = context_dsl::realize_context(&doc, &[]).expect("realize");
    let over = realized
        .context
        .clts_names()
        .first()
        .cloned()
        .expect("at least one realized automaton");
    let clts = realized.context.clts(&over).expect("clts present");
    let env = realized.environment_for(&over);
    let inits: Vec<_> = clts.initial_states().iter().copied().collect();
    assert!(!inits.is_empty(), "automaton has an initial state");

    let holds_at_init = |fname: &str| -> bool {
        let rf = realized.formulas.get(fname).expect("formula declared");
        let result = realized
            .context
            .evaluate_mu(&over, &rf.formula, &env, None)
            .expect("evaluate_mu");
        inits
            .iter()
            .all(|sid| result.get(sid.index()).map(|b| *b).unwrap_or(false))
    };

    assert!(
        holds_at_init("v_reach"),
        "`v == 1` must be reachable — the atom must bind to the unrolled variable value, \
         not fall through to silent-false"
    );
    assert!(
        holds_at_init("v_init"),
        "`v == 0` must hold at the initial state where v = 0 — proves the atom binds"
    );
}
