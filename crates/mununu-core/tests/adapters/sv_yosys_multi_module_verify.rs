//! R-MM-5b — end-to-end verify of an SV multi-module design through the
//! KMTS (`sv-yosys`) route's multi-module composition branch.
//!
//! Drives `verify_project` on `producer_consumer_top` (producer ⊗
//! bounded_buffer ⊗ consumer) with `adapter = "sv-yosys"` and the source
//! option `multi_module = true`. The orchestrator routes to
//! `compose_sv_multi_module` → `clts_to_ctxdsl`, the composed CTXDSL
//! re-enters the standard parse→realise→evaluate pipeline, and a property
//! atom over an instance-qualified signal (`u_consumer__state == k`) binds
//! to the actual value (R-MM-5b-i made the composed valuations all-numeric).
//!
//! Gated on `yosys` being on PATH; skips cleanly otherwise (mirrors the
//! per-module-BTOR2 integration tests).

use mununu_core::verify::config::VerifyConfig;
use mununu_core::verify::verify_project;
use std::path::Path;

fn yosys_available() -> bool {
    std::process::Command::new("yosys")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn examples_sv_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/systemverilog")
}

/// The verify manifest exercising the sv-yosys multi-module branch. The
/// consumer FSM cycles state 0→1→2; states 1 and 2 are reachable, value 5
/// never occurs. Three properties together prove the instance-qualified
/// atom `u_consumer__state == k` binds to actual values (not a vacuous
/// all-true / all-false fallthrough):
/// - `consumer_state2_here` (bare atom) — holds on a STRICT non-empty
///   subset (the states where the consumer is in state 2).
/// - `consumer_busy_reachable` — reachability holds (state 2 is reachable).
/// - `consumer_state5_unreachable` — reachability of an absent value is
///   empty.
const MANIFEST: &str = r#"
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
multi_module = true
top = "producer_consumer_top"

[alphabet]
strategy = "direct"

[composition]
semantics = "synchronous"
members = ["system"]
name = "MMSystem"

[[properties]]
name = "consumer_state2_here"
formula = "u_consumer__state == 2"
over = "Circuit"

[[properties]]
name = "consumer_busy_reachable"
formula = "mu X. ((u_consumer__state == 2) || (<> X))"
over = "Circuit"

[[properties]]
name = "consumer_state5_unreachable"
formula = "mu X. ((u_consumer__state == 5) || (<> X))"
over = "Circuit"
"#;

#[test]
fn sv_yosys_multi_module_verify_resolves_qualified_atoms() {
    if !yosys_available() {
        eprintln!("skip: yosys not installed");
        return;
    }
    let dir = examples_sv_dir();
    // Skip if the fixtures are not present (some checkouts trim examples).
    if !dir.join("multi_producer_consumer_top.sv").exists() {
        eprintln!("skip: multi-module fixtures not found at {}", dir.display());
        return;
    }

    let config = VerifyConfig::from_toml(MANIFEST).expect("parse verify manifest");
    let report = verify_project(&config, &dir).expect("verify_project");

    let verdict = |name: &str| {
        report
            .property_verdicts
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("missing verdict for '{name}'"))
    };

    // Bare atom — binds to a STRICT non-empty subset of states (those where
    // the consumer is in state 2). This is the direct proof the qualified
    // numeric atom resolves per-state through the composed pipeline: an
    // empty-bitset fallthrough would give 0, an all-true fallthrough would
    // give total_states.
    let here = verdict("consumer_state2_here");
    assert!(
        here.satisfying_states > 0 && here.satisfying_states < here.total_states,
        "u_consumer__state == 2 binds to a strict non-empty subset; got {}/{}",
        here.satisfying_states,
        here.total_states
    );

    // Reachability holds (state 2 is reachable from the initial state).
    let reachable = verdict("consumer_busy_reachable");
    assert!(
        reachable.satisfied(),
        "u_consumer__state == 2 is reachable; satisfying {}/{} states, initial {:?}",
        reachable.satisfying_states,
        reachable.total_states,
        reachable.initial_satisfying
    );

    // Reachability of an absent value is empty — the contrast with the
    // `== 2` cases confirms the bind is value-sensitive, not a constant.
    let unreachable = verdict("consumer_state5_unreachable");
    assert!(
        !unreachable.satisfied(),
        "u_consumer__state == 5 never holds (value unreachable); satisfying {}/{} states",
        unreachable.satisfying_states,
        unreachable.total_states
    );
    assert_eq!(
        unreachable.satisfying_states, 0,
        "no state has u_consumer__state == 5"
    );
}
