//! mununu#933 — a resume breadcrumb is tied to the run's inputs, not only to the build.
//!
//! Before the fix a breadcrumb record carried its `build` and nothing that named the design or the
//! options, so `--resume-from` accepted a breadcrumb written for another design (or an edited
//! one) and carried its `holds` / `violated` onto every row with the same name. This test writes a
//! breadcrumb with this binary's build and the input identity of ANOTHER run, and resumes a
//! different design from it: the run must refuse it before the lift, as it refuses another build.
//!
//! One test in its own binary: it registers a process-wide build identity, which no other test in
//! this binary can then observe differently.

use mununu_core::adapter::slang::verify_auto::{VerifyAutoOptions, verify_auto};
use mununu_core::adapter::yosys::YosysOptions;

#[test]
fn a_breadcrumb_written_for_another_design_is_refused_before_the_lift() {
    let sha = "e7a5b21c0ffee0000000000000000000000000000";
    mununu_core::build_info::register(mununu_core::build_info::identity_from(Some(sha), Some("0")));
    // A breadcrumb from a run of some OTHER design: right build, a run identity that is not this
    // run's (no real inputs hash to it), and a row name every annotated design has.
    let crumb = format!(
        "{{\"index\":0,\"property\":\"ann_guarantee_0\",\"outcome\":\"holds\",\"phase\":\"merged\",\
         \"build\":\"{sha}\",\"run\":\"0000000000000000\"}}\n"
    );
    let opts = VerifyAutoOptions {
        resume_from: Some(crumb),
        ..Default::default()
    };
    // Not SystemVerilog: if the resume accepted the breadcrumb, the run would go on to the lift and
    // fail THERE, with a frontend error instead of a refusal.
    let sources = vec![(
        "other_design.sv".to_string(),
        "this is not SystemVerilog".to_string(),
    )];
    let err = verify_auto(&sources, &YosysOptions::default(), &opts)
        .expect_err("a breadcrumb about other inputs must not be resumed from");
    assert!(
        err.message.starts_with("resume-from: ") && err.message.contains("other inputs"),
        "the refusal must be the resume's, naming the input mismatch, not the lift's: {}",
        err.message
    );
}
