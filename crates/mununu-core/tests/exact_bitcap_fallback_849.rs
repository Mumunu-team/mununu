//! mununu#849 — a recoverability row whose cone is over the exact engine's bit cap is refused by
//! the exact member before any witness is attempted, and the member's fallback, the scalable
//! recoverability engine, decides it VIOLATED with no path. The row must say so: a
//! `witness-unavailable` note naming the refusal, and `decided_by` naming the engine that decided,
//! not the exact member's label (the OpenTitan edn endpoint `AG EF Idle` rows under the slang-only
//! lift: VIOLATED, `decided_by: exact-symbolic`, no path, no note).
//!
//! Its own top-level integration test (target `exact_bitcap_fallback_849`), outside
//! `verify_auto.rs` where the fix lives, so `make verify-repro IGNORED=1` builds the parent
//! without the fix; it uses only API the parent already has. `#[ignore]`d: it lifts SystemVerilog
//! through yosys-slang, which CI does not carry; the e2e sweep runs it in mununu-sva (#952).

use mununu_core::adapter::slang::verify_auto::{
    PortfolioMode, VerifyAutoOptions, VerifyOutcome, verify_auto,
};
use mununu_core::adapter::yosys::YosysOptions;

/// Two 100-bit registers that count up together from 0 and 1: they never meet, so
/// `AG EF (data_q == target_q)` is VIOLATED, and the 200-bit cone is over the 192-bit cap.
const WIDE_NEVER_MEET: &str = r#"
module wide_never_meet (input logic clk, input logic rst_n);
    logic [99:0] data_q;
    logic [99:0] target_q;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            data_q <= 100'd0;
            target_q <= 100'd1;
        end else begin
            data_q <= data_q + 100'd1;
            target_q <= target_q + 100'd1;
        end
    end
endmodule
// @mununu_guarantee(wide_never_meet) nu Y. ((mu X. ((data_q == target_q) || <> X)) && [] Y)
"#;

mod exact_bitcap_fallback_849 {
    use super::*;

    #[test]
    #[ignore = "requires the mununu-sva image (yosys + slang + z3); run by the e2e sweep"]
    fn e2e_a_recoverability_row_decided_by_the_fallback_after_an_exact_bit_cap_refusal_says_why_it_ships_no_path()
     {
        let sources = vec![(
            "wide_never_meet.sv".to_string(),
            WIDE_NEVER_MEET.to_string(),
        )];
        let yopts = YosysOptions {
            top: Some("wide_never_meet".to_string()),
            ..Default::default()
        };
        // The CLI's and the API's default engine: the sequential portfolio, exact first.
        let opts = VerifyAutoOptions {
            portfolio: Some(PortfolioMode::Sequential),
            ..Default::default()
        };
        let report = verify_auto(&sources, &yopts, &opts).expect("verify_auto setup");
        let row = report
            .properties
            .iter()
            .find(|p| {
                p.label.as_deref() == Some("wide_never_meet") || p.formula.contains("target_q")
            })
            .unwrap_or_else(|| panic!("no row: {:#?}", report.properties));
        assert!(
            matches!(row.outcome, VerifyOutcome::Violated { .. }),
            "the verdict stands: {:?}",
            row.outcome
        );
        assert!(row.counterexample.is_none(), "the fallback carries no path");
        assert_ne!(
            row.decided_by.as_deref(),
            Some("exact-symbolic"),
            "the exact engine refused this row; the label must name the engine that decided"
        );
        let note = report
            .notes
            .iter()
            .find(|n| n.kind == "witness-unavailable" && n.property.as_deref() == Some(&row.name))
            .unwrap_or_else(|| {
                panic!(
                    "the missing path is explained, not silent; notes: {:#?}",
                    report
                        .notes
                        .iter()
                        .map(|n| (&n.kind, &n.summary))
                        .collect::<Vec<_>>()
                )
            });
        assert!(
            note.detail.contains("bit") && note.detail.contains("recoverability"),
            "the note names the exact refusal and the deciding engine: {}",
            note.detail
        );
    }
}
