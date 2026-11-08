//! J4 follow-up (J4b): the merged portfolio report's per-property wall is the SUM over the members
//! that ran the row. Before the fix `merge_portfolio_reports` cloned the FIRST member's report as
//! its base — the exact engine, which runs no cube loop and records no walls — so every real run
//! shipped `wall_ms` absent (found by the first on-prem bundle check, 2026-11-08).
//!
//! An integration test on purpose: `make verify-repro` copies the test-bearing file from HEAD into
//! the parent tree, so a reproducer inside `verify_auto.rs` would carry the fix with it and pass
//! on the parent (the co-location trap of mununu#853). Here the parent gets this file and its own
//! merge, and fails.
use mununu_core::adapter::slang::translate::SvaKind;
use mununu_core::adapter::slang::verify_auto::{
    AutoVerifyReport, PortfolioMode, PropertyVerdict, VerifyOutcome, merge_portfolio_reports,
};

fn report(rows: &[(&str, VerifyOutcome, Option<u64>)]) -> AutoVerifyReport {
    let mut r = AutoVerifyReport {
        properties: rows
            .iter()
            .map(|(name, outcome, _)| PropertyVerdict {
                name: name.to_string(),
                label: None,
                kind: SvaKind::Assert,
                formula: format!("formula::{name}"),
                outcome: outcome.clone(),
                seeded_predicates: Vec::new(),
                counterexample: None,
                bottom_reason: None,
                decided_by: None,
                vacuity: None,
                portfolio: None,
            })
            .collect(),
        ..Default::default()
    };
    for (name, _, ms) in rows {
        if let Some(ms) = ms {
            r.property_wall_ms.insert(name.to_string(), *ms);
        }
    }
    r
}

#[test]
fn the_merged_report_sums_the_members_walls_per_property() {
    // The exact member decides one row and records nothing for the other; the cube member runs
    // both and records both. Neither map alone is the run's cost.
    let exact = report(&[
        ("sva_1", VerifyOutcome::Unknown { unknown_cells: 1 }, None),
        ("sva_2", VerifyOutcome::Holds, Some(40)),
    ]);
    let cube = report(&[
        ("sva_1", VerifyOutcome::Holds, Some(120)),
        ("sva_2", VerifyOutcome::Holds, Some(5)),
    ]);
    let merged = merge_portfolio_reports(
        &[("exact-symbolic", Ok(exact)), ("symbolic", Ok(cube))],
        PortfolioMode::Sequential,
    )
    .expect("merge ok");
    assert_eq!(merged.property_wall_ms.get("sva_1"), Some(&120));
    assert_eq!(merged.property_wall_ms.get("sva_2"), Some(&45));
}

#[test]
fn a_base_member_that_recorded_nothing_does_not_erase_the_others_walls() {
    // The real shape of the defect: the first member's map is empty, the second's is not.
    let empty_base = report(&[("sva_1", VerifyOutcome::Unknown { unknown_cells: 1 }, None)]);
    let only = report(&[("sva_1", VerifyOutcome::Holds, Some(77))]);
    let merged = merge_portfolio_reports(
        &[("exact-symbolic", Ok(empty_base)), ("symbolic", Ok(only))],
        PortfolioMode::Sequential,
    )
    .expect("merge ok");
    assert_eq!(
        merged.property_wall_ms.get("sva_1"),
        Some(&77),
        "the base's empty map must not be the merged report's map"
    );
}
