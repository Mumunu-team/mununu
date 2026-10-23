//! mununu#757 — a property that asks for SOME trace reads the cycle-0 input existentially.
//!
//! The inputs-in-the-state lift samples the cycle-0 input into the initial state, and the exact
//! engine read a verdict as `init ⊆ sat`, which quantified that input universally. A latch that
//! breaks for good on a bad cycle-0 input (`ok = prev ∧ a`, the shape of the assumption monitor
//! `mununu_assume_ok`) made `EF ok` fail at the initial states with `a = 0`, so a `cover`
//! relativized to an input-reading `assume` came back UNREACHABLE although the environment can
//! choose `a = 1`. A property over EVERY trace keeps the universal reading.

use mununu_core::adapter::btor2::symbolic_bitblast::{ExactVerdict, exact_symbolic_verdict};

const LATCH: &str = r#"
1 sort bitvec 1
2 input 1 a
3 state 1 prev_ok
4 one 1
5 init 1 3 4
6 and 1 3 2 ok
7 next 1 3 6
"#;

fn verdict(f: &str) -> ExactVerdict {
    let formula = mununu_core::mu_calculus::parser::parse(f).expect("formula");
    exact_symbolic_verdict(LATCH, &formula).expect("decides")
}

#[test]
fn an_existential_property_reads_the_cycle0_input_existentially() {
    assert_eq!(
        verdict("mu X. (ok || <> X)"),
        ExactVerdict::Holds,
        "`EF ok`: the environment picks `a = 1` at cycle 0"
    );
    assert_eq!(
        verdict("mu X. ((!(ok)) || <> X)"),
        ExactVerdict::Holds,
        "`EF !ok`: and `a = 0` breaks it"
    );
}

#[test]
fn a_universal_property_keeps_the_universal_reading() {
    assert_eq!(
        verdict("nu X. (ok && [] X)"),
        ExactVerdict::Violated,
        "`AG ok` quantifies the cycle-0 input universally"
    );
    assert_eq!(
        verdict("(!(mu X. ((!(ok)) || <> X)))"),
        ExactVerdict::Violated,
        "`!EF !ok` is `AG ok` — the negation flips the first modality to universal"
    );
}
