//! mununu#1248, mununu#1249 — an `sv-yosys` source with `multi_module = true` is refused by
//! every entry of the verify framework, before anything is read or lifted.
//!
//! The per-module composition lifts each module at its default parameter values and leaves the
//! top module's own logic out, so its verdicts are not about the design. Until both are fixed
//! the option is refused by the project validation, which `verify_project` (the `verify`
//! command and the HTTP route) and `inspect_project` (`verify --print-alphabet`) both start
//! with. This file used to run the composition end to end and read its verdicts; that test
//! comes back with the fix. The composition itself keeps its tests in
//! `adapter/yosys/multi_module.rs`.
//!
//! The refusal needs no tool: it comes before the first file is opened, so the directory given
//! to those tests is empty. The last test follows the remedy the refusal names once, on the
//! example project, and needs a yosys that reads SystemVerilog.

use mununu_core::verify::config::{ConfigIssue, VerifyConfig};
use mununu_core::verify::report::VerifyError;
use mununu_core::verify::{inspect_project, verify_project};
use std::path::Path;

/// The manifest of the multi-module example, as it was run before the refusal.
fn manifest(multi_module: &str) -> String {
    format!(
        r#"
[project]
name = "MMSys"

[[sources]]
id = "system"
adapter = "sv-yosys"
files = [
    "multi_producer_consumer_top.sv",
    "multi_producer.sv",
    "multi_consumer.sv",
    "bounded_buffer.sv",
]

[sources.options]
multi_module = {multi_module}
top = "producer_consumer_top"

[alphabet]
strategy = "direct"

[composition]
semantics = "synchronous"
members = ["system"]
name = "MMSystem"

[[properties]]
name = "consumer_busy_reachable"
formula = "mu X. ((u_consumer__state == 2) || (<> X))"
over = "Circuit"
"#
    )
}

/// Whether `err` is the validation failing with the refusal of `multi_module = true` on the
/// source `system`, and with nothing else.
fn is_the_refusal(err: &VerifyError) -> bool {
    match err {
        VerifyError::ConfigValidationFailed(issues) => matches!(
            issues.as_slice(),
            [ConfigIssue::SourceOptionRefused { source_id, option, .. }]
                if source_id == "system" && *option == "multi_module = true"
        ),
        _ => false,
    }
}

#[test]
fn x1248_verify_refuses_the_per_module_composition_before_reading_a_file() {
    let empty = tempfile::tempdir().expect("a directory");
    let config = VerifyConfig::from_toml(&manifest("true")).expect("the manifest parses");
    let err = verify_project(&config, empty.path()).expect_err("no report: the option is refused");
    assert!(is_the_refusal(&err), "{err:?}");
}

#[test]
fn x1248_the_alphabet_inspection_refuses_it_too() {
    let empty = tempfile::tempdir().expect("a directory");
    let config = VerifyConfig::from_toml(&manifest("true")).expect("the manifest parses");
    let err = inspect_project(&config, empty.path()).expect_err("no inspection of that model");
    assert!(is_the_refusal(&err), "{err:?}");
}

/// The control: with `multi_module = false` the same project passes the validation and goes on
/// to its files — which are not in the empty directory, so the run stops there, on something
/// that is not the refusal.
#[test]
fn x1248_the_flattened_lift_of_the_same_project_is_not_refused() {
    let empty = tempfile::tempdir().expect("a directory");
    let config = VerifyConfig::from_toml(&manifest("false")).expect("the manifest parses");
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    let err = verify_project(&config, empty.path()).expect_err("the files are not there");
    assert!(
        matches!(err, VerifyError::SourceReadFailed { .. }),
        "{err:?}"
    );
}

/// The example project of the option (`examples/verify/sv_multi_module`: a top that instantiates
/// a producer and a consumer), with the option off. The consumer's state is 0, 1 or 2; in the
/// flattened model it is `u_consumer_state`, where the composition called it
/// `u_consumer__state`.
const FLATTENED_EXAMPLE: &str = r#"
[project]
name = "SvMultiModuleFlattened"

[[sources]]
id = "system"
adapter = "sv-yosys"
files = [
    "producer_consumer_system_top.sv",
    "producer.sv",
    "consumer.sv",
]

[sources.options]
multi_module = false
top = "producer_consumer_system_top"

[alphabet]
strategy = "direct"

[composition]
semantics = "synchronous"
members = ["system"]
name = "ProducerConsumerSystem"

[[properties]]
name = "consumer_busy_here"
formula = "u_consumer_state == 1"
over = "Circuit"

[[properties]]
name = "consumer_busy_reachable"
formula = "mu X. ((u_consumer_state == 1) || (<> X))"
over = "Circuit"

[[properties]]
name = "consumer_state3_reachable"
formula = "mu X. ((u_consumer_state == 3) || (<> X))"
over = "Circuit"
"#;

/// The remedy, followed once: with `multi_module = false` the example project is lifted
/// flattened and its properties get verdicts. The three together show the atom reads the
/// consumer's state and is not a constant: it holds at some states and not at others, a value
/// the consumer takes is reachable, and a value it never takes is not.
#[test]
fn x1248_the_remedy_the_refusal_names_gives_the_example_its_verdicts() {
    if !crate::support::slang_lift_available() {
        eprintln!("skip: no yosys that reads SystemVerilog");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/verify/sv_multi_module");
    let config = VerifyConfig::from_toml(FLATTENED_EXAMPLE).expect("the manifest parses");
    let report = verify_project(&config, &dir).expect("the flattened lift is not refused");
    let verdict = |name: &str| {
        report
            .property_verdicts
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("no verdict for `{name}`"))
    };
    let here = verdict("consumer_busy_here");
    assert!(
        here.satisfying_states > 0 && here.satisfying_states < here.total_states,
        "the atom holds at some states and not at all: {}/{}",
        here.satisfying_states,
        here.total_states
    );
    assert!(
        verdict("consumer_busy_reachable").satisfied(),
        "the consumer gets to state 1"
    );
    assert!(
        !verdict("consumer_state3_reachable").satisfied(),
        "the consumer has no state 3"
    );
}
