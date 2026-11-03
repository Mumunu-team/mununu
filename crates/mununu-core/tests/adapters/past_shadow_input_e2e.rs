//! Regression: a `$past` over a primary INPUT must be DECIDED, not skipped.
//!
//! The data-integrity shape — "what was pushed is what got stored" —
//! is written `push |=> d0_q == $past(din)`, and `din` is a module input.
//! Until the `resolve_shadow_source` fix, the BTOR2 shadow synthesiser only
//! accepted a base that resolved to a *state cell*, so `din__past` never got a
//! flop and every property of this shape came back `Skipped`. That is the worst
//! kind of gap: the run is green and nothing was checked.
//!
//! Two things are asserted here that a unit test on `augment_with_past_shadows`
//! structurally cannot:
//!
//!   1. the fix is REACHABLE — `verify_auto` pre-filters the shadow bases before
//!      calling the augmenter, and for a while that filter still asked
//!      `resolve_state_by_symbol`, dropping input bases before they arrived. The
//!      unit tests passed the whole time because they call the augmenter
//!      directly. Only a test through `verify_auto` sees the filter.
//!   2. the property is REAL — a contrast twin with the capture corrupted must
//!      come back `Violated`. A property that cannot be made to fail is not
//!      being checked (the same two-sided gate the recoverability examples use).
//!
//! Two pre-existing defects were found while writing this and are NOT addressed
//! here — both reproduce identically on a REGISTER-sourced `$past`, so neither is
//! introduced by input support, and both look like one root cause: an appended
//! shadow flop breaking the KMTS `must ⊑ may` containment.
//!
//!   * `symbolic_engine: true` over any `$past` model panics at
//!     `mu_calculus/symbolic.rs:179`, "TritBdd invariant must ⊑ may".
//!   * `must_edge_inference: SmtPerTarget` reports `Violated` for a CORRECT
//!     design carrying a `$past` property — a false positive. A property with no
//!     `$past` is unaffected in both postures.
//!
//! Docker-gated (`mununu-sva`): needs slang + yosys. Run with `--ignored`.

use mununu_core::adapter::slang::verify_auto::{VerifyAutoOptions, VerifyOutcome, verify_auto};
use mununu_core::adapter::yosys::{SvFrontend, YosysOptions};

/// The capture register. `din` is an INPUT — that is the whole point.
const DUT: &str = r#"
module past_input (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       push,
    input  logic [3:0] din,
    output logic [3:0] d0_q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)    d0_q <= 4'd0;
        else if (push) d0_q <= din;
    end
endmodule
"#;

/// The same register with the capture DROPPED — the contrast twin. The write
/// still fires, so every control-flow property still holds; only the data is
/// wrong, which is precisely the class `$past` exists to catch and the class that
/// was unverifiable while input bases were rejected.
const DUT_FAULTY: &str = r#"
module past_input (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       push,
    input  logic [3:0] din,
    output logic [3:0] d0_q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)    d0_q <= 4'd0;
        else if (push) d0_q <= 4'd0;         // >>> THE DEFECT <<<
    end
endmodule
"#;

/// Assertions live in a bound module, never inline — the house convention.
const SVA: &str = r#"
module past_input_sva (
    input logic       clk,
    input logic       rst_n,
    input logic       push,
    input logic [3:0] din,
    input logic [3:0] d0_q
);
    a_capture: assert property (
        @(posedge clk) disable iff (!rst_n)
        push |=> d0_q == $past(din)
    );
endmodule

bind past_input past_input_sva u_sva (.*);
"#;

