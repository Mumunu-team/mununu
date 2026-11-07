//! mununu#892: in the full reach portfolio, a `--timeout-ms` raised above the subprocess default
//! reaches the owned members too. Before, it set the subprocess wall and btormc's depth cap only;
//! native BMC / k-induction kept its 5 s per-query default whatever the flag said, and two
//! HWMCC'20 array rows that native decides in under forty seconds of query time stayed ⊥ in a
//! full run given ten minutes.
//!
//! `make verify-repro` reports INCONCLUSIVE for this test: `portfolio_member_timeouts` is the
//! seam this commit introduces, and the budget it replaces was computed inline. The measured
//! reproducer is the box's run of `zipcpu_zipcpu_dcache-p236` / `-p428` recorded on the issue.

use mununu_core::adapter::reach_portfolio::portfolio_member_timeouts;
use std::time::Duration;

const SUBPROCESS_DEFAULT: Duration = Duration::from_secs(60);

#[test]
fn a_raised_timeout_becomes_natives_query_budget_in_the_full_portfolio() {
    let t = portfolio_member_timeouts(None, Duration::from_secs(600));
    assert_eq!(t.native_query_ms, 600_000);
    assert_eq!(t.interp_overall_ms, 600_000);
    assert_eq!(t.subprocess, Duration::from_secs(600));
}

#[test]
fn the_default_wall_leaves_every_budget_as_before() {
    let t = portfolio_member_timeouts(None, SUBPROCESS_DEFAULT);
    assert_eq!(t.native_query_ms, 5_000, "native's per-query default");
    assert_eq!(t.subprocess, SUBPROCESS_DEFAULT);
    let shorter = portfolio_member_timeouts(None, Duration::from_secs(10));
    assert_eq!(
        shorter.native_query_ms, 5_000,
        "a wall below the default raises nothing"
    );
    assert_eq!(shorter.subprocess, Duration::from_secs(10));
}

#[test]
fn a_run_budget_in_effect_still_wins() {
    let t = portfolio_member_timeouts(Some(2_000), Duration::from_secs(600));
    assert_eq!(
        t.native_query_ms, 2_000,
        "the time left bounds every member"
    );
    assert_eq!(t.subprocess, Duration::from_millis(2_000));
}