/// Always the DEFAULT posture — the one a user gets from `mununu sv verify-auto`
/// with no engine flag, and the one the pre-filter sits on. Not exact-symbolic:
/// that engine deliberately REFUSES this property class, because it leaves inputs
/// free and a formula that pins an input (`push |=> ...`) would decouple the
/// antecedent from its consequent. It says so and skips rather than guessing.
fn capture_report(dut: &str) -> mununu_core::adapter::slang::verify_auto::AutoVerifyReport {
    let sources = vec![
        ("past_input.sv".to_string(), dut.to_string()),
        ("past_input_sva.sv".to_string(), SVA.to_string()),
    ];
    let yopts = YosysOptions {
        top: Some("past_input".to_string()),
        frontend: SvFrontend::Slang, // never sv2v on an assertion-carrying flow
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    let opts = VerifyAutoOptions::default();
    verify_auto(&sources, &yopts, &opts).expect("verify_auto setup")
}

fn capture_outcome(dut: &str) -> VerifyOutcome {
    let report = capture_report(dut);
    report
        .properties
        .iter()
        .find(|p| p.name.contains("sva_0"))
        .unwrap_or_else(|| {
            panic!(
                "the capture assertion is missing from the report; got {:?} / unsupported {:?}",
                report
                    .properties
                    .iter()
                    .map(|p| &p.name)
                    .collect::<Vec<_>>(),
                report.unsupported
            )
        })
        .outcome
        .clone()
}

#[test]
#[ignore = "requires slang + yosys (mununu-sva image); run with --ignored"]
fn a_past_over_a_primary_input_is_decided_not_skipped() {
    let outcome = capture_outcome(DUT);
    if let VerifyOutcome::Skipped { reason } = &outcome {
        panic!(
            "`$past(din)` over an INPUT came back Skipped ({reason}) — the shadow \
             synthesiser accepts input bases, so this means `verify_auto`'s \
             pre-filter dropped the base before the augmenter ever saw it"
        );
    }
    assert!(
        matches!(outcome, VerifyOutcome::Holds),
        "the capture register does store what was pushed; got {outcome:?}"
    );
}

#[test]
#[ignore = "requires slang + yosys (mununu-sva image); run with --ignored"]
fn the_contrast_twin_does_not_hold_so_the_property_is_not_vacuous() {
    // The gate a vacuous property fails. If `din__past` were unbound the atom
    // would be trivially true and BOTH designs would come back `Holds`; the good
    // one holds and this one does not, so the atom is genuinely constraining.
    //
    // `Unknown` rather than `Violated`, and that is the honest ceiling here, not
    // a weak assertion. The default posture is a may-only OVER-approximation:
    // a definite HOLDS transfers to the RTL, but refuting needs must-edges, and
    // the violation is data-dependent (`d0_q == din__past` is false only for the
    // `din` values the abstraction has no predicate for). Widening the data to
    // 1 bit, raising `max_iterations`, and hinting `din == 0` / `din__past == 0`
    // were all tried: every one leaves it at ⊥. `exact_symbolic` refuses the
    // class outright — it leaves inputs free, so a formula pinning an input
    // would decouple antecedent from consequent, and it says so rather than
    // guessing. Turning on `must_edge_inference` DOES produce `Violated` — and
    // also produces `Violated` for the CORRECT design, and for a register-sourced
    // `$past` on a correct design, so that posture is not usable as an oracle
    // here. See the ticket referenced in the module docs.
    let outcome = capture_outcome(DUT_FAULTY);
    assert!(
        !matches!(outcome, VerifyOutcome::Holds),
        "a dropped capture must not come back HOLDS — that would mean the shadow \
         flop exists but the atom never binds to it; got {outcome:?}"
    );
    assert!(
        !matches!(outcome, VerifyOutcome::Skipped { .. }),
        "and it must still be translated, not skipped; got {outcome:?}"
    );
}

#[test]
#[ignore = "requires slang + yosys (mununu-sva image); run with --ignored"]
fn the_shadow_flop_is_actually_in_the_model() {
    // The structural half of the same claim, independent of any verdict: the
    // model gained a register (the design has one flop, `d0_q`; the model has
    // two), and the translated formula names the shadow.
    let report = capture_report(DUT);
    assert_eq!(
        report.diagnostics.state_register_count, 2,
        "the design's one flop plus the synthesised `din__past` shadow"
    );
    let formula = &report
        .properties
        .iter()
        .find(|p| p.name.contains("sva_0"))
        .expect("the capture assertion")
        .formula;
    assert!(
        formula.contains("din__past"),
        "the atom must bind to the shadow; got `{formula}`"
    );
}

// ---------------------------------------------------------------------------
// When is the invented cycle-0 value OBSERVABLE?
//
// Pinning the shadow's power-up picks one of the 2^n histories the hardware
// could have had, so it matters exactly when a property READS the shadow in the
// initial state — the only state whose shadow was never driven by the design.
//
// The lift answers this structurally. `nu X. (body && [] X)` evaluates `body` at
// every reachable state INCLUDING the initial one, and the two implication forms
// place the consequent differently:
//
//     push |-> d0_q == $past(din)   ->  nu X. ((!push ||    (d0_q == din__past)) && [] X)
//     push |=> d0_q == $past(din)   ->  nu X. ((!push || [] (d0_q == din__past)) && [] X)
//
// Under `[]` the atom is only ever read at a state that HAS a predecessor, where
// the shadow holds a value the design actually drove. Outside it, the atom is
// read at the initial state, against the invented history.
//
// So: the approximation is invisible to the `|=>` shapes — which is every
// data-integrity property, because sampling an antecedent and checking the
// result one cycle later is what `|=>` means — and visible to `|->`.

/// The same property with a same-cycle implication. Nothing else differs.
const SVA_SAME_CYCLE: &str = r#"
module past_input_sva (input logic clk, input logic rst_n, input logic push,
    input logic [3:0] din, input logic [3:0] d0_q);
    a_same: assert property (@(posedge clk) disable iff (!rst_n)
        push |-> d0_q == $past(din));
endmodule
bind past_input past_input_sva u_sva (.*);
"#;

fn formula_for(sva: &str) -> String {
    let sources = vec![
        ("past_input.sv".to_string(), DUT.to_string()),
        ("past_input_sva.sv".to_string(), sva.to_string()),
    ];
    let yopts = YosysOptions {
        top: Some("past_input".to_string()),
        frontend: SvFrontend::Slang,
        additional_sources: sources[1..].to_vec(),
        ..Default::default()
    };
    verify_auto(&sources, &yopts, &VerifyAutoOptions::default())
        .expect("verify_auto setup")
        .properties
        .first()
        .expect("one assertion")
        .formula
        .clone()
}

#[test]
#[ignore = "requires slang + yosys (mununu-sva image); run with --ignored"]
fn only_a_same_cycle_past_reads_the_invented_history() {
    let atom = "(d0_q == din__past)";
    let boxed = format!("[] {atom}");

    let next = formula_for(SVA);
    assert!(
        next.contains(&boxed),
        "a `|=>` property must read the shadow under `[]`, so never in the \
         initial state; got `{next}`"
    );

    let same = formula_for(SVA_SAME_CYCLE);
    assert!(
        same.contains(atom) && !same.contains(&boxed),
        "a `|->` property reads the shadow at the current state, so it DOES see \
         the invented cycle-0 value — that is the boundary of the approximation; \
         got `{same}`"
    );
}
